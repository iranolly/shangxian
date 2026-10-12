//! 结构化事件流（`events.jsonl` v1，D2 §5）。
//!
//! - 一行一个紧凑 JSON 对象；UTF-8 无 BOM；行尾 `\n`。
//! - 追加前查大小，>2MB 时 `.1 → .2`、当前 → `.1`（最多两份备份）。
//! - 同 `(kind, level, code, subject.id, message)` 的行 10 秒内只写第一条。
//! - 单会话 >10 条/秒：丢弃，并写一条「事件太密集，后面 10 秒不记了」的 warn（最多一次），
//!   随后 10 秒静默。
//! - 写失败绝不 panic、绝不阻塞：本会话记一次错误（调用方转写到 app.log），之后静默丢弃。

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::reason::ReasonCode;
use crate::state::{now_ms, truncate_chars};

/// 事件行格式版本，恒为 1。
pub const EVENT_VERSION: u32 = 1;
/// 当前文件超过这个大小就轮转。
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// 同样的事件 10 秒内只写一条。
pub const DEDUP_WINDOW_MS: i64 = 10_000;
/// 每秒最多写这么多条。
pub const RATE_MAX_PER_SEC: usize = 10;
/// 太密集之后静默这么久。
pub const DENSE_SUPPRESS_MS: i64 = 10_000;
/// 事件 message 最长 200 字符。
pub const MESSAGE_MAX_CHARS: usize = 200;

/// 事件种类（`kind` 字段）。
pub const KIND_APP: &str = "app";
pub const KIND_TUNNEL: &str = "tunnel";
pub const KIND_SERVICE: &str = "service";
pub const KIND_PROTECTION: &str = "protection";
pub const KIND_UPDATE: &str = "update";
pub const KIND_DOCTOR: &str = "doctor";

/// 事件级别（`level` 字段）。
pub const LEVEL_INFO: &str = "info";
pub const LEVEL_WARN: &str = "warn";
pub const LEVEL_ERROR: &str = "error";

/// 事件主体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSubject {
    /// `"app" | "tunnel" | "service" | "server"`。
    pub r#type: String,
    pub id: String,
    pub name: String,
}

impl EventSubject {
    /// 应用级事件的主体：`{"type":"app","id":"app","name":"上线助手"}`。
    pub fn app() -> Self {
        Self {
            r#type: "app".to_string(),
            id: "app".to_string(),
            name: "上线助手".to_string(),
        }
    }

    /// 通用构造。
    pub fn new(kind: &str, id: &str, name: &str) -> Self {
        Self {
            r#type: kind.to_string(),
            id: id.to_string(),
            name: name.to_string(),
        }
    }
}

/// 一行事件（`events.jsonl` 的行 schema，冻结）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLine {
    /// 行格式版本，恒为 1。
    pub v: u32,
    /// epoch ms。
    pub ts: i64,
    /// `"app" | "tunnel" | "service" | "protection" | "update" | "doctor"`。
    pub kind: String,
    /// `"info" | "warn" | "error"`。
    pub level: String,
    /// §7 的 code；没有就是 null。
    pub code: Option<String>,
    /// 主体；应用级事件为 [`EventSubject::app`]。
    pub subject: Option<EventSubject>,
    /// 大白话中文短句，≤200 字符。
    pub message: String,
    /// 可选附加数据（机器用）。禁止放密码哈希、token、密钥内容、明文密码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl EventLine {
    /// 造一行（ts = 当前时间，message 自动截断到 200 字符）。
    pub fn new(
        kind: &str,
        level: &str,
        code: Option<ReasonCode>,
        subject: Option<EventSubject>,
        message: impl AsRef<str>,
    ) -> Self {
        Self {
            v: EVENT_VERSION,
            ts: now_ms(),
            kind: kind.to_string(),
            level: level.to_string(),
            code: code.map(|c| c.as_str().to_string()),
            subject,
            message: truncate_chars(message.as_ref(), MESSAGE_MAX_CHARS),
            data: None,
        }
    }

    /// 挂一段机器用附加数据。
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// 用固定时间造一行（单测用）。
    pub fn at(
        ts: i64,
        kind: &str,
        level: &str,
        code: Option<ReasonCode>,
        subject: Option<EventSubject>,
        message: impl AsRef<str>,
    ) -> Self {
        let mut line = Self::new(kind, level, code, subject, message);
        line.ts = ts;
        line
    }

    /// 去抖用的键：`(kind, level, code, subject.id, message)`。
    fn dedup_key(&self) -> String {
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
            self.kind,
            self.level,
            self.code.as_deref().unwrap_or(""),
            self.subject.as_ref().map(|s| s.id.as_str()).unwrap_or(""),
            self.message
        )
    }
}

