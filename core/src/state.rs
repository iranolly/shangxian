//! 运行状态（`state.json` v1）：重启不失忆的运行事实。
//!
//! - 机器私有：整份删掉 = 一切回到初次运行，无副作用；任何字段都不进 `config.json`。
//! - 单一写者：只有运行中的应用进程写（CLI 改配置走 ctl 交给应用）。
//! - `mark_dirty()` 去抖落盘：距上次落盘 ≥1 秒才写，且脏状态最多挂 5 秒必写；
//!   退出前 `flush()`。
//! - 损坏 → 改名 `state.json.bad`；版本过高 → 改名 `state.json.unsupported`；
//!   都用默认状态继续，不弹红条（由调用方补一条 `stateReset` 事件）。
//!
//! 契约见 `.setup/design-v2/D2-contracts.md` §4、§17.2。

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, LocalService, ServerConfig, Tunnel};
use crate::reason::ReasonCode;

/// `state.json` 的行格式版本（冻结）。
pub const STATE_VERSION: u32 = 1;
/// 墓碑存活期：10 分钟。
pub const TRASH_TTL_MS: i64 = 10 * 60 * 1000;
/// 墓碑最多留 20 条（超出丢最旧）。
pub const TRASH_MAX: usize = 20;
/// 失败原因人话最长 200 字符。
pub const FAILURE_REASON_MAX_CHARS: usize = 200;
/// 白名单最多保留 64 条。
pub const ALLOW_IPS_MAX: usize = 64;
/// 去抖：距上次落盘至少 1 秒才写。
pub const DEBOUNCE_MS: i64 = 1000;
/// 脏状态最多挂 5 秒必写（防止一直等不到落盘窗口）。
pub const MAX_DIRTY_AGE_MS: i64 = 5_000;

// ---------- 时间工具 ----------

/// 当前 Unix epoch 毫秒。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 把 `days`（1970-01-01 起的天数）转成公历 (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 公历 (年, 月, 日) → 1970-01-01 起的天数。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 用某个时区偏移把 epoch 毫秒算成 `"YYYY-MM-DD"`（纯函数，可单测）。
pub fn day_key_from_ms(now_ms: i64, offset_minutes: i64) -> String {
    let shifted = now_ms + offset_minutes * 60_000;
    let days = shifted.div_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 本机时区相对 UTC 的分钟偏移（含夏令时，取当下时刻的值）。
#[cfg(windows)]
pub fn local_utc_offset_minutes() -> i64 {
    use windows_sys::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    unsafe {
        let mut local: windows_sys::Win32::Foundation::SYSTEMTIME = std::mem::zeroed();
        let mut utc: windows_sys::Win32::Foundation::SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
        let to_ms = |st: &windows_sys::Win32::Foundation::SYSTEMTIME| -> i64 {
            let days = days_from_civil(st.wYear as i64, st.wMonth as i64, st.wDay as i64);
            days * 86_400_000
                + st.wHour as i64 * 3_600_000
                + st.wMinute as i64 * 60_000
                + st.wSecond as i64 * 1000
        };
        (((to_ms(&local) - to_ms(&utc)) as f64) / 60_000.0).round() as i64
    }
}

/// 非 Windows 平台没有本机时区信息，退回 UTC（本产品只发 Windows）。
#[cfg(not(windows))]
pub fn local_utc_offset_minutes() -> i64 {
    0
}

/// 本机自然日 `"YYYY-MM-DD"`。
pub fn local_day_key(now_ms: i64) -> String {
    day_key_from_ms(now_ms, local_utc_offset_minutes())
}

/// 人话文本按「字符」截断（不是字节，中文不会被切坏）。
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

// ---------- 数据结构（D2 §4.1，字段名冻结） ----------

/// `state.json` 顶层。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RuntimeState {
    /// 行格式版本，恒为 1。
    pub state_version: u32,
    /// 本机自然日 `"YYYY-MM-DD"`；变化时重置当日计数。
    pub day_key: String,
    /// 最近一次落盘时刻（epoch ms）。
    pub last_saved_at: i64,
    /// 最近一次更新检查结果。
    pub update_info: UpdateInfo,
    /// key = 隧道 id。
    pub tunnels: BTreeMap<String, TunnelState>,
    /// key = 服务 id。
    pub services: BTreeMap<String, ServiceState>,
    /// 删除撤销记录（最多 20 条，存活 10 分钟）。
    pub trash: Vec<TrashRecord>,
    /// key = 服务器 id；云端环境识别缓存（D0 §2 裁决：放 state 不放 config）。
    /// 结构由 D3/C1 决定，这里只存 JSON、只做透传。
    pub servers: BTreeMap<String, serde_json::Value>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            state_version: STATE_VERSION,
            day_key: String::new(),
            last_saved_at: 0,
            update_info: UpdateInfo::default(),
            tunnels: BTreeMap::new(),
            services: BTreeMap::new(),
            trash: Vec::new(),
            servers: BTreeMap::new(),
        }
    }
}

/// 一条隧道的运行状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TunnelState {
    /// 最近一次失败（连上成功后清空）。重启后仍在 → 界面能显示上次为啥断。
    pub last_failure: Option<FailureRecord>,
    /// 最近一次云端保护核对结果。
    pub protection_check: Option<ProtectionCheck>,
    pub stats: TunnelStats,
}

/// 一条隧道的当日统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TunnelStats {
    /// 今日重连次数（同一本地自然日、同一隧道第 2 次及以后的成功连接各计 1）。
    pub reconnects_today: u32,
    /// 今天是否已经成功连过（区分「今日首连」与「重连」；跨日清零）。
    pub connected_today: bool,
    /// 最近一次成功连接的时刻（epoch ms）。
    pub last_connected_at: Option<i64>,
}

/// 一台本机服务的运行状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServiceState {
    /// 最近一次失败（health 变 up 时清空）。
    pub last_failure: Option<FailureRecord>,
}

/// 最近一次失败的人话记录。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FailureRecord {
    pub ts: i64,
    /// 人话（§7 文案，≤200 字符）。
    pub reason: String,
    /// §7 的 ReasonCode（`"authDenied"` 等）。
    pub code: String,
}

