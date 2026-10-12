//! 守护：按配置管理全部隧道和本机服务（启停、重试、状态汇总、导入与配置编辑）。
//!
//! 纪律（对齐 DESIGN.md §1.3 设计红线）：
//! - 每条隧道一条独立进程、独立日志；每台本机服务独立进程、独立日志；
//! - 没有全局清理路径：暂停一条隧道只停它自己；
//! - 端口写死，冲突不自动漂移；
//! - 删本机服务前先看有没有隧道在用它（有就拦住报错，不自动连带删）。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use crate::cleanup::{CleanupSpec, SshExec};
use crate::config::{self, AppConfig, LaunchSpec, LocalService, QuietHours, ServerConfig, Settings, Tunnel};
use crate::events::{EventLine, EventLog, EventSubject, LEVEL_INFO, LEVEL_WARN};
use crate::import::ImportSuggestion;
use crate::localservice::{LocalRunner, LocalSpec};
use crate::log::{LogLevel, Reporter, TAG_GUARD, TAG_SERVICE, TAG_TUNNEL};
use crate::notify::{HealthItem, HealthKind};
use crate::protection::AccessCheck;
use crate::reason::ReasonCode;
use crate::ssh::{self, TunnelTarget};
use crate::state::{FailureRecord, ProtectionCheck, StateStore};
use crate::supervisor::{
    service_log_path, Event, Phase, RetryState, Tunnel as TunnelProc, TunnelSpec,
};
use crate::wake::{WakeKind, WakeMonitor};

// ---------- 给界面看的视图 ----------

/// 开机自启的宽限期：第一轮先认领「端口已经在监听」的服务，然后等这一小会儿，
/// 给上个会话留下的进程一点绑端口的时间；到期后才真正拉起没认到的自启服务。
const AUTOSTART_GRACE: Duration = Duration::from_secs(5);

/// 删除成功后的返回（供 ctl 拼 `undoToken` / `undoExpiresAt`）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub id: String,
    pub name: String,
    /// 撤销令牌；没有注入 state（单测）时为空串 = 不可撤销。
    pub token: String,
    /// 撤销过期时刻（epoch ms）；不可撤销时为 0。
    pub expires_at: i64,
}

/// 撤销成功后的返回（D2 §10.2 `restored` 形状）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
}

/// 只停运行、不改配置的「停一切」结果（卸载 / 覆盖升级前用）。
/// JSON 键与 ctl 现有 `stop_all` 的返回一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopAllReport {
    pub tunnels_stopped: usize,
    pub tunnels_failed: usize,
    pub services_stopped: usize,
    pub services_failed: usize,
}

/// 一台本机服务（界面用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    pub id: String,
    pub name: String,
    pub local_port: u16,
    /// stopped | starting | running | not_listening | failed
    pub status: String,
    /// up（端口在听）| down（探测过，不通/已停/失败）| unknown（还没探过）
    pub health: String,
    pub watch: bool,
    pub autostart: bool,
    pub launch_exe: Option<String>,
    pub launch_args: Option<String>,
    pub launch_workdir: Option<String>,
    pub note: Option<String>,
    /// 已经发布到几条隧道
    pub tunnel_count: usize,
    /// 这些隧道的名字（界面上展开看）
    pub tunnel_names: Vec<String>,
    /// 出问题的原因（人话）
    pub reason: Option<String>,
    /// 运行中日志是否已超上限（界面提示手动轮转）
    pub log_overflowed: bool,
    /// 最近一次失败（重启不失忆；health 变 up 时清空）
    pub last_failure: Option<FailureRecord>,
    pub log_path: String,
}

/// 一条发布隧道（界面用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelView {
    pub id: String,
    pub name: String,
    pub service_id: String,
    pub service_name: String,
    pub local_port: u16,
    pub server_id: String,
    pub remote_port: u16,
    pub url: Option<String>,
    /// running | connecting | disconnected | error | paused
    pub status: String,
    pub enabled: bool,
    /// 出错原因（人话），等于 `lastFailure.reason` 的镜像
    pub reason: Option<String>,
    /// 与 `reason` 对应的原因码；`reason` 为 null 时它为 null
    pub reason_code: Option<String>,
    /// `"active"` | `"stopped"`
    pub retry_state: String,
    pub attempt: u32,
    pub retry_in_secs: Option<u64>,
    /// 当前这次连接建立时刻（epoch ms）；未连接为 null
    pub connected_since: Option<u64>,
    /// 当前连接已持续秒数；未连接为 0
    pub uptime_secs: u64,
    /// 今日重连次数（本应用运行期内、按本机自然日）
    pub reconnects_today: u32,
    /// 最近一次失败（重启不失忆；连上后清空）
    pub last_failure: Option<FailureRecord>,
    /// 云端保护核对结果（永远存在；未核对 = status:"unknown"）
    pub protection_check: ProtectionCheck,
    /// 访问保护：["password"]、["ip"] 或两者叠加；空 = 谁都能进
    pub access: Vec<String>,
    pub access_user: Option<String>,
    /// 是否已经设过访问密码（密码本身不落盘）
    pub has_password: bool,
    pub access_detail: Option<String>,
    /// 本机服务在不在跑（界面要标出来）
    pub service_ok: bool,
    pub service_status: String,
    /// 单独指定的密钥（可空 = 继承服务器）
    pub key_path: Option<String>,
    /// nginx 片段预设：web | ws | upload
    pub preset: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerView {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub ssh_port: u16,
    pub key_path: Option<String>,
    /// 这台服务器上面板放站点的目录（可空 = 按环境变量/小皮默认找）
    pub vhost_dir: Option<String>,
}

// ---------- 界面提交的输入 ----------

/// 新增/编辑本机服务。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInput {
    pub id: Option<String>,
    pub name: String,
    pub local_port: u16,
    pub launch_exe: Option<String>,
    pub launch_args: Option<String>,
    pub launch_workdir: Option<String>,
    pub watch: bool,
    pub autostart: bool,
    pub note: Option<String>,
}

/// 新增隧道（向导提交）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTunnelInput {
    pub name: String,
    pub service_id: String,
    pub server_id: String,
    pub remote_port: u16,
    pub url: Option<String>,
    /// 访问保护：可以同时要密码 + 只允许指定 IP；缺失 = 谁都能进
    #[serde(default, deserialize_with = "config::deserialize_access")]
    pub access: Vec<String>,
    pub access_user: Option<String>,
    pub access_hash: Option<String>,
    pub access_detail: Option<String>,
    /// nginx 片段预设：web（缺省）| ws | upload
    pub preset: Option<String>,
}

/// 编辑已有隧道。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTunnelInput {
    pub id: String,
    pub name: String,
    pub remote_port: u16,
    pub url: Option<String>,
    /// 访问保护：缺失或 null = 保持原样；给了就替换（空数组或 ["none"] = 取消保护）
    #[serde(default, deserialize_with = "config::deserialize_access_opt")]
    pub access: Option<Vec<String>>,
    pub access_user: Option<String>,
    /// 新密码的哈希；None = 保持原密码
    pub access_hash: Option<String>,
    pub access_detail: Option<String>,
    /// nginx 片段预设：web | ws | upload；缺省或 null = 保持原样
    pub preset: Option<String>,
}

/// 新增/编辑服务器。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInput {
    pub id: Option<String>,
    pub name: String,
    pub host: String,
    pub ssh_port: u16,
    pub user: String,
    pub key_path: Option<String>,
    /// 面板放站点的目录（可空 = 小皮默认 /xp/panel/vhost）；空串按『没填』处理
    pub vhost_dir: Option<String>,
}

/// 停止一台本机服务的结果。
///
/// JSON 用 `kind` 区分（serde 内部标签，camelCase）：
/// `{"kind":"stopped","pid":123}` /
/// `{"kind":"needsConfirm","pid":123,"exe":"D:\\x\\a.exe"}` /
/// `{"kind":"foreignStopped","pid":123}`
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StopOutcome {
    /// 我们自己启动的进程已经停掉（`pid` 是它的进程号）；端口上本来就没有进程时 `pid` 为 None。
    Stopped { pid: Option<u32> },
    /// 端口被外部进程占着：需要用户确认后才结束。
    NeedsConfirm { pid: u32, exe: Option<String> },
    /// 用户确认后，外部进程已经结束。
    ForeignStopped { pid: u32 },
}

pub struct Guardian {
    config: AppConfig,
    config_path: PathBuf,
    log_dir: PathBuf,
    /// 运行中的隧道（按隧道 id）
    tunnels: HashMap<String, TunnelProc>,
    /// 运行中隧道「实际用的参数」指纹（按隧道 id）；和配置对不上就停掉按新参数重建
    tunnel_fingerprints: HashMap<String, String>,
    /// 隧道最近一次出错原因
    reasons: HashMap<String, String>,
    /// 本机服务的运行时（按服务 id）
    local: HashMap<String, LocalRunner>,
    /// 开机自启处理完了
    autostart_done: bool,
    /// 第一次认领的结果（宽限到期时用它决定哪些不用再拉）
    autostart_claimed: HashSet<String>,
    /// 开机自启的宽限截止时间；None = 还没做第一次认领
    autostart_deadline: Option<Instant>,
    /// 配置能不能安全写回（读坏留底后是 false，此时禁止自动保存）
    config_trusted: bool,
    /// app.log 接收器（本地看护和隧道都往里写）
    reporter: Arc<dyn Reporter>,
    /// 运行状态（未注入时降级：不持久化、不 panic）
    state: Option<Arc<StateStore>>,
    /// 事件日志（未注入时降级：不写事件、不 panic）
    events: Option<Arc<EventLog>>,
    /// 睡眠唤醒 / 网络 / 时钟检测
    wake_monitor: WakeMonitor,
    /// 上次处理各类系统事件的时刻（10 秒内同类只触发一次）
    wake_last: [Option<Instant>; 3],
    /// 配置版本号：commit_config 成功后 +1
    config_generation: u64,
    /// 已经同步过的配置版本号
    synced_generation: u64,
    /// 每条隧道已经记过连接事件的次数（reconnects_total 快照）
    reconnects_seen: HashMap<String, u32>,
    /// 服务的上轮健康状态（写 serviceUp / serviceDown 事件用）
    service_health: HashMap<String, bool>,
    /// 已经同步进 state 的服务失败（去重，避免每 tick 重复写盘）
    service_failures: HashMap<String, (ReasonCode, String)>,
    /// 已经记过事件的 protectionCheck（`status|ts|source`）
    protection_seen: HashMap<String, String>,
}

impl Guardian {
    pub fn new(
        config: AppConfig,
        config_path: PathBuf,
        log_dir: PathBuf,
        reporter: Arc<dyn Reporter>,
    ) -> Self {
        let _ = std::fs::create_dir_all(&log_dir);
        // 磁盘上现在读不出配置（读坏留过底）时禁止自动保存：不能拿内存里的空配置
        // 把用户的配置冲掉。见 config::config_save_allowed。
        let config_trusted = config::config_save_allowed(&config_path);
        Self {
            config,
            config_path,
            log_dir,
            tunnels: HashMap::new(),
            tunnel_fingerprints: HashMap::new(),
            reasons: HashMap::new(),
            local: HashMap::new(),
            autostart_done: false,
            autostart_claimed: HashSet::new(),
            autostart_deadline: None,
            config_trusted,
            reporter,
            state: None,
            events: None,
            wake_monitor: WakeMonitor::new(),
            wake_last: [None; 3],
            config_generation: 1,
            synced_generation: 0,
            reconnects_seen: HashMap::new(),
            service_health: HashMap::new(),
            service_failures: HashMap::new(),
            protection_seen: HashMap::new(),
        }
    }

    /// 注入运行状态（`state.json`）。要在第一次 `poll` 之前调用；
    /// 未注入时降级：不持久化，但不得 panic（方便单测）。
    pub fn attach_state(&mut self, state: Arc<StateStore>) {
        self.state = Some(state);
    }

    /// 注入事件日志（`events.jsonl`）。要在第一次 `poll` 之前调用；
    /// 未注入时降级：不写事件，但不得 panic（方便单测）。
    pub fn attach_events(&mut self, events: Arc<EventLog>) {
        self.events = Some(events);
    }

    /// 配置版本号：每次 `commit_config` 成功后 +1（看护 tick 用它跳过全量同步）。
    pub fn config_generation(&self) -> u64 {
        self.config_generation
    }

    // ---------- 看护 ----------

    /// 周期调用：先处理系统事件，再让隧道表和配置对齐，再推进每条隧道和每台本机服务。
    pub fn poll(&mut self) -> Vec<(String, Event)> {
        let mut events = Vec::new();

        // 0) 系统事件：电脑刚醒 / 网络恢复 / 时钟跳变 → 立刻动作并冒泡给界面
        if let Some(kind) = self.wake_monitor.poll() {
            let affected = self.kick_reconnect(kind);
            events.push((String::new(), Event::SystemWake { kind, affected }));
        }

        if !self.autostart_done {
            match self.autostart_deadline {
                None => {
                    // 第一轮先把「端口已经在监听」的服务认领掉（可能是上个会话留下的）；
                    // 还没在听的由 localservice 开认领窗口接着探。
                    self.autostart_claimed = self.claim_running_services();
                    self.autostart_deadline = Some(Instant::now() + AUTOSTART_GRACE);
                }
                Some(at) if Instant::now() >= at => {
                    self.autostart_done = true;
                    // 宽限期结束才真正拉：给上个会话留下的进程一点绑端口的时间；
                    // 已经认到的绝不重复启动。
                    let claimed = std::mem::take(&mut self.autostart_claimed);
                    self.start_autostart_services(&claimed);
                }
                Some(_) => {}
            }
        }

        // 配置没变就跳过全量同步（D1 §5.4：看护 tick 只比一次版本号）
        if self.config_generation != self.synced_generation {
            self.sync_tunnels();
            self.sync_local();
            self.synced_generation = self.config_generation;
        }

        let ids: Vec<String> = self.tunnels.keys().cloned().collect();
        for id in ids {
            let tunnel_events = self
                .tunnels
                .get_mut(&id)
                .map(|tunnel| tunnel.poll())
                .unwrap_or_default();
            for event in &tunnel_events {
                match event {
                    Event::SpawnFailed { code, .. } => {
                        let reason = failure_reason(*code, None);
                        self.note_tunnel_failure(&id, *code, reason.clone());
                        self.append_tunnel_event(&id, *code, &reason, LEVEL_WARN);
                    }
                    Event::Exited { exit_code, code, .. } => {
                        let reason = failure_reason(*code, *exit_code);
                        self.note_tunnel_failure(&id, *code, reason.clone());
                        self.append_tunnel_event(&id, *code, &reason, LEVEL_WARN);
                    }
                    Event::CleanupFinished { ok, message } => {
                        if !ok {
                            let reason = ReasonCode::CleanupFail.text().to_string();
                            self.note_tunnel_failure(&id, ReasonCode::CleanupFail, reason.clone());
                            self.append_tunnel_event(&id, ReasonCode::CleanupFail, &reason, LEVEL_WARN);
                            self.reporter.report(
                                LogLevel::Warn,
                                TAG_TUNNEL,
                                &format!("隧道 {id} 的远端清理没成功：{message}"),
                            );
                        }
                    }
                    Event::Started { .. } | Event::SystemWake { .. } => {}
                }
                events.push((id.clone(), event.clone()));
            }

            // 真的连上了（reconnects_total 涨了）→ 清失败、记状态、写一条事件
            let connected = self.tunnels.get(&id).map(|tunnel| {
                (
                    tunnel.connected() && matches!(tunnel.phase(), Phase::Running { .. }),
                    tunnel.reconnects_total(),
                )
            });
            if let Some((true, total)) = connected {
                let seen = self.reconnects_seen.get(&id).copied().unwrap_or(0);
                if total > seen {
                    self.reconnects_seen.insert(id.clone(), total);
                    self.reasons.remove(&id);
                    if let Some(state) = &self.state {
                        state.record_connected(&id, crate::state::now_ms());
                        state.clear_failure("tunnel", &id);
                    }
                    self.append_tunnel_event(
                        &id,
                        ReasonCode::TunnelConnected,
                        ReasonCode::TunnelConnected.text(),
                        LEVEL_INFO,
                    );
                }
            }
        }
        for runner in self.local.values_mut() {
            runner.poll();
        }
        self.sync_service_state();
        self.sync_protection_events();
        events
    }

    /// 记一次隧道失败：更新界面原因 + 持久化进 state。
    /// 每次事件（退出/启动失败）都要更新 `lastFailure.ts`；只有配置引用缺失这种
    /// 每轮都会重算的情况由调用方先去重。
    fn note_tunnel_failure(&mut self, id: &str, code: ReasonCode, reason: String) {
        self.reasons.insert(id.to_string(), reason.clone());
        if let Some(state) = &self.state {
            state.record_failure("tunnel", id, code, &reason, crate::state::now_ms());
        }
    }

    /// 写一条隧道事件（没注入事件日志时静默跳过）。
    fn append_tunnel_event(&self, id: &str, code: ReasonCode, message: &str, level: &str) {
        let Some(events) = &self.events else {
            return;
        };
        let name = self
            .config
            .tunnel(id)
            .map(|tun| tun.name.clone())
            .unwrap_or_default();
        events.append(EventLine::new(
            "tunnel",
            level,
            Some(code),
            Some(EventSubject::new("tunnel", id, &name)),
            message,
        ));
    }

    /// 写一条本机服务事件（没注入事件日志时静默跳过）。
    fn append_service_event(&self, id: &str, name: &str, code: ReasonCode, message: &str, level: &str) {
        let Some(events) = &self.events else {
            return;
        };
        events.append(EventLine::new(
            "service",
            level,
            Some(code),
            Some(EventSubject::new("service", id, name)),
            message,
        ));
    }