struct Inner {
    /// 最近写过的事件键与时间（去抖）。
    recent: VecDeque<(String, i64)>,
    /// 当前这一秒写了几条。
    second_start: i64,
    second_count: usize,
    /// 太密集后的静默截止时刻。
    dense_until: i64,
    /// 本会话是否已经写过「太密集」那条警告。
    dense_warned: bool,
    /// 本会话是否已经上报过写失败。
    write_error_taken: bool,
    /// 最近一次写盘失败原因。
    last_error: Option<String>,
    /// 本次启动以来成功写入的条数（内存计数，不持久化）。
    written: u64,
}

/// 事件日志：自带锁，可跨线程共享（`Arc<EventLog>`）。
pub struct EventLog {
    path: PathBuf,
    max_bytes: u64,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for EventLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLog")
            .field("path", &self.path)
            .field("max_bytes", &self.max_bytes)
            .finish_non_exhaustive()
    }
}

impl EventLog {
    /// 事件日志固定放在 `<dataDir>/events.jsonl`。
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("events.jsonl"),
            max_bytes: MAX_FILE_BYTES,
            inner: Mutex::new(Inner {
                recent: VecDeque::new(),
                second_start: 0,
                second_count: 0,
                dense_until: 0,
                dense_warned: false,
                write_error_taken: false,
                last_error: None,
                written: 0,
            }),
        }
    }

    /// 当前文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 第 n 份轮转备份的路径（1 = `.1`，2 = `.2`）；其他值返回 None。
    pub fn archive_path(&self, archive: u8) -> Option<PathBuf> {
        match archive {
            1 | 2 => {
                let mut raw = self.path.as_os_str().to_os_string();
                raw.push(format!(".{archive}"));
                Some(PathBuf::from(raw))
            }
            _ => None,
        }
    }

    /// 本次启动以来成功写入的事件条数（`stats.eventsSinceStart` 用）。
    pub fn events_since_start(&self) -> u64 {
        self.lock().written
    }

    /// 取走一次写盘失败原因（调用方转写到 app.log；同一会话只报一次）。
    pub fn take_write_error(&self) -> Option<String> {
        let mut inner = self.lock();
        if inner.write_error_taken {
            return None;
        }
        let err = inner.last_error.clone()?;
        inner.write_error_taken = true;
        Some(err)
    }

    /// 追加一行：轮转 + 去抖 + 密度护栏 + 失败静默。
    pub fn append(&self, line: EventLine) {
        let mut inner = self.lock();
        let now = line.ts;

        // 太密集静默期内：直接丢
        if now < inner.dense_until {
            return;
        }

        // 每秒密度护栏（>10 条/秒 → 丢 + 写一次警告 + 静默 10 秒）
        if now - inner.second_start >= 1_000 {
            inner.second_start = now;
            inner.second_count = 0;
        }
        if inner.second_count >= RATE_MAX_PER_SEC {
            inner.second_count = inner.second_count.saturating_add(1);
            inner.dense_until = now + DENSE_SUPPRESS_MS;
            if !inner.dense_warned {
                inner.dense_warned = true;
                let warn = EventLine::at(
                    now,
                    KIND_APP,
                    LEVEL_WARN,
                    None,
                    Some(EventSubject::app()),
                    "事件太密集，后面 10 秒不记了",
                );
                let _ = self.write_line(&mut inner, &warn);
            }
            return;
        }

        // 去抖：同样的事件 10 秒内只写第一条
        let key = line.dedup_key();
        while inner
            .recent
            .front()
            .map(|(_, ts)| now - *ts >= DEDUP_WINDOW_MS)
            .unwrap_or(false)
        {
            inner.recent.pop_front();
        }
        if inner
            .recent
            .iter()
            .any(|(k, ts)| *k == key && now - *ts < DEDUP_WINDOW_MS)
        {
            return;
        }
        inner.recent.push_back((key, now));
        inner.second_count = inner.second_count.saturating_add(1);

        // 轮转检查（失败不阻塞，下一条再试）
        self.rotate_if_needed(&mut inner);
        let _ = self.write_line(&mut inner, &line);
    }

    /// 读最近的 `lines` 行。`archive`：0 = 当前文件，1 = `.1`，2 = `.2`。    /// `v > 1` 的行跳过、解析失败的行跳过，都不报错（也不中断）。
    pub fn read(&self, lines: usize, archive: u8) -> Vec<EventLine> {
        let path = if archive == 0 {
            self.path.clone()
        } else {
            match self.archive_path(archive) {
                Some(path) => path,
                None => return Vec::new(),
            }
        };
        let Ok(text) = fs::read_to_string(path) else {
            return Vec::new();
        };
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines);
        all[start..]
            .iter()
            .filter_map(|raw| serde_json::from_str::<EventLine>(raw).ok())
            .filter(|line| line.v == EVENT_VERSION)
            .collect()
    }

    // ---------- 内部 ----------

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 轮转：当前文件 >2MB 时 `.1 → .2`、当前 → `.1`。改名失败不阻塞。
    fn rotate_if_needed(&self, _inner: &mut Inner) {
        let size = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size <= self.max_bytes {
            return;
        }
        let a1 = match self.archive_path(1) {
            Some(path) => path,
            None => return,
        };
        let a2 = match self.archive_path(2) {
            Some(path) => path,
            None => return,
        };
        let _ = fs::remove_file(&a2);
        let _ = fs::rename(&a1, &a2);
        let _ = fs::rename(&self.path, &a1);
    }

    /// 追加一行紧凑 JSON + `\n` 并 flush。失败只记错误。
    fn write_line(&self, inner: &mut Inner, line: &EventLine) -> std::io::Result<()> {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            let mut text = serde_json::to_string(line)?;
            text.push('\n');
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            file.write_all(text.as_bytes())?;
            file.flush()
        })();
        match &result {
            Ok(()) => {
                inner.written = inner.written.saturating_add(1);
                inner.last_error = None;
            }
            Err(e) => {
                inner.last_error = Some(format!("事件日志写不进去：{e}"));
            }
        }
        result
    }
}

