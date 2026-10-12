//! 单条隧道的看护状态机。
//!
//! 行为（对齐 DESIGN.md §3.3「重连守护」与 D1 §9「重试上限与指纹重置」）：
//! - 启动：spawn ssh.exe → 放进独立作业对象（防孤儿）。
//! - 退出检测：轮询 `try_wait()`；退出后按退避表安排重试。
//! - 退避：2s 起步，翻倍，封顶 30s；跑满 `stable_after`（默认 60s）后重置为 2s。
//! - 站稳过一次（`stable_once`）→ 永久重试（7×24：断网几小时也要自己回来）；
//!   从没站稳（地址写错、认证错、远端端口被占）→ 连续失败 `MAX_ATTEMPTS_BEFORE_STOP`
//!   次后 `retryState="stopped"`、`phase=GaveUp`，停下报信，等用户「再试一次」。
//! - 停止：杀子进程 + 释放作业（子树一起走）。
//!
//! 注意：这是轮询式状态机，调用方（守护循环 / 后续的 tokio 任务）负责周期调用 `poll()`。
//! 单个隧道绝不碰别的隧道：没有全局 kill、没有按名字杀——清理只在远端按端口做（后续模块）。

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::job::Job;
use crate::log::{LogLevel, Reporter, TAG_TUNNEL};
use crate::logfile::{rotate_if_needed, SERVICE_LOG_BACKUPS, SERVICE_LOG_MAX_BYTES};
use crate::reason::ReasonCode;
use crate::wake::WakeKind;

/// 创建进程时不弹控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一条隧道的启动参数。
#[derive(Debug, Clone)]
pub struct TunnelSpec {
    /// 稳定标识（隧道 id）：日志文件名用它，改名不换日志文件。
    /// 空 = 没传，退回用名字（老调用方还能跑）。
    pub id: String,
    /// 展示用名字（服务名）。
    pub name: String,
    /// ssh.exe 路径（固定系统路径）。
    pub ssh_exe: PathBuf,
    /// ssh 参数（由 `ssh::tunnel_args` 生成）。
    pub args: Vec<String>,
    /// 退避起点。
    pub retry_base: Duration,
    /// 退避上限。
    pub retry_max: Duration,
    /// 连续存活超过它，认为本次"站稳了"，退避重置。
    pub stable_after: Duration,
    /// 远端残留清理参数（None = 不清理）
    pub cleanup: Option<crate::cleanup::CleanupSpec>,
}

impl TunnelSpec {
    pub fn new(name: impl Into<String>, ssh_exe: PathBuf, args: Vec<String>) -> Self {
        Self {
            id: String::new(),
            name: name.into(),
            ssh_exe,
            args,
            retry_base: Duration::from_secs(2),
            retry_max: Duration::from_secs(30),
            stable_after: Duration::from_secs(60),
            cleanup: None,
        }
    }

    /// 附带远端清理参数（链式调用）。
    pub fn with_cleanup(mut self, spec: crate::cleanup::CleanupSpec) -> Self {
        self.cleanup = Some(spec);
        self
    }

    /// 指定隧道 id：日志文件按 id 命名，改名不换日志文件（链式调用）。
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }
}

/// 隧道状态（给界面展示用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// 还没开始过。
    Idle,
    /// 正在跑。
    Running { since: Instant },
    /// 已断开，等待重试。
    Backoff { until: Instant },
    /// 已停止（用户操作）。
    Stopped,
    /// 连续失败到上限，不再自动重试，等用户「再试一次」。
    GaveUp,
}

/// 连续失败到这么多次（且从没站稳过）就不再自动重试。
/// 退避 2→30 秒，20 次约 8.5 分钟（D1 §9.1）。
pub const MAX_ATTEMPTS_BEFORE_STOP: u32 = 20;

/// 重试状态：只有两个值，界面按它决定显示「重连中」还是「出错 + 再试一次」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RetryState {
    /// 看护会继续自动重连（含已连、连接中、退避等待）。
    Active,
    /// 用户暂停 / 连续失败已停手，不会自动重试。
    Stopped,
}

/// 最近一次失败（给界面看；判定一律用 `Instant`，这里只留墙钟做展示）。
#[derive(Debug, Clone)]
pub struct LastFailure {
    /// 给人看的墙钟（epoch ms）。
    pub ts_ms: u64,
    pub code: ReasonCode,
    pub reason: String,
}

/// 当前墙钟毫秒（只做展示）。
fn now_wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 状态机产生的事件（日志 / 通知 / 界面刷新用）。
#[derive(Debug, Clone)]
pub enum Event {
    Started {
        attempt: u32,
        pid: u32,
    },
    /// 本次进程退出，以及多久后重试。
    Exited {
        attempt: u32,
        exit_code: Option<u32>,
        uptime: Duration,
        retry_in: Duration,
        reset_backoff: bool,
        /// 这次退出的原因码（认不出是 `Unknown`；连续失败停手是 `RetryExhausted`）。
        code: ReasonCode,
    },
    /// 启动失败（例如 ssh.exe 找不到）。
    SpawnFailed {
        error: String,
        retry_in: Duration,
        code: ReasonCode,
    },
    /// 系统级事件（电脑刚醒 / 网络恢复 / 时钟跳变）：guardian 调 `kick_reconnect` 后
    /// 冒泡给界面顶栏用。`affected` 是受影响的隧道条数。
    SystemWake { kind: WakeKind, affected: usize },
    /// 远端残留清理结束（`ok=false` 时 guardian 记一条 cleanupFail 事件）。
    CleanupFinished { ok: bool, message: String },
}