    /// 写一条云端核对事件（没注入事件日志时静默跳过）。
    fn append_protection_event(
        &self,
        id: &str,
        name: &str,
        code: ReasonCode,
        message: &str,
        level: &str,
        data: Option<serde_json::Value>,
    ) {
        let Some(events) = &self.events else {
            return;
        };
        let mut line = EventLine::new(
            "protection",
            level,
            Some(code),
            Some(EventSubject::new("tunnel", id, name)),
            message,
        );
        if let Some(data) = data {
            line = line.with_data(data);
        }
        events.append(line);
    }

    /// 写一条带主体的普通事件（没注入事件日志时静默跳过）。
    fn append_event(
        &self,
        kind: &str,
        level: &str,
        code: ReasonCode,
        subject: EventSubject,
        message: &str,
    ) {
        let Some(events) = &self.events else {
            return;
        };
        events.append(EventLine::new(kind, level, Some(code), Some(subject), message));
    }

    /// 这台服务器缓存在 state 里的环境识别结果（没缓存 / 解析不了 = None）。
    fn cached_cloud_env(&self, server_id: &str) -> Option<crate::cloudcheck::CloudEnv> {
        let state = self.state.as_ref()?;
        let value = state.server_env(server_id)?;
        serde_json::from_value(value).ok()
    }

    /// 环境识别到的站点目录（解析链第二层；空串按没填算）。
    fn detected_vhost_dir(&self, server_id: &str) -> Option<String> {
        self.cached_cloud_env(server_id)
            .and_then(|env| env.vhost_dir)
            .map(|dir| dir.trim().to_string())
            .filter(|dir| !dir.is_empty())
    }

    /// 上次云端检查记下的「站点配置里有没有 /.well-known 块」；
    /// 没检查过 / 没记 = None（生成片段时维持老行为）。
    fn wellknown_present(&self, tunnel_id: &str) -> Option<bool> {
        let state = self.state.as_ref()?;
        let check = state.tunnel_state(tunnel_id)?.protection_check?;
        wellknown_present_from_extra(check.extra.as_ref())
    }

    /// 上次云端检查结果里带的云端环境（`protectionCheck.extra.env`）。
    /// 只有 Env 模式会写 `state.servers` 缓存；其他模式靠这里把 WS_MAP 兜回来。
    fn last_check_cloud_env(&self, tunnel_id: &str) -> Option<crate::cloudcheck::CloudEnv> {
        let state = self.state.as_ref()?;
        let check = state.tunnel_state(tunnel_id)?.protection_check?;
        let env = check.extra?.get("env")?.clone();
        serde_json::from_value(env).ok()
    }

    /// 把本机服务的失败记录同步进 state，并在健康状态迁移时写 serviceUp / serviceDown。
    fn sync_service_state(&mut self) {
        let ids: Vec<String> = self.local.keys().cloned().collect();
        for id in ids {
            let (healthy, ever, failure, user_stopped, name) = {
                let runner = &self.local[&id];
                (
                    runner.probe().alive(),
                    runner.probe().ever_checked(),
                    runner.last_failure().cloned(),
                    runner.is_user_stopped(),
                    self.config
                        .service(&id)
                        .map(|svc| svc.name.clone())
                        .unwrap_or_default(),
                )
            };

            // 失败记录同步进 state：按 (code, reason) 去重，避免每 tick 重复写盘。
            match &failure {
                Some(record) => {
                    let key = (record.code, record.reason.clone());
                    if self.service_failures.get(&id) != Some(&key) {
                        self.service_failures.insert(id.clone(), key);
                        if let Some(state) = &self.state {
                            state.record_failure(
                                "service",
                                &id,
                                record.code,
                                &record.reason,
                                record.ts_ms as i64,
                            );
                        }
                    }
                }
                None => {
                    if self.service_failures.remove(&id).is_some() {
                        if let Some(state) = &self.state {
                            state.clear_failure("service", &id);
                        }
                    }
                }
            }

            // 健康状态迁移 → 事件（未注入事件日志时静默）
            let prev = self.service_health.insert(id.clone(), healthy);
            if prev == Some(healthy) {
                continue;
            }
            if healthy {
                let first_up = prev.is_none() && !user_stopped && ever;
                if prev == Some(false) || first_up {
                    self.append_service_event(
                        &id,
                        &name,
                        ReasonCode::ServiceUp,
                        ReasonCode::ServiceUp.text(),
                        LEVEL_INFO,
                    );
                }
            } else if let Some(record) = &failure {
                if (prev == Some(true) || prev.is_none()) && !user_stopped {
                    self.append_service_event(&id, &name, record.code, &record.reason, LEVEL_WARN);
                }
            }
        }
    }

    /// 观察 state 里的 protectionCheck：谁写的都行（手动 / 自动），变化就写一条事件。
    fn sync_protection_events(&mut self) {
        let Some(state) = self.state.clone() else {
            return;
        };
        let ids: Vec<String> = self.config.tunnels.iter().map(|tun| tun.id.clone()).collect();
        for id in ids {
            let Some(tunnel_state) = state.tunnel_state(&id) else {
                continue;
            };
            let Some(check) = tunnel_state.protection_check else {
                // 配置变更清成 unknown（记录被删）：下次出现重新观察
                self.protection_seen.remove(&id);
                continue;
            };
            let key = format!(
                "{}|{}|{}",
                check.status,
                check.ts.unwrap_or(0),
                check.source.as_deref().unwrap_or("")
            );
            if self.protection_seen.get(&id) == Some(&key) {
                continue;
            }
            self.protection_seen.insert(id.clone(), key);
            let name = self
                .config
                .tunnel(&id)
                .map(|tun| tun.name.clone())
                .unwrap_or_default();
            match check.status.as_str() {
                "ok" => self.append_protection_event(
                    &id,
                    &name,
                    ReasonCode::ProtectionChecked,
                    ReasonCode::ProtectionChecked.text(),
                    LEVEL_INFO,
                    None,
                ),
                "drift" => {
                    let diff = drift_fields_text(&check.drift_fields);
                    let message = ReasonCode::ProtectionDrift.render(&[("差异", &diff)]);
                    self.append_protection_event(
                        &id,
                        &name,
                        ReasonCode::ProtectionDrift,
                        &message,
                        LEVEL_WARN,
                        Some(serde_json::json!({ "driftFields": check.drift_fields })),
                    );
                }
                "error" => {
                    let reason = check
                        .error
                        .clone()
                        .unwrap_or_else(|| "（没写原因）".to_string());
                    let message = ReasonCode::ProtectionError.render(&[("原因", &reason)]);
                    self.append_protection_event(
                        &id,
                        &name,
                        ReasonCode::ProtectionError,
                        &message,
                        LEVEL_WARN,
                        None,
                    );
                }
                _ => {}
            }
        }
    }

    /// 系统事件后的动作（D1 §1.1 分档；10 秒内同类只触发一次）。返回受影响的隧道条数。
    pub fn kick_reconnect(&mut self, kind: WakeKind) -> usize {
        const RATE_WINDOW: Duration = Duration::from_secs(10);
        let slot = match kind {
            WakeKind::Resumed { .. } => 0,
            WakeKind::NetworkChanged => 1,
            WakeKind::ClockJump { .. } => 2,
        };
        let now = Instant::now();
        if let Some(last) = self.wake_last[slot] {
            if now.duration_since(last) < RATE_WINDOW {
                return 0; // 同类系统事件 10 秒内的重复通知直接清掉
            }
        }
        self.wake_last[slot] = Some(now);
        self.reporter.report(
            LogLevel::Info,
            TAG_GUARD,
            &format!("收到系统事件：{kind:?}，开始处理"),
        );

        match kind {
            // 睡过一觉：所有启用的隧道停掉重建（paused 的本来就不在 tunnels 里）
            WakeKind::Resumed { .. } => {
                let ids: Vec<String> = self.tunnels.keys().cloned().collect();
                let mut affected = 0;
                for id in ids {
                    if let Some(mut tunnel) = self.tunnels.remove(&id) {
                        tunnel.stop();
                    }
                    self.tunnel_fingerprints.remove(&id);
                    self.reconnects_seen.remove(&id);
                    self.reasons.remove(&id);
                    affected += 1;
                }
                self.sync_tunnels();
                affected
            }
            // 网络变化 / 时钟跳变：只把正在退避等待的隧道提前到「现在」
            WakeKind::NetworkChanged | WakeKind::ClockJump { .. } => {
                let mut affected = 0;
                for tunnel in self.tunnels.values_mut() {
                    if tunnel.kick_retry() {
                        affected += 1;
                    }
                }
                affected
            }
        }
    }