// ---------- 事件发射帮手（文案/编码在这里定死，接线方直接调） ----------

/// 读配置后把「迁移成功」「归一化」写成事件（D2 §16）；接线方在 `load_detailed` 后调一次。
pub fn emit_config_load_events(log: &EventLog, outcome: &crate::config::LoadOutcome) {
    if let Some(from) = outcome.migrated_from {
        let line = EventLine::new(
            KIND_APP,
            LEVEL_INFO,
            Some(ReasonCode::ConfigMigrated),
            Some(EventSubject::app()),
            ReasonCode::ConfigMigrated.text(),
        )
        .with_data(serde_json::json!({ "migratedFrom": from }));
        log.append(line);
    }
    for field in &outcome.normalized {
        log.append(EventLine::new(
            KIND_APP,
            LEVEL_WARN,
            Some(ReasonCode::ConfigNormalized),
            Some(EventSubject::app()),
            ReasonCode::ConfigNormalized.render(&[("字段", field.as_str())]),
        ));
    }
}

/// 降级拦截事件（config 版本高于程序支持，D2 §13.2）。
pub fn emit_config_unsupported(log: &EventLog, found: u32) {
    let line = EventLine::new(
        KIND_APP,
        LEVEL_ERROR,
        Some(ReasonCode::ConfigUnsupported),
        Some(EventSubject::app()),
        crate::config::config_unsupported_message(found),
    )
    .with_data(serde_json::json!({
        "found": found,
        "supported": crate::config::CONFIG_VERSION,
    }));
    log.append(line);
}