/// 单条隧道的看护器。
pub struct Tunnel {
    spec: TunnelSpec,
    log_dir: PathBuf,
    child: Option<Child>,
    /// 作业对象：Drop 时系统结束里面的一切（保底清理）。
    job: Option<Job>,
    attempt: u32,
    backoff: Duration,
    started_at: Option<Instant>,
    retry_at: Option<Instant>,
    first_spawned: bool,
    /// 曾经连上并稳定存活过（用来区分"连接中"和"已断开"）
    ever_connected: bool,
    /// 最近一次启动就失败（例如 ssh.exe 找不到）
    spawn_failed: bool,
    /// 本次连接是否真的建立（看到 ssh 的 "remote forward success" 才算）
    connected: bool,
    /// 本次连接建立的时刻 / 墙钟（未连接为 None）
    connected_at: Option<Instant>,
    connected_at_wall: Option<u64>,
    /// 曾经站稳过（单次跑满 stable_after）：永久重试，不再 20 次停手。
    stable_once: bool,
    /// 重试状态（active / stopped）
    retry_state: RetryState,
    /// 最近一次失败原因码；没失败是 None（JSON 里输出 null）。
    reason_code: Option<ReasonCode>,
    /// 最近一次失败（重启不失忆，连上后清空）
    last_failure: Option<LastFailure>,
    /// 本次实例建立过多少次连接（guardian 用它累计「今天重连几次」）
    reconnects_total: u32,
    /// 本次启动时日志文件的大小（只扫这之后的新内容）
    log_scan_from: u64,
    /// 需要做一次远端端口清理
    cleanup_pending: bool,
    /// 正在跑的清理任务
    cleanup_rx: Option<std::sync::mpsc::Receiver<crate::cleanup::CleanupOutcome>>,
    /// 上次清理时间（限流：每分钟最多一次）
    last_cleanup_at: Option<Instant>,
    /// app.log 接收器（启动失败、作业分配失败等降级点往里写）
    reporter: Arc<dyn Reporter>,
    phase: Phase,
}

impl Tunnel {
    pub fn new(
        spec: TunnelSpec,
        log_dir: impl Into<PathBuf>,
        reporter: Arc<dyn Reporter>,
    ) -> Self {
        let backoff = spec.retry_base;
        Self {
            spec,
            log_dir: log_dir.into(),
            child: None,
            job: None,
            attempt: 0,
            backoff,
            started_at: None,
            retry_at: None,
            first_spawned: false,
            ever_connected: false,
            spawn_failed: false,
            connected: false,
            connected_at: None,
            connected_at_wall: None,
            stable_once: false,
            retry_state: RetryState::Active,
            reason_code: None,
            last_failure: None,
            reconnects_total: 0,
            log_scan_from: 0,
            cleanup_pending: false,
            cleanup_rx: None,
            last_cleanup_at: None,
            reporter,
            phase: Phase::Idle,
        }
    }

