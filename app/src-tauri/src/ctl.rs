//! 控制通道服务端：后台线程监听本机回环端口，把 CLI / 别的程序的请求交给 Guardian。
//!
//! 跟界面用的是同一份状态（同一个 Guardian + 同一份 state/events），所以命令行改完，
//! 界面立刻能看到，反过来也一样。协议见 `shangxian_core::ctl`。
//!
//! 这里同时是 Tauri 命令的共用实现层：界面命令只做“拼一个 [`Ctx`] 然后调
//! [`dispatch`]”，避免同一套逻辑写两遍。
//!
//! **锁纪律**：要联网的 SSH 操作（检查云端、测连接、配免密、体检）一律
//! 「短锁克隆连接信息 → 放锁跑 → 短锁写回」；界面 800ms 快照和看护 150ms 轮询
//! 都在等这把全局锁，占着就是界面卡死。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use shangxian_core::cloudcheck::{self, CheckInput, CheckMode, CloudEnv, ScriptInput};
use shangxian_core::config::{AppConfig, default_data_dir};
use shangxian_core::doctor::{DoctorMode, DoctorRequest, DoctorService, DoctorTunnel};
use shangxian_core::ctl as core_ctl;
use shangxian_core::events::{EventLine, EventLog, EventSubject};
use shangxian_core::guardian::{
    Guardian, NewTunnelInput, ServerInput, ServiceInput, StopOutcome, UpdateTunnelInput,
};
use shangxian_core::log::{TAG_CTL, log_info, log_warn};
use shangxian_core::reason::ReasonCode;
use shangxian_core::state::{
    self, ExpectedProtection, ProtectionCheck, StateStore, UpdateInfo,
};

/// 一行请求最多 64KB（防着有人塞个超大 JSON 把内存撑爆）。
const MAX_LINE_BYTES: usize = 64 * 1024;
/// 连上之后最多 10 秒要把请求发完（防着连上就不说话）。
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 同时在处理的连接数上限（一条连接一个线程，不能让人无限开）。
const MAX_CONNECTIONS: usize = 8;
/// `logs_query` 一次最多要多少行。
const LOGS_MAX_LINES: usize = 5000;
/// `logs_query` 合并来源的总条数上限（D2 §5.4）。
const LOGS_TOTAL_CAP: usize = 2000;
/// 诊断包大小上限（D2 §12.1）。
const DIAG_MAX_BYTES: usize = 20 * 1024 * 1024;
/// 更新检查地址（D0 §15 / D5 §7）。
pub const UPDATE_API: &str = "https://api.github.com/repos/iranolly/shangxian/releases/latest";
pub const UPDATE_PAGE: &str = "https://github.com/iranolly/shangxian/releases/latest";

// ============================================================
// 运行上下文：应用启动时拼一次，控制通道 / Tauri 命令共用
// ============================================================

/// 一条命令要用到的全部共享状态（`Arc` 克隆很便宜，可以直接丢进 `spawn_blocking`）。
#[derive(Clone)]
pub(crate) struct Ctx {
    pub guardian: Arc<Mutex<Guardian>>,
    pub state: Arc<StateStore>,
    pub events: Arc<EventLog>,
    pub data_dir: PathBuf,
    /// 本次应用启动时刻（epoch ms）。
    pub started_at: i64,
    /// 启动自检：子进程保底清理可用吗（D2 §6.5）。
    pub job_guard_ok: bool,
    pub job_guard_error: Option<String>,
}

impl Ctx {
    /// 短锁拿 Guardian。
    pub(crate) fn lock_guardian(&self) -> Result<std::sync::MutexGuard<'_, Guardian>, String> {
        self.guardian
            .lock()
            .map_err(|_| "内部状态锁已损坏：请重启应用".to_string())
    }

    /// 配置能不能安全写回（界面提示用）。
    pub(crate) fn config_trusted(&self) -> bool {
        self.lock_guardian()
            .map(|g| g.config_trusted())
            .unwrap_or(false)
    }
}

fn now_ms() -> i64 {
    state::now_ms()
}

fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// 把时间戳渲染成本机时间的 `HH:MM:SS`（用 core 的时区偏移，不引时间库）。
fn hhmmss(ts_ms: i64) -> String {
    let total = ts_ms.div_euclid(1000) + state::local_utc_offset_minutes() * 60;
    let s = total.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

// ============================================================
// 控制通道服务端
// ============================================================

/// 连接名额：拿到就占一个，线程结束（哪怕 panic）自动还回去。
struct ConnSlot(Arc<AtomicUsize>);

impl ConnSlot {
    fn try_acquire(live: &Arc<AtomicUsize>) -> Option<Self> {
        if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            live.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(live.clone()))
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 开控制通道：绑定随机端口 → 写 ctl.json → 等连接。
pub(crate) fn spawn(ctx: Ctx) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(l) => l,
            Err(e) => {
                log_warn(TAG_CTL, &format!("控制通道启动失败，命令行无法使用：{e}"));
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(_) => return,
        };
        let info = core_ctl::CtlInfo {
            port,
            token: core_ctl::new_token(),
            pid: std::process::id(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        if let Err(e) = core_ctl::write_ctl_info(&ctx.data_dir, &info) {
            log_warn(TAG_CTL, &format!("写入 ctl.json 失败，命令行无法使用：{e}"));
            return;
        }
        log_info(TAG_CTL, &format!("控制通道已开启：127.0.0.1:{port}"));

        let live = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // 名额满了就不接了，回一句人话把人打发走，别把线程数撑爆。
            let Some(slot) = ConnSlot::try_acquire(&live) else {
                let _ = reply(
                    &stream,
                    &json!({"id": 0, "ok": false, "error": format!("同时处理的连接过多（最多 {MAX_CONNECTIONS} 条），请稍后重试")}),
                );
                continue;
            };
            let ctx = ctx.clone();
            let token = info.token.clone();
            std::thread::spawn(move || {
                let _slot = slot; // 线程结束（哪怕 panic）自动把名额还回去
                if let Err(e) = handle(stream, &ctx, &token) {
                    log_warn(TAG_CTL, &format!("一条命令行请求处理失败：{e}"));
                }
            });
        }
    });
}

/// 一条连接 = 一条请求。
fn handle(stream: TcpStream, ctx: &Ctx, token: &str) -> std::io::Result<()> {
    // 读请求既限时也限量：连上一直不发、或者发一行没边的巨型 JSON，都不能拖住应用。
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    let mut limited = reader.by_ref().take((MAX_LINE_BYTES + 1) as u64);
    match limited.read_line(&mut line) {
        Ok(0) => return Ok(()), // 连上了但什么都没发
        Ok(_) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            return reply(
                &stream,
                &json!({"id": 0, "ok": false, "error": "读取请求超时（已连接但未发送请求）"}),
            );
        }
        Err(e) => return Err(e),
    }
    if line.len() > MAX_LINE_BYTES {
        return reply(
            &stream,
            &json!({"id": 0, "ok": false, "error": "请求过长：每行最多 64KB"}),
        );
    }

    let req: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => {
            return reply(
                &stream,
                &json!({"id": 0, "ok": false, "error": format!("无法解析请求：{e}")}),
            );
        }
    };
    let id = req.get("id").cloned().unwrap_or(json!(1));
    if req.get("token").and_then(|v| v.as_str()) != Some(token) {
        return reply(
            &stream,
            &json!({"id": id, "ok": false, "error": "通行证不正确（ctl.json 可能已过期，重启应用即可）"}),
        );
    }
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));
    let result = dispatch(ctx, method, &params);
    reply_result(&stream, &id, result)
}

/// 统一的回包格式：成功给 result，失败给 error，id 原样带回。
fn reply_result(
    stream: &TcpStream,
    id: &Value,
    result: Result<Value, String>,
) -> std::io::Result<()> {
    match result {
        Ok(v) => reply(stream, &json!({"id": id, "ok": true, "result": v})),
        Err(e) => reply(stream, &json!({"id": id, "ok": false, "error": e})),
    }
}

fn reply(stream: &TcpStream, value: &Value) -> std::io::Result<()> {
    let mut s = stream;
    s.write_all(value.to_string().as_bytes())?;
    s.write_all(b"\n")?;
    s.flush()
}

// ============================================================
// 参数小工具
// ============================================================

fn need_str(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

fn opt_str(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
}

fn need_bool(params: &Value, key: &str) -> Result<bool, String> {
    params
        .get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("缺少参数 {key}（要 true/false）"))
}

/// 服务器 id：标准写法是 `serverId`，也兼容 `id`（各调用方写法不一，认就行）。
fn need_server_id(params: &Value) -> Result<String, String> {
    params
        .get("serverId")
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "缺少参数 serverId".to_string())
}

fn opt_usize(params: &Value, key: &str, fallback: usize) -> usize {
    params
        .get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(fallback)
}

fn from_params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, String> {
    serde_json::from_value(params.clone()).map_err(|e| format!("参数不对：{e}"))
}

// ============================================================
// 日志读取（logs_query / build_diag 共用）
// ============================================================

/// 读文件尾巴（按行）。文件不存在返回空。
fn read_tail_lines(path: &Path, lines: usize) -> Vec<String> {
    const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let text = {
        let mut reader = std::io::BufReader::new(file).take(MAX_READ_BYTES);
        let mut buf = String::new();
        if reader.read_to_string(&mut buf).is_err() {
            return Vec::new();
        }
        buf
    };
    let mut all: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if all.len() > lines {
        all.drain(0..all.len() - lines);
    }
    all
}

/// 轮转备份文件路径：`x.log` + archive=1 → `x.log.1`。
fn archive_path(path: &Path, archive: u8) -> PathBuf {
    if archive == 0 {
        return path.to_path_buf();
    }
    PathBuf::from(format!("{}.{}", path.display(), archive))
}

/// 我们的日志行形如 `[1760000000000] 正文`；返回 (时间戳, 正文)。
fn split_epoch_prefix(line: &str) -> (Option<i64>, &str) {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix('[') else {
        return (None, line);
    };
    let Some(end) = rest.find(']') else {
        return (None, line);
    };
    let digits = &rest[..end];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return (None, line);
    }
    match digits.parse::<i64>() {
        Ok(ts) => (Some(ts), rest[end + 1..].trim_start()),
        Err(_) => (None, line),
    }
}

/// 从日志行里认级别：`[INFO]` / `[WARN]` / `[ERROR]`（认不出给 None）。
fn detect_level(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("[error]") {
        Some("error".into())
    } else if lower.contains("[warn]") {
        Some("warn".into())
    } else if lower.contains("[info]") {
        Some("info".into())
    } else {
        None
    }
}

/// 拼一条 LogEntry（D2 §15.3 形状）。
#[allow(clippy::too_many_arguments)]
fn log_entry(
    ts: Option<i64>,
    source: &str,
    origin: &str,
    level: Option<String>,
    code: Option<String>,
    subject: Option<Value>,
    message: Option<String>,
    raw: String,
) -> Value {
    json!({
        "ts": ts,
        "source": source,
        "origin": origin,
        "level": level,
        "code": code,
        "subject": subject,
        "message": message,
        "raw": raw,
    })
}

/// 事件行 → LogEntry。
fn event_entry(line: &shangxian_core::events::EventLine) -> Value {
    let source = match line.kind.as_str() {
        "tunnel" => "tunnel",
        "service" => "service",
        _ => "app",
    };
    let origin_label = match (source, line.subject.as_ref()) {
        ("tunnel", Some(s)) => format!("发布:{}", s.name),
        ("service", Some(s)) => format!("程序:{}", s.name),
        _ => "应用".to_string(),
    };
    let raw = format!("[{}] [{}] {}", hhmmss(line.ts), origin_label, line.message);
    log_entry(
        Some(line.ts),
        source,
        "events",
        Some(line.level.clone()),
        line.code.clone(),
        line.subject.as_ref().map(to_json),
        Some(line.message.clone()),
        raw,
    )
}

/// 原始日志行 → LogEntry。
fn raw_log_entry(line: &str, source: &str, origin: &str) -> Value {
    let (ts, body) = split_epoch_prefix(line);
    let level = detect_level(body);
    let display = if line.starts_with('[') && ts.is_some() {
        // 带我们自己的毫秒前缀：正文就是给人看的行
        body.to_string()
    } else {
        line.to_string()
    };
    log_entry(ts, source, origin, level, None, None, None, display)
}

/// 隧道日志文件路径（复用 core 的命名规则，保证跟写入侧一致）。
fn tunnel_log_path(ctx: &Ctx, id: &str) -> PathBuf {
    let logs = ctx.data_dir.join("logs");
    let view = ctx
        .lock_guardian()
        .ok()
        .and_then(|g| g.tunnel_snapshot().into_iter().find(|t| t.id == id));
    let key = view
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_else(|| id.to_string());
    shangxian_core::supervisor::service_log_path(&logs, &key)
}

/// 服务日志文件路径：先按 core 的命名找，找不到就按 id 扫目录兜底。
fn service_log_path(ctx: &Ctx, id: &str) -> Option<PathBuf> {
    let logs = ctx.data_dir.join("logs");
    let key = ctx
        .lock_guardian()
        .ok()
        .and_then(|g| g.snapshot().into_iter().find(|s| s.id == id))
        .map(|s| s.id)
        .unwrap_or_else(|| id.to_string());
    let direct = logs.join(format!("service-{key}.log"));
    if direct.exists() {
        return Some(direct);
    }
    // id 里不含路径分隔符，直接扫目录按名字包含匹配（文件名里带 id）。
    let entries = std::fs::read_dir(&logs).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_service_log = name.starts_with("service-") && name.contains(".log");
        if is_service_log && name.contains(&key) {
            return Some(entry.path());
        }
    }
    None
}

/// 一条来源的读取结果。
struct SourceLog {
    id: String,
    entries: Vec<Value>,
}

impl SourceLog {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            entries: Vec::new(),
        }
    }
}