    /// 开机自启：把勾了自启的服务拉起来（应用启动时做一次）。
    ///
    /// `claimed` 是本轮已经认领（端口在监听）的服务，跳过不重复启动。
    fn start_autostart_services(&mut self, claimed: &HashSet<String>) {
        let ids: Vec<String> = self
            .config
            .services
            .iter()
            .filter(|s| s.autostart && s.launch.is_some() && !claimed.contains(&s.id))
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.sync_local(); // 保证 runner 存在
            if let Some(runner) = self.local.get_mut(&id) {
                runner.start_for_autostart();
                runner.poll();
            }
        }
    }

    /// 应用每次启动后的「认领」：已配启动方式的服务，如果端口已经在监听、本会话又没
    /// 有自己的子进程，就认成「在跑」（phase=Running、user_stopped=false）——这样勾了
    /// 看护的服务以后掉了会被 watch 拉起，且绝不重复启动。
    ///
    /// 返回本轮认领的服务 id。
    fn claim_running_services(&mut self) -> HashSet<String> {
        self.sync_local();
        let mut claimed = HashSet::new();
        for svc in &self.config.services {
            if svc.launch.is_none() {
                continue; // 只监控的服务平时 poll 自己会探测端口
            }
            if let Some(runner) = self.local.get_mut(&svc.id) {
                if runner.claim_if_listening() {
                    claimed.insert(svc.id.clone());
                }
            }
        }
        claimed
    }

    /// 让运行中的隧道与配置保持一致：停掉被暂停/删除的，补上新增的；
    /// 已经在跑但参数（服务器地址/端口/账号/密钥、本机端口、云端端口）变了的，
    /// 停掉旧的、按新参数重建（否则改了端口/服务器，跑着的还是老一套）。
    fn sync_tunnels(&mut self) {
        // 引用了不存在的服务/服务器的隧道：记一句人话原因（不反复写盘）
        let missing: Vec<(String, ReasonCode, String)> = self
            .config
            .tunnels
            .iter()
            .filter(|tun| tun.enabled)
            .filter_map(|tun| {
                if self.config.service(&tun.service_id).is_none() {
                    Some((
                        tun.id.clone(),
                        ReasonCode::ServiceMissing,
                        ReasonCode::ServiceMissing.text().to_string(),
                    ))
                } else if !self.config.servers.iter().any(|s| s.id == tun.server_id) {
                    Some((
                        tun.id.clone(),
                        ReasonCode::ServerMissing,
                        ReasonCode::ServerMissing.text().to_string(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        for (id, code, reason) in missing {
            // 引用缺失每轮都会重算：同一句原因只写一次盘（事件也不需要重复刷）
            if self.reasons.get(&id).map(|old| old != &reason).unwrap_or(true) {
                self.note_tunnel_failure(&id, code, reason);
            }
        }

        let enabled: HashSet<String> = self
            .config
            .tunnels
            .iter()
            .filter(|t| t.enabled && self.config.service(&t.service_id).is_some())
            .filter(|t| self.config.servers.iter().any(|s| s.id == t.server_id))
            .map(|t| t.id.clone())
            .collect();

        let to_remove: Vec<String> = self
            .tunnels
            .keys()
            .filter(|id| !enabled.contains(*id))
            .cloned()
            .collect();
        for id in to_remove {
            if let Some(mut tunnel) = self.tunnels.remove(&id) {
                tunnel.stop();
            }
            self.tunnel_fingerprints.remove(&id);
            self.reconnects_seen.remove(&id);
        }

        let tunnels: Vec<Tunnel> = self
            .config
            .tunnels
            .iter()
            .filter(|t| enabled.contains(&t.id))
            .cloned()
            .collect();
        for tun in tunnels {
            let Some(service) = self.config.service(&tun.service_id).cloned() else {
                continue;
            };
            let fingerprint = tunnel_fingerprint(&self.config, &tun, &service);
            let running = self.tunnels.contains_key(&tun.id);
            let same = self
                .tunnel_fingerprints
                .get(&tun.id)
                .map(|old| old == &fingerprint)
                .unwrap_or(false);
            if running && same {
                continue;
            }
            // 参数变了（或上次就没跑起来）：停掉旧的，按新参数重建。
            // 顺带清掉旧原因，别让界面继续挂着上一次的错。
            if let Some(mut old) = self.tunnels.remove(&tun.id) {
                old.stop();
            }
            self.tunnel_fingerprints.remove(&tun.id);
            self.reconnects_seen.remove(&tun.id);
            self.reasons.remove(&tun.id);
            match self.build_tunnel(&tun, &service) {
                Ok(tunnel) => {
                    self.tunnels.insert(tun.id.clone(), tunnel);
                    self.tunnel_fingerprints.insert(tun.id.clone(), fingerprint);
                }
                Err(err) => {
                    self.note_tunnel_failure(&tun.id, ReasonCode::ServerMissing, err);
                }
            }
        }
    }

    /// 让本机服务的运行时表和配置保持一致。
    fn sync_local(&mut self) {
        // 删掉的：必须先显式停（保活作业下，Drop 不再杀进程）
        let ids: HashSet<String> = self.config.services.iter().map(|s| s.id.clone()).collect();
        let removed: Vec<String> = self
            .local
            .keys()
            .filter(|id| !ids.contains(*id))
            .cloned()
            .collect();
        for id in removed {
            if let Some(mut runner) = self.local.remove(&id) {
                runner.stop_now();
            }
        }

        // 新增 / 改配置的（改 launch 只换 spec，不杀正在跑的旧进程、不重置看护状态）
        for svc in &self.config.services {
            let spec = LocalSpec {
                name: svc.name.clone(),
                local_port: svc.local_port,
                probe_kind: svc.probe_kind_enum(),
                launch: svc.launch.clone(),
                watch: svc.watch,
            };
            match self.local.get_mut(&svc.id) {
                Some(runner) => {
                    let same = {
                        let old = runner.spec();
                        old.name == spec.name
                            && old.local_port == spec.local_port
                            && old.watch == spec.watch
                            && old.probe_kind == spec.probe_kind
                            && launch_eq(&old.launch, &spec.launch)
                    };
                    if !same {
                        runner.set_spec(spec);
                    }
                }
                None => {
                    // 日志按服务 id 命名（改端口不换日志文件），别用默认的 port-xxxx
                    let mut runner =
                        LocalRunner::new(spec, &self.log_dir, Arc::clone(&self.reporter));
                    runner.set_id(svc.id.clone());
                    self.local.insert(svc.id.clone(), runner);
                }
            }
        }
    }

    fn build_tunnel(&self, tun: &Tunnel, service: &LocalService) -> Result<TunnelProc, String> {
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == tun.server_id)
            .ok_or_else(|| format!("找不到云端服务器（{}）", tun.server_id))?;

        let identity = tun
            .key_path
            .clone()
            .or_else(|| server.key_path.clone())
            .map(PathBuf::from);

        let target = TunnelTarget {
            ssh_target: format!("{}@{}", server.user, server.host),
            ssh_port: server.ssh_port,
            remote_port: tun.remote_port,
            local_port: service.local_port,
            identity_file: identity.clone(),
        };
        let cleanup = CleanupSpec {
            exec: SshExec {
                ssh_exe: ssh::default_ssh_exe(),
                target: format!("{}@{}", server.user, server.host),
                ssh_port: server.ssh_port,
                identity_file: identity,
            },
            remote_port: tun.remote_port,
        };
        let spec = TunnelSpec::new(
            tun.name.clone(),
            ssh::default_ssh_exe(),
            ssh::tunnel_args(&target),
        )
        // 日志文件按隧道 id 命名：改名不换日志文件（见 supervisor::with_id）
        .with_id(tun.id.clone())
        .with_cleanup(cleanup);
        Ok(TunnelProc::new(
            spec,
            &self.log_dir,
            Arc::clone(&self.reporter),
        ))
    }

    // ---------- 快照 ----------

    /// 服务的健康词（D2 §6.3）：端口在听 = up；还在启动且没探过 = unknown；其余 down。
    fn service_health_word(runner: Option<&LocalRunner>) -> &'static str {
        let Some(runner) = runner else {
            return "down";
        };
        if runner.listening() {
            "up"
        } else if runner.status_word() == "starting" && !runner.probe().ever_checked() {
            "unknown"
        } else {
            "down"
        }
    }

    /// 服务的状态词：用户明确停过、且端口现在没人听 → 「已停止」
    /// （探测防抖不能让界面把用户停掉的服务又显示成「在跑」）。
    fn service_status_word(runner: Option<&LocalRunner>) -> &'static str {
        match runner {
            Some(runner) if runner.is_user_stopped() && !runner.listening() => "stopped",
            Some(runner) => runner.status_word(),
            None => "stopped",
        }
    }

    /// 服务的最近一次失败（优先读 state，重启不失忆；没注入时读运行时）。
    fn service_failure_view(&self, id: &str) -> Option<FailureRecord> {
        if let Some(state) = &self.state {
            if let Some(record) = state.service_state(id).and_then(|svc| svc.last_failure) {
                return Some(record);
            }
        }
        self.local.get(id).and_then(|runner| {
            runner.last_failure().map(|failure| FailureRecord {
                ts: failure.ts_ms as i64,
                reason: failure.reason.clone(),
                code: failure.code.as_str().to_string(),
            })
        })
    }

    /// 隧道的最近一次失败（优先读 state，重启不失忆；没注入时读运行时/内存原因）。
    fn tunnel_failure_view(&self, id: &str) -> Option<FailureRecord> {
        if let Some(state) = &self.state {
            if let Some(record) = state.tunnel_state(id).and_then(|tun| tun.last_failure) {
                return Some(record);
            }
        }
        if let Some(failure) = self.tunnels.get(id).and_then(|tunnel| tunnel.last_failure()) {
            return Some(FailureRecord {
                ts: failure.ts_ms as i64,
                reason: failure.reason.clone(),
                code: failure.code.as_str().to_string(),
            });
        }
        self.reasons.get(id).map(|reason| FailureRecord {
            ts: 0,
            reason: reason.clone(),
            code: ReasonCode::ExitUnknown.as_str().to_string(),
        })
    }

    /// 本机服务列表（界面「本机服务」页用）。
    pub fn snapshot(&self) -> Vec<ServiceView> {
        self.config
            .services
            .iter()
            .map(|svc| {
                let runner = self.local.get(&svc.id);
                let tunnels = self.config.tunnels_of_service(&svc.id);
                ServiceView {
                    id: svc.id.clone(),
                    name: svc.name.clone(),
                    local_port: svc.local_port,
                    status: Self::service_status_word(runner).to_string(),
                    health: Self::service_health_word(runner).to_string(),
                    watch: svc.watch,
                    autostart: svc.autostart,
                    launch_exe: svc.launch.as_ref().map(|l| l.exe.clone()),
                    launch_args: svc.launch.as_ref().map(|l| l.args.clone()),
                    launch_workdir: svc.launch.as_ref().and_then(|l| l.workdir.clone()),
                    note: svc.note.clone(),
                    tunnel_count: tunnels.len(),
                    tunnel_names: tunnels.iter().map(|t| t.name.clone()).collect(),
                    reason: runner.and_then(|r| r.reason().map(str::to_string)),
                    log_overflowed: runner.map(|r| r.log_overflowed()).unwrap_or(false),
                    last_failure: self.service_failure_view(&svc.id),
                    log_path: runner
                        .map(|r| r.log_path().display().to_string())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// 隧道列表（界面「发布隧道」页用）。
    pub fn tunnel_snapshot(&self) -> Vec<TunnelView> {
        self.config
            .tunnels
            .iter()
            .map(|tun| {
                let service = self.config.service(&tun.service_id);
                let runner = self.local.get(&tun.service_id);
                let proc = self.tunnels.get(&tun.id);
                let last_failure = self.tunnel_failure_view(&tun.id);

                // retryState：暂停 / 引用缺失 / 不正常重试 → stopped；否则按实例状态
                let refs_ok = service.is_some()
                    && self.config.servers.iter().any(|s| s.id == tun.server_id);
                let retry_state: &str = if !tun.enabled || !refs_ok {
                    "stopped"
                } else {
                    match proc.map(|p| p.retry_state()) {
                        Some(RetryState::Stopped) => "stopped",
                        _ => "active",
                    }
                };

                let reconnects_today = self
                    .state
                    .as_ref()
                    .and_then(|state| state.tunnel_state(&tun.id))
                    .map(|entry| entry.stats.reconnects_today)
                    .unwrap_or(0);
                let protection_check = self
                    .state
                    .as_ref()
                    .and_then(|state| state.tunnel_state(&tun.id))
                    .and_then(|entry| entry.protection_check)
                    .unwrap_or_default();

                TunnelView {
                    id: tun.id.clone(),
                    name: tun.name.clone(),
                    service_id: tun.service_id.clone(),
                    service_name: service.map(|s| s.name.clone()).unwrap_or_default(),
                    local_port: service.map(|s| s.local_port).unwrap_or(0),
                    server_id: tun.server_id.clone(),
                    remote_port: tun.remote_port,
                    url: tun.url.clone(),
                    status: self.status_of(tun).to_string(),
                    enabled: tun.enabled,
                    reason: last_failure.as_ref().map(|f| f.reason.clone()),
                    reason_code: last_failure.as_ref().map(|f| f.code.clone()),
                    retry_state: retry_state.to_string(),
                    attempt: proc.map(|p| p.attempt()).unwrap_or(0),
                    retry_in_secs: proc.and_then(|p| p.retry_in_secs()),
                    connected_since: proc.and_then(|p| p.connected_since_wall_ms()),
                    uptime_secs: proc.and_then(|p| p.uptime_secs()).unwrap_or(0),
                    reconnects_today,
                    last_failure,
                    protection_check,
                    access: tun.access.clone(),
                    access_user: tun.access_user.clone(),
                    has_password: tun
                        .access_hash
                        .as_deref()
                        .map(|h| !h.is_empty())
                        .unwrap_or(false),
                    access_detail: tun.access_detail.clone(),
                    service_ok: runner.map(|r| r.listening()).unwrap_or(false),
                    service_status: Self::service_status_word(runner).to_string(),
                    key_path: tun.key_path.clone(),
                    preset: tun.preset.clone(),
                }
            })
            .collect()
    }

    /// 算一条隧道的状态词（快照和掉线通知共用）。
    fn status_of(&self, tun: &Tunnel) -> &'static str {
        if !tun.enabled {
            "paused"
        } else if self.config.service(&tun.service_id).is_none() {
            "error"
        } else if let Some(tunnel) = self.tunnels.get(&tun.id) {
            match tunnel.phase() {
                Phase::Running { .. } => {
                    if tunnel.connected() {
                        "running"
                    } else {
                        "connecting"
                    }
                }
                Phase::Backoff { .. } => {
                    if tunnel.spawn_failed() {
                        "error"
                    } else if tunnel.ever_connected() {
                        "disconnected"
                    } else {
                        "connecting"
                    }
                }
                Phase::Idle => "connecting",
                Phase::Stopped => "paused",
                // 连续失败到上限、不再自动重试：界面显示「出错」，等用户「再试一次」
                Phase::GaveUp => "error",
            }
        } else if self.config.servers.iter().any(|s| s.id == tun.server_id) {
            "connecting"
        } else {
            "error"
        }
    }

    /// 掉线通知用：隧道 + 本机服务的健康条目（D1 §7.3）。
    pub fn health_snapshot(&self) -> Vec<HealthItem> {
        let mut items = Vec::new();
        for tun in &self.config.tunnels {
            if !tun.enabled {
                continue;
            }
            items.push(HealthItem {
                kind: HealthKind::Tunnel,
                id: tun.id.clone(),
                name: tun.name.clone(),
                healthy: self.status_of(tun) == "running",
                should_notify: true,
            });
        }
        for svc in &self.config.services {
            let runner = self.local.get(&svc.id);
            // 只监控（没配启动方式）的和用户自己停的不参与通知，但条目保留在列表里
            let (healthy, should_notify) = match runner {
                Some(runner) => (
                    runner.probe().alive(),
                    svc.launch.is_some() && !runner.is_user_stopped(),
                ),
                None => (false, false),
            };
            items.push(HealthItem {
                kind: HealthKind::Service,
                id: svc.id.clone(),
                name: svc.name.clone(),
                healthy,
                should_notify,
            });
        }
        items
    }

    /// 免打扰时段（设置页读；D1 §7.3）。
    pub fn notify_quiet(&self) -> QuietHours {
        self.config.settings.quiet_hours.clone()
    }

    /// 免打扰时段（设置页写；时间格式错 / 起止相同给一句人话）。
    pub fn set_notify_quiet(&mut self, quiet: QuietHours) -> Result<(), String> {
        if config::parse_hhmm(&quiet.start).is_none() || config::parse_hhmm(&quiet.end).is_none() {
            return Err("静音时段要写 24 小时制的时间，例如 22:00".to_string());
        }
        if quiet.start == quiet.end {
            return Err(
                "静音时段的开始和结束不能是同一个时间；不想静音就把开关关掉".to_string(),
            );
        }
        let mut next = self.config.clone();
        next.settings.quiet_hours = quiet;
        self.commit_config(next)
    }

    /// 全局设置（快照 `app.settings` 与设置页读）。
    pub fn settings(&self) -> Settings {
        self.config.settings.clone()
    }

    /// 整份设置保存（D2 §9.2：三块全给；缺字段 / 越界 / 未知字段都拒绝并给人话）。
    pub fn set_settings(&mut self, value: serde_json::Value) -> Result<Settings, String> {
        let settings = config::parse_settings_strict(&value)?;
        let mut next = self.config.clone();
        next.settings = settings.clone();
        self.commit_config(next)?;
        Ok(settings)
    }

    /// 掉线通知开关。
    pub fn notify_enabled(&self) -> bool {
        self.config.notify_enabled
    }

    pub fn set_notify_enabled(&mut self, enabled: bool) -> Result<(), String> {
        let mut next = self.config.clone();
        next.notify_enabled = enabled;
        self.commit_config(next)
    }

    pub fn servers(&self) -> Vec<ServerView> {
        self.config
            .servers
            .iter()
            .map(|s| ServerView {
                id: s.id.clone(),
                name: s.name.clone(),
                host: s.host.clone(),
                user: s.user.clone(),
                ssh_port: s.ssh_port,
                key_path: s.key_path.clone(),
                vhost_dir: s.vhost_dir.clone(),
            })
            .collect()
    }

    // ---------- 本机服务：增删改 + 启停 ----------

    /// 新增或编辑一台本机服务。
    pub fn save_service(&mut self, input: ServiceInput) -> Result<String, String> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err("请填写服务名".into());
        }
        if input.local_port == 0 {
            return Err("请填写本机端口".into());
        }
        if self
            .config
            .services
            .iter()
            .any(|s| s.local_port == input.local_port && Some(s.id.as_str()) != input.id.as_deref())
        {
            return Err(format!(
                "本机端口 {} 已经被另一台服务用了",
                input.local_port
            ));
        }

        let launch = build_launch(&input);
        // 没有启动程序就没有「看护」和「开机自启」可言（前端也会同步禁用这两个开关）
        if launch.is_none() && (input.watch || input.autostart) {
            return Err(
                "这台服务没填启动程序（只能看状态），开不了看护或开机自启；先到编辑界面把启动程序填上"
                    .into(),
            );
        }

        let mut next = self.config.clone();
        let id = match input.id.clone() {
            Some(id) => {
                let svc = next
                    .services
                    .iter_mut()
                    .find(|s| s.id == id)
                    .ok_or_else(|| format!("找不到服务：{id}"))?;
                svc.name = name;
                svc.local_port = input.local_port;
                svc.launch = launch;
                svc.watch = input.watch;
                svc.autostart = input.autostart;
                svc.note = clean_optional(input.note);
                id
            }
            None => {
                let id = config::new_id("svc");
                next.services.push(LocalService {
                    id: id.clone(),
                    name,
                    local_port: input.local_port,
                    probe_kind: config::DEFAULT_PROBE_KIND.to_string(),
                    launch,
                    watch: input.watch,
                    autostart: input.autostart,
                    note: clean_optional(input.note),
                });
                id
            }
        };
        // 先落盘、成功了再换内存；存盘失败内存保持原样，界面不会虚报成功
        self.commit_config(next)?;
        self.sync_local();
        Ok(id)
    }

    /// 删除一台本机服务：还有隧道在用它就拦住（不自动连带删）。
    /// 成功时返回可撤销令牌（先写配置，再写墓碑；墓碑写不进去就不可撤销）。
    pub fn remove_service(&mut self, id: &str) -> Result<DeleteResult, String> {
        let used = self.config.tunnels_of_service(id);
        if !used.is_empty() {
            let names: Vec<String> = used.iter().map(|t| format!("「{}」", t.name)).collect();
            return Err(format!(
                "还有 {} 条隧道在用它（{}），先把这些隧道删掉",
                used.len(),
                names.join("、")
            ));
        }
        let (name, item) = {
            let svc = self
                .config
                .service(id)
                .ok_or_else(|| format!("找不到服务：{id}"))?;
            (
                svc.name.clone(),
                serde_json::to_value(svc).unwrap_or(serde_json::Value::Null),
            )
        };
        let mut next = self.config.clone();
        next.services.retain(|s| s.id != id);
        // 先落盘，成功了再停运行中的进程；存盘失败就什么都不动
        self.commit_config(next)?;
        if let Some(mut runner) = self.local.remove(id) {
            runner.stop_now();
        }
        let (token, expires_at) = self.push_tombstone("service", item, BTreeMap::new());
        self.append_service_event(
            id,
            &name,
            ReasonCode::Deleted,
            &ReasonCode::Deleted.render(&[("name", &name)]),
            LEVEL_INFO,
        );
        Ok(DeleteResult {
            id: id.to_string(),
            name,
            token,
            expires_at,
        })
    }

    /// 启动一台本机服务。
    pub fn start_service(&mut self, id: &str) -> Result<(), String> {
        let svc = self
            .config
            .service(id)
            .ok_or_else(|| format!("找不到服务：{id}"))?;
        if svc.launch.is_none() {
            return Err("这台服务没有配置启动方式（只能看状态，不能帮你启动）".into());
        }
        self.sync_local();
        if let Some(runner) = self.local.get_mut(id) {
            runner.start_now();
            runner.poll();
        }
        Ok(())
    }

    /// 停止一台本机服务。
    ///
    /// 分支顺序（安全红线：只有确认过、且能核对是这台服务配置的程序的进程才会被结束）：
    /// 1. 先记下自家子进程号并枚举它的整棵进程树（`.bat/.cmd/.ps1` 包装脚本会再起子孙
    ///    进程绑端口），随后 `runner.stop_now()`：设置 `user_stopped`（看护不再拉起）并借
    ///    Job 结束整棵树；
    /// 2. 轮询端口（3×200ms）等自家进程树退干净，并做最终确认：自家树里不能还有 PID
    ///    占着端口（作业分配失败 / `TerminateJobObject` 失败 / 子孙进程抢跑时会发生），
    ///    否则直接报「没能停干净」；确认后再把仍在监听、但不在自家树里的 PID 当成
    ///    「外部监听者」（只排除直接子进程会把自家后代误判成外部进程）；
    /// 3. 自家树确认退出、端口没有其他监听者 → [`StopOutcome::Stopped`]（`pid` 是自家子进程）；
    /// 4. 只监控的服务（没配启动方式）：不管 `kill_foreign` 是 false 还是 true，都只返回
    ///    人话错误（提示端口上现在是谁在听、先把启动方式补全），绝不返回 `NeedsConfirm`
    ///    ——确认了也一定被拒，是死路；
    /// 5. 还有真正的外部监听者：`kill_foreign = false` 返回 [`StopOutcome::NeedsConfirm`]；
    ///    `kill_foreign = true` 时重新解析端口，核对 PID（若提供）和程序路径，通过才真的
    ///    结束它。
    ///
    /// - 端口上本来就没有进程：返回 [`StopOutcome::Stopped`]。
    pub fn stop_service(
        &mut self,
        id: &str,
        kill_foreign: bool,
        expected_pid: Option<u32>,
    ) -> Result<StopOutcome, String> {
        let (port, launch_exe) = {
            let svc = self
                .config
                .service(id)
                .ok_or_else(|| format!("找不到服务：{id}"))?;
            (svc.local_port, svc.launch.as_ref().map(|l| l.exe.clone()))
        };
        self.sync_local();

        // 1) 先记下自家子进程整棵树（必须在 stop_now 之前记：stop_now 会把 child 清掉），
        //    再停：stop_now 设置 user_stopped 并借 Job 结束整棵树。
        let own_pid = self.local.get(id).and_then(|r| r.pid());
        let own_tree = own_pid.map(process_tree).unwrap_or_default();
        if let Some(runner) = self.local.get_mut(id) {
            runner.stop_now();
            // 用户明确停了：立刻把探测结果清成 down，界面不能因为探测防抖还显示「在跑」
            runner.probe().feed(crate::probe::ProbeOutcome::Down {
                error: "你刚点了停止".to_string(),
            });
        }

        // 2) 有自家进程时轮询等端口清空；仍在监听、又不在自家树里的，才是外部进程。
        let raw_owners = if own_pid.is_some() {
            poll_port_owners(port)
        } else {
            crate::local_ports::listening_pids(port)
        };

        // 2.5) 最终确认自家进程树真的退出：作业分配失败 / TerminateJobObject 失败 /
        //      子孙进程抢跑时，自家进程可能还活着占着端口——不能静默当成功。
        let own_left: Vec<u32> = raw_owners
            .iter()
            .copied()
            .filter(|pid| own_tree.contains(pid))
            .collect();
        if !own_left.is_empty() {
            return Err(format!(
                "没能停干净：端口 {port} 还被这台服务自己的进程占着（PID {}），可能子进程还没退掉；请稍等几秒再点停止，或到任务管理器里结束这些进程",
                join_pids(&own_left)
            ));
        }

        let owners: Vec<u32> = raw_owners
            .into_iter()
            .filter(|pid| !own_tree.contains(pid))
            .collect();

        // 3) 端口没有外部监听者：自家进程已经停了
        if owners.is_empty() {
            return Ok(StopOutcome::Stopped { pid: own_pid });
        }

        // 4) 只监控的服务（没配启动方式）：不管 kill_foreign，都绝不接管外部进程。
        //    之前 kill_foreign=false 会返回 NeedsConfirm，但确认后也一定被拒，是个死路。
        let Some(configured) = launch_exe.as_deref() else {
            let who = owners
                .first()
                .map(|pid| {
                    crate::local_ports::process_path(*pid)
                        .map(|exe| format!("PID {pid}「{exe}」"))
                        .unwrap_or_else(|| format!("PID {pid}"))
                })
                .unwrap_or_else(|| join_pids(&owners));
            return Err(format!(
                "这台服务没配启动方式，是你自己开的程序，我不会去动它（端口上现在是 {who}）；想接管它，先在编辑界面点端口把启动方式补全，再来停止"
            ));
        };

        // 5) 真正的外部监听者：选目标 → 要确认 / 校验后结束
        let target = match expected_pid {
            Some(pid) if owners.contains(&pid) => pid,
            Some(pid) => {
                return Err(format!(
                    "这台服务的端口 {port} 现在的主人已经不是 PID {pid} 了（可能刚退出或被换了），我没有结束任何进程，请重新确认"
                ));
            }
            None => pick_unique_owner(&owners, Some(configured))?,
        };

        if !kill_foreign {
            return Ok(StopOutcome::NeedsConfirm {
                pid: target,
                exe: crate::local_ports::process_path(target),
            });
        }

        // 程序路径必须和配置一致（看护在上面第 1 步已经松手，不会被拉起来）
        let fresh_exe = crate::local_ports::process_path(target).ok_or_else(|| {
            format!("读不到 PID {target} 的程序路径（它可能刚退出），没有结束它")
        })?;
        if !same_exe(configured, &fresh_exe) {
            return Err(format!(
                "为了安全没有结束 PID {target}：它现在运行的是「{fresh_exe}」，不是这台服务配置的「{configured}」"
            ));
        }

        kill_process(target)?;
        Ok(StopOutcome::ForeignStopped { pid: target })
    }

    /// 看护开关。没填启动程序的服务开不了看护（只能看状态）。
    pub fn set_service_watch(&mut self, id: &str, watch: bool) -> Result<(), String> {
        let mut next = self.config.clone();
        let svc = next
            .services
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("找不到服务：{id}"))?;
        if watch && svc.launch.is_none() {
            return Err(
                "这台服务没填启动程序（只能看状态），开不了看护；先到编辑界面把启动程序填上"
                    .into(),
            );
        }
        svc.watch = watch;
        self.commit_config(next)?;
        self.sync_local();
        Ok(())
    }

    /// 开机自启开关。没填启动程序的服务开不了自启。
    pub fn set_service_autostart(&mut self, id: &str, autostart: bool) -> Result<(), String> {
        let mut next = self.config.clone();
        let svc = next
            .services
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("找不到服务：{id}"))?;
        if autostart && svc.launch.is_none() {
            return Err(
                "这台服务没填启动程序（只能看状态），开不了开机自启；先到编辑界面把启动程序填上"
                    .into(),
            );
        }
        svc.autostart = autostart;
        self.commit_config(next)
    }

    /// 本机服务当前进程号（没有在跑就是 None）。
    pub fn service_pid(&self, id: &str) -> Option<u32> {
        self.local.get(id).and_then(|r| r.pid())
    }

    /// 本机服务自己的输出（stdout/stderr + 看护记录）。
    pub fn service_log_tail(&self, id: &str, lines: usize) -> String {
        let Some(runner) = self.local.get(id) else {
            return String::new();
        };
        read_tail(runner.log_path(), lines)
    }

    /// 退出应用前（托盘菜单「退出并停止全部本地服务」）把应用自己拉起来的本地服务
    /// 全停掉。
    ///
    /// 1. 自家子进程：借保活作业的 `terminate()` 结束整棵树；
    /// 2. 仍在监听的外部/认领进程：按 [`Self::stop_service`] 相同的安全规则核对——
    ///    fresh exe 与配置 launchExe 规范化后（忽略大小写、配置只写程序名时比文件名）
    ///    相等才 `TerminateProcess`；核对不过的绝不硬杀，计入「没能停」。
    ///
    /// 自家进程树没能退干净（作业失败 / 子孙抢跑，仍有自家 PID 占着端口）的，
    /// 也计入「没能停」，绝不静默当成功。
    ///
    /// 返回（已停止, 没能停）。
    pub fn stop_all_for_exit(&mut self) -> (usize, usize) {
        self.sync_local();
        let ids: Vec<String> = self.local.keys().cloned().collect();
        let mut stopped = 0usize;
        let mut failed = 0usize;

        for id in ids {
            let Some(svc) = self.config.service(&id) else {
                continue;
            };
            let port = svc.local_port;
            let configured = svc.launch.as_ref().map(|l| l.exe.clone());

            // 先记自家子进程整棵树（必须在 stop_now 清 child 之前记）
            let own_pid = self.local.get(&id).and_then(|r| r.pid());
            let own_tree = own_pid.map(process_tree).unwrap_or_default();
            if let Some(runner) = self.local.get_mut(&id) {
                runner.stop_now();
                runner.probe().feed(crate::probe::ProbeOutcome::Down {
                    error: "退出时按你的要求停了".to_string(),
                });
            }

            // 等自家进程树退干净；还在监听、又不在自家树里的 = 外部/认领进程
            let raw_owners = poll_port_owners(port);

            // 自家树里还有 PID 占着端口 = 没停干净（作业失败 / 子孙抢跑），计入「没能停」
            if raw_owners.iter().any(|pid| own_tree.contains(pid)) {
                failed += 1;
                continue;
            }

            let owners: Vec<u32> = raw_owners
                .into_iter()
                .filter(|pid| !own_tree.contains(pid))
                .collect();

            if owners.is_empty() {
                if own_pid.is_some() {
                    stopped += 1;
                }
                continue;
            }

            // 只监控的服务：没有 launch 可核对归属，绝不硬杀
            let Some(configured) = configured.as_deref() else {
                failed += 1;
                continue;
            };

            // 和 stop_service 一样：多个监听者里只能挑唯一能确认是配置程序的那个
            let target = match pick_unique_owner(&owners, Some(configured)) {
                Ok(pid) => pid,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };

            // fresh 复核：路径必须仍是配置的程序，才允许结束
            let Some(fresh) = crate::local_ports::process_path(target) else {
                failed += 1;
                continue;
            };
            if !same_exe(configured, &fresh) || kill_process(target).is_err() {
                failed += 1;
                continue;
            }

            if wait_port_clear(port) {
                stopped += 1;
            } else {
                failed += 1;
            }
        }

        (stopped, failed)
    }

    /// 停掉当前正在跑的隧道和看护这次会话拉起来的本机服务，**不改配置、不落盘**。
    ///
    /// 与 [`Self::stop_all_for_exit`] 的分工：
    /// - 隧道：把运行中的实例停成 `RetryState::Stopped` 并从运行表里拿掉；配置里的
    ///   `enabled` 原样不动，也不写 `config.json`、`config_generation` 不涨。
    ///   之后「再试一次」/ 改设置 / 重开应用会按配置重建（因为配置还说它该发布）。
    /// - 本机服务：走 [`Self::stop_all_for_exit`] 的同一套安全规则——自家进程树和
    ///   核对过 exe 的认领进程会停，核对不过的绝不硬杀、计入「没能停」。
    /// - 退出前的清理由 [`Self::stop_all_for_exit`] 负责，那条路径的行为不变。
    pub fn stop_all_runtime(&mut self) -> StopAllReport {
        // 隧道：停进程并从运行表拿掉（指纹一起清）；看护 tick 不会因为版本号没变而
        // 重建，下次提交配置 / 唤醒 / 用户点开始才会按配置重新拉起。
        let ids: Vec<String> = self.tunnels.keys().cloned().collect();
        let mut tunnels_stopped = 0usize;
        for id in ids {
            if let Some(mut tunnel) = self.tunnels.remove(&id) {
                tunnel.stop();
                tunnels_stopped += 1;
            }
            self.tunnel_fingerprints.remove(&id);
            self.reconnects_seen.remove(&id);
        }

        // 本机服务：直接复用退出前那套「只停自家进程树 + 核对过 exe 的认领进程」逻辑
        let (services_stopped, services_failed) = self.stop_all_for_exit();

        StopAllReport {
            tunnels_stopped,
            // 隧道停止是同步的（kill + wait），到不了「失败」；字段保留只为 JSON 信封一致
            tunnels_failed: 0,
            services_stopped,
            services_failed,
        }
    }

    // ---------- 隧道：增删改 + 启停 ----------

    /// 新增一条隧道。
    pub fn add_tunnel(&mut self, input: NewTunnelInput) -> Result<String, String> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err("请填写隧道名字".into());
        }
        // 发布预设：缺省 web；非法值用固定文案拒绝（读配置时归一，写配置时严格）
        let preset =
            clean_optional(input.preset).unwrap_or_else(|| config::DEFAULT_PRESET.to_string());
        config::check_preset_strict(&preset)?;
        let service = self
            .config
            .service(&input.service_id)
            .ok_or("请先选择一台本机服务")?;
        let local_port = service.local_port;
        if !self.config.servers.iter().any(|s| s.id == input.server_id) {
            return Err("请先选择云端服务器".into());
        }
        if self
            .config
            .used_remote_ports(&input.server_id, None)
            .contains(&input.remote_port)
        {
            return Err(format!(
                "云端端口 {} 已经被这个服务器上的其他隧道用了",
                input.remote_port
            ));
        }
        let _ = local_port;

        // 勾了保护就必须有对应的内容：密码要有哈希、白名单要有 IP
        if input.access.iter().any(|a| a == "password")
            && clean_optional(input.access_hash.clone()).is_none()
        {
            return Err("这条隧道选了「要密码才能进」，请先设置访问密码".into());
        }
        if input.access.iter().any(|a| a == "ip")
            && clean_optional(input.access_detail.clone()).is_none()
        {
            return Err("这条隧道选了「只允许指定 IP」，请先把允许访问的 IP 填上".into());
        }

        let id = config::new_id("tun");
        let mut next = self.config.clone();
        next.tunnels.push(Tunnel {
            id: id.clone(),
            name,
            service_id: input.service_id,
            server_id: input.server_id,
            remote_port: input.remote_port,
            url: clean_optional(input.url),
            key_path: None,
            access: input.access,
            access_user: clean_optional(input.access_user),
            access_hash: clean_optional(input.access_hash),
            access_detail: clean_optional(input.access_detail),
            enabled: true,
            preset,
        });
        self.commit_config(next)?;
        Ok(id)
    }

    /// 编辑一条隧道。改云端端口会停掉旧隧道，由看护线程按新端口重新拉起。
    pub fn update_tunnel(&mut self, input: UpdateTunnelInput) -> Result<(), String> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err("请填写隧道名字".into());
        }
        // 发布预设：给了才改；非法值用固定文案拒绝（读配置时归一，写配置时严格）
        let preset = match clean_optional(input.preset.clone()) {
            Some(value) => {
                config::check_preset_strict(&value)?;
                Some(value)
            }
            None => None,
        };

        let (server_id, port_changed, old_access, old_hash, old_user, old_detail) = {
            let tun = self
                .config
                .tunnel(&input.id)
                .ok_or_else(|| format!("找不到隧道：{}", input.id))?;
            (
                tun.server_id.clone(),
                tun.remote_port != input.remote_port,
                tun.access.clone(),
                tun.access_hash.clone(),
                tun.access_user.clone(),
                tun.access_detail.clone(),
            )
        };

        if self
            .config
            .used_remote_ports(&server_id, Some(&input.id))
            .contains(&input.remote_port)
        {
            return Err(format!(
                "云端端口 {} 已经被这个服务器上的其他隧道用了",
                input.remote_port
            ));
        }

        // 没传 access 就保持原样；传了就整体替换。
        let access = input.access.clone().unwrap_or(old_access);
        let new_hash = clean_optional(input.access_hash.clone());
        let old_hash = clean_optional(old_hash);
        let old_user = clean_optional(old_user);
        let wants_password = access.iter().any(|a| a == "password");
        let wants_ip = access.iter().any(|a| a == "ip");
        // IP 校验/写回都用「合并后的 detail」：新值优先，没传就用旧的（CLI 不传时保留原值）。
        let final_detail = if wants_ip {
            clean_optional(input.access_detail.clone()).or(old_detail)
        } else {
            // 不要 IP 保护了：把旧的 IP 列表清掉（等于界面传 null）。
            None
        };
        if wants_password && new_hash.is_none() && old_hash.is_none() {
            return Err("这条隧道选了「要密码才能进」，请先设置访问密码".into());
        }
        if wants_ip && final_detail.is_none() {
            return Err("这条隧道选了「只允许指定 IP」，请先把允许访问的 IP 填上".into());
        }

        // 保护内容跟着「最终 access」走：
        // - 不含 password：hash 和 user 一起清掉（关掉保护后旧密码不能复活）；
        // - 含 password：hash 只在显式给了新值时替换，user 只在显式给了新值时覆盖
        //   （None/null = 保持现值，跟 access_hash 对称）。
        let (final_hash, final_user) = if wants_password {
            (
                new_hash.or(old_hash),
                clean_optional(input.access_user.clone()).or(old_user),
            )
        } else {
            (None, None)
        };

        let mut next = self.config.clone();
        {
            let tun = next
                .tunnels
                .iter_mut()
                .find(|t| t.id == input.id)
                .ok_or_else(|| format!("找不到隧道：{}", input.id))?;
            tun.name = name;
            tun.remote_port = input.remote_port;
            tun.url = clean_optional(input.url);
            tun.access = access;
            tun.access_user = final_user;
            tun.access_hash = final_hash;
            tun.access_detail = final_detail;
            if let Some(preset) = &preset {
                tun.preset = preset.clone();
            }
        }
        // 先落盘，成功了再动运行中的隧道；存盘失败时旧隧道照跑，界面不会虚报成功
        self.commit_config(next)?;

        if port_changed {
            if let Some(mut tunnel) = self.tunnels.remove(&input.id) {
                tunnel.stop();
            }
            self.tunnel_fingerprints.remove(&input.id);
            self.reasons.remove(&input.id);
        }
        Ok(())
    }

    /// 开始发布 / 暂停发布。
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let mut next = self.config.clone();
        let tun = next
            .tunnels
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("找不到隧道：{id}"))?;
        tun.enabled = enabled;
        self.commit_config(next)
    }

    /// 立刻重连：停掉当前隧道并重新开始（配置里顺带恢复发布）。
    pub fn restart(&mut self, id: &str) -> Result<(), String> {
        let mut next = self.config.clone();
        {
            let tun = next
                .tunnels
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| format!("找不到隧道：{id}"))?;
            tun.enabled = true;
        }
        self.commit_config(next)?;
        if let Some(mut tunnel) = self.tunnels.remove(id) {
            tunnel.stop();
        }
        self.tunnel_fingerprints.remove(id);
        self.reasons.remove(id);
        Ok(())
    }

    /// 停止并删除一条隧道（配置一起删；返回可撤销令牌）。
    pub fn remove_tunnel(&mut self, id: &str) -> Result<DeleteResult, String> {
        let (name, item, deps) = {
            let tun = self
                .config
                .tunnel(id)
                .ok_or_else(|| format!("找不到隧道：{id}"))?;
            (
                tun.name.clone(),
                serde_json::to_value(tun).unwrap_or(serde_json::Value::Null),
                self.tunnel_deps(tun),
            )
        };
        let mut next = self.config.clone();
        next.tunnels.retain(|t| t.id != id);
        // 先落盘，成功了再停运行中的进程；存盘失败就什么都不动
        self.commit_config(next)?;
        if let Some(mut tunnel) = self.tunnels.remove(id) {
            tunnel.stop();
        }
        self.tunnel_fingerprints.remove(id);
        self.reconnects_seen.remove(id);
        self.reasons.remove(id);
        let (token, expires_at) = self.push_tombstone("tunnel", item, deps);
        self.append_event(
            "tunnel",
            LEVEL_INFO,
            ReasonCode::Deleted,
            EventSubject::new("tunnel", id, &name),
            &ReasonCode::Deleted.render(&[("name", &name)]),
        );
        Ok(DeleteResult {
            id: id.to_string(),
            name,
            token,
            expires_at,
        })
    }

    // ---------- 导入 ----------

    /// 导入接管：把扫描到的建议写成「已暂停」的服务 + 隧道（用户确认后手动开始发布）。
    pub fn import_services(&mut self, items: Vec<ImportSuggestion>) -> Result<usize, String> {
        let mut next = self.config.clone();
        let mut added = 0usize;
        for item in items {
            let server_id = match next
                .servers
                .iter()
                .find(|s| s.host == item.server_host && s.user == item.server_user)
            {
                Some(s) => s.id.clone(),
                None => {
                    let id = config::new_id("srv");
                    next.servers.push(ServerConfig {
                        id: id.clone(),
                        name: format!("{}（导入）", item.server_host),
                        host: item.server_host.clone(),
                        ssh_port: item.ssh_port,
                        user: item.server_user.clone(),
                        key_path: None,
                        vhost_dir: None,
                    });
                    id
                }
            };
            // 从这份配置里挑到的服务器 id 也要挡路径穿越（手工改过的配置可能在）
            validate_server_id(&server_id)?;
            if next
                .tunnels
                .iter()
                .any(|t| t.server_id == server_id && t.remote_port == item.remote_port)
            {
                continue; // 已有同端口的隧道，跳过
            }

            // 同一个本机端口只建一台本机服务
            let service_id = match next
                .services
                .iter()
                .find(|s| s.local_port == item.local_port)
            {
                Some(s) => s.id.clone(),
                None => {
                    let id = config::new_id("svc");
                    next.services.push(LocalService {
                        id: id.clone(),
                        name: item.name.clone(),
                        local_port: item.local_port,
                        probe_kind: config::DEFAULT_PROBE_KIND.to_string(),
                        launch: None,
                        watch: false,
                        autostart: false,
                        note: Some("从现有脚本导入".into()),
                    });
                    id
                }
            };

            next.tunnels.push(Tunnel {
                id: config::new_id("tun"),
                name: item.name.clone(),
                service_id,
                server_id,
                remote_port: item.remote_port,
                url: item.url.clone(),
                key_path: None,
                access: Vec::new(),
                access_user: None,
                access_hash: None,
                access_detail: None,
                enabled: false,
                preset: config::DEFAULT_PRESET.to_string(),
            });
            added += 1;
        }
        // 先落盘、成功了再换内存；存盘失败就整个导入回滚，界面不会虚报成功
        self.commit_config(next)?;
        Ok(added)
    }

    // ---------- 服务器管理 ----------

    pub fn save_server(&mut self, input: ServerInput) -> Result<String, String> {
        let host = input.host.trim().to_string();
        let user = input.user.trim().to_string();
        if host.is_empty() || user.is_empty() {
            return Err("服务器地址和登录账号都不能为空".into());
        }
        // 名字不填就按地址生成一个默认名，列表里不出现无名服务器
        let name = {
            let name = input.name.trim();
            if name.is_empty() {
                host.clone()
            } else {
                name.to_string()
            }
        };
        let id = input.id.clone().unwrap_or_else(|| config::new_id("srv"));
        validate_server_id(&id)?;
        let ssh_port = input.ssh_port;
        let key_path = clean_optional(input.key_path);
        let vhost_dir = clean_optional(input.vhost_dir);

        let mut next = self.config.clone();
        if let Some(server) = next.servers.iter_mut().find(|s| s.id == id) {
            server.name = name;
            server.host = host;
            server.ssh_port = ssh_port;
            server.user = user;
            server.key_path = key_path;
            server.vhost_dir = vhost_dir;
        } else {
            next.servers.push(ServerConfig {
                id: id.clone(),
                name,
                host,
                ssh_port,
                user,
                key_path,
                vhost_dir,
            });
        }
        self.commit_config(next)?;
        Ok(id)
    }

    /// 配免密要用：拿到服务器的地址/端口/账号。
    pub fn server_basics(&self, id: &str) -> Result<(String, u16, String), String> {
        let s = self
            .config
            .servers
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("找不到服务器：{id}"))?;
        Ok((s.host.clone(), s.ssh_port, s.user.clone()))
    }

    /// 配好免密后，把密钥路径记到这台服务器上（隧道就会用它）。
    pub fn set_server_key(&mut self, id: &str, key_path: &str) -> Result<(), String> {
        let mut next = self.config.clone();
        {
            let s = next
                .servers
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("找不到服务器：{id}"))?;
            s.key_path = Some(key_path.to_string());
        }
        self.commit_config(next)
    }

    /// 删除一台服务器（还有隧道挂着就拦住；返回可撤销令牌）。
    pub fn delete_server(&mut self, id: &str) -> Result<DeleteResult, String> {
        if self.config.tunnels.iter().any(|t| t.server_id == id) {
            return Err("还有隧道挂在这台服务器上，先处理它们".into());
        }
        let (name, item) = {
            let server = self
                .config
                .servers
                .iter()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("找不到服务器：{id}"))?;
            (
                server.name.clone(),
                serde_json::to_value(server).unwrap_or(serde_json::Value::Null),
            )
        };
        let mut next = self.config.clone();
        next.servers.retain(|s| s.id != id);
        self.commit_config(next)?;
        let (token, expires_at) = self.push_tombstone("server", item, BTreeMap::new());
        self.append_event(
            "app",
            LEVEL_INFO,
            ReasonCode::Deleted,
            EventSubject::new("server", id, &name),
            &ReasonCode::Deleted.render(&[("name", &name)]),
        );
        Ok(DeleteResult {
            id: id.to_string(),
            name,
            token,
            expires_at,
        })
    }

    // ---------- 日志 / 密码 / nginx ----------

    /// 读一条隧道的日志尾部（界面「看日志」用）。
    pub fn log_tail(&self, id: &str, lines: usize) -> String {
        let Some(tun) = self.config.tunnel(id) else {
            return String::new();
        };
        read_tail(&service_log_path(&self.log_dir, &tun.id), lines)
    }

    /// 生成这条隧道的 nginx 配置片段（人工应用到面板）。
    pub fn nginx_snippet(&self, id: &str) -> Result<crate::nginx::NginxSnippet, String> {
        let tun = self
            .config
            .tunnel(id)
            .ok_or_else(|| format!("找不到隧道：{id}"))?;
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == tun.server_id)
            .ok_or_else(|| "找不到这条隧道挂的云端服务器".to_string())?;
        let local_port = self
            .config
            .service(&tun.service_id)
            .map(|s| s.local_port)
            .unwrap_or(0);
        // 片段也要用「环境识别缓存的站点目录 / WS_MAP / 上次检查的 .well-known 情况」；
        // 没有缓存时和以前一样按默认解析（GenOptions 全 None = generate 的老行为）。
        let detected = self.detected_vhost_dir(&server.id);
        // WS_MAP 尽力探测：先看服务器环境识别缓存，再看这条隧道上次检查结果里带的环境
        let ws_map = self
            .cached_cloud_env(&server.id)
            .and_then(|env| env.ws_map)
            .or_else(|| self.last_check_cloud_env(id).and_then(|env| env.ws_map));
        let wellknown_present = self.wellknown_present(id);
        let opts = crate::nginx::GenOptions {
            ws_map,
            wellknown_present,
            detected_vhost_dir: detected.as_deref(),
        };
        Ok(crate::nginx::generate_with(tun, local_port, server, &opts))
    }

    /// 在本机生成 bcrypt 密码哈希、结果写进服务器上的密码文件（新建/编辑隧道用）。
    pub fn hash_password(&self, server_id: &str, password: &str) -> Result<String, String> {
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .ok_or("找不到云端服务器")?;
        let exec = SshExec {
            ssh_exe: ssh::default_ssh_exe(),
            target: format!("{}@{}", server.user, server.host),
            ssh_port: server.ssh_port,
            identity_file: server.key_path.clone().map(PathBuf::from),
        };
        crate::cleanup::hash_password(&exec, password)
    }

    /// 只读地把核对需要的连接信息（目标、端口、密钥、域名）克隆出来，不联网。
    ///
    /// 真正跑 SSH 的是自由函数 [`run_access_check`]，调用方（Tauri 命令、控制通道）
    /// 可以在短锁里拿到 request、放锁后再跑，别占着看护轮询的全局锁最长 20 秒。
    pub fn access_check_request(&self, id: &str) -> Result<AccessCheckRequest, String> {
        let tun = self
            .config
            .tunnel(id)
            .ok_or_else(|| format!("找不到隧道：{id}"))?;
        let raw_url = tun
            .url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                format!(
                    "隧道「{}」还没填公网网址，没法核对云端保护；先回应用里把网址填上",
                    tun.name
                )
            })?;
        let domain = normalize_domain(raw_url);
        if domain.is_empty() {
            return Err(format!(
                "隧道「{}」还没填公网网址，没法核对云端保护；先回应用里把网址填上",
                tun.name
            ));
        }
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == tun.server_id)
            .ok_or_else(|| "找不到这条隧道挂的云端服务器".to_string())?;
        Ok(AccessCheckRequest {
            target: format!("{}@{}", server.user, server.host),
            ssh_port: server.ssh_port,
            identity_file: server.key_path.clone().map(PathBuf::from),
            domain,
            // 站点目录解析链第二层：环境识别到、缓存在 state 里的目录
            // （服务器手填的 vhostDir 优先级仍然更高，见 nginx::htpasswd_dir_with）
            vhost_dir: crate::nginx::htpasswd_dir_with(
                server,
                self.detected_vhost_dir(&server.id).as_deref(),
            ),
        })
    }

    /// 核对这条隧道的网站上实际开着的访问保护：通过 SSH 只读地看云端 nginx 配置，
    /// 不改服务器上的任何东西。薄包装：先取连接信息，再跑核对。
    pub fn check_tunnel_access(&self, id: &str) -> Result<AccessCheck, String> {
        let req = self.access_check_request(id)?;
        run_access_check(&req)
    }

    /// 配置现在能不能安全写回（界面可以用来提示用户）。
    pub fn config_trusted(&self) -> bool {
        self.config_trusted
    }

    /// 先把新配置落盘，成功了再换进内存；失败就保持原样并返回错误。
    ///
    /// 这是所有配置写入的唯一原子提交点（供导入替换复用）：
    /// - 成功后 `config_generation` +1，看护 tick 据此重新同步；
    /// - 影响访问保护的配置变了 → 把相关隧道的 `protectionCheck` 清成 unknown
    ///   （D2 §6.4：配置一变，旧核对结果就作废了）；
    /// - 提交后立刻按新配置对齐隧道/本机服务。
    pub fn commit_config(&mut self, next: AppConfig) -> Result<(), String> {
        if !self.config_trusted {
            return Err(
                "配置文件上次没读成功（已经留底成 config.json.bad），这次没有把新配置写回磁盘；请先修好配置文件再重开应用"
                    .into(),
            );
        }
        next.save(&self.config_path)
            .map_err(|e| format!("保存配置失败：{e}"))?;

        // 配置变更：清掉过期的云端核对结果（只看与访问保护相关的字段，
        // 起停 / 改名 / 换云端端口不该把「核对过」的状态清掉）
        if let Some(state) = &self.state {
            let old_servers: BTreeMap<&str, serde_json::Value> = self
                .config
                .servers
                .iter()
                .map(|s| (s.id.as_str(), protection_relevant_server(s)))
                .collect();
            let new_servers: BTreeMap<&str, serde_json::Value> = next
                .servers
                .iter()
                .map(|s| (s.id.as_str(), protection_relevant_server(s)))
                .collect();
            if old_servers != new_servers {
                state.clear_all_protection();
            } else {
                for tun in &next.tunnels {
                    let changed = self
                        .config
                        .tunnel(&tun.id)
                        .map(|old| protection_relevant_tunnel(old) != protection_relevant_tunnel(tun))
                        .unwrap_or(true);
                    if changed {
                        state.clear_protection_check(&tun.id);
                    }
                }
            }
            self.protection_seen.clear();
        }

        self.config = next;
        self.config_generation = self.config_generation.saturating_add(1);
        self.sync_tunnels();
        self.sync_local();
        self.synced_generation = self.config_generation;
        // D2 §5：配置落盘成功就记一条 configSaved（导入走同一入口，另有一条专用文案）
        self.append_event(
            "app",
            LEVEL_INFO,
            ReasonCode::ConfigSaved,
            EventSubject::app(),
            ReasonCode::ConfigSaved.text(),
        );
        Ok(())
    }

    /// 撤销删除（D2 §10）：把墓碑插回配置；隧道一律恢复为暂停。
    pub fn restore_deleted(&mut self, token: &str) -> Result<RestoreResult, String> {
        let Some(state) = self.state.clone() else {
            return Err("撤销需要在应用里进行（当前没有运行记录）".to_string());
        };
        let record = state.peek_trash(token).ok_or_else(|| {
            "这条撤销已经过期了（删除超过 10 分钟）；要恢复只能重新建一个".to_string()
        })?;
        // 服务器 id 会拼进密钥路径（config::key_path_for）：撤销手工改过的墓碑前先挡住路径穿越
        if record.kind == "server" {
            let id = record
                .item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            validate_server_id(id)?;
        }
        let next = crate::state::insert_restored(&self.config, &record)?;
        self.commit_config(next)?;
        state.take_trash(token);
        let id = record
            .item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = record
            .item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        self.append_event(
            event_kind_of(&record.kind),
            LEVEL_INFO,
            ReasonCode::Restored,
            EventSubject::new(&record.kind, &id, &name),
            &ReasonCode::Restored.render(&[("name", &name)]),
        );
        Ok(RestoreResult {
            kind: record.kind,
            id,
            name,
        })
    }

    /// 删除后落一条墓碑（撤销入口）；没注入 state 时删除仍然生效、只是不可撤销。
    fn push_tombstone(
        &self,
        kind: &str,
        item: serde_json::Value,
        dependencies: BTreeMap<String, String>,
    ) -> (String, i64) {
        let record = crate::state::new_trash(kind, item, dependencies, crate::state::now_ms());
        let token = record.token.clone();
        let expires_at = record.expires_at;
        match &self.state {
            Some(state) => {
                state.push_trash(record);
                (token, expires_at)
            }
            None => (String::new(), 0),
        }
    }

    /// 隧道依赖名字快照（撤销失败时给人话用）。
    fn tunnel_deps(&self, tun: &Tunnel) -> BTreeMap<String, String> {
        let mut deps = BTreeMap::new();
        if let Some(service) = self.config.service(&tun.service_id) {
            deps.insert("serviceId".to_string(), service.name.clone());
        }
        if let Some(server) = self.config.servers.iter().find(|s| s.id == tun.server_id) {
            deps.insert("serverId".to_string(), server.name.clone());
        }
        deps
    }

    /// 重置服务器指纹：把这条隧道对应的服务器从 known_hosts 里删掉，并重启隧道。
    /// 调用方必须先拿到用户确认（这是 TOFU 的重信任操作）。
    pub fn forget_host_key(&mut self, id: &str) -> Result<String, String> {
        let (host, port) = {
            let tun = self
                .config
                .tunnel(id)
                .ok_or_else(|| format!("找不到隧道：{id}"))?;
            let server = self
                .config
                .servers
                .iter()
                .find(|s| s.id == tun.server_id)
                .ok_or_else(|| "找不到这条隧道挂的云端服务器".to_string())?;
            (server.host.clone(), server.ssh_port)
        };
        let message = crate::ssh::forget_host_key(&host, port)?;
        self.restart(id)?;
        Ok(message)
    }

    /// 重启一台本机服务并轮转日志（日志超过上限时的手动入口）。
    pub fn restart_service_rotate_logs(&mut self, id: &str) -> Result<(), String> {
        let svc = self
            .config
            .service(id)
            .ok_or_else(|| format!("找不到服务：{id}"))?;
        if svc.launch.is_none() {
            return Err("这台服务没配启动方式，不能重启（只能看状态）".to_string());
        }
        self.sync_local();
        if let Some(runner) = self.local.get_mut(id) {
            runner.restart_and_rotate();
        }
        self.reporter.report(
            LogLevel::Info,
            TAG_SERVICE,
            &format!("服务 {id} 已重启并轮转日志"),
        );
        Ok(())
    }
}