    pub fn name(&self) -> &str {
        &self.spec.name
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// 曾经连上并稳定存活过。
    pub fn ever_connected(&self) -> bool {
        self.ever_connected
    }

    /// 本次连接是否真的建立（远端端口已经绑上）。
    pub fn connected(&self) -> bool {
        self.connected
    }

    /// 最近一次是"启动就失败"（区别于连上后断开）。
    pub fn spawn_failed(&self) -> bool {
        self.spawn_failed
    }

    /// 曾经站稳过（单次跑满 `stable_after`）：这种隧道永久重试，不再 20 次停手。
    pub fn stable_once(&self) -> bool {
        self.stable_once
    }

    /// 重试状态（active / stopped）。
    pub fn retry_state(&self) -> RetryState {
        self.retry_state
    }

    /// 最近一次失败原因码（没失败是 None，JSON 里输出 null）。
    pub fn reason_code(&self) -> Option<ReasonCode> {
        self.reason_code
    }

    /// 距离下次重试的秒数（向上取整）；不在退避或已停手时为 None。
    pub fn retry_in_secs(&self) -> Option<u64> {
        match (&self.phase, self.retry_state) {
            (Phase::Backoff { until }, RetryState::Active) => {
                let left = until.saturating_duration_since(Instant::now());
                let secs = left.as_secs() + if left.subsec_millis() > 0 { 1 } else { 0 };
                Some(secs)
            }
            _ => None,
        }
    }

    /// 当前这次连接建立的时刻（未连接是 None）。
    pub fn connected_since(&self) -> Option<Instant> {
        if self.connected && self.child.is_some() {
            self.connected_at
        } else {
            None
        }
    }

    /// 当前这次连接建立的墙钟（epoch ms，只做展示）。
    pub fn connected_since_wall_ms(&self) -> Option<u64> {
        if self.connected && self.child.is_some() {
            self.connected_at_wall
        } else {
            None
        }
    }

    /// 当前连接已经持续多少秒（未连接是 None）。
    pub fn uptime_secs(&self) -> Option<u64> {
        self.connected_since().map(|t| t.elapsed().as_secs())
    }

    /// 最近一次失败（连上后清空）。
    pub fn last_failure(&self) -> Option<&LastFailure> {
        self.last_failure.as_ref()
    }

    /// 本次实例一共连上过几次（guardian 用它累计「今天重连几次」）。
    pub fn reconnects_total(&self) -> u32 {
        self.reconnects_total
    }

    /// 系统事件（网络恢复 / 时钟跳变）时把退避提前到「现在」，让这条隧道立刻重试。
    /// 只对正在退避的隧道生效；返回是否真的调整了。
    pub fn kick_retry(&mut self) -> bool {
        if matches!(self.phase, Phase::Backoff { .. }) {
            let now = Instant::now();
            self.retry_at = Some(now);
            self.phase = Phase::Backoff { until: now };
            self.log_line("[看护] 收到系统事件，退避提前到现在");
            true
        } else {
            false
        }
    }

    /// 「再试一次」：清失败计数/原因/退避，回到 Idle，下一轮立刻重连。
    /// `stable_once` 不在这里清——它只在实例重建时重新考核（D1 §9.2）。
    pub fn reset_for_retry(&mut self) {
        self.attempt = 0;
        self.backoff = self.spec.retry_base;
        self.retry_at = None;
        self.retry_state = RetryState::Active;
        self.reason_code = None;
        self.last_failure = None;
        self.spawn_failed = false;
        self.first_spawned = false;
        self.phase = Phase::Idle;
    }

    /// 从没站稳过，且连续失败到上限 → 该停手了。
    fn should_give_up(&self) -> bool {
        !self.stable_once && self.attempt >= MAX_ATTEMPTS_BEFORE_STOP
    }

    /// 停手：不再自动重试，等用户「再试一次」。
    fn give_up(&mut self) {
        self.retry_at = None;
        self.retry_state = RetryState::Stopped;
        self.reason_code = Some(ReasonCode::RetryStopped);
        self.last_failure = Some(LastFailure {
            ts_ms: now_wall_ms(),
            code: ReasonCode::RetryStopped,
            reason: ReasonCode::RetryStopped.text().to_string(),
        });
        self.phase = Phase::GaveUp;
        let text = format!("隧道「{}」连续失败到上限，已经停止自动重试", self.spec.name);
        self.log_line("[看护] 连续失败到上限，停止自动重试（等用户点「再试一次」）");
        self.reporter.report(LogLevel::Warn, TAG_TUNNEL, &text);
    }

    /// 记一次失败（原因文案用原因码的默认大白话）。
    fn note_failure(&mut self, code: ReasonCode) {
        self.note_failure_text(code, code.text().to_string());
    }

    /// 记一次失败（带具体细节的文案）。
    fn note_failure_text(&mut self, code: ReasonCode, reason: String) {
        self.reason_code = Some(code);
        self.last_failure = Some(LastFailure {
            ts_ms: now_wall_ms(),
            code,
            reason,
        });
    }

    /// 周期调用（建议 100–250ms）：推进状态机并返回发生的事件。
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if matches!(self.phase, Phase::Stopped | Phase::GaveUp) {
            return events;
        }

        // 1) 有子进程：检查是否退出。
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let uptime = self.started_at.map(|t| t.elapsed()).unwrap_or_default();
                    let reset_backoff = uptime >= self.spec.stable_after;
                    let previous_attempt = self.attempt;
                    let exit_code = status.code().map(|c| c as u32);

                    self.child = None;
                    self.job = None; // 句柄关闭；进程已退出，作业里没有活物

                    let tail = self.read_log_tail();
                    // 若退出原因是"远端端口被占"，安排一次精确清理（每分钟最多一次）
                    if self.spec.cleanup.is_some() && tail.contains("remote port forwarding failed") {
                        let due = self
                            .last_cleanup_at
                            .map(|t| t.elapsed() >= Duration::from_secs(60))
                            .unwrap_or(true);
                        if due {
                            self.cleanup_pending = true;
                        }
                    }

                    // 连上过、但一次次活不到 60 秒 = 抖动（D2 里对应 connReset，不是自创码）；
                    // 否则按日志/退出码认原因。
                    let code = if self.connected && uptime < self.spec.stable_after {
                        ReasonCode::ConnReset
                    } else {
                        classify_exit(&tail, exit_code)
                    };
                    self.note_failure(code);
                    self.connected = false;
                    self.connected_at = None;
                    self.connected_at_wall = None;

                    if reset_backoff {
                        self.backoff = self.spec.retry_base;
                        self.attempt = 0;
                        self.stable_once = true;
                    }

                    self.log_line(&format!(
                        "[看护] 进程退出了（退出码 {}，活了 {} 秒）：{}",
                        exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "未知".into()),
                        uptime.as_secs(),
                        code.text()
                    ));