impl FailureRecord {
    /// 造一条（reason 自动按 200 字符截断）。
    pub fn new(ts: i64, code: ReasonCode, reason: &str) -> Self {
        Self {
            ts,
            reason: truncate_chars(reason, FAILURE_REASON_MAX_CHARS),
            code: code.as_str().to_string(),
        }
    }
}

/// 最近一次云端保护核对结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProtectionCheck {
    /// `"unknown" | "ok" | "drift" | "error"`。
    pub status: String,
    pub ts: Option<i64>,
    /// `"manual" | "auto"`；status=unknown 时为 null。
    pub source: Option<String>,
    /// 云端实际状态；status=ok/drift 必有；unknown/error 为 null。
    pub cloud: Option<CloudProtection>,
    /// 检查时的「设置期望」快照。
    pub expected: ExpectedProtection,
    /// drift 字段集合：`"conf" | "password" | "ip" | "allowIps" | "htpasswd"`。
    pub drift_fields: Vec<String>,
    /// status="error" 时的人话原因；其余为 null。
    pub error: Option<String>,
    /// D3 专用扩展位；D2 不认识、只透传。**不允许**放密码哈希/token/密钥内容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

impl Default for ProtectionCheck {
    fn default() -> Self {
        Self {
            status: "unknown".to_string(),
            ts: None,
            source: None,
            cloud: None,
            expected: ExpectedProtection::default(),
            drift_fields: Vec::new(),
            error: None,
            extra: None,
        }
    }
}

/// 云端实际保护状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudProtection {
    pub conf_found: bool,
    pub password_on: bool,
    pub ip_on: bool,
    pub allow_ips: Vec<String>,
    pub htpasswd_exists: bool,
}

/// 检查时的「设置期望」快照。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExpectedProtection {
    pub password: bool,
    pub ip: bool,
    pub allow_ips: Vec<String>,
    /// 规范化后的域名。
    pub url: String,
}

/// 更新检查结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateInfo {
    /// `"unknown" | "upToDate" | "available" | "error"`。
    pub status: String,
    /// `"0.6.1"`；没有就是 null。
    pub latest: Option<String>,
    /// 发布页地址。
    pub url: Option<String>,
    pub checked_at: Option<i64>,
    /// status="error" 的人话原因。
    pub error: Option<String>,
}

impl Default for UpdateInfo {
    fn default() -> Self {
        Self {
            status: "unknown".to_string(),
            latest: None,
            url: None,
            checked_at: None,
            error: None,
        }
    }
}

/// 删除撤销墓碑。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrashRecord {
    /// `"und-<8位十六进制>"`。
    pub token: String,
    /// `"service" | "tunnel" | "server"`。
    pub kind: String,
    pub deleted_at: i64,
    /// deleted_at + 10 分钟。
    pub expires_at: i64,
    /// 删除时的配置记录原文（含 accessHash；state.json 原文不进诊断包/导出）。
    pub item: serde_json::Value,
    /// 依赖名字快照：serviceId → 名字、serverId → 名字（撤销失败时给人话用）。
    pub dependencies: BTreeMap<String, String>,
}

impl Default for TrashRecord {
    fn default() -> Self {
        Self {
            token: String::new(),
            kind: String::new(),
            deleted_at: 0,
            expires_at: 0,
            item: serde_json::Value::Null,
            dependencies: BTreeMap::new(),
        }
    }
}

/// 生成一个撤销令牌：`und-` + 8 位十六进制。
pub fn new_undo_token() -> String {
    format!("und-{:08x}", rand::random::<u32>())
}

/// 造一条墓碑（token/时间戳自动填好）。
pub fn new_trash(
    kind: &str,
    item: serde_json::Value,
    dependencies: BTreeMap<String, String>,
    now_ms: i64,
) -> TrashRecord {
    TrashRecord {
        token: new_undo_token(),
        kind: kind.to_string(),
        deleted_at: now_ms,
        expires_at: now_ms + TRASH_TTL_MS,
        item,
        dependencies,
    }
}

// ---------- 撤销：把墓碑插回配置 ----------

/// 把墓碑里的记录重新插回配置（纯函数，方便单测，也方便 ctl 直接复用）。
///
/// 语义（D2 §10.1）：
/// - 隧道一律恢复为暂停（`enabled:false`），不管删之前是不是在发布；
/// - 服务只恢复登记信息，不启动进程；`watch`/`autostart` 原样保留；
/// - 服务器原样恢复，没有任何进程动作；
/// - 同 id 已存在 → 拒绝；隧道依赖的 serviceId/serverId 不存在 → 拒绝（列出缺的名字）。
pub fn insert_restored(cfg: &AppConfig, rec: &TrashRecord) -> Result<AppConfig, String> {
    let mut next = cfg.clone();
    match rec.kind.as_str() {
        "tunnel" => {
            let mut tunnel: Tunnel = serde_json::from_value(rec.item.clone())
                .map_err(|e| format!("撤销不了：记录里这条隧道读不出来（{e}）"))?;
            if next.tunnels.iter().any(|t| t.id == tunnel.id) {
                return Err(format!(
                    "撤销不了：已经有一条 id 是 {} 的隧道了，先把同 id 的删掉再撤销",
                    tunnel.id
                ));
            }
            let mut missing: Vec<String> = Vec::new();
            if next.service(&tunnel.service_id).is_none() {
                missing.push(dep_name(rec, "serviceId", &tunnel.service_id, "本机服务"));
            }
            if next.servers.iter().all(|s| s.id != tunnel.server_id) {
                missing.push(dep_name(rec, "serverId", &tunnel.server_id, "云端服务器"));
            }
            if !missing.is_empty() {
                return Err(format!(
                    "撤销不了：这条隧道要用的{}不在了；先把它们补回来，或者重新建一条隧道",
                    missing.join("、")
                ));
            }
            // 恢复后一律暂停
            tunnel.enabled = false;
            next.tunnels.push(tunnel);
            Ok(next)
        }
        "service" => {
            let service: LocalService = serde_json::from_value(rec.item.clone())
                .map_err(|e| format!("撤销不了：记录里这台本机服务读不出来（{e}）"))?;
            if next.services.iter().any(|s| s.id == service.id) {
                return Err(format!(
                    "撤销不了：已经有一台 id 是 {} 的本机服务了，先把同 id 的删掉再撤销",
                    service.id
                ));
            }
            next.services.push(service);
            Ok(next)
        }
        "server" => {
            let server: ServerConfig = serde_json::from_value(rec.item.clone())
                .map_err(|e| format!("撤销不了：记录里这台云端服务器读不出来（{e}）"))?;
            if next.servers.iter().any(|s| s.id == server.id) {
                return Err(format!(
                    "撤销不了：已经有一台 id 是 {} 的云端服务器了，先把同 id 的删掉再撤销",
                    server.id
                ));
            }
            next.servers.push(server);
            Ok(next)
        }
        other => Err(format!("撤销不了：不认识的记录类型「{other}」")),
    }
}