/// 与云端访问保护核对相关的隧道字段：这些变了，旧的 protectionCheck 就作废。
fn protection_relevant_tunnel(tun: &Tunnel) -> serde_json::Value {
    serde_json::json!({
        "serviceId": tun.service_id,
        "serverId": tun.server_id,
        "url": tun.url,
        "access": tun.access,
        "accessUser": tun.access_user,
        "accessHash": tun.access_hash,
        "accessDetail": tun.access_detail,
    })
}

/// 与云端访问保护核对相关的服务器字段（改名不影响核对结果）。
fn protection_relevant_server(server: &ServerConfig) -> serde_json::Value {
    serde_json::json!({
        "host": server.host,
        "sshPort": server.ssh_port,
        "user": server.user,
        "keyPath": server.key_path,
        "vhostDir": server.vhost_dir,
    })
}

/// 一条隧道「实际跑起来用的参数」：任何一项变了，运行中的旧进程都得停掉、按新参数重建。
/// 包含：服务器地址 / SSH 端口 / 登录账号 / 密钥、本机端口、云端端口。
/// （隧道名不算：日志文件按 id 命名，改名不需要重连。）
fn tunnel_fingerprint(config: &AppConfig, tun: &Tunnel, service: &LocalService) -> String {
    let server = config.servers.iter().find(|s| s.id == tun.server_id);
    let key = tun
        .key_path
        .clone()
        .or_else(|| server.and_then(|s| s.key_path.clone()))
        .unwrap_or_default();
    format!(
        "host={}|ssh_port={}|user={}|key={}|local_port={}|remote_port={}",
        server.map(|s| s.host.as_str()).unwrap_or(""),
        server.map(|s| s.ssh_port).unwrap_or(0),
        server.map(|s| s.user.as_str()).unwrap_or(""),
        key,
        service.local_port,
        tun.remote_port
    )
}