                    // 从没站稳过 + 连续失败到上限 → 停手，等用户「再试一次」。
                    if self.should_give_up() {
                        self.give_up();
                        events.push(Event::Exited {
                            attempt: previous_attempt,
                            exit_code,
                            uptime,
                            retry_in: Duration::ZERO,
                            reset_backoff,
                            code: ReasonCode::RetryStopped,
                        });
                        return events;
                    }

                    let retry_in = self.backoff;
                    self.retry_at = Some(Instant::now() + retry_in);
                    self.phase = Phase::Backoff {
                        until: Instant::now() + retry_in,
                    };

                    events.push(Event::Exited {
                        attempt: previous_attempt,
                        exit_code,
                        uptime,
                        retry_in,
                        reset_backoff,
                        code,
                    });

                    if !reset_backoff {
                        self.backoff = std::cmp::min(self.backoff * 2, self.spec.retry_max);
                    }
                }
                Ok(None) => {
                    // 进程活着 ≠ 真的连上了：必须看到 ssh 说 "remote forward success"
                    // （ExitOnForwardFailure=yes 保证这行出现时远端端口真的绑上了）。
                    // 反例：服务器卡死时 TCP 能连上但握手一直挂着，进程不死也不转发。
                    if !self.connected && self.log_has_marker_since(self.log_scan_from) {
                        self.connected = true;
                        self.ever_connected = true;
                        self.connected_at = Some(Instant::now());
                        self.connected_at_wall = Some(now_wall_ms());
                        self.reconnects_total = self.reconnects_total.saturating_add(1);
                        // 真正连上：上次的失败原因清掉（D2 §4.3）。
                        self.reason_code = None;
                        self.last_failure = None;
                        self.retry_state = RetryState::Active;
                        self.log_line("[隧道] 已连上（云端转发已建立）");
                    }
                    // 跑满 stable_after：算「站稳过」，失败计数清零，以后永久重试。
                    if self.connected
                        && !self.stable_once
                        && self
                            .started_at
                            .map(|t| t.elapsed() >= self.spec.stable_after)
                            .unwrap_or(false)
                    {
                        self.stable_once = true;
                        self.attempt = 0;
                        self.log_line("[看护] 已经稳定运行，失败计数清零");
                    }
                }
                Err(err) => {
                    events.push(Event::Exited {
                        attempt: self.attempt,
                        exit_code: None,
                        uptime: self.started_at.map(|t| t.elapsed()).unwrap_or_default(),
                        retry_in: self.backoff,
                        reset_backoff: false,
                        code: ReasonCode::ExitUnknown,
                    });
                    self.note_failure(ReasonCode::ExitUnknown);
                    self.child = None;
                    self.job = None;
                    if self.should_give_up() {
                        self.give_up();
                        let _ = err;
                        return events;
                    }
                    self.retry_at = Some(Instant::now() + self.backoff);
                    self.phase = Phase::Backoff {
                        until: Instant::now() + self.backoff,
                    };
                    let _ = err;
                }
            }
            return events;
        }

        // 远端清理：先等正在跑的清理结束；需要时启动一次清理
        if let Some(rx) = self.cleanup_rx.take() {
            match rx.try_recv() {
                Ok(outcome) => {
                    if let crate::cleanup::CleanupOutcome::OwnerUnverified { .. } = &outcome {
                        // 端口被另一个登录账号的会话占着：换端口才能解决。
                        // D2 §7.2 没有单独的「别的账号占着」码，用 remotePortBusy（换端口）表达。
                        self.note_failure(ReasonCode::RemotePortBusy);
                    }
                    let human = outcome.human();
                    self.log_line(&format!("[清理] {human}"));
                    let ok = matches!(
                        outcome,
                        crate::cleanup::CleanupOutcome::Killed { .. }
                            | crate::cleanup::CleanupOutcome::PortFree
                    );
                    events.push(Event::CleanupFinished {
                        ok,
                        message: human,
                    });
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.cleanup_rx = Some(rx);
                    return events; // 等清理结束再重试
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if self.cleanup_pending {
            self.cleanup_pending = false;
            if let Some(spec) = self.spec.cleanup.clone() {
                self.last_cleanup_at = Some(Instant::now());
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(crate::cleanup::run_cleanup(&spec));
                });
                self.cleanup_rx = Some(rx);
                return events; // 下一轮等清理结果
            }
        }

        // 2) 没子进程：到点就重试。
        let due = match self.retry_at {
            Some(at) if Instant::now() >= at => true,
            Some(_) => false,
            // 首次启动：立即。
            None => !self.first_spawned,
        };
        if !due {
            return events;
        }

        self.retry_at = None;
        self.first_spawned = true;
        self.attempt = self.attempt.saturating_add(1);

        match self.spawn() {
            Ok(pid) => {
                events.push(Event::Started {
                    attempt: self.attempt,
                    pid,
                });
            }
            Err(err) => {
                self.spawn_failed = true;
                self.note_failure_text(
                    ReasonCode::SpawnFail,
                    format!("{}：{err}", ReasonCode::SpawnFail.text()),
                );
                self.log_line(&format!("[隧道] 启动失败：{err}"));
                self.reporter.report(
                    LogLevel::Warn,
                    TAG_TUNNEL,
                    &format!("隧道「{}」启动失败：{err}", self.spec.name),
                );

                if self.should_give_up() {
                    self.give_up();
                    events.push(Event::SpawnFailed {
                        error: err.to_string(),
                        retry_in: Duration::ZERO,
                        code: ReasonCode::RetryStopped,
                    });
                    return events;
                }

                let retry_in = self.backoff;
                self.retry_at = Some(Instant::now() + retry_in);
                self.phase = Phase::Backoff {
                    until: Instant::now() + retry_in,
                };
                self.backoff = std::cmp::min(self.backoff * 2, self.spec.retry_max);
                events.push(Event::SpawnFailed {
                    error: err.to_string(),
                    retry_in,
                    code: ReasonCode::SpawnFail,
                });
            }
        }

        events
    }

    /// 立即杀掉当前子进程（模拟崩溃/弱网断开用）。
    /// 注意：这里只杀不取走，让下一次 `poll()` 走正常的「退出→退避重试」流程。
    pub fn kill_child(&mut self) -> bool {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            return true;
        }
        false
    }

    /// 用户主动停止：不再重试。
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.job = None;
        self.retry_state = RetryState::Stopped;
        self.phase = Phase::Stopped;
    }

    fn spawn(&mut self) -> io::Result<u32> {
        let job = Job::new()?;

        let log_path = self.log_path();
        // 日志太大先轮转，别让它无限长。
        rotate_log_if_needed(&log_path);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| io::Error::new(e.kind(), format!("打开日志失败: {e}")))?;
        // 只扫本次启动之后的新日志（避免把上次连接的成功当成本次）
        self.log_scan_from = log.metadata().map(|m| m.len()).unwrap_or(0);
        self.connected = false;
        self.connected_at = None;
        self.connected_at_wall = None;
        self.retry_state = RetryState::Active;
        let err_log: File = log;

        let mut cmd = Command::new(&self.spec.ssh_exe);
        cmd.args(&self.spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(err_log))
            .creation_flags(CREATE_NO_WINDOW);

        let child = cmd.spawn().map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("无法启动 {}: {e}", self.spec.ssh_exe.display()),
            )
        })?;

        // 放进作业对象：主程序没了，它也会被系统一起结束。
        if let Err(e) = job.assign_process(child.as_raw_handle()) {
            // 作业分配失败不阻止本次运行，但必须让调用方知道（保底清理失效）。
            self.reporter.report(
                LogLevel::Warn,
                TAG_TUNNEL,
                &format!(
                    "隧道「{}」放进作业失败：崩溃时可能留下连接（{e}）",
                    self.spec.name
                ),
            );
        }

        let pid = child.id();
        self.started_at = Some(Instant::now());
        self.spawn_failed = false;
        self.child = Some(child);
        self.job = Some(job);
        self.phase = Phase::Running {
            since: Instant::now(),
        };
        Ok(pid)
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // 释放时确保子进程被结束并回收（作业对象机制兜底）。
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.job = None;
    }
}