fn dep_name(rec: &TrashRecord, key: &str, id: &str, kind: &str) -> String {
    match rec.dependencies.get(key) {
        Some(name) => format!("「{name}」（{kind}）"),
        None => format!("（{kind} {id}）"),
    }
}

// ---------- 读写 ----------

/// `state.json` 损坏或版本过高时，为什么重置了运行记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateResetReason {
    /// 解析失败（原文件改名成 `state.json.bad`）。
    Corrupt,
    /// 版本比程序新（原文件改名成 `state.json.unsupported`）。
    Unsupported,
}

impl StateResetReason {
    /// 事件码（`stateReset`）。
    pub fn code(self) -> ReasonCode {
        ReasonCode::StateReset
    }

    /// 人话（D2 §16 文案）。
    pub fn message(self) -> &'static str {
        "运行记录重新开始了（不影响你的设置）"
    }
}

/// 运行状态容器：自带锁，可跨线程共享（`Arc<StateStore>`）。
pub struct StateStore {
    path: PathBuf,
    inner: Mutex<Inner>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    reset_reason: Option<StateResetReason>,
}

struct Inner {
    state: RuntimeState,
    dirty: bool,
    dirty_since: Option<i64>,
    last_error: Option<String>,
}

impl std::fmt::Debug for StateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateStore")
            .field("path", &self.path)
            .field("reset_reason", &self.reset_reason)
            .finish_non_exhaustive()
    }
}

impl StateStore {
    /// 从数据目录加载 `state.json`；损坏 / 版本过高自动降级成默认状态
    /// （原因看 [`StateStore::reset_reason`]，调用方据此补一条 `stateReset` 事件）。
    pub fn load(data_dir: &Path) -> Self {
        Self::with_clock(data_dir, Box::new(now_ms))
    }

    /// 带可替换时钟的构造（单测用，注入假时间把去抖/跨日测准）。
    pub fn with_clock(data_dir: &Path, now: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        let path = data_dir.join("state.json");
        let mut reset_reason = None;
        let mut state = read_state(&path, &mut reset_reason, &*now);
        // 启动时跨日检查一次
        let today = local_day_key(now());
        if state.day_key != today {
            reset_day_stats(&mut state);
            state.day_key = today;
        }
        // 启动时清掉已经过期的撤销墓碑（§4.3）
        state.trash.retain(|r| r.expires_at > now());
        Self {
            path,
            inner: Mutex::new(Inner {
                state,
                dirty: false,
                dirty_since: None,
                last_error: None,
            }),
            now,
            reset_reason,
        }
    }

    /// `state.json` 的完整路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 加载时是否因为损坏/版本过高重置过（None = 正常读入或本来就是新文件）。
    pub fn reset_reason(&self) -> Option<StateResetReason> {
        self.reset_reason
    }

    /// 上次落盘失败的原因（成功一次后清空）。
    pub fn last_error(&self) -> Option<String> {
        self.lock().last_error.clone()
    }

    /// 数据变了：去抖落盘调度。距上次落盘 ≥1 秒立即写；否则挂脏，等 `tick`/`flush`。
    pub fn mark_dirty(&self) {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.dirty = true;
        if inner.dirty_since.is_none() {
            inner.dirty_since = Some(now);
        }
        if now - inner.state.last_saved_at >= DEBOUNCE_MS {
            let _ = self.save_locked(&mut inner, now);
        }
    }

    /// 调度线程每秒调一次：满足去抖窗口或脏状态挂了 5 秒就落盘。
    pub fn tick(&self) {
        let now = (self.now)();
        let mut inner = self.lock();
        if !inner.dirty {
            return;
        }
        let due = now - inner.state.last_saved_at >= DEBOUNCE_MS
            || inner
                .dirty_since
                .map(|t| now - t >= MAX_DIRTY_AGE_MS)
                .unwrap_or(false);
        if due {
            let _ = self.save_locked(&mut inner, now);
        }
    }

    /// 退出前同步落盘（没脏就不写）。
    pub fn flush(&self) {
        let now = (self.now)();
        let mut inner = self.lock();
        if inner.dirty {
            let _ = self.save_locked(&mut inner, now);
        }
    }

    /// 跨日重置（应用运行中每分钟调一次 + 启动时一次）。返回是否真的跨天了。
    pub fn day_rollover(&self, now_ms: i64) -> bool {
        let today = local_day_key(now_ms);
        let mut inner = self.lock();
        if inner.state.day_key == today {
            return false;
        }
        reset_day_stats(&mut inner.state);
        inner.state.day_key = today;
        self.touch_locked(&mut inner);
        true
    }

    /// 记一次失败（隧道/本机服务）。kind = `"tunnel"` / `"service"`。
    pub fn record_failure(
        &self,
        kind: &str,
        id: &str,
        code: ReasonCode,
        reason: &str,
        now_ms: i64,
    ) {
        let record = FailureRecord::new(now_ms, code, reason);
        let mut inner = self.lock();
        match kind {
            "tunnel" => {
                inner
                    .state
                    .tunnels
                    .entry(id.to_string())
                    .or_default()
                    .last_failure = Some(record);
            }
            "service" => {
                inner
                    .state
                    .services
                    .entry(id.to_string())
                    .or_default()
                    .last_failure = Some(record);
            }
            _ => return,
        }
        self.touch_locked(&mut inner);
    }