fn build_launch(input: &ServiceInput) -> Option<LaunchSpec> {
    let exe = input
        .launch_exe
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    Some(LaunchSpec {
        exe,
        args: input.launch_args.clone().unwrap_or_default(),
        workdir: clean_optional(input.launch_workdir.clone()),
    })
}

fn launch_eq(a: &Option<LaunchSpec>, b: &Option<LaunchSpec>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            x.exe == y.exe && x.args == y.args && x.workdir == y.workdir
        }
        _ => false,
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 服务器 id 只允许 `[A-Za-z0-9_-]`、长度 1..=64。
/// `config::key_path_for` 会拿 id 拼密钥路径，手工改过的配置不能靠 `..\` 跑出数据目录。
fn validate_server_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "服务器编号「{id}」不合法：只能用字母、数字、下划线、横杠（1~64 位）；请删掉重新添加这台服务器"
        ))
    }
}

/// 从 `protectionCheck.extra` 里读「站点配置有没有 /.well-known 块」。
/// 兼容几种写法；都没有 = None（维持老行为）。
fn wellknown_present_from_extra(extra: Option<&serde_json::Value>) -> Option<bool> {
    let extra = extra?;
    for key in ["wellknownPresent", "wellKnownPresent", "wellknown_present"] {
        if let Some(value) = extra.get(key).and_then(serde_json::Value::as_bool) {
            return Some(value);
        }
    }
    extra
        .get("wellknown")
        .and_then(|value| value.get("present"))
        .and_then(serde_json::Value::as_bool)
}

/// 实体类型 → 事件 `kind`（"server" 没有自己的事件种类，记到应用级）。
fn event_kind_of(entity: &str) -> &'static str {
    match entity {
        "tunnel" => "tunnel",
        "service" => "service",
        _ => "app",
    }
}

/// 核对云端访问保护需要的最小连接信息（从配置里只读克隆出来，不联网）。
#[derive(Debug, Clone)]
pub struct AccessCheckRequest {
    /// `user@host`
    pub target: String,
    pub ssh_port: u16,
    pub identity_file: Option<PathBuf>,
    /// 已规范化过的域名（去掉了协议前缀和路径）
    pub domain: String,
    /// 面板放站点的目录（nginx 配置在它下面的 nginx/ 里，htpasswd 直接在它下面）
    pub vhost_dir: String,
}