impl Tunnel {
    fn log_path(&self) -> PathBuf {
        // 有隧道 id 就用 id（改名不换文件）；没有就退回用名字。
        let key = if self.spec.id.trim().is_empty() {
            self.spec.name.as_str()
        } else {
            self.spec.id.as_str()
        };
        service_log_path(&self.log_dir, key)
    }

    /// 从 `from` 开始扫日志，看有没有“远端转发成功”的标志。
    fn log_has_marker_since(&self, from: u64) -> bool {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(self.log_path()) else {
            return false;
        };
        if f.seek(SeekFrom::Start(from)).is_err() {
            return false;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return false;
        }
        String::from_utf8_lossy(&buf).contains("remote forward success")
    }

    /// 读日志尾部（判断退出原因用）。
    fn read_log_tail(&self) -> String {
        use std::io::{Read, Seek, SeekFrom};
        let path = self.log_path();
        let Ok(mut f) = std::fs::File::open(&path) else {
            return String::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let start = len.saturating_sub(8192);
        let _ = f.seek(SeekFrom::Start(start));
        let mut buf = String::new();
        let _ = f.read_to_string(&mut buf);
        buf
    }

    /// 往服务日志里追加一行（清理结果等）。
    fn log_line(&self, line: &str) {
        use std::io::Write;
        let path = self.log_path();
        rotate_log_if_needed(&path);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            // 程序自己写的行带 `[<epoch-ms>] ` 前缀（D0 第 14 条 / D2 §5.4）；
            // ssh 原文不走这里（Stdio 直接写文件），一个字节都不动。
            let _ = writeln!(f, "{}{line}", crate::log::epoch_prefix());
        }
    }
}