    /// 清掉一条失败记录（隧道连上 / 服务 health 变 up）。kind = `"tunnel"` / `"service"`。
    pub fn clear_failure(&self, kind: &str, id: &str) {
        let mut inner = self.lock();
        let cleared = match kind {
            "tunnel" => inner
                .state
                .tunnels
                .get_mut(id)
                .map(|entry| entry.last_failure.take().is_some())
                .unwrap_or(false),
            "service" => inner
                .state
                .services
                .get_mut(id)
                .map(|entry| entry.last_failure.take().is_some())
                .unwrap_or(false),
            _ => false,
        };
        if cleared {
            self.touch_locked(&mut inner);
        }
    }

    /// 记一次成功连接，返回更新后的 `reconnectsToday`（当日首连返回 0，不计数）。
    pub fn record_connected(&self, tunnel_id: &str, now_ms: i64) -> u32 {
        let mut inner = self.lock();
        let today = local_day_key(now_ms);
        if inner.state.day_key != today {
            reset_day_stats(&mut inner.state);
            inner.state.day_key = today;
        }
        let stats = &mut inner.state.tunnels.entry(tunnel_id.to_string()).or_default().stats;
        let count = if stats.connected_today {
            stats.reconnects_today.saturating_add(1)
        } else {
            0
        };
        stats.connected_today = true;
        stats.reconnects_today = count;
        stats.last_connected_at = Some(now_ms);
        self.touch_locked(&mut inner);
        count
    }

    /// 写入一次云端核对结果。
    pub fn set_protection_check(&self, tunnel_id: &str, mut check: ProtectionCheck) {
        if let Some(cloud) = check.cloud.as_mut() {
            cloud.allow_ips.truncate(ALLOW_IPS_MAX);
        }
        check.expected.allow_ips.truncate(ALLOW_IPS_MAX);
        if let Some(error) = check.error.as_mut() {
            *error = truncate_chars(error, FAILURE_REASON_MAX_CHARS);
        }
        let mut inner = self.lock();
        inner
            .state
            .tunnels
            .entry(tunnel_id.to_string())
            .or_default()
            .protection_check = Some(check);
        self.touch_locked(&mut inner);
    }

    /// 只刷新一条隧道核对结果的时刻（改内存，不标脏、不落盘）。
    ///
    /// 自动复检结果和上次一模一样时用：界面「上次核对」要显示最新的，稳定环境
    /// 才不会过 24 小时被误标成「待核对」；但没必要只为这个时间戳反复重写
    /// `state.json`，重启后旧 ts 自然会把这条隧道再查一次。
    pub fn touch_protection_check(&self, tunnel_id: &str, ts: i64) {
        let mut inner = self.lock();
        if let Some(check) = inner
            .state
            .tunnels
            .get_mut(tunnel_id)
            .and_then(|entry| entry.protection_check.as_mut())
        {
            check.ts = Some(ts);
        }
        // 故意不调 touch_locked：只改内存，不标脏。
    }

    /// 把一条隧道的核对结果清成「不知道」。
    pub fn clear_protection_check(&self, tunnel_id: &str) {
        let mut inner = self.lock();
        let cleared = inner
            .state
            .tunnels
            .get_mut(tunnel_id)
            .map(|entry| entry.protection_check.take().is_some())
            .unwrap_or(false);
        if cleared {
            self.touch_locked(&mut inner);
        }
    }

    /// 所有隧道的核对结果清成「不知道」（导入/服务器字段变更时用）。
    pub fn clear_all_protection(&self) {
        let mut inner = self.lock();
        let mut changed = false;
        for entry in inner.state.tunnels.values_mut() {
            if entry.protection_check.take().is_some() {
                changed = true;
            }
        }
        if changed {
            self.touch_locked(&mut inner);
        }
    }

    /// 最近一次更新检查结果。
    pub fn update_info(&self) -> UpdateInfo {
        self.lock().state.update_info.clone()
    }

    /// 写入更新检查结果。
    pub fn set_update_info(&self, info: UpdateInfo) {
        let mut inner = self.lock();
        inner.state.update_info = info;
        self.touch_locked(&mut inner);
    }

    /// 遍历读取（加锁期间回调，不要在里面做重活）。
    pub fn with_state<R>(&self, f: impl FnOnce(&RuntimeState) -> R) -> R {
        f(&self.lock().state)
    }

    /// 整份克隆一份（诊断/快照用）。
    pub fn state(&self) -> RuntimeState {
        self.lock().state.clone()
    }

    /// 读一条隧道的运行状态。
    pub fn tunnel_state(&self, tunnel_id: &str) -> Option<TunnelState> {
        self.lock().state.tunnels.get(tunnel_id).cloned()
    }

    /// 读一台本机服务的运行状态。
    pub fn service_state(&self, service_id: &str) -> Option<ServiceState> {
        self.lock().state.services.get(service_id).cloned()
    }

    /// 当前的自然日 key。
    pub fn day_key(&self) -> String {
        self.lock().state.day_key.clone()
    }

    // ---------- 云端环境识别缓存（D0 §2，由 C1 写入） ----------

    /// 存一台服务器的环境识别缓存（JSON 原样透传）。
    pub fn set_server_env(&self, server_id: &str, value: serde_json::Value) {
        let mut inner = self.lock();
        inner.state.servers.insert(server_id.to_string(), value);
        self.touch_locked(&mut inner);
    }

    /// 读一台服务器的环境识别缓存。
    pub fn server_env(&self, server_id: &str) -> Option<serde_json::Value> {
        self.lock().state.servers.get(server_id).cloned()
    }

    /// 删掉一台服务器的环境识别缓存。
    pub fn remove_server_env(&self, server_id: &str) {
        let mut inner = self.lock();
        if inner.state.servers.remove(server_id).is_some() {
            self.touch_locked(&mut inner);
        }
    }