/// `logs_query` 的真正实现。
fn logs_query(ctx: &Ctx, params: &Value) -> Result<Value, String> {
    let source = opt_str(params, "source").unwrap_or_else(|| "all".to_string());
    let source = match source.as_str() {
        "all" | "tunnel" | "service" | "app" => source,
        other => {
            return Err(format!(
                "source 仅支持 all / tunnel / service / app，不支持「{other}」"
            ));
        }
    };
    let lines = opt_usize(params, "lines", 200).clamp(1, LOGS_MAX_LINES);
    let archive = opt_usize(params, "archive", 0);
    if archive > 2 {
        return Err("--archive 只能是 0、1、2；0 = 仅查看当前文件".to_string());
    }
    let archive = archive as u8;
    let format = opt_str(params, "format").unwrap_or_else(|| "lines".to_string());
    let id = opt_str(params, "id");

    let mut groups: Vec<SourceLog> = Vec::new();
    let mut truncated = false;

    match source.as_str() {
        "app" => {
            // events + app.log
            let mut events_group = SourceLog::new("events");
            for line in ctx.events.read(lines, archive) {
                events_group.entries.push(event_entry(&line));
            }
            if source_hit_limit(&events_group, lines) {
                truncated = true;
            }
            groups.push(events_group);
            let app_log = ctx.data_dir.join("logs/app.log");
            let mut app_group = SourceLog::new("app");
            for line in read_tail_lines(&archive_path(&app_log, archive), lines) {
                app_group
                    .entries
                    .push(raw_log_entry(&line, "app", "appLog"));
            }
            if source_hit_limit(&app_group, lines) {
                truncated = true;
            }
            groups.push(app_group);
        }
        "tunnel" => {
            let id =
                id.ok_or_else(|| "查看隧道日志需要指定 id（logs_query 的 source=tunnel）".to_string())?;
            let path = tunnel_log_path(ctx, &id);
            let mut group = SourceLog::new(&format!("tunnel:{id}"));
            for line in read_tail_lines(&archive_path(&path, archive), lines) {
                group.entries.push(raw_log_entry(&line, "tunnel", "tunnel"));
            }
            if source_hit_limit(&group, lines) {
                truncated = true;
            }
            groups.push(group);
        }
        "service" => {
            let id =
                id.ok_or_else(|| "查看服务日志需要指定 id（logs_query 的 source=service）".to_string())?;
            let path = service_log_path(ctx, &id)
                .ok_or_else(|| format!("找不到这台服务的日志文件：{id}"))?;
            let mut group = SourceLog::new(&format!("service:{id}"));
            for line in read_tail_lines(&archive_path(&path, archive), lines) {
                group
                    .entries
                    .push(raw_log_entry(&line, "service", "service"));
            }
            if source_hit_limit(&group, lines) {
                truncated = true;
            }
            groups.push(group);
        }
        _ => {
            // all：events → appLog → 每条隧道 → 每个服务（D2 §15.3 冻结顺序）
            let mut events_group = SourceLog::new("events");
            for line in ctx.events.read(lines, archive) {
                events_group.entries.push(event_entry(&line));
            }
            if source_hit_limit(&events_group, lines) {
                truncated = true;
            }
            groups.push(events_group);

            let app_log = ctx.data_dir.join("logs/app.log");
            let mut app_group = SourceLog::new("app");
            for line in read_tail_lines(&archive_path(&app_log, archive), lines) {
                app_group
                    .entries
                    .push(raw_log_entry(&line, "app", "appLog"));
            }
            if source_hit_limit(&app_group, lines) {
                truncated = true;
            }
            groups.push(app_group);

            let (tunnels, services) = {
                let g = ctx.lock_guardian()?;
                (g.tunnel_snapshot(), g.snapshot())
            };
            for t in tunnels {
                let mut group = SourceLog::new(&format!("tunnel:{}", t.id));
                let path = tunnel_log_path(ctx, &t.id);
                for line in read_tail_lines(&archive_path(&path, archive), lines) {
                    group.entries.push(raw_log_entry(&line, "tunnel", "tunnel"));
                }
                if source_hit_limit(&group, lines) {
                    truncated = true;
                }
                groups.push(group);
            }
            for s in services {
                let mut group = SourceLog::new(&format!("service:{}", s.id));
                if let Some(path) = service_log_path(ctx, &s.id) {
                    for line in read_tail_lines(&archive_path(&path, archive), lines) {
                        group
                            .entries
                            .push(raw_log_entry(&line, "service", "service"));
                    }
                    if source_hit_limit(&group, lines) {
                        truncated = true;
                    }
                }
                groups.push(group);
            }
        }
    }

    // 总量封顶 2000：按 sources 冻结顺序从前往后留，多出来的丢掉并标记 truncated。
    if cap_log_groups(&mut groups, LOGS_TOTAL_CAP) {
        truncated = true;
    }

    // events / app 是标准来源，即使这次没内容也保留；隧道/服务的空分组不列进 sources。
    groups.retain(|g| !g.entries.is_empty() || g.id == "events" || g.id == "app");
    let sources: Vec<String> = groups.iter().map(|g| g.id.clone()).collect();
    let mut entries: Vec<Value> = Vec::new();
    for group in groups {
        entries.extend(group.entries);
    }

    let mut result = json!({
        "source": source,
        "archive": archive,
        "lines": lines,
        "truncated": truncated,
        "sources": sources,
        "entries": entries,
    });
    if format == "lines" {
        let text = entries_text(result.get("entries").and_then(|v| v.as_array()));
        result["text"] = Value::String(text);
    }
    Ok(result)
}

/// 一个来源读到了 lines 上限：说明后面还有，算被截断（D2 §5.4）。
/// 所有来源（事件 / 应用 / 隧道 / 服务）一视同仁。
fn source_hit_limit(group: &SourceLog, lines: usize) -> bool {
    group.entries.len() >= lines
}

/// 总量封顶：按给定顺序（events → app → 隧道 → 服务）从前往后留，
/// 后面的先被丢掉并标记 truncated。返回这次有没有发生截断。
fn cap_log_groups(groups: &mut [SourceLog], cap: usize) -> bool {
    let mut truncated = false;
    let mut total = 0usize;
    for group in groups.iter_mut() {
        if total >= cap {
            truncated = true;
            group.entries.clear();
            continue;
        }
        let room = cap - total;
        if group.entries.len() > room {
            truncated = true;
            group.entries.truncate(room);
        }
        if !group.entries.is_empty() {
            total += group.entries.len();
        }
    }
    truncated
}

/// `format:"lines"` 的 text 渲染。
fn entries_text(entries: Option<&Vec<Value>>) -> String {
    let Some(entries) = entries else {
        return String::new();
    };
    let mut out = String::new();
    for entry in entries {
        let ts = entry.get("ts").and_then(|v| v.as_i64());
        let time = ts.map(hhmmss).unwrap_or_else(|| "--:--:--".to_string());
        let source = entry
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("app");
        let label = entry
            .get("subject")
            .and_then(|s| s.get("name"))
            .and_then(|v| v.as_str())
            .map(|n| match source {
                "tunnel" => format!("发布:{n}"),
                "service" => format!("程序:{n}"),
                _ => "应用".to_string(),
            })
            .unwrap_or_else(|| match source {
                "tunnel" => "发布".to_string(),
                "service" => "程序".to_string(),
                _ => "应用".to_string(),
            });
        let body = entry
            .get("raw")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        out.push_str(&format!("[{time}] [{label}] {body}\n"));
    }
    out.trim_end_matches('\n').to_string()
}

// ============================================================
// stats（D2 §8）
// ============================================================

fn stats(ctx: &Ctx) -> Result<Value, String> {
    let (services, tunnels, update) = {
        let g = ctx.lock_guardian()?;
        (g.snapshot(), g.tunnel_snapshot(), ctx.state.update_info())
    };

    let uptime = (now_ms().saturating_sub(ctx.started_at) / 1000).max(0);
    let tunnel_items: Vec<Value> = tunnels
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "name": t.name,
                "status": t.status,
                "retryState": t.retry_state,
                "attempt": t.attempt,
                "retryInSecs": t.retry_in_secs,
                "connectedSince": t.connected_since,
                "uptimeSecs": t.uptime_secs,
                "reconnectsToday": t.reconnects_today,
            })
        })
        .collect();
    let retrying = tunnels.iter().filter(|t| t.retry_in_secs.is_some()).count();
    let running = tunnels.iter().filter(|t| t.status == "running").count();
    let service_items: Vec<Value> = services
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "status": s.status,
                "health": s.health,
            })
        })
        .collect();

    Ok(json!({
        "schemaVersion": 1,
        "generatedAt": now_ms(),
        "app": {
            "pid": std::process::id(),
            "appVersion": env!("CARGO_PKG_VERSION"),
            "startedAt": ctx.started_at,
            "uptimeSecs": uptime,
            "dayKey": ctx.state.day_key(),
            "jobGuardOk": ctx.job_guard_ok,
            "eventsSinceStart": ctx.events.events_since_start(),
        },
        "tunnels": {
            "total": tunnels.len(),
            "enabled": tunnels.iter().filter(|t| t.enabled).count(),
            "running": running,
            "retrying": retrying,
            "reconnectsToday": tunnels.iter().map(|t| t.reconnects_today).sum::<u32>(),
            "items": tunnel_items,
        },
        "services": {
            "total": services.len(),
            "up": services.iter().filter(|s| s.health == "up").count(),
            "down": services.iter().filter(|s| s.health == "down").count(),
            "unknown": services.iter().filter(|s| s.health == "unknown").count(),
            "items": service_items,
        },
        "update": to_json(&update),
    }))
}

// ============================================================
// 导出 / 脱敏（export_config 与 build_diag 共用）
// ============================================================

fn clone_key(from: &Value, to: &mut Map<String, Value>, key: &str) {
    if let Some(v) = from.get(key) {
        to.insert(key.to_string(), v.clone());
    }
}

fn key_path_present(item: &Value) -> bool {
    item.get("keyPath")
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// 按 D2 §11.1 / §12.2 白名单重建配置。
/// `diag` = 诊断包模式（launch.args 省略）；`with_paths` = 导出时带上真实 keyPath。
fn redacted_config(raw: &Value, diag: bool, with_paths: bool) -> Value {
    let mut out = Map::new();
    clone_key(raw, &mut out, "version");
    clone_key(raw, &mut out, "notifyEnabled");
    clone_key(raw, &mut out, "settings");

    let mut servers = Vec::new();
    if let Some(items) = raw.get("servers").and_then(|v| v.as_array()) {
        for item in items {
            let mut s = Map::new();
            for key in ["id", "name", "host", "sshPort", "user", "vhostDir"] {
                clone_key(item, &mut s, key);
            }
            if with_paths {
                clone_key(item, &mut s, "keyPath");
            } else {
                s.insert("keyPathPresent".to_string(), json!(key_path_present(item)));
            }
            servers.push(Value::Object(s));
        }
    }
    out.insert("servers".to_string(), Value::Array(servers));

    let mut services = Vec::new();
    if let Some(items) = raw.get("services").and_then(|v| v.as_array()) {
        for item in items {
            let mut s = Map::new();
            for key in ["id", "name", "localPort", "watch", "autostart", "note"] {
                clone_key(item, &mut s, key);
            }
            if let Some(launch) = item.get("launch") {
                let mut l = Map::new();
                clone_key(launch, &mut l, "exe");
                clone_key(launch, &mut l, "workdir");
                if diag {
                    let len = launch
                        .get("args")
                        .and_then(|v| v.as_str())
                        .map(|s| s.chars().count())
                        .unwrap_or(0);
                    l.insert("args".to_string(), Value::String("<已省略>".to_string()));
                    l.insert("argsLength".to_string(), json!(len));
                } else {
                    clone_key(launch, &mut l, "args");
                }
                s.insert("launch".to_string(), Value::Object(l));
            }
            services.push(Value::Object(s));
        }
    }
    out.insert("services".to_string(), Value::Array(services));

    let mut tunnels = Vec::new();
    if let Some(items) = raw.get("tunnels").and_then(|v| v.as_array()) {
        for item in items {
            let mut t = Map::new();
            for key in [
                "id",
                "name",
                "serviceId",
                "serverId",
                "remotePort",
                "url",
                "access",
                "accessUser",
                "accessDetail",
                "enabled",
                // preset 是历史字段（新版界面/CLI 不再设置），但导出、撤销恢复要原样带上，
                // 否则「导出 → 替换导入」会把老隧道的激活行降级成注释菜单。
                "preset",
            ] {
                clone_key(item, &mut t, key);
            }
            // accessHash 永远不出现（连长度都不给；bcrypt 哈希能拿去离线爆破）。
            let has_password = item
                .get("accessHash")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            t.insert("hasPassword".to_string(), json!(has_password));
            if with_paths {
                clone_key(item, &mut t, "keyPath");
            } else {
                t.insert("keyPathPresent".to_string(), json!(key_path_present(item)));
            }
            tunnels.push(Value::Object(t));
        }
    }
    out.insert("tunnels".to_string(), Value::Array(tunnels));

    Value::Object(out)
}

/// 按 D2 §12.2 重建 state.json。
fn redacted_state(raw: &Value) -> Value {
    let mut out = Map::new();
    for key in ["stateVersion", "dayKey", "lastSavedAt", "updateInfo"] {
        clone_key(raw, &mut out, key);
    }
    // tunnels / services：lastFailure、protectionCheck、stats 全部保留
    for section in ["tunnels", "services"] {
        let mut rebuilt = Map::new();
        if let Some(map) = raw.get(section).and_then(|v| v.as_object()) {
            for (id, item) in map {
                let mut entry = Map::new();
                for key in ["lastFailure", "protectionCheck", "stats"] {
                    clone_key(item, &mut entry, key);
                }
                rebuilt.insert(id.clone(), Value::Object(entry));
            }
        }
        out.insert(section.to_string(), Value::Object(rebuilt));
    }
    // trash：item 按配置白名单重建
    let mut trash = Vec::new();
    if let Some(items) = raw.get("trash").and_then(|v| v.as_array()) {
        for item in items {
            let mut t = Map::new();
            for key in ["token", "kind", "deletedAt", "expiresAt", "dependencies"] {
                clone_key(item, &mut t, key);
            }
            if let Some(rec) = item.get("item") {
                let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let wrapped = match kind {
                    "service" => json!({ "services": [rec] }),
                    "tunnel" => json!({ "tunnels": [rec] }),
                    "server" => json!({ "servers": [rec] }),
                    _ => Value::Null,
                };
                if wrapped.is_null() {
                    t.insert("item".to_string(), Value::Null);
                } else {
                    let rebuilt = redacted_config(&wrapped, true, false);
                    let list_key = match kind {
                        "service" => "services",
                        "tunnel" => "tunnels",
                        _ => "servers",
                    };
                    let first = rebuilt
                        .get(list_key)
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .cloned()
                        .unwrap_or(Value::Null);
                    t.insert("item".to_string(), first);
                }
            }
            trash.push(Value::Object(t));
        }
    }
    out.insert("trash".to_string(), Value::Array(trash));
    Value::Object(out)
}

/// 快照按白名单重建：servers/tunnels 的 keyPath → keyPathPresent。
fn redacted_snapshot(mut snapshot: Value) -> Value {
    if let Some(map) = snapshot.as_object_mut() {
        for section in ["servers", "tunnels"] {
            if let Some(items) = map.get_mut(section).and_then(|v| v.as_array_mut()) {
                for item in items.iter_mut() {
                    if let Some(obj) = item.as_object_mut() {
                        let present = obj
                            .get("keyPath")
                            .and_then(|v| v.as_str())
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false);
                        obj.remove("keyPath");
                        obj.insert("keyPathPresent".to_string(), json!(present));
                    }
                }
            }
        }
    }
    snapshot
}

// ============================================================
// 文本脱敏（诊断包里的日志行；D2 §12.2）
// ============================================================

const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "token",
    "secret",
    "api_key",
    "api-key",
    "apikey",
    "authorization",
    // 密钥路径：doctor / 导出文本里可能带真实路径（D2 §12.2）
    "keypath",
    "key_path",
];