/// 文件名里能留的字符：Unicode 字母数字 + `-` `_`。
/// 中文名不再全变下划线（全变下划线会让不同隧道挤进同一个日志文件）。
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// FNV-1a 32 位：给文件名加一个跟原始 key 绑定的短尾巴，
/// 保证不同名字/不同 id 落到不同文件（光靠 sanitize 还会撞）。
fn short_hash(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in text.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// 某个服务的 ssh 日志文件路径（界面看日志、清理读原因都用它）。
/// 文件名 = 可读前缀 + 原始 key 的短哈希。
pub fn service_log_path(log_dir: &std::path::Path, name: &str) -> PathBuf {
    log_dir.join(format!("{}-{:08x}.ssh.log", sanitize(name), short_hash(name)))
}

/// 备份文件名：x.ssh.log.1、x.ssh.log.2 …（测试验证轮转结果用）。
#[cfg(test)]
fn backup_path(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// 日志超过上限就轮转：实现统一在 [`crate::logfile`]（隧道日志和服务日志同一套 5MB×2 参数）。
fn rotate_log_if_needed(path: &Path) {
    let _ = rotate_if_needed(path, SERVICE_LOG_MAX_BYTES, SERVICE_LOG_BACKUPS);
}

/// 按 ssh 日志尾部 + 退出码认退出原因（单一入口在 `crate::reason`，不在这里再存一份关键字表）。
/// 退出码 0 且认不出关键字时给 `ExitUnknown`（干净退出不算「意外退出」）。
fn classify_exit(tail: &str, exit_code: Option<u32>) -> ReasonCode {
    match exit_code {
        Some(0) => ReasonCode::ExitUnknown,
        Some(code) => ReasonCode::from_ssh_exit(tail, code as i32),
        None => ReasonCode::from_ssh_exit(tail, -1),
    }
}

/// 方便调用方判断某路径是不是默认 ssh。
pub fn is_default_ssh(path: &Path) -> bool {
    path.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(crate::ssh::DEFAULT_SSH_EXE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn null_reporter() -> Arc<dyn Reporter> {
        Arc::new(crate::log::NullReporter)
    }

    /// 一个已经退出的子进程句柄（`try_wait` 会直接给 `Some`）。
    fn exited_child() -> Child {
        let mut child = Command::new("cmd.exe")
            .args(["/C", "exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("起测试进程");
        let _ = child.wait();
        child
    }

    /// 一个活着不退出的子进程。
    fn live_child() -> Child {
        Command::new("cmd.exe")
            .args(["/C", "ping", "-n", "20", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("起测试进程")
    }

    fn failing_tunnel(dir: &Path) -> Tunnel {
        // 不存在的 ssh.exe：每次尝试都 SpawnFailed
        let spec = TunnelSpec::new(
            "错地址",
            PathBuf::from(r"C:\definitely-missing-ssh-for-test.exe"),
            Vec::new(),
        );
        Tunnel::new(spec, dir.to_path_buf(), null_reporter())
    }

    #[test]
    fn chinese_names_get_distinct_log_files() {
        let dir = std::env::temp_dir();
        let a = service_log_path(&dir, "测试隧道");
        let b = service_log_path(&dir, "另一个隧道");
        assert_ne!(a, b, "不同中文名不能落到同一个日志文件");
        assert_eq!(service_log_path(&dir, "测试隧道"), a, "同名路径要稳定");
        let file = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(file.starts_with("测试隧道-"), "文件名保留中文方便认：{file}");
    }

    #[test]
    fn tunnel_log_path_prefers_id_over_name() {
        let dir = std::env::temp_dir();
        let spec =
            TunnelSpec::new("中文名", PathBuf::from("ssh.exe"), Vec::new()).with_id("tun-1");
        let tunnel = Tunnel::new(spec, dir.clone(), null_reporter());
        assert_eq!(tunnel.log_path(), service_log_path(&dir, "tun-1"));

        // 没传 id 的老调用方：退回按名字（路径仍然唯一）
        let spec = TunnelSpec::new("中文名", PathBuf::from("ssh.exe"), Vec::new());
        let tunnel = Tunnel::new(spec, dir.clone(), null_reporter());
        assert_eq!(tunnel.log_path(), service_log_path(&dir, "中文名"));
    }

    #[test]
    fn log_rotation_keeps_two_backups() {
        let dir = std::env::temp_dir().join(format!("sx-rotate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.ssh.log");
        let big = || vec![b'a'; (SERVICE_LOG_MAX_BYTES + 1) as usize];

        std::fs::write(&path, big()).unwrap();
        rotate_log_if_needed(&path);
        assert!(!path.exists(), "超上限的当前文件应该被移走");
        assert!(backup_path(&path, 1).exists());

        std::fs::write(&path, big()).unwrap();
        rotate_log_if_needed(&path);
        assert!(backup_path(&path, 1).exists());
        assert!(backup_path(&path, 2).exists());

        std::fs::write(&path, big()).unwrap();
        rotate_log_if_needed(&path);
        assert!(backup_path(&path, 2).exists(), "只保留两份备份");
        assert!(!backup_path(&path, 3).exists());

        // 没超上限就不动它
        std::fs::write(&path, b"small").unwrap();
        rotate_log_if_needed(&path);
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"small");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gave_up_after_twenty_without_stable() {
        let dir = std::env::temp_dir();
        let mut tunnel = failing_tunnel(&dir);
        for _ in 0..MAX_ATTEMPTS_BEFORE_STOP {
            tunnel.retry_at = Some(Instant::now() - Duration::from_millis(1));
            tunnel.poll();
        }
        assert_eq!(tunnel.attempt(), MAX_ATTEMPTS_BEFORE_STOP);
        assert_eq!(tunnel.retry_state(), RetryState::Stopped);
        assert_eq!(tunnel.reason_code(), Some(ReasonCode::RetryStopped));
        assert!(matches!(tunnel.phase(), Phase::GaveUp));
        assert!(tunnel.last_failure().is_some());
        assert_eq!(tunnel.retry_in_secs(), None, "停手后没有倒计时");

        // 停手后不再自动重试
        let events = tunnel.poll();
        assert!(events.is_empty());
        assert_eq!(tunnel.attempt(), MAX_ATTEMPTS_BEFORE_STOP);
    }

    #[test]
    fn stable_run_never_gives_up() {
        let dir = std::env::temp_dir();
        let mut tunnel = failing_tunnel(&dir);

        // 白盒：模拟一次已经连上、且跑满 stable_after 的连接
        tunnel.child = Some(live_child());
        tunnel.connected = true;
        tunnel.ever_connected = true;
        tunnel.connected_at = Some(Instant::now() - Duration::from_secs(61));
        tunnel.connected_at_wall = Some(1);
        tunnel.started_at = Some(Instant::now() - tunnel.spec.stable_after - Duration::from_secs(1));
        tunnel.attempt = 5;
        tunnel.phase = Phase::Running {
            since: Instant::now() - Duration::from_secs(61),
        };
        tunnel.poll();
        assert!(tunnel.stable_once(), "跑满 60 秒算站稳过");
        assert_eq!(tunnel.attempt(), 0, "站稳后失败计数归零");
        assert_eq!(tunnel.reconnects_total(), 0, "这一轮是白盒造的状态，没真的连上");

        // 站稳过的隧道：再怎么失败也永久重试
        if let Some(mut child) = tunnel.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        tunnel.child = Some(exited_child());
        tunnel.connected = false;
        for _ in 0..(MAX_ATTEMPTS_BEFORE_STOP + 10) {
            tunnel.retry_at = Some(Instant::now() - Duration::from_millis(1));
            tunnel.poll();
            if !matches!(tunnel.phase(), Phase::GaveUp) {
                tunnel.retry_at = Some(Instant::now() - Duration::from_millis(1));
            }
        }
        assert_eq!(tunnel.retry_state(), RetryState::Active);
        assert!(!matches!(tunnel.phase(), Phase::GaveUp), "站稳过就不许停手");
        assert!(tunnel.attempt() > MAX_ATTEMPTS_BEFORE_STOP);
    }

    #[test]
    fn flapping_marked() {
        let dir = std::env::temp_dir();
        let mut tunnel = failing_tunnel(&dir);
        tunnel.child = Some(exited_child());
        tunnel.connected = true;
        tunnel.ever_connected = true;
        tunnel.connected_at = Some(Instant::now());
        tunnel.connected_at_wall = Some(1);
        tunnel.started_at = Some(Instant::now() - Duration::from_secs(1));
        tunnel.phase = Phase::Running {
            since: Instant::now() - Duration::from_secs(1),
        };
        tunnel.poll();
        assert_eq!(tunnel.reason_code(), Some(ReasonCode::ConnReset));
        assert!(matches!(tunnel.phase(), Phase::Backoff { .. }));
    }

    #[test]
    fn kick_retry_moves_backoff_to_now() {
        let dir = std::env::temp_dir();
        let mut tunnel = failing_tunnel(&dir);
        // 非退避状态：不动
        assert!(!tunnel.kick_retry());

        // 正在退避：提前到「现在」，下一轮立刻重试
        tunnel.retry_at = Some(Instant::now() + Duration::from_secs(30));
        tunnel.phase = Phase::Backoff {
            until: Instant::now() + Duration::from_secs(30),
        };
        assert!(tunnel.kick_retry());
        assert!(matches!(tunnel.phase(), Phase::Backoff { .. }));
        assert!(
            tunnel.retry_at.unwrap() <= Instant::now(),
            "退避应该提前到现在"
        );
    }

    #[test]
    fn reset_for_retry_clears() {
        let dir = std::env::temp_dir();
        let mut tunnel = failing_tunnel(&dir);
        for _ in 0..MAX_ATTEMPTS_BEFORE_STOP {
            tunnel.retry_at = Some(Instant::now() - Duration::from_millis(1));
            tunnel.poll();
        }
        assert!(matches!(tunnel.phase(), Phase::GaveUp));

        tunnel.reset_for_retry();
        assert_eq!(tunnel.attempt(), 0);
        assert_eq!(tunnel.retry_state(), RetryState::Active);
        assert_eq!(tunnel.reason_code(), None);
        assert!(tunnel.last_failure().is_none());
        assert!(matches!(tunnel.phase(), Phase::Idle));
        assert_eq!(tunnel.retry_in_secs(), None, "回 Idle 后立刻重连，没有倒计时");
    }

    #[test]
    fn retry_in_secs_rounds_up_and_stops() {
        let dir = std::env::temp_dir();
        let mut tunnel = failing_tunnel(&dir);
        tunnel.phase = Phase::Backoff {
            until: Instant::now() + Duration::from_secs(3600),
        };
        assert_eq!(tunnel.retry_in_secs(), Some(3600), "向上取整");

        tunnel.phase = Phase::Backoff {
            until: Instant::now() - Duration::from_millis(1),
        };
        assert_eq!(tunnel.retry_in_secs(), Some(0), "到期瞬间是 0");

        tunnel.phase = Phase::Backoff {
            until: Instant::now() + Duration::from_secs(5),
        };
        tunnel.retry_state = RetryState::Stopped;
        assert_eq!(tunnel.retry_in_secs(), None, "停手时不给倒计时");
    }

    #[test]
    fn exit_reason_maps_to_codes() {
        // 关键字表已收归 crate::reason，这里只确认接得对（D2 名字）
        assert_eq!(
            classify_exit("Permission denied (publickey).", Some(255)),
            ReasonCode::AuthDenied
        );
        assert_eq!(
            classify_exit("Host key verification failed.", Some(255)),
            ReasonCode::HostKeyChanged
        );
        assert_eq!(
            classify_exit("ssh: Could not resolve hostname bad: ...", Some(255)),
            ReasonCode::DnsFail
        );
        assert_eq!(
            classify_exit("connect to host x port 22: Connection refused", Some(255)),
            ReasonCode::ConnRefused
        );
        assert_eq!(
            classify_exit("operation timed out", Some(255)),
            ReasonCode::NetTimeout
        );
        assert_eq!(
            classify_exit("connection reset by peer", Some(255)),
            ReasonCode::ConnReset
        );
        assert_eq!(
            classify_exit("remote port forwarding failed for listen port 18080", Some(255)),
            ReasonCode::RemotePortBusy
        );
        assert_eq!(
            classify_exit("everything is fine", Some(255)),
            ReasonCode::ExitUnknown
        );
        assert_eq!(
            classify_exit("everything is fine", Some(0)),
            ReasonCode::ExitUnknown,
            "干净退出不许报「意外退出」"
        );
    }

    #[test]
    fn retry_state_serializes_camel_case() {
        // 这个字面量直接进 JSON，不能被改
        assert_eq!(
            serde_json::to_string(&RetryState::Stopped).unwrap(),
            "\"stopped\""
        );
        assert_eq!(
            serde_json::to_string(&RetryState::Active).unwrap(),
            "\"active\""
        );
    }

    #[test]
    fn connected_since_only_while_connected() {
        let dir = std::env::temp_dir();
        let mut tunnel = failing_tunnel(&dir);
        assert!(tunnel.connected_since().is_none());
        assert!(tunnel.connected_since_wall_ms().is_none());
        assert!(tunnel.uptime_secs().is_none());

        tunnel.child = Some(live_child());
        tunnel.connected = true;
        tunnel.connected_at = Some(Instant::now() - Duration::from_secs(3));
        tunnel.connected_at_wall = Some(1234);
        assert!(tunnel.connected_since().is_some());
        assert_eq!(tunnel.connected_since_wall_ms(), Some(1234));
        assert!(tunnel.uptime_secs().unwrap() >= 3);

        tunnel.connected = false;
        assert!(tunnel.connected_since().is_none(), "断开后不给 connectedSince");

        if let Some(mut child) = tunnel.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn log_line_gets_epoch_prefix_and_marker_detection_still_works() {
        let dir = std::env::temp_dir().join(format!("sx-epoch-sup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let spec = TunnelSpec::new("前缀隧道", PathBuf::from("ssh.exe"), Vec::new());
        let tunnel = Tunnel::new(spec, dir.clone(), null_reporter());
        tunnel.log_line("[隧道] 测试一行");

        // 落盘格式必须是 `[<epoch-ms>] ` + 原行，正文一个字节不改。
        let text = std::fs::read_to_string(tunnel.log_path()).unwrap();
        let line = text.lines().next().expect("应该有日志行");
        let close = line.find("] ").expect("前缀要有 `] ` 收尾");
        let digits = &line[1..close];
        assert!(!digits.is_empty(), "前缀要有数字：{line:?}");
        assert!(
            digits.bytes().all(|b| b.is_ascii_digit()),
            "前缀应该是 epoch 毫秒：{line:?}"
        );
        let ts: i64 = digits.parse().expect("前缀要能解析成整数");
        assert!(ts > 1_500_000_000_000, "epoch 毫秒看着不对：{line:?}");
        assert_eq!(&line[close + 2..], "[隧道] 测试一行", "正文不能被改：{line:?}");

        // 加上前缀后，应用自己写的行不能误判成 ssh 的「远端转发成功」。
        assert!(
            !tunnel.log_has_marker_since(0),
            "中文标记行不能误判成 remote forward success"
        );
        // ssh 原文由 Stdio 直接写文件、不带前缀，关键字判定必须照旧。
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(tunnel.log_path())
            .unwrap();
        writeln!(f, "remote forward success for listen port 18080").unwrap();
        drop(f);
        assert!(
            tunnel.log_has_marker_since(0),
            "ssh 原文关键字还要能认出来"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