    // ---------- 删除墓碑 ----------

    /// 记一条墓碑（超出 20 条丢最旧；先清掉过期的）。
    pub fn push_trash(&self, rec: TrashRecord) {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.state.trash.retain(|r| r.expires_at > now);
        inner.state.trash.push(rec);
        while inner.state.trash.len() > TRASH_MAX {
            inner.state.trash.remove(0);
        }
        self.touch_locked(&mut inner);
    }

    /// 取出墓碑（撤销用）：过期/不存在返回 None；取走后墓碑删除。
    pub fn take_trash(&self, token: &str) -> Option<TrashRecord> {
        let now = (self.now)();
        let mut inner = self.lock();
        inner.state.trash.retain(|r| r.expires_at > now);
        let index = inner.state.trash.iter().position(|r| r.token == token)?;
        let rec = inner.state.trash.remove(index);
        self.touch_locked(&mut inner);
        Some(rec)
    }

    /// 看一眼墓碑（不取走）；过期/不存在返回 None。
    pub fn peek_trash(&self, token: &str) -> Option<TrashRecord> {
        let now = (self.now)();
        self.lock()
            .state
            .trash
            .iter()
            .find(|r| r.token == token && r.expires_at > now)
            .cloned()
    }

    /// 列出还能撤销的墓碑（先清过期的）。
    pub fn list_trash(&self) -> Vec<TrashRecord> {
        let now = (self.now)();
        let mut inner = self.lock();
        let before = inner.state.trash.len();
        inner.state.trash.retain(|r| r.expires_at > now);
        let list = inner.state.trash.clone();
        if inner.state.trash.len() != before {
            self.touch_locked(&mut inner);
        }
        list
    }

    // ---------- 对象删除后的清理 ----------

    /// 隧道没了：删掉它的运行状态。
    pub fn forget_tunnel(&self, tunnel_id: &str) {
        let mut inner = self.lock();
        if inner.state.tunnels.remove(tunnel_id).is_some() {
            self.touch_locked(&mut inner);
        }
    }

    /// 服务没了：删掉它的运行状态。
    pub fn forget_service(&self, service_id: &str) {
        let mut inner = self.lock();
        if inner.state.services.remove(service_id).is_some() {
            self.touch_locked(&mut inner);
        }
    }

    /// 服务器没了：删掉环境识别缓存。
    pub fn forget_server(&self, server_id: &str) {
        self.remove_server_env(server_id);
    }

    /// 按配置现状清理：只留还存在 id 的状态（配置改名/删除后调一次）。
    pub fn prune(&self, tunnels: &[String], services: &[String], servers: &[String]) {
        let mut inner = self.lock();
        let before = (
            inner.state.tunnels.len(),
            inner.state.services.len(),
            inner.state.servers.len(),
        );
        inner
            .state
            .tunnels
            .retain(|id, _| tunnels.iter().any(|t| t == id));
        inner
            .state
            .services
            .retain(|id, _| services.iter().any(|s| s == id));
        inner
            .state
            .servers
            .retain(|id, _| servers.iter().any(|s| s == id));
        let after = (
            inner.state.tunnels.len(),
            inner.state.services.len(),
            inner.state.servers.len(),
        );
        if before != after {
            self.touch_locked(&mut inner);
        }
    }

    // ---------- 内部 ----------

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 锁中毒也继续用：state 是可有可无的运行数据，不能因此让看护崩掉
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn touch_locked(&self, inner: &mut Inner) {
        let now = (self.now)();
        inner.dirty = true;
        if inner.dirty_since.is_none() {
            inner.dirty_since = Some(now);
        }
        if now - inner.state.last_saved_at >= DEBOUNCE_MS {
            let _ = self.save_locked(inner, now);
        }
    }