/// 一行文本脱敏：key=value / key: value / `"key": "value"` / `--key value` 密钥、
/// bcrypt 哈希、Windows 用户名路径。
fn scrub_line(line: &str) -> String {
    let mut text = line.to_string();

    // 1) 各种赋值形态
    let lower = text.to_ascii_lowercase();
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for key in SECRET_KEYS {
        let mut from = 0usize;
        while let Some(pos) = lower[from..].find(key) {
            let start = from + pos;
            let after = start + key.len();
            // 关键词后面允许跟一个 JSON 的收尾引号，再跳过空格
            let mut probe = after;
            if lower.as_bytes().get(probe) == Some(&b'"') {
                probe += 1;
            }
            let mut had_space = false;
            while probe < lower.len() && lower.as_bytes()[probe] == b' ' {
                probe += 1;
                had_space = true;
            }
            let sep = lower.as_bytes().get(probe).copied();
            // `--password x`：紧贴在关键词前的 `-` 且再前面不是字母数字，才把空格当分隔符
            let before_flag = if start >= 2 {
                lower.as_bytes().get(start - 2).copied()
            } else {
                None
            };
            let flag = start > 0
                && lower.as_bytes().get(start - 1) == Some(&b'-')
                && before_flag.map(|b| !b.is_ascii_alphanumeric()).unwrap_or(true);
            let space_sep =
                flag && had_space && sep != Some(b':') && sep != Some(b'=');
            if sep == Some(b':') || sep == Some(b'=') || space_sep {
                // `=`/`:` 后面可能有空格；空格分隔时 probe 已经指向值本身
                let mut vs = if sep == Some(b':') || sep == Some(b'=') {
                    probe + 1
                } else {
                    probe
                };
                while vs < text.len() && text.as_bytes()[vs] == b' ' {
                    vs += 1;
                }
                match text.as_bytes().get(vs) {
                    Some(&quote @ (b'"' | b'\'')) => {
                        // 引号形式（JSON / shell）：只换引号里的内容，引号留着
                        let mut ve = vs + 1;
                        while ve < text.len() && text.as_bytes()[ve] != quote {
                            ve += 1;
                        }
                        if ve > vs + 1 {
                            hits.push((vs + 1, ve));
                        }
                        from = ve.saturating_add(1).min(lower.len()).max(after);
                    }
                    _ => {
                        let mut ve = vs;
                        while ve < text.len() && !text.as_bytes()[ve].is_ascii_whitespace() {
                            ve += 1;
                        }
                        if ve > vs {
                            hits.push((vs, ve));
                        }
                        from = ve.max(after);
                    }
                }
            } else {
                from = after;
            }
        }
    }
    // 先合并重叠区间再反向替换，免得两次替换的字节下标互相错位
    hits.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (vs, ve) in hits {
        if let Some(last) = merged.last_mut() {
            if vs <= last.1 {
                last.1 = last.1.max(ve);
                continue;
            }
        }
        merged.push((vs, ve));
    }
    for (vs, ve) in merged.into_iter().rev() {
        text.replace_range(vs..ve, "<已省略>");
    }

    // 2) bcrypt 哈希 `$2y$12$` + 53 位
    text = scrub_bcrypt(&text);

    // 3) C:\Users\<名字>
    text = scrub_user_path(&text);
    text
}

fn scrub_bcrypt(text: &str) -> String {
    let is_crypt_char = |b: u8| b.is_ascii_alphanumeric() || b == b'.' || b == b'/';
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < bytes.len() {
        // apr1（服务器 nginx 验不了 bcrypt 时用的兼容格式）也要脱敏
        if text[i..].starts_with("$apr1$") {
            // $apr1$ + 8 位盐 + $ + 22 位哈希
            let mut j = i + 6;
            let mut salt_len = 0;
            while j < bytes.len() && salt_len < 8 && is_crypt_char(bytes[j]) {
                j += 1;
                salt_len += 1;
            }
            if salt_len == 8 && j < bytes.len() && bytes[j] == b'$' {
                j += 1;
                let mut hash_len = 0;
                while j < bytes.len() && hash_len < 22 && is_crypt_char(bytes[j]) {
                    j += 1;
                    hash_len += 1;
                }
                if hash_len == 22 {
                    out.push_str("<密码哈希已省略>");
                    i = j;
                    continue;
                }
            }
        }
        let is_bcrypt = bytes[i] == b'$'
            && i + 3 < bytes.len()
            && bytes[i + 1] == b'2'
            && matches!(bytes[i + 2], b'a' | b'b' | b'y')
            && bytes[i + 3] == b'$';
        if is_bcrypt {
            // $2y$NN$ + 53 位 [./A-Za-z0-9]
            let mut j = i + 4;
            let mut digits = 0;
            while j < bytes.len() && bytes[j].is_ascii_digit() && digits < 2 {
                j += 1;
                digits += 1;
            }
            if digits == 2 && j < bytes.len() && bytes[j] == b'$' {
                j += 1;
                let mut hash_len = 0;
                while j < bytes.len()
                    && hash_len < 53
                    && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'/')
                {
                    j += 1;
                    hash_len += 1;
                }
                if hash_len == 53 {
                    out.push_str("<密码哈希已省略>");
                    i = j;
                    continue;
                }
            }
        }
        // 非 UTF-8 边界保护：直接 push 一个字符
        let ch = text[i..].chars().next().unwrap_or('?');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn scrub_user_path(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = "c:\\users\\";
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut search_from = 0usize;
    while let Some(pos) = lower[search_from..].find(needle) {
        let start = search_from + pos;
        out.push_str(&text[cursor..start + needle.len()]);
        let mut end = start + needle.len();
        while end < text.len() {
            let ch = text[end..].chars().next().unwrap_or('?');
            if ch == '\\' || ch.is_whitespace() || ch == '"' || ch == '\'' {
                break;
            }
            end += ch.len_utf8();
        }
        out.push_str("<用户>");
        cursor = end;
        search_from = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// 整段文本脱敏；私钥块整块替换。
fn scrub_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_private_key = false;
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        let upper = line.to_ascii_uppercase();
        if !in_private_key && upper.contains("-----BEGIN ") && upper.contains("PRIVATE KEY-----") {
            out.push_str("<私钥内容已省略>");
            in_private_key = true;
            continue;
        }
        if in_private_key {
            if upper.contains("-----END ") && upper.contains("PRIVATE KEY-----") {
                in_private_key = false;
            }
            continue;
        }
        out.push_str(&scrub_line(line));
    }
    out
}

// ============================================================
// 只读 ZIP 打包（store，不压缩；不引新依赖）
// ============================================================

fn crc32_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    })
}