/// `state.json` 损坏/版本过高重置后的事件（D2 §4.2）。
pub fn emit_state_reset(log: &EventLog, reason: crate::state::StateResetReason) {
    log.append(EventLine::new(
        KIND_APP,
        LEVEL_WARN,
        Some(ReasonCode::StateReset),
        Some(EventSubject::app()),
        reason.message(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_760_000_000_000;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sx-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tunnel_line(ts: i64, code: ReasonCode, message: &str) -> EventLine {
        EventLine::at(
            ts,
            KIND_TUNNEL,
            LEVEL_WARN,
            Some(code),
            Some(EventSubject::new("tunnel", "tun-7a8b9c", "相册发布")),
            message,
        )
    }

    #[test]
    fn event_line_round_trips() {
        let line = EventLine::at(
            1_760_000_000_000,
            KIND_PROTECTION,
            LEVEL_WARN,
            Some(ReasonCode::ProtectionDrift),
            Some(EventSubject::new("tunnel", "tun-7a8b9c", "相册发布")),
            "云端设置和你的设置对不上：云端没开密码",
        )
        .with_data(serde_json::json!({"driftFields": ["password"]}));
        let text = serde_json::to_string(&line).unwrap();
        let back: EventLine = serde_json::from_str(&text).unwrap();
        assert_eq!(line, back);
        assert_eq!(back.v, 1);
        assert_eq!(back.code.as_deref(), Some("protectionDrift"));
        assert_eq!(back.data.unwrap()["driftFields"][0], "password");

        // 字段名符合契约（camelCase），data 没有就不出现
        assert!(text.contains("\"subject\""));
        let minimal = EventLine::at(
            1,
            KIND_APP,
            LEVEL_INFO,
            None,
            Some(EventSubject::app()),
            "应用已启动",
        );
        let minimal_text = serde_json::to_string(&minimal).unwrap();
        assert!(!minimal_text.contains("\"data\""));
        assert!(minimal_text.contains("\"code\":null"));
        assert_eq!(minimal.subject.unwrap().r#type, "app");
    }

    #[test]
    fn message_is_truncated_to_200_chars() {
        let long = "字".repeat(300);
        let line = EventLine::at(1, KIND_APP, LEVEL_INFO, None, None, &long);
        assert_eq!(line.message.chars().count(), 200);
    }

    #[test]
    fn append_dedups_within_ten_seconds() {
        let dir = temp_dir("dedup");
        let log = EventLog::new(&dir);

        log.append(tunnel_line(T0, ReasonCode::RemotePortBusy, "云端端口被占了"));
        log.append(tunnel_line(T0, ReasonCode::RemotePortBusy, "云端端口被占了"));
        assert_eq!(log.read(100, 0).len(), 1, "10 秒内同样的事件只写第一条");

        // 换消息 → 写
        log.append(tunnel_line(T0, ReasonCode::AuthDenied, "登录被拒绝"));
        assert_eq!(log.read(100, 0).len(), 2);

        // 过了 10 秒，同样的消息又能写
        log.append(tunnel_line(
            T0 + DEDUP_WINDOW_MS,
            ReasonCode::RemotePortBusy,
            "云端端口被占了",
        ));
        assert_eq!(log.read(100, 0).len(), 3);
        assert_eq!(log.events_since_start(), 3);
    }

    #[test]
    fn rate_guard_suppresses_and_warns_once() {
        let dir = temp_dir("dense");
        let log = EventLog::new(&dir);

        // 同一秒内 15 条各不相同的消息：前 10 条写下，之后触发护栏
        for i in 0..15 {
            log.append(tunnel_line(
                T0,
                ReasonCode::ConnReset,
                &format!("第 {i} 条不同的话"),
            ));
        }
        let lines = log.read(100, 0);
        assert_eq!(lines.len(), 11, "10 条正常 + 1 条「太密集」警告");
        assert_eq!(lines[10].message, "事件太密集，后面 10 秒不记了");
        assert_eq!(lines[10].level, LEVEL_WARN);

        // 静默期内新消息不写
        log.append(tunnel_line(
            T0 + 500,
            ReasonCode::AuthDenied,
            "静默期里的话",
        ));
        assert_eq!(log.read(100, 0).len(), 11);

        // 静默期过后恢复
        log.append(tunnel_line(
            T0 + 500 + DENSE_SUPPRESS_MS,
            ReasonCode::AuthDenied,
            "恢复后的话",
        ));
        assert_eq!(log.read(100, 0).len(), 12);
    }

    #[test]
    fn read_skips_future_versions_and_garbage() {
        let dir = temp_dir("read");
        let path = dir.join("events.jsonl");
        let good = EventLine::at(
            1_760_000_000_000,
            KIND_APP,
            LEVEL_INFO,
            Some(ReasonCode::AppStarted),
            Some(EventSubject::app()),
            "应用已启动",
        );
        let mut v2 = good.clone();
        v2.v = 2;
        let text = format!(
            "{}\n这不是 json\n{}\n",
            serde_json::to_string(&good).unwrap(),
            serde_json::to_string(&v2).unwrap()
        );
        std::fs::write(&path, text).unwrap();

        let log = EventLog::new(&dir);
        let lines = log.read(100, 0);
        assert_eq!(lines.len(), 1, "v:2 和坏行都要跳过，不报错");
        assert_eq!(lines[0].message, "应用已启动");

        // 读不存在的归档 → 空，不报错
        assert!(log.read(100, 1).is_empty());
        assert!(log.read(100, 9).is_empty());
    }

    #[test]
    fn rotation_keeps_two_backups() {
        let dir = temp_dir("rotate");
        let mut log = EventLog::new(&dir);
        log.max_bytes = 100; // 单测里调小，方便触发

        let a1 = log.archive_path(1).unwrap();
        let a2 = log.archive_path(2).unwrap();
        assert_eq!(a1.file_name().unwrap(), "events.jsonl.1");
        assert_eq!(a2.file_name().unwrap(), "events.jsonl.2");

        // 写满超过 100 字节：第一条触发轮转（把 .1 挤到 .2）
        for i in 0..3 {
            log.append(tunnel_line(
                T0,
                ReasonCode::ConnReset,
                &format!("轮转测试第 {i} 条，写长一点，把文件撑到 100 字节以上"),
            ));
        }
        assert!(log.path().exists(), "当前文件还在");
        assert!(a1.exists(), ".1 备份要存在");
        // .1 里能看到旧内容（第一条）
        let old = log.read(100, 1);
        assert!(!old.is_empty());
        assert!(!log.read(100, 0).is_empty());

        // 再撑一轮：.2 出现
        for i in 0..3 {
            log.append(tunnel_line(
                T0,
                ReasonCode::AuthDenied,
                &format!("第二轮第 {i} 条，同样写很长很长很长很长很长很长很长"),
            ));
        }
        assert!(log.archive_path(2).unwrap().exists(), ".2 备份要存在");
    }

    #[test]
    fn read_respects_line_limit_and_order() {
        let dir = temp_dir("limit");
        let log = EventLog::new(&dir);
        for i in 0..5 {
            log.append(tunnel_line(
                T0 + i as i64 * (DEDUP_WINDOW_MS + 1),
                ReasonCode::ConnReset,
                &format!("第 {i} 条"),
            ));
        }
        let all = log.read(100, 0);
        assert_eq!(all.len(), 5);
        assert_eq!(all.first().unwrap().message, "第 0 条", "文件顺序：旧的在前");
        assert_eq!(all.last().unwrap().message, "第 4 条");
        let tail = log.read(2, 0);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail.last().unwrap().message, "第 4 条");
    }

    #[test]
    fn config_load_events_use_frozen_copy() {
        let dir = temp_dir("emit-config");
        let log = EventLog::new(&dir);
        crate::events::emit_config_load_events(
            &log,
            &crate::config::LoadOutcome {
                migrated_from: Some(5),
                normalized: vec!["tunnels[].preset".to_string()],
            },
        );
        let lines = log.read(10, 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].code.as_deref(), Some("configMigrated"));
        assert_eq!(lines[0].data.clone().unwrap()["migratedFrom"], 5);
        assert_eq!(lines[1].code.as_deref(), Some("configNormalized"));
        assert_eq!(
            lines[1].message,
            "设置里有不认识的取值，已经按默认值处理：tunnels[].preset"
        );

        let dir2 = temp_dir("emit-unsupported");
        let log2 = EventLog::new(&dir2);
        crate::events::emit_config_unsupported(&log2, 7);
        let lines = log2.read(10, 0);
        assert_eq!(lines[0].level, LEVEL_ERROR);
        assert_eq!(lines[0].data.clone().unwrap()["supported"], 6);
        assert_eq!(
            crate::state::StateResetReason::Corrupt.code(),
            ReasonCode::StateReset
        );
    }
}