    /// 原子落盘：`state.json.tmp` → rename。失败只记错误，不 panic、不丢脏标记。
    fn save_locked(&self, inner: &mut Inner, now: i64) -> io::Result<()> {
        inner.state.last_saved_at = now;
        let result = (|| -> io::Result<()> {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            let text = serde_json::to_string_pretty(&inner.state)?;
            let tmp = self.path.with_extension("json.tmp");
            fs::write(&tmp, text)?;
            fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        match &result {
            Ok(()) => {
                inner.dirty = false;
                inner.dirty_since = None;
                inner.last_error = None;
            }
            Err(e) => {
                inner.last_error = Some(format!("运行记录没写下去：{e}"));
            }
        }
        result
    }
}

/// 默认状态（带当天的 dayKey）。
fn default_state(now: i64) -> RuntimeState {
    RuntimeState {
        day_key: local_day_key(now),
        ..RuntimeState::default()
    }
}

/// 跨日：清空每条隧道的当日统计（`last_connected_at` 保留，界面还能显示"上次连上"）。
fn reset_day_stats(state: &mut RuntimeState) {
    for entry in state.tunnels.values_mut() {
        entry.stats.reconnects_today = 0;
        entry.stats.connected_today = false;
    }
}

/// 读 `state.json`：损坏/版本过高改名留底并用默认值。
fn read_state(
    path: &Path,
    reset: &mut Option<StateResetReason>,
    now: &dyn Fn() -> i64,
) -> RuntimeState {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return default_state(now()),
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => {
            quarantine(path, "json.bad");
            *reset = Some(StateResetReason::Corrupt);
            return default_state(now());
        }
    };
    let version = value
        .get("stateVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(STATE_VERSION as u64);
    if version != STATE_VERSION as u64 {
        quarantine(path, "json.unsupported");
        *reset = Some(StateResetReason::Unsupported);
        return default_state(now());
    }
    match serde_json::from_value::<RuntimeState>(value) {
        Ok(state) => state,
        Err(_) => {
            quarantine(path, "json.bad");
            *reset = Some(StateResetReason::Corrupt);
            default_state(now())
        }
    }
}

/// 把读不了的 `state.json` 改名留底（覆盖旧的）；改名失败也不阻塞。
fn quarantine(path: &Path, ext: &str) {
    let target = path.with_extension(ext);
    let _ = fs::rename(path, &target);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sx-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_clock(start: i64) -> (Arc<AtomicI64>, Box<dyn Fn() -> i64 + Send + Sync>) {
        let clock = Arc::new(AtomicI64::new(start));
        let handle = clock.clone();
        (clock, Box::new(move || handle.load(Ordering::SeqCst)))
    }

    fn sample_state(now: i64) -> RuntimeState {
        let mut state = RuntimeState {
            day_key: local_day_key(now),
            last_saved_at: now,
            ..RuntimeState::default()
        };
        state.update_info = UpdateInfo {
            status: "upToDate".into(),
            latest: Some("0.6.0".into()),
            url: Some("https://example.com/releases".into()),
            checked_at: Some(now),
            error: None,
        };
        let mut tunnel = TunnelState::default();
        tunnel.last_failure = Some(FailureRecord::new(now, ReasonCode::RemotePortBusy, "云端端口被占了"));
        tunnel.protection_check = Some(ProtectionCheck {
            status: "ok".into(),
            ts: Some(now),
            source: Some("manual".into()),
            cloud: Some(CloudProtection {
                conf_found: true,
                password_on: true,
                ip_on: false,
                allow_ips: vec!["1.2.3.4".into()],
                htpasswd_exists: true,
            }),
            expected: ExpectedProtection {
                password: true,
                ip: false,
                allow_ips: vec![],
                url: "album.example.com".into(),
            },
            drift_fields: vec![],
            error: None,
            extra: None,
        });
        tunnel.stats = TunnelStats {
            reconnects_today: 2,
            connected_today: true,
            last_connected_at: Some(now),
        };
        state.tunnels.insert("tun-1".into(), tunnel);
        state
            .services
            .insert("svc-1".into(), ServiceState::default());
        state.servers.insert(
            "srv-1".into(),
            serde_json::json!({"nginx": true, "panel": "xiaoPi"}),
        );
        state.trash.push(new_trash(
            "tunnel",
            serde_json::json!({"id": "tun-9", "name": "旧站", "enabled": true}),
            BTreeMap::new(),
            now,
        ));
        state
    }

    #[test]
    fn state_round_trips() {
        let now = 1_760_000_000_000i64;
        let state = sample_state(now);
        let text = serde_json::to_string(&state).unwrap();
        let back: RuntimeState = serde_json::from_str(&text).unwrap();
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::to_value(&back).unwrap()
        );

        // 缺字段不报错（所有结构体都有 default 兜底）
        let minimal: RuntimeState = serde_json::from_str("{}").unwrap();
        assert_eq!(minimal.state_version, STATE_VERSION);
        assert_eq!(minimal.update_info.status, "unknown");
        assert!(minimal.tunnels.is_empty());
    }

    #[test]
    fn missing_state_file_is_fresh() {
        let dir = temp_dir("fresh");
        let now = 1_760_000_000_000i64;
        let (_clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);
        assert_eq!(store.reset_reason(), None);
        assert_eq!(store.day_key(), local_day_key(now));
        assert!(store.update_info().status == "unknown");
        // 没脏不写盘
        store.flush();
        assert!(!dir.join("state.json").exists());
    }

    #[test]
    fn corrupt_state_is_quarantined() {
        let dir = temp_dir("corrupt");
        let path = dir.join("state.json");
        std::fs::write(&path, "{这不是 json").unwrap();
        let now = 1_760_000_000_000i64;
        let (_clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);
        assert_eq!(store.reset_reason(), Some(StateResetReason::Corrupt));
        assert_eq!(StateResetReason::Corrupt.code(), ReasonCode::StateReset);
        assert!(dir.join("state.json.bad").exists(), "损坏要改名留底");
        assert!(!path.exists());
        assert!(store.update_info().status == "unknown", "默认状态继续跑");
    }

    #[test]
    fn too_new_state_is_unsupported() {
        let dir = temp_dir("unsupported");
        let path = dir.join("state.json");
        std::fs::write(&path, r#"{"stateVersion":2,"dayKey":"2099-01-01"}"#).unwrap();
        let now = 1_760_000_000_000i64;
        let (_clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);
        assert_eq!(store.reset_reason(), Some(StateResetReason::Unsupported));
        assert!(dir.join("state.json.unsupported").exists());
        assert!(!path.exists());
        assert_eq!(store.day_key(), local_day_key(now));
    }

    #[test]
    fn failure_recorded_truncated_and_cleared() {
        let dir = temp_dir("failure");
        let now = 1_760_000_000_000i64;
        let (clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);
        let long = "原".repeat(300);
        store.record_failure("tunnel", "tun-1", ReasonCode::AuthDenied, &long, now);
        store.record_failure("service", "svc-1", ReasonCode::CrashLoop, "连续启动失败", now);

        let tun = store.tunnel_state("tun-1").unwrap();
        let failure = tun.last_failure.unwrap();
        assert_eq!(failure.code, "authDenied");
        assert_eq!(failure.reason.chars().count(), 200, "reason 截断到 200 字符");
        assert_eq!(store.service_state("svc-1").unwrap().last_failure.unwrap().code, "crashLoop");

        store.clear_failure("tunnel", "tun-1");
        assert!(store.tunnel_state("tun-1").unwrap().last_failure.is_none());
        // 清理不该把整条状态删掉
        assert!(store.tunnel_state("tun-1").is_some());

        // 落盘后重启还能读回来
        clock.fetch_add(5000, Ordering::SeqCst);
        store.flush();
        let (_c2, clock_fn2) = fake_clock(now + 5000);
        let again = StateStore::with_clock(&dir, clock_fn2);
        assert_eq!(
            again.service_state("svc-1").unwrap().last_failure.unwrap().code,
            "crashLoop"
        );
    }

    #[test]
    fn connected_counts_and_day_rollover() {
        let dir = temp_dir("connected");
        let now = 1_760_000_000_000i64;
        let (clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);

        assert_eq!(store.record_connected("tun-1", now), 0, "当天首连不计重连");
        assert_eq!(store.record_connected("tun-1", now + 1000), 1);
        assert_eq!(store.record_connected("tun-1", now + 2000), 2);
        let stats = store.tunnel_state("tun-1").unwrap().stats;
        assert!(stats.connected_today);
        assert_eq!(stats.reconnects_today, 2);
        assert_eq!(stats.last_connected_at, Some(now + 2000));

        // 跨日（26 小时，稳过任何时区的午夜）
        clock.fetch_add(26 * 60 * 60 * 1000, Ordering::SeqCst);
        let next = clock.load(Ordering::SeqCst);
        assert_eq!(store.record_connected("tun-1", next), 0, "跨日重新算首连");
        let stats = store.tunnel_state("tun-1").unwrap().stats;
        assert_eq!(stats.reconnects_today, 0);
        assert_eq!(stats.last_connected_at, Some(next));

        // 显式 day_rollover 不会重复重置
        assert!(!store.day_rollover(next));
        assert!(store.day_rollover(next + 26 * 60 * 60 * 1000));
    }

    #[test]
    fn protection_check_stored_and_capped() {
        let dir = temp_dir("protection");
        let now = 1_760_000_000_000i64;
        let (_clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);

        let ips: Vec<String> = (0..100).map(|i| format!("10.0.0.{i}")).collect();
        let check = ProtectionCheck {
            status: "drift".into(),
            ts: Some(now),
            source: Some("auto".into()),
            cloud: Some(CloudProtection {
                conf_found: true,
                password_on: false,
                ip_on: true,
                allow_ips: ips.clone(),
                htpasswd_exists: false,
            }),
            expected: ExpectedProtection {
                password: true,
                ip: true,
                allow_ips: ips,
                url: "a.example.com".into(),
            },
            drift_fields: vec!["password".into()],
            error: None,
            extra: Some(serde_json::json!({"https": true})),
        };
        store.set_protection_check("tun-1", check);
        let stored = store
            .tunnel_state("tun-1")
            .unwrap()
            .protection_check
            .unwrap();
        assert_eq!(stored.cloud.as_ref().unwrap().allow_ips.len(), ALLOW_IPS_MAX);
        assert_eq!(stored.expected.allow_ips.len(), ALLOW_IPS_MAX);
        assert_eq!(stored.drift_fields, vec!["password"]);
        assert_eq!(stored.extra.unwrap()["https"], true);

        store.clear_protection_check("tun-1");
        assert!(store
            .tunnel_state("tun-1")
            .unwrap()
            .protection_check
            .is_none());
    }

    #[test]
    fn touch_protection_check_refreshes_memory_only() {
        let dir = temp_dir("touch-protection");
        let start = 1_760_000_000_000i64;
        let (clock, clock_fn) = fake_clock(start);
        let store = StateStore::with_clock(&dir, clock_fn);
        let path = dir.join("state.json");

        let check = ProtectionCheck {
            status: "ok".into(),
            ts: Some(start),
            source: Some("auto".into()),
            ..ProtectionCheck::default()
        };
        store.set_protection_check("tun-1", check);
        let first = fs::read_to_string(&path).unwrap();
        assert!(first.contains(&start.to_string()), "首次写盘带旧时间戳");

        // 复检结果没变：只把内存里的 ts 往前挪，不标脏、不落盘
        clock.fetch_add(500, Ordering::SeqCst);
        let later = clock.load(Ordering::SeqCst);
        store.touch_protection_check("tun-1", later);
        assert_eq!(
            store
                .tunnel_state("tun-1")
                .unwrap()
                .protection_check
                .unwrap()
                .ts,
            Some(later),
            "内存里的时间戳要刷新"
        );
        store.tick();
        store.flush();
        assert_eq!(fs::read_to_string(&path).unwrap(), first, "只刷新内存，不该落盘");

        // 隧道不存在 / 没有核对记录：静默忽略，不 panic
        store.touch_protection_check("tun-9", later);
        assert!(store.tunnel_state("tun-9").is_none());
    }

    #[test]
    fn update_info_round_trips() {
        let dir = temp_dir("update");
        let now = 1_760_000_000_000i64;
        let (_clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);
        let info = UpdateInfo {
            status: "available".into(),
            latest: Some("0.6.1".into()),
            url: Some("https://github.com/iranolly/shangxian/releases".into()),
            checked_at: Some(now),
            error: None,
        };
        store.set_update_info(info.clone());
        let back = store.update_info();
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn trash_keeps_latest_20_and_expires() {
        let dir = temp_dir("trash");
        let start = 1_760_000_000_000i64;
        let (clock, clock_fn) = fake_clock(start);
        let store = StateStore::with_clock(&dir, clock_fn);

        for i in 0..25 {
            let rec = new_trash(
                "tunnel",
                serde_json::json!({"id": format!("tun-{i}")}),
                BTreeMap::new(),
                start + i,
            );
            // 保证每个 token 唯一（随机理论上会撞，只是防御）
            let mut rec = rec;
            rec.token = format!("und-{i:08x}");
            store.push_trash(rec);
        }
        let list = store.list_trash();
        assert_eq!(list.len(), TRASH_MAX, "最多留 20 条");
        assert_eq!(list.first().unwrap().token, "und-00000005", "丢的是最旧的");

        // 还没到期
        clock.fetch_add(TRASH_TTL_MS - 1000, Ordering::SeqCst);
        assert_eq!(store.list_trash().len(), TRASH_MAX);

        // 到期后全部清掉
        clock.fetch_add(2000, Ordering::SeqCst);
        assert!(store.list_trash().is_empty());

        // take 会取走；过期取不到（用当前时钟造一条没到期的）
        let now = clock.load(Ordering::SeqCst);
        let rec = new_trash("server", serde_json::json!({"id": "srv-1"}), BTreeMap::new(), now);
        let token = rec.token.clone();
        store.push_trash(rec);
        assert!(store.peek_trash(&token).is_some());
        assert!(store.take_trash(&token).is_some());
        assert!(store.take_trash(&token).is_none());
    }

    #[test]
    fn undo_restore_semantics() {
        let cfg = AppConfig {
            version: config::CONFIG_VERSION,
            servers: vec![ServerConfig {
                id: "srv-1".into(),
                name: "香港-1".into(),
                host: "a.example.com".into(),
                ssh_port: 22,
                user: "root".into(),
                key_path: None,
                vhost_dir: None,
            }],
            services: vec![LocalService {
                id: "svc-1".into(),
                name: "相册".into(),
                local_port: 8080,
                probe_kind: config::DEFAULT_PROBE_KIND.to_string(),
                launch: None,
                watch: true,
                autostart: false,
                note: None,
            }],
            tunnels: Vec::new(),
            notify_enabled: true,
            settings: config::Settings::default(),
        };

        let item = serde_json::json!({
            "id": "tun-9", "name": "旧站", "serviceId": "svc-1", "serverId": "srv-1",
            "remotePort": 18081, "url": null, "keyPath": null, "access": [],
            "accessUser": null, "accessHash": null, "accessDetail": null,
            "enabled": true, "preset": "web"
        });
        let mut deps = BTreeMap::new();
        deps.insert("serviceId".to_string(), "相册".to_string());
        deps.insert("serverId".to_string(), "香港-1".to_string());
        let rec = new_trash("tunnel", item.clone(), deps, 1000);

        // 恢复的隧道一律暂停（删除前是 enabled=true）
        let next = insert_restored(&cfg, &rec).unwrap();
        assert_eq!(next.tunnels.len(), 1);
        assert!(!next.tunnels[0].enabled, "撤销恢复的隧道必须保持暂停");

        // 同 id 冲突拒绝
        let conflict = insert_restored(&next, &rec).unwrap_err();
        assert!(conflict.contains("已经有一条"), "err={conflict}");

        // 依赖缺失拒绝，并带上快照里的名字
        let mut broken = cfg.clone();
        broken.services.clear();
        let err = insert_restored(&broken, &rec).unwrap_err();
        assert!(err.contains("相册"), "err={err}");
        assert!(err.contains("本机服务"), "err={err}");

        // 服务恢复只插数据，不动看护开关
        let service_item = serde_json::to_value(&cfg.services[0]).unwrap();
        let srec = new_trash("service", service_item, BTreeMap::new(), 1000);
        let mut without = cfg.clone();
        without.services.clear();
        let restored = insert_restored(&without, &srec).unwrap();
        assert_eq!(restored.services.len(), 1);
        assert!(restored.services[0].watch);

        // 不认识的类型
        let bad = TrashRecord {
            kind: "banana".into(),
            ..rec.clone()
        };
        assert!(insert_restored(&cfg, &bad).unwrap_err().contains("不认识的记录类型"));
    }

    #[test]
    fn mark_dirty_is_debounced_but_tick_forces_write() {
        let dir = temp_dir("debounce");
        let start = 1_760_000_000_000i64;
        let (clock, clock_fn) = fake_clock(start);
        let store = StateStore::with_clock(&dir, clock_fn);
        let path = dir.join("state.json");

        // 第一次 mark_dirty：距上次落盘很久 → 立即写
        store.record_failure("tunnel", "t1", ReasonCode::NetTimeout, "超时", start);
        assert!(path.exists(), "距上次落盘 ≥1 秒应该立即写");
        let first = fs::read_to_string(&path).unwrap();

        // 500ms 后又改：去抖窗口内，不写
        clock.fetch_add(500, Ordering::SeqCst);
        let now = clock.load(Ordering::SeqCst);
        store.record_failure("tunnel", "t1", ReasonCode::NetTimeout, "又超时", now);
        assert_eq!(fs::read_to_string(&path).unwrap(), first, "去抖窗口内不能写");

        // tick 也不写（还在 1 秒内）
        store.tick();
        assert_eq!(fs::read_to_string(&path).unwrap(), first);

        // 过了 1 秒：tick 落盘
        clock.fetch_add(600, Ordering::SeqCst);
        store.tick();
        let second = fs::read_to_string(&path).unwrap();
        assert_ne!(second, first, "过了去抖窗口应该落盘");
        assert!(second.contains("又超时"));

        // 干净的 store，flush 不产生新写入
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        store.flush();
        let after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn server_env_cache_and_prune() {
        let dir = temp_dir("server-env");
        let now = 1_760_000_000_000i64;
        let (_clock, clock_fn) = fake_clock(now);
        let store = StateStore::with_clock(&dir, clock_fn);

        store.set_server_env("srv-1", serde_json::json!({"panel": "baoTa"}));
        store.set_server_env("srv-2", serde_json::json!({"panel": "xiaoPi"}));
        assert_eq!(store.server_env("srv-1").unwrap()["panel"], "baoTa");

        store.forget_server("srv-1");
        assert!(store.server_env("srv-1").is_none());

        store.record_failure("tunnel", "gone", ReasonCode::ExitUnknown, "x", now);
        store.prune(&[], &[], &["srv-2".to_string()]);
        assert!(store.tunnel_state("gone").is_none());
        assert!(store.server_env("srv-2").is_some());
        assert_eq!(store.state().servers.len(), 1);
    }

    #[test]
    fn day_key_helper_is_correct() {
        // 1970-01-01T00:00:00Z
        assert_eq!(day_key_from_ms(0, 0), "1970-01-01");
        // 2025-10-09T08:53:20Z
        assert_eq!(day_key_from_ms(1_760_000_000_000, 0), "2025-10-09");
        // 跨午夜：2025-10-08T00:00Z 还是 10-08；东八区当天 +16h 已是 10-09
        let midnight_utc = 1_759_881_600_000; // 2025-10-08T00:00:00Z
        assert_eq!(day_key_from_ms(midnight_utc, 0), "2025-10-08");
        assert_eq!(day_key_from_ms(midnight_utc + 16 * 3_600_000, 480), "2025-10-09");
        // 西八区：UTC 10-09 凌晨其实是当地 10-08
        assert_eq!(day_key_from_ms(1_759_971_200_000, -480), "2025-10-08");
    }
}