/// 跑一次云端访问保护核对（只读 SSH，最长 20 秒后超时）。
///
/// 不碰 Guardian，也不持有任何锁，所以调用方可以放锁后再调（见 P2-2）。
pub fn run_access_check(req: &AccessCheckRequest) -> Result<AccessCheck, String> {
    let exec = SshExec {
        ssh_exe: ssh::default_ssh_exe(),
        target: req.target.clone(),
        ssh_port: req.ssh_port,
        identity_file: req.identity_file.clone(),
    };
    let out = crate::cleanup::run(
        &exec,
        &access_check_script(&req.domain, &req.vhost_dir),
        Duration::from_secs(20),
    )?;
    Ok(crate::protection::parse(&out))
}

/// 网址 → 域名：去空白、去掉 http:// / https:// 前缀、砍掉第一个 `/` 或空白
/// 后面的路径。例如 `https://karryn.iranol1.xyz/x` → `karryn.iranol1.xyz`。
fn normalize_domain(raw: &str) -> String {
    let mut domain = raw.trim();
    for prefix in ["https://", "http://"] {
        if let Some(rest) = domain.strip_prefix(prefix) {
            domain = rest;
            break;
        }
        // 大写写法（HTTP://...）也认；前缀是纯 ASCII，按下标切不会切坏字符
        if domain.len() >= prefix.len()
            && domain.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            domain = &domain[prefix.len()..];
            break;
        }
    }
    let end = domain
        .find(|c: char| c == '/' || c.is_whitespace())
        .unwrap_or(domain.len());
    domain[..end].trim().to_string()
}

/// 核对访问保护的远端脚本：只读地把站点配置夹在正文标记之间打印出来，
/// 标记之外再报告配置文件 / 密码文件在不在。
/// 域名和站点目录都用单引号包好，避免被 shell 当成命令解释。
/// 站点目录由 [`crate::nginx::htpasswd_dir_for`] 解析：nginx 配置在 <目录>/nginx/ 下，
/// htpasswd 密码文件直接在 <目录> 下。
fn access_check_script(domain: &str, vhost_dir: &str) -> String {
    format!(
        "d={}; dir={}; f=\"$dir/nginx/$d.conf\"; if [ -f \"$f\" ]; then echo '===SHANGXIAN-CONF-BEGIN==='; cat \"$f\"; echo; echo '===SHANGXIAN-CONF-END==='; echo CONF_FOUND; else echo CONF_MISSING; fi; if [ -f \"$dir/$d.htpasswd\" ]; then echo HTPASSWD_YES; else echo HTPASSWD_NO; fi",
        crate::cleanup::shell_quote(domain),
        crate::cleanup::shell_quote(vhost_dir)
    )
}

/// 读日志尾部：只从文件末尾读 `MAX_TAIL_BYTES`，不把整个大日志读进内存；
/// 调用方（界面看日志、退出原因）可能是拿着锁调的，这里必须快。行数也有上限，
/// 免得日志只要一行、却有几兆长。
fn read_tail(path: &Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};

    // 尾部最多读 64KB
    const MAX_TAIL_BYTES: u64 = 64 * 1024;
    // 最多回多少行
    const MAX_TAIL_LINES: usize = 2000;

    let Ok(mut file) = std::fs::File::open(path) else {
        return format!("（还没有日志：{}）", path.display());
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(MAX_TAIL_BYTES);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return format!("（读不到日志：{}）", path.display());
    }
    let mut buf = Vec::new();
    if file.take(MAX_TAIL_BYTES).read_to_end(&mut buf).is_err() {
        return format!("（读不到日志：{}）", path.display());
    }
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    // 从中间截出来时，第一行可能是半截：只保留第一个换行之后的内容
    if from > 0 {
        if let Some(pos) = text.find('\n') {
            text = text[pos + 1..].to_string();
        } else {
            text.clear(); // 尾部连一个完整行都没有
        }
    }
    let all: Vec<&str> = text.lines().collect();
    let want = lines.min(MAX_TAIL_LINES);
    let start = all.len().saturating_sub(want);
    all[start..].join("\n")
}

/// 从日志尾部挑一句人话的退出原因。关键字表已收归 `crate::reason`（单一翻译器），
/// 这里只做“文本 → 退出码兜底”的薄包装，给还没拿到原因码的调用方用。
/// 认不出且退出码不是 255 时返回 None。
pub fn exit_reason_from_log(tail: &str, exit_code: Option<u32>) -> Option<String> {
    let code = match exit_code {
        Some(code) => ReasonCode::from_ssh_exit(tail, code as i32),
        None => ReasonCode::from_ssh_text(tail).unwrap_or(ReasonCode::ExitUnknown),
    };
    if ReasonCode::from_ssh_text(tail).is_none() && exit_code != Some(255) {
        return None;
    }
    Some(code.text().to_string())
}

/// 退出/启动失败事件 → 一句人话原因（统一从 `crate::reason` 取文案）。
fn failure_reason(code: ReasonCode, exit_code: Option<u32>) -> String {
    match code {
        ReasonCode::SignalExit => {
            let value = exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "未知".to_string());
            code.render(&[("code", &value)])
        }
        _ => code.text().to_string(),
    }
}

/// 漂移字段 → 人话差异（写 protectionDrift 事件用）。
fn drift_fields_text(fields: &[String]) -> String {
    let names: Vec<&str> = fields
        .iter()
        .map(|field| match field.as_str() {
            "conf" => "云端没找到站点配置",
            "password" => "密码开关对不上",
            "ip" => "IP 白名单开关对不上",
            "allowIps" => "允许的 IP 不一致",
            "htpasswd" => "密码文件不在",
            _ => "有一项对不上",
        })
        .collect();
    if names.is_empty() {
        "具体差异没记下来".to_string()
    } else {
        names.join("、")
    }
}

// ---------- 停止外部进程的安全工具 ----------

/// 停止自家进程时的端口轮询次数 / 间隔：留时间给 Job 清掉整棵进程树
/// （`.cmd/.ps1` 包装脚本再拉起来的子孙进程也会晚一拍才退干净）。
const STOP_PORT_POLLS: usize = 3;
const STOP_PORT_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// 摸端口上现在的监听者；前面几次非空就歇 200ms 再看，最多看 `STOP_PORT_POLLS` 次。
fn poll_port_owners(port: u16) -> Vec<u32> {
    let mut owners = crate::local_ports::listening_pids(port);
    for _ in 1..STOP_PORT_POLLS {
        if owners.is_empty() {
            break;
        }
        std::thread::sleep(STOP_PORT_POLL_INTERVAL);
        owners = crate::local_ports::listening_pids(port);
    }
    owners
}

/// 等端口彻底没人监听（最多约 600ms）。用于退出前确认核对过的认领进程真的停了。
fn wait_port_clear(port: u16) -> bool {
    for _ in 0..STOP_PORT_POLLS {
        if crate::local_ports::listening_pids(port).is_empty() {
            return true;
        }
        std::thread::sleep(STOP_PORT_POLL_INTERVAL);
    }
    crate::local_ports::listening_pids(port).is_empty()
}

/// 进程 `root` 及其全部后代（ToolHelp 快照，只读）。
///
/// 用来区分「自家子进程树」和「真正的外部监听者」：包装脚本起的孙子进程也算自家树，
/// 不能只排除直接子进程 pid，否则会把自家后代误判成外部进程去弹二次确认。
fn process_tree(root: u32) -> HashSet<u32> {
    let mut tree = HashSet::new();
    tree.insert(root);
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return tree;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut ok = Process32FirstW(snapshot, &mut entry);
        while ok != 0 {
            children
                .entry(entry.th32ParentProcessID)
                .or_default()
                .push(entry.th32ProcessID);
            ok = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);

        let mut pending: Vec<u32> = vec![root];
        while let Some(pid) = pending.pop() {
            if let Some(kids) = children.get(&pid) {
                for &kid in kids {
                    if tree.insert(kid) {
                        pending.push(kid);
                    }
                }
            }
        }
    }
    tree
}

/// 端口有多个监听者、调用方又没指定 PID 时，挑出唯一“能确认是这台服务配置的程序”的那个。
/// 挑不出来就给一句人话错误，绝不动手。
fn pick_unique_owner(owners: &[u32], launch_exe: Option<&str>) -> Result<u32, String> {
    if owners.len() == 1 {
        return Ok(owners[0]);
    }
    let matched: Vec<u32> = match launch_exe {
        Some(configured) => owners
            .iter()
            .copied()
            .filter(|pid| {
                crate::local_ports::process_path(*pid)
                    .map(|exe| same_exe(configured, &exe))
                    .unwrap_or(false)
            })
            .collect(),
        None => Vec::new(),
    };
    pick_owner_by_matches(owners, launch_exe.is_some(), &matched)
}

/// [`pick_unique_owner`] 的纯选择逻辑：`owners` 是端口上全部监听者，`matched` 是其中
/// 能确认是配置程序的子集。不碰 netstat / OpenProcess，可以对合成数据单测
/// （matched = 0 / 1 / >1 三种分支）。
fn pick_owner_by_matches(
    owners: &[u32],
    has_configured_exe: bool,
    matched: &[u32],
) -> Result<u32, String> {
    if owners.len() == 1 {
        return Ok(owners[0]);
    }
    if !has_configured_exe {
        return Err(format!(
            "这个端口同时被多个进程监听（PID {}），分不清该停哪一个；为避免误杀，我没有动它们",
            join_pids(owners)
        ));
    }
    match matched.len() {
        0 => Err(format!(
            "这个端口同时被多个进程监听（PID {}），其中没有能确认是这台服务配置的程序；为避免误杀，我没有动它们",
            join_pids(owners)
        )),
        1 => Ok(matched[0]),
        _ => Err(format!(
            "这个端口有多个监听进程都像是这台服务配置的程序（PID {}），分不清该停哪一个；请手动处理",
            join_pids(matched)
        )),
    }
}

fn join_pids(pids: &[u32]) -> String {
    pids.iter()
        .map(|pid| pid.to_string())
        .collect::<Vec<_>>()
        .join("、")
}

/// 程序路径规范化：去首尾引号/空白，能 canonicalize 就 canonicalize，
/// 剥掉 `\\?\` 前缀，去掉尾部分隔符，用于忽略大小写的比较。
fn normalize_exe_path(path: &str) -> String {
    let raw = path.trim().trim_matches('"').trim();
    let canonical = std::fs::canonicalize(raw).unwrap_or_else(|_| std::path::PathBuf::from(raw));
    let mut text = canonical.to_string_lossy().to_string();
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        text = stripped.to_string();
    }
    text.trim_end_matches(['\\', '/']).to_string()
}

/// 配置的启动程序与端口上实际运行的 exe 是不是同一个（忽略大小写、规范化路径）。
/// 配置里只写了程序名（没有目录）时，按文件名比。
fn same_exe(configured: &str, running: &str) -> bool {
    let a = normalize_exe_path(configured);
    let b = normalize_exe_path(running);
    if a.eq_ignore_ascii_case(&b) {
        return true;
    }
    if !a.contains('\\') && !a.contains('/') {
        return file_name_of(&a).eq_ignore_ascii_case(&file_name_of(&b));
    }
    false
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// 结束一个已经确认过的外部进程。失败一律映射成人话错误。
fn kill_process(pid: u32) -> Result<(), String> {
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err(terminate_error_text(pid, GetLastError()));
        }
        let ok = TerminateProcess(handle, 1);
        let code = if ok == 0 { Some(GetLastError()) } else { None };
        CloseHandle(handle);
        match code {
            None => Ok(()),
            Some(code) => Err(terminate_error_text(pid, code)),
        }
    }
}