fn crc32(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut c = 0xFFFF_FFFFu32;
    for byte in data {
        c = table[((c ^ *byte as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// 本机当前时间的 DOS 时间 / 日期（ZIP 头用；不引时间库）。
fn dos_datetime() -> (u16, u16) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st = SYSTEMTIME::default();
        unsafe { GetLocalTime(&mut st) };
        let time = (st.wHour << 11) | (st.wMinute << 5) | (st.wSecond / 2);
        let date = ((st.wYear.saturating_sub(1980)) << 9) | (st.wMonth << 5) | st.wDay;
        (time, date)
    }
    #[cfg(not(windows))]
    {
        (0, 0x21)
    }
}

/// 打一个 store-only（不压缩）的 ZIP。条目名必须是 UTF-8。
fn build_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let (dos_time, dos_date) = dos_datetime();
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::new();

    for (name, data) in entries {
        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        offsets.push(out.len() as u32);
        // local file header
        push_u32(&mut out, 0x0403_4b50);
        push_u16(&mut out, 20); // version needed
        push_u16(&mut out, 0x0800); // UTF-8 文件名
        push_u16(&mut out, 0); // method = store
        push_u16(&mut out, dos_time);
        push_u16(&mut out, dos_date);
        push_u32(&mut out, crc);
        push_u32(&mut out, data.len() as u32);
        push_u32(&mut out, data.len() as u32);
        push_u16(&mut out, name_bytes.len() as u16);
        push_u16(&mut out, 0); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);
    }

    for (index, (name, data)) in entries.iter().enumerate() {
        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        push_u32(&mut central, 0x0201_4b50);
        push_u16(&mut central, 20); // version made by
        push_u16(&mut central, 20); // version needed
        push_u16(&mut central, 0x0800);
        push_u16(&mut central, 0);
        push_u16(&mut central, dos_time);
        push_u16(&mut central, dos_date);
        push_u32(&mut central, crc);
        push_u32(&mut central, data.len() as u32);
        push_u32(&mut central, data.len() as u32);
        push_u16(&mut central, name_bytes.len() as u16);
        push_u16(&mut central, 0); // extra
        push_u16(&mut central, 0); // comment
        push_u16(&mut central, 0); // disk
        push_u16(&mut central, 0); // internal attrs
        push_u32(&mut central, 0); // external attrs
        push_u32(&mut central, offsets[index]);
        central.extend_from_slice(name_bytes);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    // end of central directory
    push_u32(&mut out, 0x0605_4b50);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, entries.len() as u16);
    push_u16(&mut out, entries.len() as u16);
    push_u32(&mut out, cd_size);
    push_u32(&mut out, cd_offset);
    push_u16(&mut out, 0);
    out
}

// ============================================================
// 诊断包（D2 §12）
// ============================================================

fn diag_dir(ctx: &Ctx) -> PathBuf {
    ctx.data_dir.join("diag")
}

fn diag_default_path(ctx: &Ctx) -> PathBuf {
    let stamp = {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::SYSTEMTIME;
            use windows_sys::Win32::System::SystemInformation::GetLocalTime;
            let mut st = SYSTEMTIME::default();
            unsafe { GetLocalTime(&mut st) };
            format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
            )
        }
        #[cfg(not(windows))]
        {
            "unknown".to_string()
        }
    };
    let base = diag_dir(ctx).join(format!("diag-{stamp}.zip"));
    if !base.exists() {
        return base;
    }
    // 重名自动加 -2、-3…
    for n in 2..1000 {
        let candidate = diag_dir(ctx).join(format!("diag-{stamp}-{n}.zip"));
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

fn diag_readme() -> String {
    "上线助手诊断包

本包内容：
- meta.json：本次打包信息（版本、系统、是否截断）
- config.redacted.json：脱敏后的设置（密码哈希、密钥路径已移除）
- state.redacted.json：脱敏后的运行记录
- snapshot.json：当前的服务 / 隧道 / 服务器一览
- stats.json：运行统计
- doctor.json：最近一次体检报告（未运行过则无）
- events.tail.jsonl：最近的事件记录
- logs/*：应用、隧道、本机服务的日志尾部

脱敏说明：
- 访问密码哈希与密钥真实路径一定不会出现；命令行参数仅保留长度。
- 日志中的密码 / token / 私钥文本会被替换为 <已省略>。
- Windows 用户名会被替换为 <用户>。

交给谁：请自行将此包发给排查人员；此包不会自动上传，也不会联网。
"
    .to_string()
}

/// 生成诊断包。`deep` 只写进 meta；doctor-last.json 没有就现跑一次 quick
///（D2 §12.1），本函数自己不另外打网络请求。
pub(crate) fn build_diag(ctx: &Ctx, deep: bool, out: Option<&str>) -> Result<Value, String> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut truncated = false;

    // ---- config.redacted.json / state.redacted.json ----
    let config_raw: Value = match std::fs::read_to_string(ctx.data_dir.join("config.json")) {
        Ok(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    let config_version = config_raw
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    entries.push((
        "config.redacted.json".to_string(),
        serde_json::to_vec_pretty(&redacted_config(&config_raw, true, false)).unwrap_or_default(),
    ));

    let state_raw: Value = match std::fs::read_to_string(ctx.data_dir.join("state.json")) {
        Ok(text) => serde_json::from_str(&text).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    let state_version = state_raw
        .get("stateVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(state::STATE_VERSION as u64);
    entries.push((
        "state.redacted.json".to_string(),
        serde_json::to_vec_pretty(&redacted_state(&state_raw)).unwrap_or_default(),
    ));

    // ---- snapshot.json / stats.json ----
    let (services, tunnels, servers) = {
        let g = ctx.lock_guardian()?;
        (g.snapshot(), g.tunnel_snapshot(), g.servers())
    };
    let snapshot = json!({
        "app": to_json(&crate::build_app_view(ctx)),
        "services": to_json(&services),
        "tunnels": to_json(&tunnels),
        "servers": to_json(&servers),
    });
    entries.push((
        "snapshot.json".to_string(),
        serde_json::to_vec_pretty(&redacted_snapshot(snapshot)).unwrap_or_default(),
    ));
    entries.push((
        "stats.json".to_string(),
        serde_json::to_vec_pretty(&stats(ctx)?).unwrap_or_default(),
    ));

    // ---- doctor.json ----
    let doctor_path = ctx.data_dir.join("logs/doctor-last.json");
    let doctor = match std::fs::read_to_string(&doctor_path) {
        Ok(text) => text,
        Err(_) => {
            // D2 §12.1：没有就现跑一次 quick（结果会顺手落盘，下回直接用）
            match build_doctor_request(ctx, DoctorMode::Quick) {
                Ok(req) => serde_json::to_string_pretty(&shangxian_core::doctor::run(&req))
                    .unwrap_or_else(|_| {
                        "{\n  \"schemaVersion\": 0,\n  \"note\": \"体检报告序列化失败\"\n}\n"
                            .to_string()
                    }),
                Err(e) => format!(
                    "{{\n  \"schemaVersion\": 0,\n  \"note\": \"尚未运行过体检，本次也未能运行：{e}\"\n}}\n"
                ),
            }
        }
    };
    // D2 §12.2：doctor 原文里可能带真实 keyPath / 部署路径，进包前先过一遍脱敏。
    entries.push(("doctor.json".to_string(), scrub_text(&doctor).into_bytes()));

    // ---- events.tail.jsonl ----
    let events_lines = read_tail_lines(&ctx.data_dir.join("events.jsonl"), 2000);
    let mut events_text = events_lines.join("\n");
    if events_text.len() > 1024 * 1024 {
        // 往后退到字符边界再切：直接按字节切会把汉字劈成两半、panic（同下面日志尾巴的做法）。
        let cut = utf8_ceil_boundary(&events_text, events_text.len() - 1024 * 1024);
        events_text = events_text[cut..].to_string();
        truncated = true;
    }
    entries.push((
        "events.tail.jsonl".to_string(),
        scrub_text(&events_text).into_bytes(),
    ));

    // ---- 日志尾巴 ----
    let logs_dir = ctx.data_dir.join("logs");
    let app_lines = read_tail_lines(&logs_dir.join("app.log"), 200);
    entries.push((
        "logs/app.tail.txt".to_string(),
        scrub_text(&app_lines.join("\n")).into_bytes(),
    ));

    let mut tunnel_files: Vec<PathBuf> = Vec::new();
    let mut service_files: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&logs_dir) {
        for entry in dir.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".ssh.log") {
                tunnel_files.push(path);
            } else if name.starts_with("service-") && name.ends_with(".log") {
                service_files.push(path);
            }
        }
    }
    tunnel_files.sort();
    service_files.sort();
    for (index, path) in tunnel_files.iter().take(20).enumerate() {
        let name = format!("logs/tunnel/{index:02}-{}.tail.txt", safe_file_stem(path));
        let text = read_tail_lines(path, 200).join("\n");
        entries.push((name, scrub_text(&text).into_bytes()));
    }
    for (index, path) in service_files.iter().take(20).enumerate() {
        let name = format!("logs/service/{index:02}-{}.tail.txt", safe_file_stem(path));
        let text = read_tail_lines(path, 200).join("\n");
        entries.push((name, scrub_text(&text).into_bytes()));
    }

    entries.push(("README.txt".to_string(), diag_readme().into_bytes()));

    // ---- 超限就砍日志尾巴（保留 meta 说明） ----
    let mut total: usize = entries.iter().map(|(_, d)| d.len()).sum();
    if total > DIAG_MAX_BYTES {
        truncated = true;
        let log_prefixes = ["logs/", "events.tail.jsonl"];
        for (name, data) in entries.iter_mut() {
            if log_prefixes.iter().any(|p| name.starts_with(p)) {
                if data.len() > 200 * 1024 {
                    let keep = data.len() - 200 * 1024;
                    let mut cut = keep;
                    while cut < data.len() && (data[cut] & 0xC0) == 0x80 {
                        cut += 1;
                    }
                    *data = data[cut..].to_vec();
                }
            }
        }
        total = entries.iter().map(|(_, d)| d.len()).sum();
        if total > DIAG_MAX_BYTES {
            return Err("诊断包生成失败：内容超过 20MB；请先清理日志后重试".to_string());
        }
    }

    // ---- meta.json（放最前，大小信息以最终内容为准） ----
    let files: Vec<Value> = entries
        .iter()
        .map(|(name, data)| json!({"name": name, "bytes": data.len()}))
        .collect();
    let meta = json!({
        "kind": "shangxian.diag",
        "formatVersion": 1,
        "generatedAt": now_ms(),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "configVersion": config_version,
        "stateVersion": state_version,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "redacted": true,
        "deep": deep,
        "truncated": truncated,
        "files": files,
    });
    entries.insert(
        0,
        (
            "meta.json".to_string(),
            serde_json::to_vec_pretty(&meta).unwrap_or_default(),
        ),
    );

    let zip = build_zip(&entries);
    let path = match out {
        Some(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => diag_default_path(ctx),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("诊断包生成失败：目录创建失败（{e}）"))?;
    }
    // 先写同目录的临时文件，成功了再改名到最终路径：中途失败不会在最终路径留半成品。
    let tmp = temp_diag_path(&path);
    if let Err(e) = std::fs::write(&tmp, &zip) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("诊断包生成失败：写入文件失败（{e}）"));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("诊断包生成失败：写入文件失败（{e}）"));
    }

    Ok(json!({
        "path": path.display().to_string(),
        "bytes": zip.len(),
        "redacted": true,
        "deep": deep,
        "truncated": truncated,
    }))
}

/// 诊断包的临时落点：必须和最终文件同目录（rename 才原子），名字带进程号防撞。
fn temp_diag_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "diag.zip".to_string());
    path.with_file_name(format!("{name}.{}.part", std::process::id()))
}

/// 从 `index` 往后退到最近的 UTF-8 字符边界（截日志尾巴用，避免切坏汉字）。
/// `index` 超过长度时返回长度本身。
fn utf8_ceil_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn safe_file_stem(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "log".to_string())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect()
}

// ============================================================
// 更新检查（D2 §15.7 / D5 §7）
// ============================================================

fn no_window_command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// curl.exe（认环境变量代理）→ PowerShell（认系统代理）兜底，拿 releases/latest JSON。
fn fetch_latest_release() -> Result<Value, String> {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let curl = format!("{system_root}\\System32\\curl.exe");
    if let Ok(output) = no_window_command(&curl)
        .args([
            "-fsSL",
            "--connect-timeout",
            "4",
            "--max-time",
            "8",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: shangxian-update-check",
            UPDATE_API,
        ])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if let Ok(json) = serde_json::from_str::<Value>(&text) {
                return Ok(json);
            }
            return Err("无法解析更新服务器返回的内容；请稍后重试".to_string());
        }
    }

    // 兜底：PowerShell 走 .NET 系统代理（企业网络里更管用）
    let script = "$ProgressPreference='SilentlyContinue'; \
        (Invoke-RestMethod -TimeoutSec 8 -Headers @{'User-Agent'='shangxian-update-check'} \
        -Uri 'https://api.github.com/repos/iranolly/shangxian/releases/latest') | \
        ConvertTo-Json -Compress -Depth 2";
    let output = no_window_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("更新检查失败：无法连接网络；请稍后重试（{e}）"))?;
    if !output.status.success() {
        return Err("更新检查失败：无法连接网络；请稍后重试".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    serde_json::from_str::<Value>(&text)
        .map_err(|_| "无法解析更新服务器返回的内容；请稍后重试".to_string())
}

/// 版本号比较：只认 x.y.z 数字段；返回 latest 是否比 current 新。
fn version_is_newer(current: &str, latest: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<u64>().ok())
            .collect()
    }
    let a = parts(latest);
    let b = parts(current);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 跑一次更新检查并落盘 + 记事件。`announce` = 有新版本时是否允许弹一次通知。
pub(crate) fn check_update(ctx: &Ctx, announce: bool, app: Option<&tauri::AppHandle>) -> Value {
    let previous = ctx.state.update_info();
    let info = match fetch_latest_release() {
        Ok(json) => {
            let tag = json
                .get("tag_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .trim_start_matches('v')
                .to_string();
            if tag.is_empty() {
                UpdateInfo {
                    status: "error".to_string(),
                    latest: None,
                    url: Some(UPDATE_PAGE.to_string()),
                    checked_at: Some(now_ms()),
                    error: Some("无法解析更新服务器返回的内容；请稍后重试".to_string()),
                }
            } else if version_is_newer(env!("CARGO_PKG_VERSION"), &tag) {
                UpdateInfo {
                    status: "available".to_string(),
                    latest: Some(tag.clone()),
                    url: Some(UPDATE_PAGE.to_string()),
                    checked_at: Some(now_ms()),
                    error: None,
                }
            } else {
                UpdateInfo {
                    status: "upToDate".to_string(),
                    latest: Some(tag),
                    url: Some(UPDATE_PAGE.to_string()),
                    checked_at: Some(now_ms()),
                    error: None,
                }
            }
        }
        Err(e) => UpdateInfo {
            status: "error".to_string(),
            latest: None,
            url: Some(UPDATE_PAGE.to_string()),
            checked_at: Some(now_ms()),
            error: Some(e),
        },
    };

    ctx.state.set_update_info(info.clone());
    let message = match info.status.as_str() {
        "available" => format!(
            "发现新版本 {}，可前往发布页下载",
            info.latest.as_deref().unwrap_or("")
        ),
        "upToDate" => format!(
            "更新检查完成：已是最新版本（v{}）",
            env!("CARGO_PKG_VERSION")
        ),
        "error" => "更新检查完成：本次未能连接更新服务器".to_string(),
        _ => "更新检查完成".to_string(),
    };
    let code = if info.status == "available" {
        ReasonCode::UpdateAvailable
    } else {
        ReasonCode::UpdateChecked
    };
    ctx.events.append(EventLine::new(
        "update",
        if info.status == "error" {
            "warn"
        } else {
            "info"
        },
        Some(code),
        Some(EventSubject::app()),
        &message,
    ));

    // 「同一个新版本只提醒一次」：旧结果已经是 available 且 latest 相同就不再弹。
    let same_available = previous.status == "available" && previous.latest == info.latest;
    if announce && info.status == "available" && !same_available {
        if let Some(app) = app {
            let latest = info.latest.as_deref().unwrap_or("");
            crate::notify::send_toast(
                app,
                "上线助手有新版本",
                &format!("上线助手有新版本 {latest}，请打开应用点击「去下载」"),
            );
        }
    }
    to_json(&info)
}

/// 启动后 30 秒静默检查一次（受开关与 6 小时限频约束，绝不影响启动）。
pub(crate) fn spawn_update_guard(ctx: Ctx, app: tauri::AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(30));
        let (enabled, last) = {
            let settings = match ctx.lock_guardian() {
                Ok(g) => g.settings(),
                Err(_) => return,
            };
            (
                settings.check_update_on_start,
                ctx.state.update_info().checked_at,
            )
        };
        if !enabled {
            log_info(TAG_CTL, "启动时更新检查已关闭，本次跳过");
            return;
        }
        if let Some(checked) = last {
            if now_ms().saturating_sub(checked) < 6 * 60 * 60 * 1000 {
                return; // 6 小时内查过了
            }
        }
        let _ = check_update(&ctx, true, Some(&app));
    });
}

// ============================================================
// 配置导出（只读；import 见 bridge）
// ============================================================

pub(crate) fn export_config(ctx: &Ctx, redacted: bool, out: Option<&str>) -> Result<Value, String> {
    let path = ctx.data_dir.join("config.json");
    let raw: Value = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取配置失败：{e}"))?
        .parse::<Value>()
        .map_err(|e| format!("配置文件无法读取：{e}"))?;
    let config = redacted_config(&raw, false, !redacted);
    let doc = json!({
        "kind": "shangxian.config-export",
        "formatVersion": 1,
        "exportedAt": now_ms(),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "configVersion": raw.get("version").and_then(|v| v.as_u64()).unwrap_or(0),
        "redacted": redacted,
        "config": config.clone(),
    });
    let mut result = json!({ "redacted": redacted, "config": config });
    if let Some(out) = out.filter(|s| !s.trim().is_empty()) {
        let target = PathBuf::from(out.trim());
        let text = serde_json::to_string_pretty(&doc).map_err(|e| format!("导出失败：{e}"))?;
        let tmp = target.with_extension("json.tmp");
        std::fs::write(&tmp, text.as_bytes())
            .map_err(|e| format!("导出失败：写入文件失败（{e}）"))?;
        std::fs::rename(&tmp, &target).map_err(|e| format!("导出失败：{e}"))?;
        result["path"] = Value::String(target.display().to_string());
    }
    Ok(result)
}

// ============================================================
// 体检接线（core::doctor::run）
// ============================================================

/// 构造一次体检请求：实时状态在**短锁**里克隆，联网/落盘由 `core::doctor::run`
/// 在锁外跑（D1 §6.4，跟检查云端同一套锁纪律）。
///
/// 配置文件从磁盘读：Guardian 没暴露完整配置，而应用的每次提交都会先原子落盘，
/// 所以磁盘就是权威现状；配置不可信时不读，交给 doctor 自己去报。
fn build_doctor_request(ctx: &Ctx, mode: DoctorMode) -> Result<DoctorRequest, String> {
    let (config_trusted, services, tunnels, server_ids) = {
        let g = ctx.lock_guardian()?;
        let services = g.snapshot();
        let tunnels = g.tunnel_snapshot();
        let server_ids: Vec<String> = g.servers().into_iter().map(|s| s.id).collect();
        (g.config_trusted(), services, tunnels, server_ids)
    };

    let config_path = ctx.data_dir.join("config.json");
    let too_new = shangxian_core::config::config_version_too_new(&config_path);
    let (config, config_error) = if let Some(version) = too_new {
        (
            None,
            Some(shangxian_core::config::config_unsupported_message(version)),
        )
    } else if config_trusted {
        match AppConfig::load(&config_path) {
            Ok(config) => (Some(config), None),
            Err(e) => (None, Some(format!("设置文件无法读取：{e}"))),
        }
    } else {
        // 读坏留底（config.json.bad）：doctor 自己的步骤会用人话报出来
        (None, None)
    };

    // 服务器 id → 已识别到的站点目录（D0 第 12 条解析链的第二层）
    let mut detected_vhost_dirs = std::collections::BTreeMap::new();
    for id in server_ids {
        let dir = ctx
            .state
            .server_env(&id)
            .and_then(|value| serde_json::from_value::<CloudEnv>(value).ok())
            .and_then(|env| env.vhost_dir)
            .map(|dir| dir.trim().to_string())
            .filter(|dir| !dir.is_empty());
        if let Some(dir) = dir {
            detected_vhost_dirs.insert(id, dir);
        }
    }

    let servers = config
        .as_ref()
        .map(|config| config.servers.clone())
        .unwrap_or_default();
    let services = services
        .into_iter()
        .map(|s| DoctorService {
            id: s.id,
            name: s.name,
            local_port: s.local_port,
            health: s.health,
        })
        .collect();
    let tunnels = tunnels
        .into_iter()
        .map(|t| DoctorTunnel {
            id: t.id,
            name: t.name,
            service_id: t.service_id,
            server_id: t.server_id,
            local_port: t.local_port,
            remote_port: t.remote_port,
            url: t.url,
            enabled: t.enabled,
            status: t.status,
            reason: t.reason,
            key_path: t.key_path,
        })
        .collect();

    Ok(DoctorRequest {
        mode,
        app_running: true,
        app_pid: Some(std::process::id()),
        app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        data_dir: ctx.data_dir.clone(),
        config_trusted,
        config,
        config_error,
        services,
        tunnels,
        servers,
        detected_vhost_dirs,
        ssh_exe: None,
        events: Some(ctx.events.clone()),
    })
}

/// 真跑一次体检。调用方都在离 UI 的阻塞上下文里（ctl 连接线程 / Tauri 的
/// `spawn_blocking`），所以这里直接跑；`run` 内部不碰 Guardian 锁、不会失败。
fn run_doctor(ctx: &Ctx, mode: DoctorMode) -> Result<Value, String> {
    let req = build_doctor_request(ctx, mode)?;
    Ok(to_json(&shangxian_core::doctor::run(&req)))
}

// ============================================================
// 配置导入（D2 §11）
// ============================================================

/// 导入配置：`doc` / `path` 二选一；校验与合并全走 core（all-or-nothing），
/// 成功后用 `Guardian::commit_config` 换进内存并重新同步隧道 / 本机服务。
fn import_config(ctx: &Ctx, params: &Value) -> Result<Value, String> {
    let mode = shangxian_core::import::ImportMode::parse(
        opt_str(params, "mode").as_deref().unwrap_or("merge"),
    )?;
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let doc_param = opt_str(params, "doc");
    let path_param = opt_str(params, "path");
    if doc_param.is_some() && path_param.is_some() {
        return Err("doc 和 path 只能提供一个：doc 为内容，path 为文件路径".to_string());
    }
    let doc = match (doc_param, path_param) {
        (Some(doc), None) => doc,
        (None, Some(path)) => {
            let meta = std::fs::metadata(&path).map_err(|e| format!("无法读取此文件：{e}"))?;
            if meta.len() > shangxian_core::import::IMPORT_DOC_MAX_BYTES as u64 {
                return Err(
                    "导入文件过大（最多 1MB）；请使用 cli config export 导出的文件"
                        .to_string(),
                );
            }
            std::fs::read_to_string(&path).map_err(|e| format!("无法读取此文件：{e}"))?
        }
        _ => return Err("缺少参数：doc（内容）或 path（文件路径）必须提供一个".to_string()),
    };
    if doc.len() > shangxian_core::import::IMPORT_DOC_MAX_BYTES {
        return Err("导入内容过大（最多 1MB）".to_string());
    }

    // 导入是「读现状 → 校验 → 写盘 → 换内存」的原子操作：全程持锁避免和界面写操作交错
    //（配置很小，解析 / 写盘都是毫秒级，不会拖住 800ms 快照和 150ms 看护）。
    let mut g = ctx.lock_guardian()?;
    let config_path = ctx.data_dir.join("config.json");
    // 版本比程序新：会直接拒绝自动保存，先把 D2 §13 的逐字文案给出来
    if let Some(version) = shangxian_core::config::config_version_too_new(&config_path) {
        return Err(shangxian_core::config::config_unsupported_message(version));
    }
    if !g.config_trusted() {
        return Err(
            "配置文件上次未能成功读取（已备份为 config.json.bad），本次无法导入；请先修复配置文件再重新打开应用"
                .to_string(),
        );
    }
    let current = load_config_for_import(&config_path)?;
    if dry_run {
        let (_, summary) = shangxian_core::import::plan_import(&current, &doc, mode)?;
        return Ok(to_json(&summary));
    }
    // core 会先留一份 config.json.bak-<时间戳> 再原子写盘；这里再把新配置提交进
    // Guardian（提交内部还会核对可信、清过期的保护结果、按新配置同步隧道和服务）。
    let outcome =
        shangxian_core::import::import_config(&config_path, &current, &doc, mode, false)?;
    g.commit_config(outcome.config)?;
    // D2 §11 / §16：导入也发 configSaved 事件（kind:app，文案固定）
    ctx.events.append(EventLine::new(
        "app",
        "info",
        Some(ReasonCode::ConfigSaved),
        Some(EventSubject::app()),
        "设置已从文件导入",
    ));
    Ok(to_json(&outcome.summary))
}

/// 读导入前的现有配置：应用每次提交都先落盘，所以磁盘就是权威现状；
/// 第一次运行（还没有 config.json）按空配置算。
fn load_config_for_import(path: &Path) -> Result<AppConfig, String> {
    if let Some(version) = shangxian_core::config::config_version_too_new(path) {
        return Err(shangxian_core::config::config_unsupported_message(version));
    }
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    AppConfig::load(path).map_err(|e| format!("读取现有设置失败：{e}"))
}

// ============================================================
// 停一切（安装器升级 / 卸载前用，D5 §5.3）
// ============================================================

/// 停掉全部发布隧道 + 全部本机服务。
///
/// 走 core 的 `stop_all_runtime()`：只停这次运行起的隧道，**不改配置**
/// （覆盖升级后原来开着的隧道还会开着）。服务部分只停看护这次拉起的进程树，
/// 认领来但核对不过的外部进程绝不硬杀。返回 JSON 键和以前一模一样。
fn stop_all(ctx: &Ctx) -> Result<Value, String> {
    let report = {
        let mut g = ctx.lock_guardian()?;
        g.stop_all_runtime()
    };
    Ok(to_json(&report))
}

// ============================================================
// 云端保护的期望值与漂移判定（D2 §6.4；C1 落地后应搬进 core 复用）
// ============================================================

fn normalize_domain(raw: &str) -> String {
    let mut d = raw.trim();
    for prefix in ["https://", "http://"] {
        if d.len() >= prefix.len()
            && d.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            d = &d[prefix.len()..];
            break;
        }
    }
    let end = d
        .find(|c: char| c == '/' || c == ':' || c.is_whitespace())
        .unwrap_or(d.len());
    d[..end].trim().to_string()
}

fn split_allow_list(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == '，' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 从当前隧道配置算「期望的保护」（D2 §4.1 ExpectedProtection）。
fn expected_from_view(view: &shangxian_core::guardian::TunnelView) -> ExpectedProtection {
    ExpectedProtection {
        password: view.access.iter().any(|a| a == "password"),
        ip: view.access.iter().any(|a| a == "ip"),
        allow_ips: split_allow_list(view.access_detail.as_deref().unwrap_or("")),
        url: normalize_domain(view.url.as_deref().unwrap_or("")),
    }
}

/// 一次云端检查失败的 error 记录（状态判定全在 core 的 `cloudcheck::assemble_v2` /
/// `drift_fields`，这里只负责把人话原因和期望快照装起来）。
fn error_check(expected: ExpectedProtection, error: String, ts: i64, source: &str) -> ProtectionCheck {
    ProtectionCheck {
        status: "error".to_string(),
        ts: Some(ts),
        source: Some(source.to_string()),
        cloud: None,
        expected,
        drift_fields: Vec::new(),
        error: Some(error),
        extra: None,
    }
}

/// 上次检查留在 state 里的 D3 扩展信息（https / warnings / port 等）。
/// 兼容性的 access 检查和 SSH 失败路径拿不到 HTTPS 数据，写盘时把上次的带上，
/// 免得前端已经点亮的 https / warnings 红灯被一次这样的检查抹掉（D0 第 2 条）。
fn last_extra(ctx: &Ctx, tunnel_id: &str) -> Option<Value> {
    ctx.state
        .tunnel_state(tunnel_id)
        .and_then(|state| state.protection_check)
        .and_then(|check| check.extra)
}

// ============================================================
// 方法表（也给 agent 看，cli methods 会打印它）
// ============================================================

const METHODS: &[(&str, &str, &str)] = &[
    ("ping", "检查应用是否运行（会校验通行证）", "{}"),
    (
        "status",
        "一次获取全部：应用信息 + 本机服务 + 隧道 + 服务器",
        "{}",
    ),
    ("get_snapshot", "仅获取三张清单（不含应用信息）", "{}"),
    ("methods", "列出所有可调用的方法（即本表）", "{}"),
    ("stats", "运行统计：时长、今日重连、重试倒计时", "{}"),
    (
        "logs_query",
        "按来源读日志（事件/应用/隧道/服务，可含轮转备份）",
        "{source?, id?, lines?, archive?, format?}",
    ),
    (
        "doctor",
        "体检（app→dirs→ssh→ports→servers→tunnels→cloud→web→dns）",
        "{mode?}",
    ),
    ("run_doctor", "体检（doctor 的旧名字，等价）", "{mode?}"),
    ("build_diag", "生成脱敏诊断包", "{out?, deep?}"),
    (
        "stop_all",
        "停止全部发布与本机服务（卸载/升级前使用）",
        "{}",
    ),
    ("list_deleted", "最近删除（10 分钟内可撤销）", "{}"),
    ("restore_deleted", "撤销删除（隧道恢复为暂停）", "{token}"),
    ("purge_deleted", "放弃撤销", "{token}"),
    (
        "export_config",
        "导出配置（默认不含密码哈希/密钥路径）",
        "{out?, redacted?}",
    ),
    (
        "import_config",
        "导入配置（默认合并）",
        "{doc?/path?, mode?, dryRun?}",
    ),
    ("check_update", "检查是否有新版本", "{}"),
    ("get_settings", "读取设置项", "{}"),
    (
        "set_settings",
        "整份保存设置项",
        "{refreshIntervalMs, quietHours, checkUpdateOnStart}",
    ),
    (
        "list_local_ports",
        "本机正在监听的端口（带进程名和程序路径）",
        "{}",
    ),
    ("scan_imports", "扫描现有启动脚本，返回可导入的建议", "{}"),
    (
        "import_services",
        "将导入建议创建为服务（默认暂停）",
        "{items: [...]}",
    ),
    (
        "save_service",
        "新增/编辑本机服务（不填 id 即为新增）",
        "{id?, name, localPort, launchExe?, launchArgs?, launchWorkdir?, watch, autostart, note?}",
    ),
    (
        "remove_service",
        "删除本机服务（有隧道使用时会被拒绝；返回撤销令牌）",
        "{id}",
    ),
    (
        "service_start",
        "启动本机服务（使用已登记的启动方式）",
        "{id}",
    ),
    (
        "service_stop",
        "停止本机服务（默认只停止由应用启动的进程；外部进程会先返回 needsConfirm 等待用户确认）",
        "{id, killForeign?, expectedPid?}",
    ),
    (
        "service_restart",
        "重启本机服务（先停后启；外部进程占用端口时不操作，返回 needsConfirm）",
        "{id}",
    ),
    (
        "inspect_process_launch_info",
        "自动读取某端口上进程的启动方式（程序/参数/工作目录 + 警告）",
        "{port, expectedPid?}",
    ),
    (
        "preview_launch_command",
        "实际命令行预览（与启动走同一分支，仅用于显示）",
        "{exe, args}",
    ),
    (
        "set_service_watch",
        "开关看护（进程退出后自动启动）",
        "{id, watch}",
    ),
    ("set_service_autostart", "开关开机自启", "{id, autostart}"),
    ("get_service_logs", "读取本机服务的日志尾部", "{id, lines?}"),
    (
        "restart_service_rotate_logs",
        "重启本机服务并轮转日志（日志过大时使用）",
        "{id}",
    ),
    (
        "add_tunnel",
        "新增发布隧道",
        "{name, serviceId, serverId, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}",
    ),
    (
        "update_tunnel",
        "编辑发布隧道（accessHash 传 null 表示不改密码）",
        "{id, name, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}",
    ),
    ("remove_tunnel", "删除发布隧道（返回撤销令牌）", "{id}"),
    ("tunnel_start", "开始发布", "{id}"),
    ("tunnel_stop", "暂停发布", "{id}"),
    (
        "forget_host_key",
        "清除这台服务器的 SSH 指纹记录（服务器重装过时使用）",
        "{id}",
    ),
    ("get_logs", "读取隧道的日志尾部", "{id, lines?}"),
    (
        "save_server",
        "新增/编辑云端服务器（不填 id 即为新增）",
        "{id?, name, host, sshPort, user, keyPath?, vhostDir?}",
    ),
    (
        "delete_server",
        "删除云端服务器（有隧道使用时会被拒绝；返回撤销令牌）",
        "{id}",
    ),
    (
        "setup_passwordless",
        "使用密码登录一次，自动配置免密登录（密码不写入磁盘）",
        "{serverId, password}",
    ),
    (
        "test_server",
        "测试连接：实际连接一次这台服务器的 SSH",
        "{serverId}",
    ),
    (
        "hash_password",
        "生成访问密码哈希（优先 bcrypt；服务器 nginx 验不了时自动用兼容格式，本机计算）",
        "{serverId, password}",
    ),
    ("get_nginx_snippet", "生成某条隧道的 nginx 配置片段", "{id}"),
    (
        "check_tunnel_access",
        "核对云端 nginx 上这条隧道的实际访问保护（结果会存进运行记录）",
        "{id}",
    ),
    (
        "check_tunnel_cloud",
        "云端完整检查（full/drift/audit/env；含 HTTPS/证书/端口/审计）",
        "{id, mode?}",
    ),
    (
        "scan_server_env",
        "识别服务器面板与网站服务（结果缓存到运行记录）",
        "{serverId}",
    ),
    (
        "run_access_audit",
        "统计最近 1000 行的 401/403 访问情况",
        "{id}",
    ),
    ("get_public_ip", "查看服务器视角的公网 IP", "{}"),
    ("get_notify_enabled", "断开通知是否开启", "{}"),
    ("set_notify_enabled", "开关断开通知", "{enabled}"),
    ("get_notify_quiet", "免打扰时段设置", "{}"),
    (
        "set_notify_quiet",
        "设置免打扰时段",
        "{enabled, start, end}",
    ),
    ("get_autostart", "开机自启是否开启", "{}"),
    ("set_autostart", "开关开机自启", "{enabled}"),
    ("get_data_dir", "配置文件夹位置", "{}"),
    ("get_recovery_state", "崩溃自动恢复的状态", "{}"),
    (
        "clear_recovery_state",
        "崩溃自动恢复计数清零、重新启用",
        "{}",
    ),
    ("set_crash_recovery", "开关崩溃自动恢复", "{enabled}"),
];

// ============================================================
// 分发
// ============================================================

/// 一个方法名一个分支。网络方法在各自分支里「短锁 → 放锁跑」。
pub(crate) fn dispatch(ctx: &Ctx, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "ping" => Ok(json!({"pong": true})),
        "methods" => Ok(json!(METHODS
            .iter()
            .map(|(name, desc, shape)| json!({"method": name, "desc": desc, "params": shape}))
            .collect::<Vec<_>>())),
        "status" => {
            let (services, tunnels, servers) = {
                let g = ctx.lock_guardian()?;
                (g.snapshot(), g.tunnel_snapshot(), g.servers())
            };
            Ok(json!({
                "app": to_json(&crate::build_app_view(ctx)),
                "services": to_json(&services),
                "tunnels": to_json(&tunnels),
                "servers": to_json(&servers),
            }))
        }
        "get_snapshot" => {
            let g = ctx.lock_guardian()?;
            Ok(json!({
                "services": to_json(&g.snapshot()),
                "tunnels": to_json(&g.tunnel_snapshot()),
                "servers": to_json(&g.servers()),
            }))
        }
        "stats" => stats(ctx),
        "logs_query" => logs_query(ctx, params),

        // ----- 体检（core::doctor 真接线；联网步骤在锁外跑） -----
        "doctor" | "run_doctor" => {
            let text = opt_str(params, "mode").unwrap_or_default();
            let mode = DoctorMode::parse(&text)?;
            run_doctor(ctx, mode)
        }
        "build_diag" => {
            let deep = params.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);
            let out = opt_str(params, "out");
            build_diag(ctx, deep, out.as_deref())
        }

        // ----- 删除撤销 -----
        "list_deleted" => {
            let items: Vec<Value> = ctx
                .state
                .list_trash()
                .iter()
                .map(|t| {
                    json!({
                        "token": t.token,
                        "type": t.kind,
                        "id": t.item.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": t.item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "deletedAt": t.deleted_at,
                        "expiresAt": t.expires_at,
                    })
                })
                .collect();
            Ok(json!({ "items": items }))
        }
        "restore_deleted" => {
            let token = need_str(params, "token")?;
            let mut g = ctx.lock_guardian()?;
            let restored = g.restore_deleted(&token)?;
            Ok(json!({ "restored": to_json(&restored), "enabled": false }))
        }
        "purge_deleted" => {
            let token = need_str(params, "token")?;
            match ctx.state.take_trash(&token) {
                Some(_) => Ok(json!({"ok": true})),
                None => Err(
                    "此撤销已过期（删除超过 10 分钟）；如需恢复只能重新创建".to_string(),
                ),
            }
        }

        // ----- 配置导出 / 导入 -----
        "export_config" => {
            let redacted = params
                .get("redacted")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let out = opt_str(params, "out");
            export_config(ctx, redacted, out.as_deref())
        }
        "import_config" => import_config(ctx, params),

        // ----- 设置 -----
        "get_settings" => {
            let g = ctx.lock_guardian()?;
            Ok(to_json(&g.settings()))
        }
        "set_settings" => {
            let settings = from_params(params)?;
            let mut g = ctx.lock_guardian()?;
            let saved = g.set_settings(settings)?;
            Ok(to_json(&saved))
        }

        // ----- 更新 -----
        "check_update" => Ok(check_update(ctx, false, None)),

        // ----- 本机服务 -----
        "list_local_ports" => Ok(to_json(&shangxian_core::local_ports::list_listening())),
        "scan_imports" => Ok(to_json(&shangxian_core::import::scan())),
        "import_services" => {
            let items = params
                .get("items")
                .cloned()
                .ok_or_else(|| "缺少参数 items".to_string())?;
            let items: Vec<shangxian_core::import::ImportSuggestion> =
                serde_json::from_value(items).map_err(|e| format!("items 参数不正确：{e}"))?;
            ctx.lock_guardian()?
                .import_services(items)
                .map(|n| json!({"imported": n}))
        }
        "save_service" => {
            let input: ServiceInput = from_params(params)?;
            ctx.lock_guardian()?
                .save_service(input)
                .map(|id| json!({"id": id}))
        }
        "remove_service" => {
            let id = need_str(params, "id")?;
            let info = ctx.lock_guardian()?.remove_service(&id)?;
            Ok(dispatch_delete_result("service", &info))
        }
        "service_start" => {
            let id = need_str(params, "id")?;
            ctx.lock_guardian()?.start_service(&id).map(|_| json!({"ok": true}))
        }
        "service_stop" => {
            let id = need_str(params, "id")?;
            let kill_foreign = params
                .get("killForeign")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let expected_pid = params
                .get("expectedPid")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            let outcome = ctx
                .lock_guardian()?
                .stop_service(&id, kill_foreign, expected_pid)?;
            Ok(to_json(&outcome))
        }
        "service_restart" => {
            let id = need_str(params, "id")?;
            // 外部进程在听：不碰它，把 needsConfirm 原样交给调用方（CLI 会提示先手动停）
            let outcome = ctx.lock_guardian()?.stop_service(&id, false, None)?;
            if matches!(outcome, StopOutcome::NeedsConfirm { .. }) {
                return Ok(to_json(&outcome));
            }
            ctx.lock_guardian()?.start_service(&id).map(|_| json!({"ok": true}))
        }
        "restart_service_rotate_logs" => {
            let id = need_str(params, "id")?;
            ctx.lock_guardian()?
                .restart_service_rotate_logs(&id)
                .map(|_| json!({"ok": true}))
        }
        "inspect_process_launch_info" => {
            let port = params
                .get("port")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| "缺少参数 port".to_string())?;
            let port = u16::try_from(port).map_err(|_| "端口必须是 1~65535 的数字".to_string())?;
            let expected_pid = params
                .get("expectedPid")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            Ok(to_json(&shangxian_core::proc_detail::launch_info(
                port,
                expected_pid,
            )))
        }
        "preview_launch_command" => {
            let exe = need_str(params, "exe")?;
            let args = params
                .get("args")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(json!({
                "command": shangxian_core::localservice::preview_launch(&exe, &args)
            }))
        }
        "set_service_watch" => {
            let id = need_str(params, "id")?;
            let watch = need_bool(params, "watch")?;
            ctx.lock_guardian()?
                .set_service_watch(&id, watch)
                .map(|_| json!({"ok": true}))
        }
        "set_service_autostart" => {
            let id = need_str(params, "id")?;
            let on = need_bool(params, "autostart")?;
            ctx.lock_guardian()?
                .set_service_autostart(&id, on)
                .map(|_| json!({"ok": true}))
        }
        "get_service_logs" => {
            let id = need_str(params, "id")?;
            let lines = opt_usize(params, "lines", 200);
            Ok(json!({"text": ctx.lock_guardian()?.service_log_tail(&id, lines)}))
        }

        // ----- 发布隧道 -----
        "add_tunnel" => {
            let input: NewTunnelInput = from_params(params)?;
            ctx.lock_guardian()?
                .add_tunnel(input)
                .map(|id| json!({"id": id}))
        }
        "update_tunnel" => {
            let input: UpdateTunnelInput = from_params(params)?;
            ctx.lock_guardian()?
                .update_tunnel(input)
                .map(|_| json!({"ok": true}))
        }
        "remove_tunnel" => {
            let id = need_str(params, "id")?;
            let info = ctx.lock_guardian()?.remove_tunnel(&id)?;
            Ok(dispatch_delete_result("tunnel", &info))
        }
        "tunnel_start" => {
            let id = need_str(params, "id")?;
            ctx.lock_guardian()?.set_enabled(&id, true).map(|_| json!({"ok": true}))
        }
        "tunnel_stop" => {
            let id = need_str(params, "id")?;
            ctx.lock_guardian()?.set_enabled(&id, false).map(|_| json!({"ok": true}))
        }
        "forget_host_key" => {
            let id = need_str(params, "id")?;
            // 锁纪律：SSH 要跑十几秒，先短锁把 host/port 克隆出来，放锁再跑，
            // 最后短锁 restart；全程不长期占 Guardian 锁。
            let (host, port) = {
                let g = ctx.lock_guardian()?;
                let server_id = g
                    .tunnel_snapshot()
                    .into_iter()
                    .find(|t| t.id == id)
                    .map(|t| t.server_id)
                    .ok_or_else(|| format!("找不到隧道：{id}"))?;
                let server = g
                    .servers()
                    .into_iter()
                    .find(|s| s.id == server_id)
                    .ok_or_else(|| "找不到这条隧道挂的云端服务器".to_string())?;
                (server.host, server.ssh_port)
            };
            let message = shangxian_core::ssh::forget_host_key(&host, port)?;
            ctx.lock_guardian()?.restart(&id)?;
            Ok(json!({"message": message}))
        }
        "get_logs" => {
            let id = need_str(params, "id")?;
            let lines = opt_usize(params, "lines", 200);
            Ok(json!({"text": ctx.lock_guardian()?.log_tail(&id, lines)}))
        }

        // ----- 云端服务器 -----
        "save_server" => {
            let input: ServerInput = from_params(params)?;
            ctx.lock_guardian()?
                .save_server(input)
                .map(|id| json!({"id": id}))
        }
        "delete_server" => {
            let id = need_str(params, "id")?;
            let info = ctx.lock_guardian()?.delete_server(&id)?;
            Ok(dispatch_delete_result("server", &info))
        }
        "get_nginx_snippet" => {
            let id = need_str(params, "id")?;
            ctx.lock_guardian()?.nginx_snippet(&id).map(|s| to_json(&s))
        }

        // ----- 应用 -----
        "get_notify_enabled" => Ok(json!({"enabled": ctx.lock_guardian()?.notify_enabled()})),
        "set_notify_enabled" => {
            let enabled = need_bool(params, "enabled")?;
            ctx.lock_guardian()?
                .set_notify_enabled(enabled)
                .map(|_| json!({"ok": true}))
        }
        "get_notify_quiet" => Ok(to_json(&ctx.lock_guardian()?.notify_quiet())),
        "set_notify_quiet" => {
            let quiet = from_params(params)?;
            ctx.lock_guardian()?
                .set_notify_quiet(quiet)
                .map(|_| json!({"ok": true}))
        }
        "get_autostart" => Ok(json!({"enabled": crate::autostart_enabled()})),
        "set_autostart" => {
            let enabled = need_bool(params, "enabled")?;
            crate::apply_autostart(enabled).map(|_| json!({"ok": true}))
        }
        "get_data_dir" => Ok(json!({"path": default_data_dir().display().to_string()})),
        "get_recovery_state" => Ok(to_json(&crate::recovery::current_state(&ctx.data_dir))),
        "clear_recovery_state" => {
            // 「继续自动恢复」：清计数 + 重新登记 + 把开关文件改回开。
            crate::apply_crash_recovery(&ctx.data_dir, true)?;
            Ok(json!({"ok": true, "enabled": true}))
        }
        "set_crash_recovery" => {
            let enabled = need_bool(params, "enabled")?;
            crate::apply_crash_recovery(&ctx.data_dir, enabled).map(|_| json!({"ok": true}))
        }
        "stop_all" => stop_all(ctx),

        // ----- 云端完整检查（C1 的 cloudcheck 已落地） -----
        "check_tunnel_cloud" => check_tunnel_cloud(ctx, params),
        "scan_server_env" => scan_server_env(ctx, params),
        "run_access_audit" => run_access_audit(ctx, params),
        "get_public_ip" => get_public_ip(ctx),

        // ----- 联网跑 SSH 的方法（短锁拿连接信息 → 放锁跑） -----
        "check_tunnel_access" => check_tunnel_access(ctx, params),
        "hash_password" => {
            let server_id = need_server_id(params)?;
            let conn = crate::server_conn(&ctx.guardian, &server_id)?;
            let password = need_str(params, "password")?;
            hash_password_adaptive(ctx, &server_id, &conn.exec(), &password)
        }
        "setup_passwordless" => setup_passwordless(ctx, params),
        "test_server" => {
            let server_id = need_server_id(params)?;
            let conn = crate::server_conn(&ctx.guardian, &server_id)?;
            let msg = crate::run_server_test(&conn)?;
            Ok(json!({"message": msg}))
        }

        other => Err(format!(
            "没有这个方法：{other}（可用方法见：cli methods）"
        )),
    }
}

/// 删除结果统一形状（D2 §10.2）。core 的返回结构用 serde 序列化后按键取值，
/// 名字对不上也不会炸，只会少字段。
pub(crate) fn dispatch_delete_result(kind: &str, info: &impl serde::Serialize) -> Value {
    let v = to_json(info);
    let token = v
        .get("token")
        .or_else(|| v.get("undoToken"))
        .cloned()
        .unwrap_or(Value::Null);
    let expires = v
        .get("expiresAt")
        .or_else(|| v.get("undoExpiresAt"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "ok": true,
        "deleted": {
            "type": kind,
            "id": v.get("id").cloned().unwrap_or(Value::Null),
            "name": v.get("name").cloned().unwrap_or(Value::Null),
        },
        "undoToken": token,
        "undoExpiresAt": expires,
    })
}

/// 手动检查的统一计划：老的 `check_tunnel_access` 和新的 `check_tunnel_cloud`
/// 用同一套模式与来源标记，保证同一输入判定一致。
fn manual_check_plan() -> (CheckMode, &'static str) {
    (CheckMode::Full, MANUAL_SOURCE)
}

/// 核对云端访问保护；结果顺手持久化进 state（D0 §2 裁决）。
///
/// 和 `check_tunnel_cloud` 走同一条 `run_cloud_check`（同一套 assemble_v2 /
/// drift_fields），只是返回老的 AccessCheck 形状给 CLI / 旧界面。
fn check_tunnel_access(ctx: &Ctx, params: &Value) -> Result<Value, String> {
    let id = need_str(params, "id")?;
    let (mode, source) = manual_check_plan();
    let _manual = ManualCheckGuard::acquire(&id);
    let result = match run_cloud_check(ctx, &id, mode, source) {
        Ok(result) => result,
        Err(e) => return Err(finish_check_error(ctx, &id, e, true)),
    };
    ctx.state
        .set_protection_check(&id, check_for_state(&result));
    Ok(access_check_json(&result))
}

// ---------- 云端完整检查（D3 / C1 的 cloudcheck） ----------

fn parse_check_mode(params: &Value, default: CheckMode) -> Result<CheckMode, String> {
    match opt_str(params, "mode").as_deref() {
        None => Ok(default),
        Some("full") => Ok(CheckMode::Full),
        Some("drift") => Ok(CheckMode::Drift),
        Some("audit") => Ok(CheckMode::Audit),
        Some("env") => Ok(CheckMode::Env),
        Some(other) => Err(format!(
            "检查模式仅支持 full / drift / audit / env，不支持「{other}」"
        )),
    }
}

fn ssh_exec_for(
    target: &str,
    ssh_port: u16,
    identity_file: &Option<PathBuf>,
) -> shangxian_core::cleanup::SshExec {
    shangxian_core::cleanup::SshExec {
        ssh_exe: shangxian_core::ssh::default_ssh_exe(),
        target: target.to_string(),
        ssh_port,
        identity_file: identity_file.clone(),
    }
}

fn mode_timeout(mode: CheckMode) -> Duration {
    match mode {
        CheckMode::Full => Duration::from_secs(30),
        CheckMode::Drift => Duration::from_secs(25),
        CheckMode::Audit => Duration::from_secs(25),
        CheckMode::Env => Duration::from_secs(25),
    }
}

fn cached_env(ctx: &Ctx, server_id: &str) -> Option<CloudEnv> {
    ctx.state
        .server_env(server_id)
        .and_then(|value| serde_json::from_value::<CloudEnv>(value).ok())
}

/// 生成访问密码哈希：先探服务器上的 nginx 认不认 bcrypt，认→bcrypt，
/// 不认/说不准→本机 apr1（nginx 内置支持，任何版本都能验）。
///
/// 探测结果写进这台服务器的环境缓存（`server_env`），之后检查/界面都能看到。
/// 密码明文始终只在本机内存里，不落盘、不进命令行。
pub(crate) fn hash_password_adaptive(
    ctx: &Ctx,
    server_id: &str,
    exec: &shangxian_core::cleanup::SshExec,
    password: &str,
) -> Result<Value, String> {
    use shangxian_core::cryptprobe::{BcryptSupport, probe};

    // 优先用缓存里的 nginx 路径当线索；探测本身每次现跑（比缓存新，改面板后不会误判）
    let hint = cached_env(ctx, server_id).and_then(|e| e.nginx_bin);
    let probe = probe(exec, hint.as_deref(), Duration::from_secs(20)).ok();
    if let Some(p) = &probe {
        merge_bcrypt_cache(ctx, server_id, p);
    }
    let support = probe.as_ref().map(|p| p.support);
    if support == Some(BcryptSupport::Yes) {
        let hash = shangxian_core::cleanup::hash_password(exec, password)?;
        Ok(json!({"hash": hash, "format": "bcrypt"}))
    } else {
        let hash = shangxian_core::cleanup::hash_password_apr1(password)?;
        let note = match support {
            Some(BcryptSupport::No) => {
                "这台服务器的 nginx 不支持更安全的 bcrypt（多半是面板自带的旧加密库），已自动改用兼容格式；不影响使用。"
            }
            _ => "没能确认这台服务器支不支持 bcrypt，已自动用兼容格式；不影响使用。",
        };
        Ok(json!({"hash": hash, "format": "apr1", "note": note}))
    }
}

/// 把 bcrypt 探测结果并进这台服务器的环境缓存（保留缓存里别的字段）。
fn merge_bcrypt_cache(
    ctx: &Ctx,
    server_id: &str,
    probe: &shangxian_core::cryptprobe::BcryptProbe,
) {
    use shangxian_core::cryptprobe::BcryptSupport;

    let mut value = ctx.state.server_env(server_id).unwrap_or_else(|| json!({}));
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    obj.insert(
        "bcrypt".into(),
        match probe.support {
            BcryptSupport::Yes => json!(true),
            BcryptSupport::No => json!(false),
            BcryptSupport::Unknown => Value::Null,
        },
    );
    if let Some(lib) = probe.crypt_lib.as_ref() {
        obj.insert("cryptLib".into(), json!(lib));
    }
    if let Some(evidence) = probe.evidence.as_ref() {
        obj.insert("bcryptEvidence".into(), json!(evidence));
    }
    ctx.state.set_server_env(server_id, value);
}

/// 手动检查的来源标记。
const MANUAL_SOURCE: &str = "manual";

/// 正在手动检查的隧道 id。
static MANUAL_CHECKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 一次手动检查的占位；离开作用域自动还回去（panic 也不漏）。
struct ManualCheckGuard {
    id: String,
}

impl ManualCheckGuard {
    fn acquire(id: &str) -> Self {
        if let Ok(mut live) = MANUAL_CHECKS.lock() {
            if !live.iter().any(|x| x == id) {
                live.push(id.to_string());
            }
        }
        Self { id: id.to_string() }
    }
}

impl Drop for ManualCheckGuard {
    fn drop(&mut self) {
        if let Ok(mut live) = MANUAL_CHECKS.lock() {
            live.retain(|x| x != &self.id);
        }
    }
}

/// 一次云端检查里 SSH 失败时带着的 error 记录。
struct CloudCheckFailure {
    message: String,
    check: ProtectionCheck,
}

/// 云端检查失败的两类情况：前者还没连服务器（没有可写的检查记录），
/// 后者 SSH 跑了但没成（带着 error 记录，写不写由调用方按模式定）。
enum CloudCheckError {
    Setup(String),
    Failed(CloudCheckFailure),
}

/// env 模式只写 server_env 缓存，任何情况下都不改 protectionCheck（D0 §2）。
fn error_writes_protection(mode: CheckMode) -> bool {
    mode != CheckMode::Env
}

/// 检查失败收尾：必要时把 error 记录写进 state，再把错误文案抛给调用方。
/// `write_protection=false`（env 模式）绝不碰 protectionCheck（D0 §2）。
fn finish_check_error(
    ctx: &Ctx,
    tunnel_id: &str,
    error: CloudCheckError,
    write_protection: bool,
) -> String {
    match error {
        CloudCheckError::Setup(message) => message,
        CloudCheckError::Failed(failure) => {
            if write_protection {
                ctx.state.set_protection_check(tunnel_id, failure.check);
            }
            failure.message
        }
    }
}

/// 手检结果 → 落 state 的 ProtectionCheck（两条入口共用）。
fn check_for_state(result: &cloudcheck::CloudCheckResult) -> ProtectionCheck {
    result.check.clone()
}

/// 手检结果 → 老 `check_tunnel_access` 的返回形状（只回云端实际状态）。
fn access_check_json(result: &cloudcheck::CloudCheckResult) -> Value {
    to_json(&result.actual)
}

/// 手检结果 → `check_tunnel_cloud` / `run_access_audit` 的整包形状。
fn cloud_check_json(result: &cloudcheck::CloudCheckResult) -> Value {
    to_json(result)
}

/// 纯组装：同一份远端输出 + 同一份输入 → 同一份 ProtectionCheck。
/// 手动检查都过这里，判定全在 core 的 `assemble_v2` / `drift_fields`。
fn assemble_cloud_check(output: &str, input: &CheckInput<'_>) -> cloudcheck::CloudCheckResult {
    cloudcheck::assemble_v2(output, input)
}

/// 跑一次云端脚本（锁纪律：短锁取信息 → 放锁跑 SSH）。
/// `source` 写进检查结果（手动 = `"manual"`）。
/// 这里**不写 state**：写不写、写哪一条由调用方按模式决定（env 模式绝不能碰保护结果）。
fn run_cloud_check(
    ctx: &Ctx,
    tunnel_id: &str,
    mode: CheckMode,
    source: &str,
) -> Result<cloudcheck::CloudCheckResult, CloudCheckError> {
    let (req, view, server_id) = {
        let g = ctx
            .lock_guardian()
            .map_err(CloudCheckError::Setup)?;
        let req = g
            .access_check_request(tunnel_id)
            .map_err(CloudCheckError::Setup)?;
        let view = g
            .tunnel_snapshot()
            .into_iter()
            .find(|t| t.id == tunnel_id)
            .ok_or_else(|| CloudCheckError::Setup(format!("找不到隧道：{tunnel_id}")))?;
        let server_id = view.server_id.clone();
        (req, view, server_id)
    };
    let env = cached_env(ctx, &server_id);
    let nginx_bin = env.as_ref().and_then(|e| e.nginx_bin.clone());
    let script = cloudcheck::build_script(&ScriptInput {
        mode,
        domain: &req.domain,
        vhost_dir: &req.vhost_dir,
        remote_port: view.remote_port,
        nginx_bin: nginx_bin.as_deref(),
        log_path: None,
    })
    .map_err(CloudCheckError::Setup)?;
    let exec = ssh_exec_for(&req.target, req.ssh_port, &req.identity_file);
    let timeout = mode_timeout(mode);
    let ts = now_ms();

    let expected = expected_from_view(&view);
    let url_is_https = view
        .url
        .as_deref()
        .map(|u| u.trim().to_ascii_lowercase().starts_with("https://"))
        .unwrap_or(false);

    let output = match shangxian_core::cleanup::run(&exec, &script, timeout) {
        Ok(out) => out,
        Err(e) => {
            // 检查失败也留一条 error 记录（D2 §6.4）；D3 扩展位保留上次的。
            let mut check = error_check(expected, e.clone(), ts, source);
            check.extra = last_extra(ctx, tunnel_id);
            return Err(CloudCheckError::Failed(CloudCheckFailure {
                message: e,
                check,
            }));
        }
    };
    let mut result = assemble_cloud_check(
        &output,
        &CheckInput {
            domain: &req.domain,
            url_is_https,
            expected_password: expected.password,
            expected_ip: expected.ip,
            expected_allow_ips: &expected.allow_ips,
            remote_port: view.remote_port,
            source,
            ts,
            cached_env: env,
        },
    );
    // 完整检查额外做一次假账号验证：密码文件读不进去（例如格式这台 nginx 不认识）
    // 会在这里变成一句明确警告，而不是等用户发现网站 500。
    if mode == CheckMode::Full && result.actual.password_on && result.actual.htpasswd_exists {
        if let Ok(script) = cloudcheck::auth_probe_script(&req.domain) {
            if let Ok(out) = shangxian_core::cleanup::run(&exec, &script, Duration::from_secs(12)) {
                if let Some(code) = cloudcheck::parse_auth_probe(&out) {
                    result.actual.auth_probe = Some(code);
                    patch_auth_probe(&mut result, code);
                }
            }
        }
    }
    Ok(result)
}

/// 把假账号验证结果写进检查结果（`extra.authProbe` + 500 时的一句人话警告）。
fn patch_auth_probe(result: &mut cloudcheck::CloudCheckResult, code: u16) {
    let Some(extra) = result.check.extra.as_mut() else {
        return;
    };
    let Some(obj) = extra.as_object_mut() else {
        return;
    };
    obj.insert("authProbe".into(), json!(code));
    if let Some(warning) = cloudcheck::auth_probe_warning(code) {
        match obj.get_mut("warnings").and_then(|v| v.as_array_mut()) {
            Some(list) => list.push(json!(warning)),
            None => {
                obj.insert("warnings".into(), json!([warning]));
            }
        }
    }
}

fn check_tunnel_cloud(ctx: &Ctx, params: &Value) -> Result<Value, String> {
    let id = need_str(params, "id")?;
    let mode = parse_check_mode(params, CheckMode::Full)?;
    let _manual = ManualCheckGuard::acquire(&id);
    let result = match run_cloud_check(ctx, &id, mode, MANUAL_SOURCE) {
        Ok(result) => result,
        // env 模式只识别服务器环境，失败也不改 protectionCheck（D0 §2）。
        Err(e) => return Err(finish_check_error(ctx, &id, e, error_writes_protection(mode))),
    };

    if mode == CheckMode::Env {
        // env 模式只写 server_env 缓存，绝不动 protectionCheck。
        if let Some(env) = result.env.clone() {
            let server_id = {
                let g = ctx.lock_guardian()?;
                g.tunnel_snapshot()
                    .into_iter()
                    .find(|t| t.id == id)
                    .map(|t| t.server_id)
                    .ok_or_else(|| format!("找不到隧道：{id}"))?
            };
            let mut env = env;
            env.detected_at = Some(now_ms());
            if let Ok(value) = serde_json::to_value(&env) {
                ctx.state.set_server_env(&server_id, value);
            }
        }
        return Ok(cloud_check_json(&result));
    }

    // 手动检查总落盘（D3 §2.1）。
    ctx.state
        .set_protection_check(&id, check_for_state(&result));
    Ok(cloud_check_json(&result))
}

fn run_access_audit(ctx: &Ctx, params: &Value) -> Result<Value, String> {
    let id = need_str(params, "id")?;
    let _manual = ManualCheckGuard::acquire(&id);
    let result = match run_cloud_check(ctx, &id, CheckMode::Audit, MANUAL_SOURCE) {
        Ok(result) => result,
        Err(e) => return Err(finish_check_error(ctx, &id, e, true)),
    };
    ctx.state
        .set_protection_check(&id, check_for_state(&result));
    Ok(cloud_check_json(&result))
}

/// 把环境识别结果并进检查结果的 `extra.env`：保留 extra 里别的键（例如 https / port /
/// warnings），只换 env。extra 缺失或不是对象时就新建一个。
fn extra_with_env(extra: Option<&Value>, env_value: &Value) -> Value {
    let mut map = extra
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    map.insert("env".to_string(), env_value.clone());
    Value::Object(map)
}

/// 环境识别结果同时写进这台服务器上所有隧道的 `protectionCheck.extra.env`（保留其它键），
/// 这样「发布隧道」页的环境横幅切页 / 重开后还在（键名与 full 云端检查写的保持一致）。
/// `set_protection_check` 内部走 mark_dirty，攒着到时候落盘。
fn persist_env_on_server_tunnels(ctx: &Ctx, server_id: &str, env_value: &Value) {
    let tunnel_ids: Vec<String> = match ctx.lock_guardian() {
        Ok(g) => g
            .tunnel_snapshot()
            .into_iter()
            .filter(|t| t.server_id == server_id)
            .map(|t| t.id)
            .collect(),
        Err(_) => return,
    };
    for id in tunnel_ids {
        let mut check = ctx
            .state
            .tunnel_state(&id)
            .and_then(|s| s.protection_check)
            .unwrap_or_default();
        check.extra = Some(extra_with_env(check.extra.as_ref(), env_value));
        ctx.state.set_protection_check(&id, check);
    }
}

/// 识别这台服务器用的面板 / 网站服务，结果缓存进 state（D0 §2：不放 config），
/// 并同步到这台服务器各隧道的 `protectionCheck.extra.env`（发布页横幅要能读）。
fn scan_server_env(ctx: &Ctx, params: &Value) -> Result<Value, String> {
    let server_id = need_server_id(params)?;
    let (target, ssh_port, identity_file, domain, vhost_dir) = {
        let g = ctx.lock_guardian()?;
        let server = g
            .servers()
            .into_iter()
            .find(|s| s.id == server_id)
            .ok_or_else(|| format!("找不到云端服务器：{server_id}"))?;
        let tunnels = g.tunnel_snapshot();
        // ENV 脚本不用域名，但类型上必须给一个合法域名：优先用这台服务器上任一隧道的网址。
        let domain = tunnels
            .iter()
            .filter(|t| t.server_id == server_id)
            .filter_map(|t| t.url.as_deref())
            .map(normalize_domain)
            .find(|d| !d.is_empty())
            .unwrap_or_else(|| server.host.clone());
        let vhost_dir = server
            .vhost_dir
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| "/xp/panel/vhost".to_string());
        (
            format!("{}@{}", server.user, server.host),
            server.ssh_port,
            server.key_path.map(PathBuf::from),
            domain,
            vhost_dir,
        )
    };
    let cached_nginx = cached_env(ctx, &server_id)
        .as_ref()
        .and_then(|e| e.nginx_bin.clone());
    let script = cloudcheck::build_script(&ScriptInput {
        mode: CheckMode::Env,
        domain: &domain,
        vhost_dir: &vhost_dir,
        remote_port: 0,
        nginx_bin: cached_nginx.as_deref(),
        log_path: None,
    })?;
    let exec = ssh_exec_for(&target, ssh_port, &identity_file);
    let output = shangxian_core::cleanup::run(&exec, &script, mode_timeout(CheckMode::Env))?;
    let result = cloudcheck::assemble_v2(
        &output,
        &CheckInput {
            domain: &domain,
            url_is_https: false,
            expected_password: false,
            expected_ip: false,
            expected_allow_ips: &[],
            remote_port: 0,
            source: "manual",
            ts: now_ms(),
            cached_env: None,
        },
    );
    let mut env = result
        .env
        .ok_or_else(|| "环境识别未获取到结果（无法解析服务器返回的内容），请稍后重试".to_string())?;
    env.detected_at = Some(now_ms());
    if let Ok(value) = serde_json::to_value(&env) {
        ctx.state.set_server_env(&server_id, value.clone());
        persist_env_on_server_tunnels(ctx, &server_id, &value);
    }
    Ok(to_json(&env))
}

/// 公网 IP 助手：读服务器看到的 `$SSH_CONNECTION`（只读、不改服务器）。
fn get_public_ip(ctx: &Ctx) -> Result<Value, String> {
    let (target, ssh_port, identity_file, server_name) = {
        let g = ctx.lock_guardian()?;
        let tunnels = g.tunnel_snapshot();
        let servers = g.servers();
        let server_id = tunnels
            .iter()
            .find(|t| t.enabled)
            .map(|t| t.server_id.clone())
            .or_else(|| servers.first().map(|s| s.id.clone()))
            .ok_or_else(|| "尚未配置云端服务器：请先添加一台服务器，再获取公网 IP".to_string())?;
        let server = servers
            .into_iter()
            .find(|s| s.id == server_id)
            .ok_or_else(|| "未找到这台云端服务器".to_string())?;
        (
            format!("{}@{}", server.user, server.host),
            server.ssh_port,
            server.key_path.map(PathBuf::from),
            server.name,
        )
    };
    let exec = ssh_exec_for(&target, ssh_port, &identity_file);
    let script = r#"printf '%s' "${SSH_CONNECTION:-${SSH_CLIENT:-}}" | awk '{print $1}'"#;
    let output = shangxian_core::cleanup::run(&exec, script, Duration::from_secs(15))?;
    let first = output.lines().next().unwrap_or("").trim().to_string();
    let ip = if first.is_empty() || first == "unknown" {
        None
    } else {
        Some(first)
    };
    let message = match &ip {
        Some(ip) => format!(
            "服务器看到的公网 IP 是 {ip}。更换网络后可能变化；运营商共享 IP 时白名单可能不生效。"
        ),
        None => "本次未获取到公网 IP（服务器无响应）；请更换网络或稍后重试。".to_string(),
    };
    Ok(json!({
        "ip": ip,
        "source": "sshConnection",
        "at": now_ms(),
        "server": server_name,
        "message": message,
    }))
}

/// 配免密：锁只拿两次（拿连接信息、写回密钥路径），中间那段 SSH（最长十几秒）不占锁。
fn setup_passwordless(ctx: &Ctx, params: &Value) -> Result<Value, String> {
    let server_id = need_server_id(params)?;
    let password = need_str(params, "password")?;
    let key_path = shangxian_core::config::key_path_for(&ctx.data_dir, &server_id);
    let conn = crate::server_conn(&ctx.guardian, &server_id)?;

    let result = shangxian_core::keyboot::bootstrap_key(
        &conn.host,
        conn.ssh_port,
        &conn.user,
        &password,
        &key_path,
    )?;
    ctx.lock_guardian()?
        .set_server_key(&server_id, &result.key_path)?;
    Ok(json!({
        "keyPath": result.key_path,
        "message": format!("配置完成。以后连接这台服务器都使用密钥，无需再输入密码。密钥位置：{}", result.key_path),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_only_numeric_parts() {
        assert!(version_is_newer("0.5.0", "0.6.0"));
        assert!(version_is_newer("0.5.0", "v0.5.1"));
        assert!(version_is_newer("0.5.9", "0.6"));
        assert!(!version_is_newer("0.6.0", "0.6.0"));
        assert!(!version_is_newer("0.6.0", "0.5.9"));
        assert!(!version_is_newer("0.6.0", "nightly"));
    }

    #[test]
    fn epoch_prefix_and_level() {
        assert_eq!(
            split_epoch_prefix("[1760000000000] 后面是正文"),
            (Some(1760000000000), "后面是正文")
        );
        assert_eq!(split_epoch_prefix("没有前缀"), (None, "没有前缀"));
        assert_eq!(split_epoch_prefix("[abc] 坏的"), (None, "[abc] 坏的"));
        assert_eq!(
            detect_level("2026-10-12 [WARN] [看护] x"),
            Some("warn".to_string())
        );
        assert_eq!(
            detect_level("2026-10-12 [INFO] [启动] x"),
            Some("info".to_string())
        );
        assert_eq!(detect_level("原始 ssh 输出"), None);
    }

    #[test]
    fn hhmmss_uses_local_day() {
        // 2026-10-12 00:00:00 UTC 前后都能算出 6 位时间；只验证格式稳定
        let text = hhmmss(1_760_000_000_000);
        assert_eq!(text.len(), 8);
        assert_eq!(text.chars().filter(|c| *c == ':').count(), 2);
    }

    #[test]
    fn scrub_removes_secrets() {
        let text = r"ssh password=Sup3rSecret token: abcdef C:\Users\小明\AppData";
        let out = scrub_text(text);
        assert!(!out.contains("Sup3rSecret"), "{out}");
        assert!(!out.contains("abcdef"), "{out}");
        assert!(out.contains("password=<已省略>"), "{out}");
        assert!(out.contains(r"C:\Users\<用户>"), "{out}");
    }

    #[test]
    fn scrub_removes_bcrypt_and_private_key() {
        let hash = "$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0";
        let text = format!("accessHash={hash}");
        let out = scrub_text(&text);
        assert!(!out.contains("$2y$"), "{out}");
        // apr1（兼容格式）同样不能漏
        let apr1 = "$apr1$LSzMVDV.$pfCdvnQ0zdqDq05f5WBU90";
        let out = scrub_text(&format!("accessHash={apr1}"));
        assert!(!out.contains("$apr1$"), "{out}");
        assert!(out.contains("密码哈希已省略"), "{out}");
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----";
        let out = scrub_text(key);
        assert!(out.contains("<私钥内容已省略>"), "{out}");
        assert!(!out.contains("abc"), "{out}");
    }

    #[test]
    fn redacted_config_hides_secrets() {
        let raw = serde_json::json!({
            "version": 6,
            "notifyEnabled": true,
            "settings": { "refreshIntervalMs": 800 },
            "servers": [{ "id": "srv-1", "name": "香港", "host": "h", "sshPort": 22, "user": "root", "keyPath": "C:/k" }],
            "services": [{ "id": "svc-1", "name": "相册", "localPort": 1, "launch": { "exe": "a.exe", "args": "--secret", "workdir": "D:/x" } }],
            "tunnels": [{ "id": "tun-1", "name": "站", "serviceId": "svc-1", "serverId": "srv-1", "remotePort": 1, "accessHash": "$2y$12$xxx", "keyPath": null }]
        });
        let export = redacted_config(&raw, false, false);
        let text = serde_json::to_string(&export).unwrap();
        assert!(!text.contains("accessHash"), "{text}");
        assert!(!text.contains("\"keyPath\""), "{text}");
        assert!(text.contains("\"keyPathPresent\":false"), "{text}");
        assert!(text.contains("--secret"), "导出保留启动参数：{text}");

        let diag = redacted_config(&raw, true, false);
        let text = serde_json::to_string(&diag).unwrap();
        assert!(!text.contains("--secret"), "{text}");
        assert!(text.contains("argsLength"), "{text}");
    }

    #[test]
    fn redacted_state_keeps_names_but_not_hashes() {
        let raw = serde_json::json!({
            "stateVersion": 1,
            "dayKey": "2026-10-12",
            "lastSavedAt": 1,
            "updateInfo": { "status": "upToDate" },
            "tunnels": { "tun-1": { "lastFailure": null, "protectionCheck": { "status": "ok" }, "stats": { "reconnectsToday": 2 } } },
            "trash": [{
                "token": "und-1", "kind": "tunnel", "deletedAt": 1, "expiresAt": 2,
                "item": { "id": "tun-1", "name": "旧站", "accessHash": "$2y$12$abc", "keyPath": "C:/k" },
                "dependencies": { "serviceId": "相册" }
            }]
        });
        let out = redacted_state(&raw);
        let text = serde_json::to_string(&out).unwrap();
        assert!(!text.contains("$2y$"), "{text}");
        assert!(!text.contains("\"keyPath\""), "{text}");
        assert!(text.contains("reconnectsToday"), "{text}");
        assert!(text.contains("旧站"), "{text}");
    }

    #[test]
    fn zip_has_expected_structure() {
        let entries = vec![
            ("meta.json".to_string(), b"{}".to_vec()),
            ("logs/app.tail.txt".to_string(), b"hello".to_vec()),
        ];
        let zip = build_zip(&entries);
        assert_eq!(&zip[0..4], b"PK\x03\x04");
        let text = String::from_utf8_lossy(&zip).to_string();
        assert!(text.contains("meta.json"));
        assert!(text.contains("logs/app.tail.txt"));
        // EOCD：最后 22 字节的开头是 PK\x05\x06，条目数在 offset +10
        let eocd = zip.len() - 22;
        assert_eq!(&zip[eocd..eocd + 4], b"PK\x05\x06");
        let count = u16::from_le_bytes([zip[eocd + 10], zip[eocd + 11]]);
        assert_eq!(count, 2);
    }

    #[test]
    fn access_and_cloud_paths_agree_on_same_input() {
        // 同一份云端输出 + 同一份期望：老 `check_tunnel_access`（只回 actual）和新
        // `check_tunnel_cloud`（整包）都走 `assemble_cloud_check` → core 的 assemble_v2 /
        // drift_fields，判定必须一模一样。
        let output = r#"===SHANGXIAN-CONF-BEGIN===
server {
    auth_basic "密码";
    auth_basic_user_file /xp/panel/vhost/a.example.com.htpasswd;
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;
        let (mode, source) = manual_check_plan();
        assert_eq!(mode, CheckMode::Full);
        let input = CheckInput {
            domain: "a.example.com",
            url_is_https: true,
            expected_password: true,
            expected_ip: false,
            expected_allow_ips: &[],
            remote_port: 8080,
            source,
            ts: 42,
            cached_env: None,
        };
        let via_access = assemble_cloud_check(output, &input);
        let via_cloud = assemble_cloud_check(output, &input);

        assert_eq!(
            serde_json::to_value(check_for_state(&via_access)).unwrap(),
            serde_json::to_value(check_for_state(&via_cloud)).unwrap()
        );
        // 两边的返回只是同一结果的不同投影
        assert_eq!(access_check_json(&via_access), to_json(&via_cloud.actual));
        assert_eq!(
            cloud_check_json(&via_cloud)["actual"],
            access_check_json(&via_access)
        );
        // 漂移判定只由 core 的 drift_fields 说了算（旧的手写算法已删）
        assert_eq!(via_access.check.drift_fields, vec!["conf".to_string()]);
        assert_eq!(via_access.check.source.as_deref(), Some("manual"));
    }

    #[test]
    fn env_mode_never_touches_protection_check() {
        assert!(!error_writes_protection(CheckMode::Env));
        assert!(error_writes_protection(CheckMode::Full));
        assert!(error_writes_protection(CheckMode::Drift));
        assert!(error_writes_protection(CheckMode::Audit));
    }

    #[test]
    fn stop_all_report_keeps_the_four_json_keys() {
        let report = shangxian_core::guardian::StopAllReport {
            tunnels_stopped: 2,
            tunnels_failed: 1,
            services_stopped: 3,
            services_failed: 0,
        };
        let value = to_json(&report);
        assert_eq!(value["tunnelsStopped"], 2);
        assert_eq!(value["tunnelsFailed"], 1);
        assert_eq!(value["servicesStopped"], 3);
        assert_eq!(value["servicesFailed"], 0);
        assert_eq!(value.as_object().unwrap().len(), 4);
    }

    #[test]
    fn logs_truncation_marks_every_source_and_keeps_order() {
        // 单来源命中 lines 上限 → 算截断
        let mut group = SourceLog::new("app");
        group.entries = vec![json!({"raw": "a"}), json!({"raw": "b"})];
        assert!(source_hit_limit(&group, 2));
        assert!(!source_hit_limit(&group, 3));

        // 总量封顶：按 events → app → tunnel 的冻结顺序从前往后留，后面的先丢
        let mut groups = vec![
            SourceLog {
                id: "events".into(),
                entries: vec![json!({"raw": "e1"}), json!({"raw": "e2"})],
            },
            SourceLog {
                id: "app".into(),
                entries: vec![json!({"raw": "a1"}), json!({"raw": "a2"})],
            },
            SourceLog {
                id: "tunnel:t1".into(),
                entries: vec![json!({"raw": "t1"})],
            },
        ];
        assert!(cap_log_groups(&mut groups, 3));
        assert_eq!(groups[0].entries.len(), 2);
        assert_eq!(groups[1].entries.len(), 1);
        assert!(groups[2].entries.is_empty());
        let ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["events", "app", "tunnel:t1"]);
        // 没超就不动、不标
        assert!(!cap_log_groups(&mut groups, 100));
    }

    #[test]
    fn utf8_cut_never_splits_a_char() {
        let text = "汉字日志尾巴".repeat(1000);
        let cut = utf8_ceil_boundary(&text, 5);
        assert!(text.is_char_boundary(cut));
        assert!(text[cut..].starts_with("日志尾巴"), "{}", &text[cut..12]);
        assert_eq!(utf8_ceil_boundary(&text, text.len() + 99), text.len());
    }

    #[test]
    fn diag_temp_path_lives_next_to_final() {
        let final_path = PathBuf::from("data/diag/diag-1.zip");
        let tmp = temp_diag_path(&final_path);
        assert_eq!(tmp.parent(), final_path.parent());
        assert_eq!(tmp.extension().unwrap(), "part");
        assert_ne!(tmp, final_path);
    }

    #[test]
    fn scrub_removes_json_and_cli_secret_forms() {
        let text = r#"{"password":"Sup3rSecret","token": "tg-abcdef","note":"ok","keyPath":"D:\\keys\\id_rsa"} --password cli-secret"#;
        let out = scrub_text(text);
        assert!(!out.contains("Sup3rSecret"), "{out}");
        assert!(!out.contains("tg-abcdef"), "{out}");
        assert!(!out.contains("cli-secret"), "{out}");
        // doctor.json 里的真实密钥路径也不能带出去（D2 §12.2）
        assert!(!out.contains("id_rsa"), "{out}");
        assert!(out.contains(r#""password":"<已省略>""#), "{out}");
        assert!(out.contains(r#""token": "<已省略>""#), "{out}");
        assert!(out.contains("--password <已省略>"), "{out}");
        // 不是旗标的普通词不动（`user-password is required` 里的 is 不该被吃掉）
        let plain = scrub_text("user-password is required");
        assert!(plain.contains("is required"), "{plain}");
    }

    #[test]
    fn delete_result_shapes_undo_token() {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Info {
            kind: String,
            id: String,
            name: String,
            token: String,
            expires_at: i64,
        }
        let info = Info {
            kind: "tunnel".into(),
            id: "tun-1".into(),
            name: "旧站".into(),
            token: "und-1".into(),
            expires_at: 42,
        };
        let value = dispatch_delete_result("tunnel", &info);
        assert_eq!(value["deleted"]["type"], "tunnel");
        assert_eq!(value["deleted"]["name"], "旧站");
        assert_eq!(value["undoToken"], "und-1");
        assert_eq!(value["undoExpiresAt"], 42);
    }

    #[test]
    fn extra_with_env_keeps_other_keys() {
        let env = serde_json::json!({ "panel": "bt", "web": "nginx", "detectedAt": 1 });
        let existing = serde_json::json!({ "https": { "enabled": true }, "warnings": ["x"] });
        let merged = extra_with_env(Some(&existing), &env);
        assert_eq!(merged["env"], env);
        assert_eq!(merged["https"]["enabled"], true);
        assert_eq!(merged["warnings"][0], "x");

        // 旧 env 要被新结果换掉，别的键还在
        let old = serde_json::json!({ "env": { "panel": "xp" }, "port": { "bound": "loopback" } });
        let merged = extra_with_env(Some(&old), &env);
        assert_eq!(merged["env"], env);
        assert_eq!(merged["port"]["bound"], "loopback");

        // 没有 extra / extra 不是对象：只留 env
        assert_eq!(extra_with_env(None, &env), serde_json::json!({ "env": env }));
        let weird = serde_json::json!("不是对象");
        assert_eq!(extra_with_env(Some(&weird), &env), serde_json::json!({ "env": env }));
    }

    #[test]
    fn domain_and_allow_list_normalization() {
        assert_eq!(
            normalize_domain("https://Album.Example.com/x?y=1"),
            "Album.Example.com"
        );
        assert_eq!(normalize_domain("http://a.b/c"), "a.b");
        assert_eq!(
            split_allow_list("1.2.3.4, 5.6.7.8  9.9.9.9"),
            vec!["1.2.3.4", "5.6.7.8", "9.9.9.9"]
        );
    }
}