fn terminate_error_text(pid: u32, code: u32) -> String {
    match code {
        5 => format!(
            "结束 PID {pid} 失败：权限不够（它可能是管理员或系统启动的进程）；可以试试用管理员身份运行『上线助手』"
        ),
        87 | 299 => format!("结束 PID {pid} 失败：这个进程已经退出了"),
        _ => format!("结束 PID {pid} 失败（系统错误码 {code}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单测用的空接收器：不写盘、不打印。
    fn null_reporter() -> Arc<dyn Reporter> {
        Arc::new(crate::log::NullReporter)
    }

    #[test]
    fn exe_paths_compare_case_insensitively() {
        let me = std::env::current_exe().expect("current_exe");
        let text = me.to_string_lossy().to_string();
        assert!(same_exe(&text, &text));
        assert!(same_exe(&text.to_ascii_uppercase(), &text));
        assert!(same_exe(&format!("\"{text}\""), &text));
        assert!(!same_exe(r"C:\a\b.exe", r"C:\a\c.exe"));
        // 配置里只写程序名时按文件名比
        assert!(same_exe("cmd.exe", r"C:\Windows\System32\cmd.exe"));
        assert!(!same_exe("cmd.exe", r"C:\Windows\System32\powershell.exe"));
    }

    #[test]
    fn unique_owner_selection_covers_match_counts() {
        // 单 owner：不用比路径，直接选
        assert_eq!(pick_owner_by_matches(&[7], true, &[7]), Ok(7));
        assert_eq!(pick_owner_by_matches(&[7], false, &[]), Ok(7));
        // 多个 owner、恰好一个匹配配置程序 → 选它
        assert_eq!(pick_owner_by_matches(&[7, 9], true, &[9]), Ok(9));
        // 多个 owner、没有匹配 → 拒绝
        let err = pick_owner_by_matches(&[7, 9], true, &[]).unwrap_err();
        assert!(err.contains("没有能确认"), "err={err}");
        // 多个 owner、多个匹配 → 分不清，拒绝
        let err = pick_owner_by_matches(&[7, 9, 11], true, &[7, 11]).unwrap_err();
        assert!(err.contains("都像是"), "err={err}");
        // 没配程序：多个 owner 一律拒绝
        let err = pick_owner_by_matches(&[7, 9], false, &[]).unwrap_err();
        assert!(err.contains("分不清"), "err={err}");
    }

    #[test]
    fn process_tree_contains_its_root() {
        let me = std::process::id();
        let tree = process_tree(me);
        assert!(tree.contains(&me));
    }

    #[test]
    fn process_tree_includes_a_spawned_child() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/C", "ping", "127.0.0.1", "-n", "30", ">NUL"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn cmd.exe");
        let tree = process_tree(std::process::id());
        let child_pid = child.id();
        let included = tree.contains(&child_pid);
        let _ = child.kill();
        let _ = child.wait();
        assert!(included, "ToolHelp 父链应该能枚举出直接子进程 {child_pid}");
    }

    #[test]
    fn stop_outcome_serializes_with_kind_tag() {
        let value = serde_json::to_value(StopOutcome::Stopped { pid: None }).unwrap();
        assert_eq!(value, serde_json::json!({"kind": "stopped", "pid": null}));

        let value = serde_json::to_value(StopOutcome::NeedsConfirm {
            pid: 7,
            exe: Some(r"D:\x\a.exe".into()),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"kind": "needsConfirm", "pid": 7, "exe": r"D:\x\a.exe"})
        );

        let value = serde_json::to_value(StopOutcome::ForeignStopped { pid: 9 }).unwrap();
        assert_eq!(value, serde_json::json!({"kind": "foreignStopped", "pid": 9}));
    }

    #[test]
    fn check_tunnel_access_errors_are_plain_chinese() {
        let dir = std::env::temp_dir().join("sx-access-check-test");
        let _ = std::fs::remove_dir_all(&dir);

        // 找不到隧道
        let g = Guardian::new(
            AppConfig::default(),
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        let err = g.check_tunnel_access("tun-nope").unwrap_err();
        assert!(err.contains("找不到隧道"), "err={err}");

        // 隧道存在但没填网址 → 提醒先填网址（不去连服务器）
        let cfg = AppConfig {
            version: config::CONFIG_VERSION,
            servers: Vec::new(),
            services: Vec::new(),
            tunnels: vec![Tunnel {
                id: "tun-1".into(),
                name: "相册".into(),
                service_id: "svc-1".into(),
                server_id: "srv-1".into(),
                remote_port: 18080,
                url: None,
                key_path: None,
                access: Vec::new(),
                access_user: None,
                access_hash: None,
                access_detail: None,
                enabled: true,
                preset: config::DEFAULT_PRESET.to_string(),
            }],
            notify_enabled: true,
            settings: config::Settings::default(),
        };
        let g = Guardian::new(
            cfg,
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        let err = g.check_tunnel_access("tun-1").unwrap_err();
        assert!(err.contains("网址"), "err={err}");
    }

    #[test]
    fn normalize_domain_strips_scheme_path_and_spaces() {
        assert_eq!(normalize_domain("https://karryn.iranol1.xyz/x"), "karryn.iranol1.xyz");
        assert_eq!(normalize_domain("http://album.example.com"), "album.example.com");
        assert_eq!(normalize_domain(" album.example.com/y/z?q=1 "), "album.example.com");
        assert_eq!(normalize_domain("album.example.com 8080"), "album.example.com");
        assert_eq!(normalize_domain("HTTPS://Album.example.com/x"), "Album.example.com");
        assert_eq!(normalize_domain("/only-path"), "");
        assert_eq!(normalize_domain("karryn.iranol1.xyz"), "karryn.iranol1.xyz");
    }

    #[test]
    fn access_check_request_clones_connection_info() {
        let dir = std::env::temp_dir().join("sx-access-request-test");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = AppConfig {
            version: config::CONFIG_VERSION,
            servers: vec![ServerConfig {
                id: "srv-1".into(),
                name: "香港-1".into(),
                host: "karryn.iranol1.xyz".into(),
                ssh_port: 2222,
                user: "root".into(),
                key_path: Some("C:/k/id_ed25519".into()),
                vhost_dir: Some("/custom/vhost".into()),
            }],
            services: Vec::new(),
            tunnels: vec![Tunnel {
                id: "tun-1".into(),
                name: "相册".into(),
                service_id: "svc-1".into(),
                server_id: "srv-1".into(),
                remote_port: 18080,
                url: Some("  https://karryn.iranol1.xyz/album?q=1  ".into()),
                key_path: None,
                access: Vec::new(),
                access_user: None,
                access_hash: None,
                access_detail: None,
                enabled: true,
                preset: config::DEFAULT_PRESET.to_string(),
            }],
            notify_enabled: true,
            settings: config::Settings::default(),
        };
        let g = Guardian::new(
            cfg,
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        let req = g.access_check_request("tun-1").unwrap();
        assert_eq!(req.domain, "karryn.iranol1.xyz");
        assert_eq!(req.target, "root@karryn.iranol1.xyz");
        assert_eq!(req.ssh_port, 2222);
        assert_eq!(
            req.identity_file.as_deref(),
            Some(std::path::Path::new("C:/k/id_ed25519"))
        );
        // 服务器里填的站点目录要跟着请求走，检查云端才会找对地方
        assert_eq!(req.vhost_dir, "/custom/vhost");
    }

    #[test]
    #[ignore = "只读真机对照：需要能免密 SSH 到 api.iranol1.xyz；手动跑 --ignored"]
    fn real_machine_karryn_access_check() {
        // 真机对照（只读，不改服务器）：karryn.iranol1.xyz 的 conf 是
        // server 级 auth_basic "需要密码" + .well-known 里 auth_basic off/allow all，
        // 没有任何整站 allow/deny → passwordOn=true、ipOn=false、allowIps=[]。
        let req = AccessCheckRequest {
            target: "cgt@api.iranol1.xyz".into(),
            ssh_port: 22,
            identity_file: None,
            domain: "karryn.iranol1.xyz".into(),
            vhost_dir: "/xp/panel/vhost".into(),
        };
        let check = run_access_check(&req).expect("真机 SSH 只读核对失败");
        assert!(check.conf_found, "站点 conf 应该存在");
        assert!(check.htpasswd_exists, "htpasswd 应该存在");
        assert!(check.password_on, "server 级 auth_basic 应该算开密码");
        assert!(!check.ip_on, ".well-known 里的 allow all 不能算整站白名单");
        assert!(check.allow_ips.is_empty(), "没有整站白名单", );
    }

    #[test]
    fn access_check_script_wraps_conf_in_markers() {
        let script = access_check_script("album.example.com", "/xp/panel/vhost");
        assert!(script.starts_with("d='album.example.com';"));
        assert!(script.contains("dir='/xp/panel/vhost';"));
        assert!(script.contains("$dir/nginx/$d.conf"));
        assert!(script.contains("$dir/$d.htpasswd"));
        assert!(
            script.contains("===SHANGXIAN-CONF-BEGIN===")
                && script.contains("===SHANGXIAN-CONF-END==="),
            "script={script}"
        );
        assert!(script.contains("CONF_FOUND") && script.contains("CONF_MISSING"));
        // 顺序：BEGIN → cat 正文 → END → 结果标记
        let begin = script.find("===SHANGXIAN-CONF-BEGIN===").unwrap();
        let cat = script.find("cat \"$f\"").unwrap();
        let end = script.find("===SHANGXIAN-CONF-END===").unwrap();
        let found = script.find("CONF_FOUND").unwrap();
        assert!(begin < cat && cat < end && end < found, "script={script}");
        // 域名/目录里的单引号被转义，不会跑出 shell 命令
        let tricky = access_check_script("a'b", "/o' ops");
        assert!(tricky.starts_with(r#"d='a'\''b';"#), "script={tricky}");
        assert!(tricky.contains(r#"dir='/o'\'' ops';"#), "script={tricky}");
    }

    // ---------- 新逻辑的单元测试（不拉真进程） ----------

    /// 构造一份「一台服务器 + 一台本机服务 + 一条隧道」的测试配置。
    fn fingerprint_fixture() -> (AppConfig, Tunnel, LocalService) {
        let cfg = AppConfig {
            version: config::CONFIG_VERSION,
            servers: vec![ServerConfig {
                id: "srv-1".into(),
                name: "香港-1".into(),
                host: "a.example.com".into(),
                ssh_port: 22,
                user: "root".into(),
                key_path: Some("C:/k/id".into()),
                vhost_dir: None,
            }],
            services: vec![LocalService {
                id: "svc-1".into(),
                name: "相册".into(),
                local_port: 8080,
                probe_kind: config::DEFAULT_PROBE_KIND.to_string(),
                launch: None,
                watch: false,
                autostart: false,
                note: None,
            }],
            tunnels: Vec::new(),
            notify_enabled: true,
            settings: config::Settings::default(),
        };
        let tun = Tunnel {
            id: "tun-1".into(),
            name: "相册发布".into(),
            service_id: "svc-1".into(),
            server_id: "srv-1".into(),
            remote_port: 18080,
            url: None,
            key_path: None,
            access: Vec::new(),
            access_user: None,
            access_hash: None,
            access_detail: None,
            enabled: true,
            preset: config::DEFAULT_PRESET.to_string(),
        };
        let svc = cfg.service("svc-1").unwrap().clone();
        (cfg, tun, svc)
    }

    #[test]
    fn tunnel_fingerprint_changes_with_connection_params() {
        let (cfg, mut tun, svc) = fingerprint_fixture();
        let base = tunnel_fingerprint(&cfg, &tun, &svc);

        // 配置没动 → 指纹不变
        assert_eq!(base, tunnel_fingerprint(&cfg, &tun, &svc));

        // 云端端口变了
        tun.remote_port = 18081;
        assert_ne!(base, tunnel_fingerprint(&cfg, &tun, &svc));
        tun.remote_port = 18080;

        // 本机端口变了
        let mut svc2 = svc.clone();
        svc2.local_port = 8081;
        assert_ne!(base, tunnel_fingerprint(&cfg, &tun, &svc2));

        // 服务器地址 / SSH 端口 / 账号 / 密钥变了
        let mutations: Vec<fn(&mut AppConfig)> = vec![
            |c| c.servers[0].host = "b.example.com".into(),
            |c| c.servers[0].ssh_port = 2222,
            |c| c.servers[0].user = "ubuntu".into(),
            |c| c.servers[0].key_path = Some("C:/k/other".into()),
        ];
        for mutate in mutations {
            let mut cfg2 = cfg.clone();
            mutate(&mut cfg2);
            assert_ne!(base, tunnel_fingerprint(&cfg2, &tun, &svc));
        }

        // 改客户端端口以外的名字不算参数变化（日志文件按 id 命名）
        let before_name = tunnel_fingerprint(&cfg, &tun, &svc);
        tun.name = "新名字".into();
        assert_eq!(before_name, tunnel_fingerprint(&cfg, &tun, &svc));
        tun.name = "相册发布".into();

        // 隧道自己的密钥覆盖也要算进去
        tun.key_path = Some("C:/k/tunnel".into());
        assert_ne!(base, tunnel_fingerprint(&cfg, &tun, &svc));
    }

    #[test]
    fn sync_tunnels_rebuilds_when_params_change() {
        let dir = std::env::temp_dir().join("sx-guardian-rebuild-test");
        let _ = std::fs::remove_dir_all(&dir);
        let (mut cfg, tun, _) = fingerprint_fixture();
        cfg.tunnels.push(tun.clone());
        let mut g = Guardian::new(
            cfg,
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        g.sync_tunnels();
        assert!(g.tunnels.contains_key("tun-1"), "第一次同步应该建起隧道");

        // 没改参数：指纹不变
        let before = g.tunnel_fingerprints.get("tun-1").cloned().unwrap();
        g.sync_tunnels();
        assert_eq!(g.tunnel_fingerprints.get("tun-1").unwrap(), &before);

        // 改服务器地址 → 参数指纹变了 → 重建
        g.config.servers[0].host = "b.example.com".into();
        g.sync_tunnels();
        let after = g.tunnel_fingerprints.get("tun-1").cloned().unwrap();
        assert_ne!(before, after, "服务器地址变了，指纹必须跟着变");

        // 改本机端口 → 也要重建
        g.config.services[0].local_port = 8081;
        g.sync_tunnels();
        let after2 = g.tunnel_fingerprints.get("tun-1").cloned().unwrap();
        assert_ne!(after, after2, "本机端口变了，指纹必须跟着变");
    }

    #[test]
    fn exit_reason_from_log_picks_human_reason() {
        assert!(exit_reason_from_log("Permission denied (publickey).", Some(255))
            .unwrap()
            .contains("拒绝"));
        assert!(exit_reason_from_log("Host key verification failed.", Some(255))
            .unwrap()
            .contains("指纹"));
        assert!(exit_reason_from_log("ssh: Could not resolve hostname bad: ...", Some(255))
            .unwrap()
            .contains("解析"));
        assert!(exit_reason_from_log("connect to host x port 22: Connection refused", Some(255))
            .unwrap()
            .contains("拒绝连接"));
        assert!(
            exit_reason_from_log("remote port forwarding failed for listen port 18080", Some(255))
                .unwrap()
                .contains("占了")
        );
        assert!(exit_reason_from_log("everything is fine", None).is_none());
        assert!(exit_reason_from_log("everything is fine", Some(0)).is_none());
        // 退出码 255 但认不出关键字：给通用文案
        assert!(exit_reason_from_log("everything is fine", Some(255))
            .unwrap()
            .contains("这一轮没连上"));
    }

    #[test]
    fn read_tail_only_reads_file_end() {
        let dir = std::env::temp_dir().join("sx-read-tail-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");
        // 前面塞 200KB 垃圾（比尾部上限大），最后三行才是要看的
        let mut text = "x".repeat(200 * 1024);
        text.push('\n');
        text.push_str("第一行\n第二行\n第三行\n");
        std::fs::write(&path, text).unwrap();

        let tail = read_tail(&path, 3);
        assert_eq!(tail, "第一行\n第二行\n第三行", "tail={tail}");
        // 不存在的文件也给一句人话
        assert!(read_tail(&dir.join("nope.log"), 3).contains("还没有日志"));
    }

    #[test]
    fn guardian_refuses_save_when_config_was_bad() {
        let dir = std::env::temp_dir().join("sx-guardian-bad-config-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ 这不是 json").unwrap();
        // 读坏 → 留底成 config.json.bad，返回明确错误
        let _ = AppConfig::load(&path).unwrap_err();

        let mut g = Guardian::new(
            AppConfig::default(),
            path.clone(),
            dir.join("logs"),
            null_reporter(),
        );
        assert!(!g.config_trusted());
        let err = g.set_notify_enabled(false).unwrap_err();
        assert!(err.contains("config.json.bad"), "err={err}");
        // 内存没被改，磁盘上也没有新配置被写出来
        assert!(g.notify_enabled());
        assert!(!path.exists());
    }

    #[test]
    fn watch_and_autostart_need_a_launch_spec() {
        let dir = std::env::temp_dir().join("sx-guardian-launch-guard-test");
        let _ = std::fs::remove_dir_all(&dir);
        let mut g = Guardian::new(
            AppConfig::default(),
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        // 先建一台只监控的服务（没填启动程序）
        let id = g
            .save_service(ServiceInput {
                id: None,
                name: "只监控".into(),
                local_port: 18080,
                launch_exe: None,
                launch_args: None,
                launch_workdir: None,
                watch: false,
                autostart: false,
                note: None,
            })
            .unwrap();

        let err = g.set_service_watch(&id, true).unwrap_err();
        assert!(err.contains("启动程序"), "err={err}");
        let err = g.set_service_autostart(&id, true).unwrap_err();
        assert!(err.contains("启动程序"), "err={err}");
        // 关开关永远允许
        g.set_service_watch(&id, false).unwrap();
        g.set_service_autostart(&id, false).unwrap();

        // 直接通过编辑界面提交 watch=true、却把启动程序清空，也不行
        let err = g
            .save_service(ServiceInput {
                id: Some(id.clone()),
                name: "只监控".into(),
                local_port: 18080,
                launch_exe: Some("   ".into()),
                launch_args: None,
                launch_workdir: None,
                watch: true,
                autostart: false,
                note: None,
            })
            .unwrap_err();
        assert!(err.contains("启动程序"), "err={err}");
    }

    #[test]
    fn empty_server_name_defaults_to_host() {
        let dir = std::env::temp_dir().join("sx-guardian-server-name-test");
        let _ = std::fs::remove_dir_all(&dir);
        let mut g = Guardian::new(
            AppConfig::default(),
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        let id = g
            .save_server(ServerInput {
                id: None,
                name: "   ".into(),
                host: " api.example.com ".into(),
                ssh_port: 22,
                user: " root ".into(),
                key_path: None,
                vhost_dir: None,
            })
            .unwrap();
        let srv = g.servers().into_iter().find(|s| s.id == id).unwrap();
        assert_eq!(srv.name, "api.example.com");
        assert_eq!(srv.host, "api.example.com");
        assert_eq!(srv.user, "root");
    }

    // ---------- 唤醒 / 配置版本（D1 §1.5、§5.4） ----------

    fn guardian_with_fixture_tunnels(tag: &str, enabled: &[&str]) -> (Guardian, String) {
        let dir = std::env::temp_dir().join(format!("sx-guardian-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let (mut cfg, tun, _) = fingerprint_fixture();
        cfg.tunnels.push(tun);
        let mut second = cfg.tunnels[0].clone();
        second.id = "tun-2".into();
        second.name = "第二条".into();
        second.remote_port = 18081;
        cfg.tunnels.push(second);
        for t in &mut cfg.tunnels {
            t.enabled = enabled.contains(&t.id.as_str());
        }
        let g = Guardian::new(
            cfg,
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        (g, dir.display().to_string())
    }

    #[test]
    fn resume_rebuilds_enabled_only() {
        let (mut g, _dir) = guardian_with_fixture_tunnels("wake-resume", &["tun-1", "tun-2"]);
        g.sync_tunnels();
        assert_eq!(g.tunnels.len(), 2, "两条启用的隧道都建起来了");
        let affected = g.kick_reconnect(WakeKind::Resumed { slept_ms: 1000 });
        assert_eq!(affected, 2, "唤醒应该重建两条启用的隧道");
        assert_eq!(g.tunnels.len(), 2, "重建后两条都在");
        assert!(g.tunnels.contains_key("tun-1") && g.tunnels.contains_key("tun-2"));
    }

    #[test]
    fn wake_rate_limited() {
        let (mut g, _dir) = guardian_with_fixture_tunnels("wake-rate", &["tun-1"]);
        g.sync_tunnels();
        assert_eq!(g.kick_reconnect(WakeKind::Resumed { slept_ms: 1 }), 1);
        assert_eq!(
            g.kick_reconnect(WakeKind::Resumed { slept_ms: 1 }),
            0,
            "10 秒内同类系统事件只触发一次"
        );
        assert_eq!(g.kick_reconnect(WakeKind::Resumed { slept_ms: 1 }), 0);
        assert!(g.tunnels.contains_key("tun-1"));
    }

    #[test]
    fn config_generation_bumps_on_save() {
        let dir = std::env::temp_dir().join("sx-guardian-generation");
        let _ = std::fs::remove_dir_all(&dir);
        let mut g = Guardian::new(
            AppConfig::default(),
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        let before = g.config_generation();
        g.set_notify_enabled(false).unwrap();
        assert!(
            g.config_generation() > before,
            "commit_config 成功后版本号必须 +1"
        );
    }

    #[test]
    fn sync_skipped_when_generation_unchanged() {
        let (mut g, _dir) = guardian_with_fixture_tunnels("generation-skip", &["tun-1"]);
        g.sync_tunnels();
        g.synced_generation = g.config_generation;
        let before = g.tunnel_fingerprints.get("tun-1").cloned().unwrap();

        // 直接改内存配置（绕过 commit）：版本号没变，看护不能重同步
        g.config.servers[0].host = "b.example.com".into();
        if g.config_generation != g.synced_generation {
            g.sync_tunnels();
        }
        assert_eq!(
            g.tunnel_fingerprints.get("tun-1").unwrap(),
            &before,
            "版本号没变时不能重建隧道"
        );

        // 走一次真正的提交：版本号 +1 并立即按新参数重建
        let next = g.config.clone();
        g.commit_config(next).unwrap();
        let after = g.tunnel_fingerprints.get("tun-1").cloned().unwrap();
        assert_ne!(before, after, "配置提交后必须按新参数重建");
    }

    #[test]
    fn set_settings_validates_and_saves() {
        let dir = std::env::temp_dir().join("sx-guardian-settings");
        let _ = std::fs::remove_dir_all(&dir);
        let mut g = Guardian::new(
            AppConfig::default(),
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );

        let bad = serde_json::json!({
            "refreshIntervalMs": 100,
            "quietHours": { "enabled": false, "start": "22:00", "end": "08:00" },
            "checkUpdateOnStart": true
        });
        let err = g.set_settings(bad).unwrap_err();
        assert!(err.contains("500"), "err={err}");

        let missing = serde_json::json!({ "refreshIntervalMs": 800 });
        let err = g.set_settings(missing).unwrap_err();
        assert!(err.contains("设置项不完整"), "err={err}");

        let ok = serde_json::json!({
            "refreshIntervalMs": 1000,
            "quietHours": { "enabled": true, "start": "23:00", "end": "07:00" },
            "checkUpdateOnStart": false
        });
        let saved = g.set_settings(ok).unwrap();
        assert_eq!(saved.refresh_interval_ms, 1000);
        assert!(saved.quiet_hours.enabled);
        assert_eq!(g.settings().quiet_hours.start, "23:00");
        assert!(!g.settings().check_update_on_start);

        // 免打扰时段也走同一条校验
        let err = g
            .set_notify_quiet(QuietHours {
                enabled: true,
                start: "25:00".into(),
                end: "07:00".into(),
            })
            .unwrap_err();
        assert!(err.contains("22:00"), "err={err}");
    }

    #[test]
    fn tunnel_view_serializes_v2_fields() {
        let (mut g, _dir) = guardian_with_fixture_tunnels("view-v2", &["tun-1"]);
        g.sync_tunnels();
        let view = g.tunnel_snapshot().remove(0);
        let value = serde_json::to_value(&view).unwrap();
        for key in [
            "reasonCode",
            "retryState",
            "attempt",
            "retryInSecs",
            "connectedSince",
            "uptimeSecs",
            "reconnectsToday",
            "lastFailure",
            "protectionCheck",
            "preset",
        ] {
            assert!(value.get(key).is_some(), "快照缺字段 {key}: {value}");
        }
        assert_eq!(value["retryState"], "active");
        assert_eq!(value["preset"], "web");
        assert_eq!(value["attempt"], 0);
        assert_eq!(value["uptimeSecs"], 0);
        assert_eq!(value["protectionCheck"]["status"], "unknown");

        let svc = g.snapshot().remove(0);
        let svalue = serde_json::to_value(&svc).unwrap();
        for key in ["health", "logOverflowed", "lastFailure"] {
            assert!(svalue.get(key).is_some(), "服务快照缺字段 {key}: {svalue}");
        }
    }

    #[test]
    fn protection_check_cleared_only_on_relevant_change() {
        let dir = std::env::temp_dir().join("sx-guardian-protection-clear");
        let _ = std::fs::remove_dir_all(&dir);
        let state = Arc::new(crate::state::StateStore::load(&dir));
        let (mut g, _dir2) = guardian_with_fixture_tunnels("protection-clear", &["tun-1"]);
        g.attach_state(state.clone());
        g.sync_tunnels();

        let mut check = ProtectionCheck::default();
        check.status = "ok".into();
        check.ts = Some(123);
        check.source = Some("manual".into());
        state.set_protection_check("tun-1", check);
        assert!(state
            .tunnel_state("tun-1")
            .and_then(|t| t.protection_check)
            .is_some());

        // 起停 / 改名：与保护无关，不清核对结果
        g.set_enabled("tun-1", false).unwrap();
        assert!(
            state
                .tunnel_state("tun-1")
                .and_then(|t| t.protection_check)
                .is_some(),
            "起停不该清掉核对结果"
        );
        g.set_enabled("tun-1", true).unwrap();

        // 改访问保护：清成 unknown
        g.update_tunnel(UpdateTunnelInput {
            id: "tun-1".into(),
            name: "相册发布".into(),
            remote_port: 18080,
            url: Some("album.example.com".into()),
            access: Some(vec!["password".into()]),
            access_user: Some("u".into()),
            access_hash: Some("$2y$12$x".into()),
            access_detail: None,
            preset: None,
        })
        .unwrap();
        assert!(
            state
                .tunnel_state("tun-1")
                .and_then(|t| t.protection_check)
                .is_none(),
            "访问保护变了，旧核对结果必须清掉"
        );
    }

    #[test]
    fn delete_and_restore_round_trip_with_state() {
        let dir = std::env::temp_dir().join("sx-guardian-undo");
        let _ = std::fs::remove_dir_all(&dir);
        let state = Arc::new(crate::state::StateStore::load(&dir));
        let (mut g, _dir2) = guardian_with_fixture_tunnels("undo", &["tun-1"]);
        g.attach_state(state.clone());
        g.sync_tunnels();

        let info = g.remove_tunnel("tun-1").unwrap();
        assert_eq!(info.id, "tun-1");
        assert!(info.token.starts_with("und-"), "token={}", info.token);
        assert!(info.expires_at > 0);
        assert!(g.config.tunnel("tun-1").is_none());
        assert!(state.peek_trash(&info.token).is_some());

        let restored = g.restore_deleted(&info.token).unwrap();
        assert_eq!(restored.kind, "tunnel");
        assert_eq!(restored.id, "tun-1");
        assert!(g.config.tunnel("tun-1").is_some());
        assert!(!g.config.tunnel("tun-1").unwrap().enabled, "恢复的隧道必须暂停");
        assert!(state.peek_trash(&info.token).is_none(), "撤销后墓碑要取走");
    }

    // ---------- F1 修复回归：preset 写入口 / 运行态停一切 / 事件 / 站点目录第二层 / server id ----------

    fn new_tunnel_input(name: &str, remote_port: u16, preset: Option<&str>) -> NewTunnelInput {
        NewTunnelInput {
            name: name.into(),
            service_id: "svc-1".into(),
            server_id: "srv-1".into(),
            remote_port,
            url: None,
            access: Vec::new(),
            access_user: None,
            access_hash: None,
            access_detail: None,
            preset: preset.map(str::to_string),
        }
    }

    fn update_tunnel_input(
        id: &str,
        name: &str,
        remote_port: u16,
        preset: Option<&str>,
    ) -> UpdateTunnelInput {
        UpdateTunnelInput {
            id: id.into(),
            name: name.into(),
            remote_port,
            url: None,
            access: None,
            access_user: None,
            access_hash: None,
            access_detail: None,
            preset: preset.map(str::to_string),
        }
    }

    /// ① preset 写入口：add 缺省 web、add/update 显式值都写进去、非法值固定文案拒绝。
    #[test]
    fn add_and_update_tunnel_write_preset() {
        let dir = std::env::temp_dir().join("sx-guardian-preset-write");
        let _ = std::fs::remove_dir_all(&dir);
        let (cfg, _, _) = fingerprint_fixture();
        let mut g = Guardian::new(
            cfg,
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );

        // add：缺省 web
        let id_web = g.add_tunnel(new_tunnel_input("默认", 18081, None)).unwrap();
        assert_eq!(g.config.tunnel(&id_web).unwrap().preset, "web");
        // add：显式 ws
        let id_ws = g
            .add_tunnel(new_tunnel_input("长连接", 18082, Some("ws")))
            .unwrap();
        assert_eq!(g.config.tunnel(&id_ws).unwrap().preset, "ws");
        // add：非法值固定文案拒绝，配置不动
        let err = g
            .add_tunnel(new_tunnel_input("乱写", 18083, Some("wat")))
            .unwrap_err();
        assert_eq!(err, "发布预设只支持 web / ws / upload 三种");
        assert_eq!(g.config.tunnels.len(), 2);

        // update：不给 preset = 保持
        g.update_tunnel(update_tunnel_input(&id_web, "默认", 18081, None))
            .unwrap();
        assert_eq!(g.config.tunnel(&id_web).unwrap().preset, "web");
        // update：给了才改
        g.update_tunnel(update_tunnel_input(&id_web, "默认", 18081, Some("upload")))
            .unwrap();
        assert_eq!(g.config.tunnel(&id_web).unwrap().preset, "upload");
        // update：非法值固定文案拒绝，保持原值
        let err = g
            .update_tunnel(update_tunnel_input(&id_web, "默认", 18081, Some("wat")))
            .unwrap_err();
        assert_eq!(err, "发布预设只支持 web / ws / upload 三种");
        assert_eq!(g.config.tunnel(&id_web).unwrap().preset, "upload");
    }

    /// ② 运行态停一切：停进程、不动 enabled、不落盘；再来一次不重复计数。
    #[test]
    fn stop_all_runtime_stops_without_writing_config() {
        let dir = std::env::temp_dir().join("sx-guardian-stop-all-runtime");
        let _ = std::fs::remove_dir_all(&dir);
        let (mut cfg, tun, _) = fingerprint_fixture();
        cfg.tunnels.push(tun);
        // 拿一个空闲端口，避开真机可能已经占着的 8080
        let free_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        cfg.services[0].local_port = free_port;
        cfg.services[0].launch = Some(LaunchSpec {
            exe: "cmd.exe".into(),
            args: "/C ping 127.0.0.1 -n 30 >NUL".into(),
            workdir: None,
        });
        let config_path = dir.join("config.json");
        let mut g = Guardian::new(
            cfg,
            config_path.clone(),
            dir.join("logs"),
            null_reporter(),
        );
        g.sync_tunnels();
        g.start_service("svc-1").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while g.local.get("svc-1").and_then(|runner| runner.pid()).is_none() {
            assert!(Instant::now() < deadline, "测试服务没起来（cmd.exe 启动失败？）");
            g.local.get_mut("svc-1").unwrap().poll();
            std::thread::sleep(Duration::from_millis(20));
        }

        let generation = g.config_generation();
        let report = g.stop_all_runtime();
        assert_eq!(report.tunnels_stopped, 1, "运行中的隧道要停掉");
        assert_eq!(report.tunnels_failed, 0);
        assert_eq!(report.services_stopped, 1, "看护拉起的自家服务要停掉");
        assert_eq!(report.services_failed, 0);
        assert_eq!(
            g.config_generation(),
            generation,
            "stop_all_runtime 不能 commit 配置"
        );
        assert!(
            g.config.tunnels.iter().all(|t| t.enabled),
            "配置里的 enabled 不能被改"
        );
        assert!(!g.tunnels.contains_key("tun-1"), "运行表里不该还有它");
        assert!(
            g.local.get("svc-1").and_then(|runner| runner.pid()).is_none(),
            "自家进程要停掉"
        );
        assert!(!config_path.exists(), "不该写 config.json");

        // 同一个会话里再调一次：没有运行中的了，不能重复计数
        let again = g.stop_all_runtime();
        assert_eq!(again.tunnels_stopped, 0);
        assert_eq!(again.services_stopped, 0);
    }

    /// ③ configSaved / Deleted / Restored 都要有主体和固定文案。
    #[test]
    fn commit_delete_restore_append_events() {
        let dir = std::env::temp_dir().join("sx-guardian-f1-events");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = Arc::new(crate::state::StateStore::load(&dir));
        let events = Arc::new(EventLog::new(&dir));
        let (mut g, _dir) = guardian_with_fixture_tunnels("f1-events-g", &["tun-1"]);
        g.attach_state(state.clone());
        g.attach_events(events.clone());
        g.sync_tunnels();

        // configSaved：任何一次配置落盘成功都要有
        g.set_notify_enabled(false).unwrap();
        let saved = events
            .read(50, 0)
            .into_iter()
            .find(|line| line.code.as_deref() == Some("configSaved"))
            .expect("应该有 configSaved 事件");
        assert_eq!(saved.kind, "app");
        assert_eq!(saved.level, LEVEL_INFO);
        assert_eq!(saved.subject.as_ref().map(|s| s.r#type.as_str()), Some("app"));
        assert_eq!(saved.message, "设置已保存");

        // Deleted（隧道）：subject 要带实体名
        let info = g.remove_tunnel("tun-1").unwrap();
        let deleted = events
            .read(50, 0)
            .into_iter()
            .rev()
            .find(|line| line.code.as_deref() == Some("deleted") && line.kind == "tunnel")
            .expect("删隧道应该有 deleted 事件");
        let subject = deleted.subject.clone().expect("事件要有 subject");
        assert_eq!(subject.r#type, "tunnel");
        assert_eq!(subject.id, "tun-1");
        assert_eq!(subject.name, "相册发布");
        assert!(deleted.message.contains("相册发布"), "message={}", deleted.message);

        // Restored（隧道）
        g.restore_deleted(&info.token).unwrap();
        let restored = events
            .read(50, 0)
            .into_iter()
            .rev()
            .find(|line| line.code.as_deref() == Some("restored"))
            .expect("撤销应该有 restored 事件");
        assert_eq!(restored.kind, "tunnel");
        assert_eq!(restored.subject.as_ref().unwrap().id, "tun-1");

        // Deleted（服务 / 服务器）
        let svc_id = g
            .save_service(ServiceInput {
                id: None,
                name: "临时服务".into(),
                local_port: 8123,
                launch_exe: None,
                launch_args: None,
                launch_workdir: None,
                watch: false,
                autostart: false,
                note: None,
            })
            .unwrap();
        g.remove_service(&svc_id).unwrap();
        let svc_deleted = events
            .read(100, 0)
            .into_iter()
            .rev()
            .find(|line| line.code.as_deref() == Some("deleted") && line.kind == "service")
            .expect("删服务应该有 deleted 事件");
        assert_eq!(svc_deleted.subject.as_ref().unwrap().name, "临时服务");

        let srv_id = g
            .save_server(ServerInput {
                id: None,
                name: "临时服务器".into(),
                host: "tmp.example.com".into(),
                ssh_port: 22,
                user: "root".into(),
                key_path: None,
                vhost_dir: None,
            })
            .unwrap();
        g.delete_server(&srv_id).unwrap();
        let srv_deleted = events
            .read(100, 0)
            .into_iter()
            .rev()
            .find(|line| line.code.as_deref() == Some("deleted") && line.kind == "app")
            .expect("删服务器应该有 deleted 事件");
        let subject = srv_deleted.subject.as_ref().unwrap();
        assert_eq!(subject.r#type, "server");
        assert_eq!(subject.name, "临时服务器");
    }

    /// ④ 站点目录第二层：核对请求和 nginx 片段都用 state 里的识别结果；没缓存时与旧行为一致。
    #[test]
    fn detected_vhost_dir_wires_check_and_snippet() {
        let dir = std::env::temp_dir().join("sx-guardian-vhost-layer2");
        let _ = std::fs::remove_dir_all(&dir);
        let state = Arc::new(crate::state::StateStore::load(&dir));
        let (mut cfg, mut tun, _) = fingerprint_fixture();
        tun.url = Some("album.example.com".into());
        tun.access = vec!["password".into()];
        tun.access_user = Some("u".into());
        tun.access_hash = Some("$2y$12$x".into());
        tun.preset = "ws".into();
        let tun_id = tun.id.clone();
        cfg.tunnels.push(tun);
        let mut g = Guardian::new(
            cfg,
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        g.attach_state(state.clone());

        // 没有缓存：和旧行为一致（旧代码就是 htpasswd_dir_for(server)）
        {
            let server = g
                .config
                .servers
                .iter()
                .find(|s| s.id == "srv-1")
                .unwrap()
                .clone();
            let req = g.access_check_request(&tun_id).unwrap();
            assert_eq!(req.vhost_dir, crate::nginx::htpasswd_dir_for(&server));
        }

        // 环境识别缓存到了：核对与片段都用第二层的目录 + WS_MAP
        state.set_server_env(
            "srv-1",
            serde_json::json!({
                "panel": "bt",
                "web": "nginx",
                "panelConfidence": "high",
                "webConfidence": "high",
                "vhostDir": "/www/server/panel/vhost",
                "wsMap": true
            }),
        );
        let req = g.access_check_request(&tun_id).unwrap();
        assert_eq!(req.vhost_dir, "/www/server/panel/vhost");

        let snip = g.nginx_snippet(&tun_id).unwrap();
        assert!(
            snip.steps
                .iter()
                .any(|s| s.contains("/www/server/panel/vhost")),
            "steps={:?}",
            snip.steps
        );
        assert!(
            snip.preview
                .iter()
                .any(|line| line.text.contains("$connection_upgrade")),
            "wsMap=true 时预设应该优先用 $connection_upgrade"
        );

        // 服务器缓存没了时：WS_MAP 还能从上次检查结果 extra.env 兜回来；
        // 同时 extra.wellknownPresent=false → 续期片段要带整块提示
        state.remove_server_env("srv-1");
        let mut check = ProtectionCheck::default();
        check.status = "ok".into();
        check.ts = Some(1);
        check.source = Some("manual".into());
        check.extra = Some(serde_json::json!({
            "wellknownPresent": false,
            "env": {
                "panel": "bt",
                "web": "nginx",
                "panelConfidence": "high",
                "webConfidence": "high",
                "wsMap": true
            }
        }));
        state.set_protection_check(&tun_id, check);
        let snip = g.nginx_snippet(&tun_id).unwrap();
        assert!(
            snip.preview
                .iter()
                .any(|line| line.text.contains("$connection_upgrade")),
            "缓存没了也要能从上次检查结果 extra.env 兜回 WS_MAP"
        );
        let acme = snip.acme_exempt.expect("有密码保护就有续期片段");
        assert!(
            acme.contains("没有 /.well-known/") && acme.contains("location /.well-known/ {"),
            "acme={acme}"
        );
    }

    /// ⑤ server id 防路径穿越：save_server / 撤销入口都拒绝非法 id。
    #[test]
    fn server_id_rejects_path_traversal() {
        let dir = std::env::temp_dir().join("sx-guardian-server-id");
        let _ = std::fs::remove_dir_all(&dir);
        let state = Arc::new(crate::state::StateStore::load(&dir));
        let mut g = Guardian::new(
            AppConfig::default(),
            dir.join("config.json"),
            dir.join("logs"),
            null_reporter(),
        );
        g.attach_state(state.clone());

        let bad = ServerInput {
            id: Some(r"..\..\evil".into()),
            name: "坏".into(),
            host: "h.example.com".into(),
            ssh_port: 22,
            user: "root".into(),
            key_path: None,
            vhost_dir: None,
        };
        let err = g.save_server(bad.clone()).unwrap_err();
        assert!(err.contains("不合法"), "err={err}");
        assert!(g.servers().is_empty(), "非法 id 不能落进配置");

        let too_long = ServerInput {
            id: Some("a".repeat(65)),
            ..bad.clone()
        };
        let err = g.save_server(too_long).unwrap_err();
        assert!(err.contains("不合法"), "err={err}");

        // 合法 id 照常可用
        let id = g
            .save_server(ServerInput {
                id: Some("srv-ok_1".into()),
                ..bad.clone()
            })
            .unwrap();
        assert_eq!(id, "srv-ok_1");

        // 撤销路径也挡：手工做的墓碑 id 带路径分隔符不能插回配置
        let item = serde_json::json!({
            "id": r"..\evil",
            "name": "坏",
            "host": "h.example.com",
            "sshPort": 22,
            "user": "root",
            "vhostDir": null,
            "keyPath": null
        });
        let record =
            crate::state::new_trash("server", item, BTreeMap::new(), crate::state::now_ms());
        let token = record.token.clone();
        state.push_trash(record);
        let err = g.restore_deleted(&token).unwrap_err();
        assert!(err.contains("不合法"), "err={err}");
    }
}
