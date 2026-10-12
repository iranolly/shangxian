//! 本机服务：启动、停止、看护、端口探测。
//!
//! 纪律（对齐 DESIGN.md §9.4 与 D1 §4/§5）：
//! - 应用退出/重启/崩溃不结束本机服务（保活）：启动进保活作业，下次启动按端口认领；
//!   只有显式停止/删除（看护以外的路径）才结束整棵树。
//! - 只停「我们自己启动的」进程：握着子进程句柄去结束，不存在 pid 复用误杀；
//!   用户自己手动开的进程绝不碰。
//! - 用户手动点了停止，看护不再拉起（除非用户再点启动）。
//! - 端口探测出锁：每台服务一条轻量探测线程写 [`ProbeSlot`]，看护 tick 只读，
//!   连续 3 次失败才判 down / 假死（D1 §5）。
//! - 服务日志封顶：**只在 spawn 前轮转**（此刻没有子进程持有文件句柄）；运行中超限只标记 +
//!   提醒 + 给一个手动轮转入口（D1 §4）。
//! - 连续失败到上限就停下报信，不无限重试刷屏。

use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::LaunchSpec;
use crate::job::Job;
use crate::log::{LogLevel, Reporter, TAG_SERVICE};
use crate::logfile::{rotate_if_needed, size_of, SERVICE_LOG_BACKUPS, SERVICE_LOG_MAX_BYTES};
use crate::probe::{spawn_probe, ProbeHandle, ProbeKind, ProbeSlot, PROBE_INTERVAL};
use crate::reason::ReasonCode;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 启动后多久还没探测到端口，就从「正在启动」变成「进程在但端口不通」
const STARTUP_GRACE: Duration = Duration::from_secs(15);
/// 连续失败这么多次就停下（不再自动重试）
const MAX_FAILS: u32 = 5;
/// 认领窗口：应用刚启动时端口还没在听，可能是服务正在启动。
/// 这段时间里先等着、接着探，探到就认领，别让 autostart 再拉一个。
const CLAIM_RETRY_WINDOW: Duration = Duration::from_secs(8);
/// 重试退避：2 秒 → 30 秒
const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 日志大小检查间隔（探测线程每 2 秒一轮，这个跟着走）。
const LOG_CHECK_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub enum Phase {
    /// 没启动（用户没启动过，或用户手动停了）
    Stopped,
    /// 正在启动（进程已拉起，或正在等下一次重试）
    Starting,
    /// 在跑（进程活着且端口探测通过）
    Running,
    /// 进程在，但端口一直不通
    NotListening,
    /// 起不来，已经停止重试（连续失败到上限）
    Failed,
}

/// 最近一次失败（给界面看；重启不失忆，health 变 up 时清空）。
/// `code` 用 D2 §7.2 的服务码（startFail / exited / hung / crashLoop）。
#[derive(Debug, Clone)]
pub struct ServiceFailure {
    /// 给人看的墙钟（epoch ms）。
    pub ts_ms: u64,
    pub code: ReasonCode,
    pub reason: String,
}

fn now_wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 一台本机服务的运行时信息。
#[derive(Debug, Clone)]
pub struct LocalSpec {
    pub name: String,
    pub local_port: u16,
    /// 探测方式：tcp 只看能不能连上，http 要一个响应（D1 §5）
    pub probe_kind: ProbeKind,
    /// None = 只监控不启动
    pub launch: Option<LaunchSpec>,
    pub watch: bool,
}

pub struct LocalRunner {
    spec: LocalSpec,
    child: Option<Child>,
    job: Option<Job>,
    phase: Phase,
    /// 探测结果槽（探测线程写、本对象只读）
    probe: Arc<ProbeSlot>,
    /// 探测线程句柄（Drop 只置停止位，不等它退）
    probe_handle: Option<ProbeHandle>,
    /// 本次启动的时刻（判断是否还在「正在启动」宽限期）
    started_at: Option<Instant>,
    /// 用户手动停过：看护不拉起
    user_stopped: bool,
    /// 连续失败次数（进程活不过 3 秒、启动失败、假死都算）
    fail_streak: u32,
    backoff: Duration,
    retry_at: Option<Instant>,
    reason: Option<String>,
    log_path: PathBuf,
    log_dir: PathBuf,
    /// 日志文件名用的稳定标识（服务 id）；空 = 没传，用本机端口兜底。
    /// 两种都跟服务名无关，改名不换日志文件。
    log_key: String,
    /// 运行中日志是否已超上限（只标记，不轮转，见 D1 §4）
    log_overflowed: bool,
    /// 下次查日志大小的时刻（2 秒一次，开销可忽略）
    log_check_at: Option<Instant>,
    /// 最近一次失败（health 变 up 时清空）
    last_failure: Option<ServiceFailure>,
    /// 认领窗口截止时间（见 CLAIM_RETRY_WINDOW）。
    claim_deadline: Option<Instant>,
    /// 本次启动的「假死」是否已经处理过，避免每轮 poll 重复报警/连环重启。
    hang_handled: bool,
    /// app.log 接收器（作业分配失败、日志超限等降级点往里写）。
    reporter: Arc<dyn Reporter>,
}

impl LocalRunner {
    pub fn new(spec: LocalSpec, log_dir: &Path, reporter: Arc<dyn Reporter>) -> Self {
        let probe = ProbeSlot::new();
        let mut runner = Self::new_with_probe(spec, log_dir, reporter, probe);
        runner.start_probe();
        runner
    }

    /// 注入现成的探测槽（测试用：不跑探测线程，手动 `feed` 结果）。
    pub fn new_with_probe(
        spec: LocalSpec,
        log_dir: &Path,
        reporter: Arc<dyn Reporter>,
        probe: Arc<ProbeSlot>,
    ) -> Self {
        let _ = std::fs::create_dir_all(log_dir);
        // 默认用本机端口当稳定标识（guardian 里端口本来就唯一）；
        // guardian 拿到服务 id 后可以再 set_id 换成 id。
        let log_key = format!("port-{}", spec.local_port);
        let log_path = log_path_for(log_dir, &log_key);
        Self {
            spec,
            child: None,
            job: None,
            phase: Phase::Stopped,
            probe,
            probe_handle: None,
            started_at: None,
            user_stopped: true, // 应用启动时不会自动拉起；开机自启由 guardian 统一触发
            fail_streak: 0,
            backoff: BACKOFF_BASE,
            retry_at: None,
            reason: None,
            log_path,
            log_dir: log_dir.to_path_buf(),
            log_key,
            log_overflowed: false,
            log_check_at: None,
            last_failure: None,
            claim_deadline: None,
            hang_handled: false,
            reporter,
        }
    }

    /// 起探测线程（端口/探测方式变了要重起）。
    fn start_probe(&mut self) {
        self.probe_handle = Some(spawn_probe(
            self.spec.local_port,
            self.spec.probe_kind,
            PROBE_INTERVAL,
            Arc::clone(&self.probe),
        ));
    }

    pub fn spec(&self) -> &LocalSpec {
        &self.spec
    }

    pub fn set_spec(&mut self, spec: LocalSpec) {
        let probe_changed =
            self.spec.local_port != spec.local_port || self.spec.probe_kind != spec.probe_kind;
        self.spec = spec;
        if probe_changed {
            // 换端口/换探测方式：旧结果没意义了，换一个槽重起探测线程。
            self.probe_handle = None;
            self.probe = ProbeSlot::new();
            self.fail_streak = 0;
            self.backoff = BACKOFF_BASE;
            self.retry_at = None;
            self.start_probe();
        }
    }

    /// 指定服务 id：日志文件按 id 命名。创建后立刻调用；
    /// 之后改名、换端口都不换日志文件。
    pub fn set_id(&mut self, id: impl Into<String>) {
        let id = id.into();
        if id.is_empty() {
            return;
        }
        self.log_key = id;
        self.log_path = log_path_for(&self.log_dir, &self.log_key);
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// 端口探测现在通不通（读探测线程的结果，不再同步连）。
    pub fn listening(&self) -> bool {
        self.probe.alive()
    }

    /// 探测结果槽（guardian 算 health / fail_streak 用）。
    pub fn probe(&self) -> &ProbeSlot {
        &self.probe
    }

    /// 运行中日志是否已超上限（界面显示提示 + 手动轮转按钮）。
    pub fn log_overflowed(&self) -> bool {
        self.log_overflowed
    }

    /// 最近一次失败（health 变 up 时清空）。
    pub fn last_failure(&self) -> Option<&ServiceFailure> {
        self.last_failure.as_ref()
    }

    /// 用户手动停过（看护不拉起；通知也不报）。
    pub fn is_user_stopped(&self) -> bool {
        self.user_stopped
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// 当前进程号（没有在跑就是 None）。
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// 给界面看的状态词。
    pub fn status_word(&self) -> &'static str {
        match self.phase {
            Phase::Failed => "failed",
            Phase::Running | Phase::NotListening => {
                if self.listening() {
                    "running"
                } else if self
                    .started_at
                    .map(|t| t.elapsed() < STARTUP_GRACE)
                    .unwrap_or(false)
                {
                    "starting"
                } else {
                    "not_listening"
                }
            }
            Phase::Starting => "starting",
            Phase::Stopped => "stopped",
        }
    }

    /// 用户点「启动」。
    pub fn start_now(&mut self) {
        self.user_stopped = false;
        self.fail_streak = 0;
        self.backoff = BACKOFF_BASE;
        self.retry_at = None;
        self.reason = None;
        self.phase = Phase::Starting;
        self.claim_deadline = None; // 用户明确要启动，不再等认领
        self.hang_handled = false;
        if self.child.is_none() {
            self.retry_at = Some(Instant::now()); // 下一轮 poll 立刻拉起
        }
    }

    /// 用户点「停止」：只停我们自己启动的那个进程。
    pub fn stop_now(&mut self) {
        self.user_stopped = true;
        self.kill_child();
        self.phase = Phase::Stopped;
        self.retry_at = None;
        self.reason = None;
        self.claim_deadline = None; // 用户停了就别再认领
        self.hang_handled = false;
        // 用户主动停止不是失败：不发原因码、不记 lastFailure（D0 裁决）。
        self.log_line("[服务] 已按你的要求停止");
    }

    /// 界面「重启服务并轮转日志」：先按用户要求停，再立刻启动（下次 spawn 时轮转）。
    /// 复用内部顺序 `stop_now()` → `start_now()` → `poll()`；
    /// 没配启动方式的服务不可用（界面也不该显示这个按钮）。
    pub fn restart_and_rotate(&mut self) {
        self.stop_now();
        self.start_now();
        self.poll();
    }

    /// 开机自启：由应用启动时统一触发。
    /// 认领窗口还开着的话要留着——先等它启动完，别抢着再拉一个。
    pub fn start_for_autostart(&mut self) {
        let claim_deadline = self.claim_deadline;
        self.start_now();
        self.claim_deadline = claim_deadline;
    }

    /// 应用启动后的认领：端口已经在监听、本会话又没有自己的子进程时，把它认成
    /// 「在跑」（可能是上个会话留下的，也可能是用户自己开的），绝不重复启动。
    ///
    /// 认领后 `user_stopped = false`：勾了看护的服务以后掉了，watch 会照常拉起。
    /// 返回是否认领成功。
    pub fn claim_if_listening(&mut self) -> bool {
        if self.child.is_some() {
            return false; // 本会话自己起的，不需要认领
        }
        // 同步探一次（最多 100ms）——认领只在启动/用户操作时发生。
        if !self.probe.check_now(self.spec.local_port, self.spec.probe_kind) {
            // 端口还没在听：可能服务正在启动。开一个认领窗口，
            // 窗口里由 poll() 接着读探测结果；探到就认领，避免 autostart 再拉一个。
            if self.claim_deadline.is_none() {
                self.claim_deadline = Some(Instant::now() + CLAIM_RETRY_WINDOW);
                self.log_line("[服务] 端口还没在听，等它几秒再认领");
            }
            return false;
        }
        self.claim_ok();
        true
    }

    /// 认领成功：把它当成「在跑」，看护继续生效。
    fn claim_ok(&mut self) {
        self.phase = Phase::Running;
        self.user_stopped = false;
        self.retry_at = None;
        self.fail_streak = 0;
        self.backoff = BACKOFF_BASE;
        self.claim_deadline = None;
        self.note_healthy();
        self.log_line("[服务] 端口已经在监听，已认领（不会重复启动）");
    }

    /// 周期调用：推进状态机。
    pub fn poll(&mut self) {
        self.check_log_overflow();

        // 探测出锁：结果由探测线程写，这里只读，绝不在看护 tick 里连端口。
        let listening = self.probe.alive();

        // 只监控的服务（没配启动方式）：状态完全看端口通不通——
        // 用户自己手动开的服务也要能看出“在跑”。
        if self.spec.launch.is_none() {
            self.phase = if listening {
                Phase::Running
            } else {
                Phase::Stopped
            };
            if listening {
                self.note_healthy();
            }
            return;
        }

        // 0.5) 认领窗口：应用刚启动时端口还没在听，可能是服务正在启动。
        // 窗口里只读探测结果、不抢跑；探到就认领，窗口过了才走正常启动。
        if self.child.is_none() {
            if let Some(deadline) = self.claim_deadline {
                if listening {
                    self.claim_ok();
                    return;
                }
                if Instant::now() < deadline {
                    if !self.user_stopped || matches!(self.phase, Phase::Starting) {
                        self.phase = Phase::Starting;
                    }
                    return; // 还在窗口里：先不启动
                }
                self.claim_deadline = None;
                self.log_line("[服务] 认领等待结束，端口一直没在听");
            }
        }

        // 1) 有子进程：看它还在不在（端口探测结果已经由线程写好了）
        let mut hang_now = false;
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let uptime = self.started_at.map(|t| t.elapsed()).unwrap_or_default();
                    let exit_code = status.code();
                    self.child = None;
                    self.job = None; // 进程已退出，作业里没有活物
                    let code_text = exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "未知".into());
                    self.log_line(&format!(
                        "[看护] 进程退出了（退出码 {code_text}，活了 {} 秒）",
                        uptime.as_secs()
                    ));
                    self.note_failure_text(
                        ReasonCode::Exited,
                        format!("程序退出了（退出码 {code_text}）：看护开着的话会自动拉起"),
                    );

                    // 活不到 3 秒算失败（启动就崩），连续失败到上限就停手
                    if uptime < Duration::from_secs(3) {
                        self.fail_streak += 1;
                        if self.fail_streak >= MAX_FAILS {
                            self.give_up_service("连续启动失败，已经停止自动重试");
                            return;
                        }
                    } else {
                        self.fail_streak = 0;
                        self.backoff = BACKOFF_BASE;
                    }

                    if self.spec.watch && !self.user_stopped {
                        let wait = self.backoff;
                        self.retry_at = Some(Instant::now() + wait);
                        self.phase = Phase::Starting;
                        self.log_line(&format!("[看护] {} 秒后自动重启", wait.as_secs()));
                        self.backoff = std::cmp::min(self.backoff * 2, BACKOFF_MAX);
                    } else {
                        self.phase = Phase::Stopped;
                        self.retry_at = None;
                    }
                }
                Ok(None) => {
                    // 进程活着：探测结果说了算（连续 3 次失败才算不通）
                    if listening {
                        self.phase = Phase::Running;
                        self.note_healthy();
                    } else {
                        // 防抖：没到 3 次失败时保持上一次的状态词，别吓唬用户；
                        // 「在线但 fail_streak>0」由 guardian 映射成 health=degraded。
                        if self.probe.is_down() {
                            self.phase = Phase::NotListening;
                        }
                        self.note_probe_failure();
                        // 端口不通、连续 3 次探测失败、且过了启动宽限期：进程活着却不干活（假死）。
                        let hung = self
                            .started_at
                            .map(|t| t.elapsed() >= STARTUP_GRACE)
                            .unwrap_or(false)
                            && self.probe.is_down();
                        if hung && !self.hang_handled {
                            hang_now = true;
                        }
                    }
                }
                Err(err) => {
                    self.reason = Some(format!("查看进程状态失败：{err}"));
                }
            }
            if hang_now {
                self.hang_handled = true;
                self.handle_hang();
            }
            return;
        }

        // 2) 没子进程：先看端口（可能是用户自己开的，也可能我们起的进程已经退出了）
        if listening {
            // 它在跑（不管是用户自己开的还是我们起的）：状态对齐，清掉重试计划
            self.phase = Phase::Running;
            self.retry_at = None;
            self.note_healthy();
        } else {
            let was_listening = matches!(self.phase, Phase::Running);
            if was_listening && self.spec.watch && !self.user_stopped {
                // 刚刚从“在听”变成“没在听”：它掉了 → 看护负责拉起来
                self.log_line("[看护] 端口不通了（服务掉了），马上拉起来");
                self.retry_at = Some(Instant::now());
                self.phase = Phase::Starting;
            } else if was_listening {
                // 之前在跑、看护没开（或用户停的）：它掉了，如实显示
                // （没进程时不记 lastFailure；退出那条已经记过 exited 了）
                self.phase = Phase::NotListening;
            }
        }

        // 3) 到点就拉起（看护 或 用户刚点了启动）
        let due = match self.retry_at {
            Some(at) => Instant::now() >= at,
            None => false,
        };
        let should_run =
            !self.user_stopped && (self.spec.watch || matches!(self.phase, Phase::Starting));
        if due && should_run && self.spec.launch.is_some() {
            // 先同步探一次（最多 100ms）：已经在听说明它早就在跑了（比如用户自己开的，
            // 或者上次留下的），这时千万别再起一个（两个进程抢同一个端口）。
            if self.probe.check_now(self.spec.local_port, self.spec.probe_kind) {
                self.retry_at = None;
                self.phase = Phase::Running;
                self.note_healthy();
                self.log_line("[看护] 端口已经在监听，说明它已经在跑，不重复启动");
                return;
            }
            self.retry_at = None;
            match self.spawn() {
                Ok(pid) => {
                    self.log_line(&format!("[服务] 已启动，进程号 {pid}"));
                }
                Err(err) => {
                    self.fail_streak += 1;
                    self.reason = Some(format!("启动失败：{err}"));
                    self.log_line(&format!("[服务] 启动失败：{err}"));
                    self.note_failure_text(
                        ReasonCode::StartFail,
                        format!("本机服务启动失败：{err}"),
                    );
                    self.reporter.report(
                        LogLevel::Warn,
                        TAG_SERVICE,
                        &format!("服务「{}」启动失败：{err}", self.spec.name),
                    );
                    if self.fail_streak >= MAX_FAILS {
                        self.give_up_service("连续启动失败，已经停止自动重试");
                    } else {
                        let wait = self.backoff;
                        self.retry_at = Some(Instant::now() + wait);
                        self.backoff = std::cmp::min(self.backoff * 2, BACKOFF_MAX);
                        self.phase = Phase::Starting;
                    }
                }
            }
        }
    }

    /// 日志超上限就标记 + 写 app.log（只报一次；文件变小/轮转后自动清）。
    fn check_log_overflow(&mut self) {
        let due = self
            .log_check_at
            .map(|t| t.elapsed() >= LOG_CHECK_EVERY)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.log_check_at = Some(Instant::now());
        if check_log_overflow(&self.log_path) {
            if !self.log_overflowed {
                self.log_overflowed = true;
                self.reporter.report(
                    LogLevel::Warn,
                    TAG_SERVICE,
                    &format!(
                        "服务「{}」的日志超过 5MB：界面会提示「重启服务并轮转日志」",
                        self.spec.name
                    ),
                );
            }
        } else if self.log_overflowed {
            self.log_overflowed = false; // 用户手动删了或轮转过了
        }
    }

    /// 端口通了：失败计数、退避、上次原因全部清零。
    fn note_healthy(&mut self) {
        self.fail_streak = 0;
        self.backoff = BACKOFF_BASE;
        self.reason = None;
        self.last_failure = None;
        self.hang_handled = false;
    }

    /// 连续 3 次探不到端口、且**进程还活着**才记一次「端口不通」（D2 的 `hung`）。
    /// 没进程的时候（自己退过或根本没启动）不记失败：退出那条已经记过 `exited` 了。
    fn note_probe_failure(&mut self) {
        if self.child.is_some() && self.probe.is_down() {
            self.note_failure_text(ReasonCode::Hung, "进程活着但连续 3 次探不到端口".into());
        }
    }

    fn note_failure_text(&mut self, code: ReasonCode, reason: String) {
        self.last_failure = Some(ServiceFailure {
            ts_ms: now_wall_ms(),
            code,
            reason,
        });
    }

    /// 连续失败到上限：停下报信（不再自动重试）。
    fn give_up_service(&mut self, reason: &str) {
        self.phase = Phase::Failed;
        self.reason = Some(reason.to_string());
        self.retry_at = None;
        self.note_failure_text(
            ReasonCode::CrashLoop,
            "连续失败，已经停止自动重试；点启动可以再试一次".into(),
        );
        self.log_line("[看护] 连续失败，停止自动重试（你可以点启动再试一次）");
        self.reporter.report(
            LogLevel::Warn,
            TAG_SERVICE,
            &format!("服务「{}」连续失败，已停止自动重试", self.spec.name),
        );
    }

    fn spawn(&mut self) -> io::Result<u32> {
        let spec = self
            .spec
            .launch
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "这台服务没有配置启动方式"))?;
        if spec.exe.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "这台服务没有填启动程序",
            ));
        }

        let (program, args) = build_command(spec);
        // 服务日志封顶的唯一安全窗口：此刻没有子进程持有文件句柄，轮转才真的有效。
        // （运行中改名，旧写句柄还指着同一个文件对象；截断则形成稀疏空洞文件——都不能做。）
        let _ = rotate_if_needed(&self.log_path, SERVICE_LOG_MAX_BYTES, SERVICE_LOG_BACKUPS);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .creation_flags(CREATE_NO_WINDOW);
        if let Some(dir) = spec.workdir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            cmd.current_dir(dir);
        }

        // 先建好保活作业对象再启动进程：作业创建失败就直接返回，不启动、不泄漏子进程。
        // （放进保活作业后，应用退出/崩溃不会连带结束本机服务；显式停止时靠
        //   job.terminate() 结束整棵树，含 .bat/.ps1 的子孙进程。）
        let job = Job::new_keep_alive()?;

        let child = cmd
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("启动 {program} 失败：{e}")))?;

        if let Err(e) = job.assign_process(child.as_raw_handle()) {
            self.reporter.report(
                LogLevel::Warn,
                TAG_SERVICE,
                &format!(
                    "服务「{}」放进保活作业失败：停止时可能清不干净（{e}）",
                    self.spec.name
                ),
            );
        }

        let pid = child.id();
        self.started_at = Some(Instant::now());
        self.phase = Phase::Starting;
        self.claim_deadline = None;
        self.hang_handled = false;
        self.reason = None;
        // 刚才在 spawn 前轮转过：文件已经从头开始，超限标记清掉。
        self.log_overflowed = false;
        self.log_check_at = None;
        self.child = Some(child);
        self.job = Some(job);
        Ok(pid)
    }

    /// 假死处理：进程活着但端口一直不通（连续 3 次探测失败 + 过了启动宽限期）。
    /// - 勾了看护：算一次失败，先停掉它再按退避重启；失败到上限就停下报信。
    /// - 没勾看护：只报警（写 reason + 日志），不自动动它。
    fn handle_hang(&mut self) {
        self.fail_streak += 1;
        let secs = self.started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        self.log_line(&format!(
            "[看护] 进程活着但端口一直不通（假死，已 {} 秒），连续第 {} 次",
            secs, self.fail_streak
        ));
        self.note_failure_text(
            ReasonCode::Hung,
            "进程活着但端口不通（假死）".into(),
        );

        if self.spec.watch && !self.user_stopped {
            self.kill_child();
            if self.fail_streak >= MAX_FAILS {
                self.give_up_service("连续假死，已经停止自动重试（你可以点启动再试一次）");
            } else {
                let wait = self.backoff;
                self.retry_at = Some(Instant::now() + wait);
                self.phase = Phase::Starting;
                self.log_line(&format!("[看护] 假死，先停掉它，{} 秒后重启", wait.as_secs()));
                self.backoff = std::cmp::min(self.backoff * 2, BACKOFF_MAX);
            }
        } else {
            // 看护没开：报警，但不自动动用户/我们起的进程
            self.phase = Phase::NotListening;
            self.reason = Some("进程活着但端口不通（假死）；看护没开，可以点启动重试".into());
        }
    }

    /// 只结束我们自己启动的那个进程树。
    ///
    /// 先 `job.terminate()` 把作业内整棵树（包装脚本的子孙进程也在内）结束，
    /// 再 `child.kill + wait` 兜底（作业分配失败或刚退出的情况）。
    fn kill_child(&mut self) {
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn log_line(&self, text: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            // 程序自己写的行带 `[<epoch-ms>] ` 前缀（D0 第 14 条 / D2 §5.4）；
            // 服务进程原文由 Stdio 直接写文件，一个字节都不动。
            let _ = writeln!(f, "{}{text}", crate::log::epoch_prefix());
        }
    }
}

impl Drop for LocalRunner {
    fn drop(&mut self) {
        // 应用退出/重启不杀本机服务（保活）：只释放句柄，进程继续跑。
        // 显式停止（stop_now）/删除服务（guardian 先 stop）走 kill_child()。
    }
}

/// 服务日志是不是超了上限（抽成纯函数方便单测）。
fn check_log_overflow(path: &Path) -> bool {
    size_of(path) > SERVICE_LOG_MAX_BYTES
}

/// 按文件类型决定怎么启动：.bat/.cmd 走 cmd，.ps1 走 powershell，.sh 走 bash。
fn build_command(spec: &LaunchSpec) -> (String, Vec<String>) {
    let exe = spec.exe.trim().to_string();
    let args = split_args(&spec.args);
    let lower = exe.to_ascii_lowercase();

    if lower.ends_with(".bat") || lower.ends_with(".cmd") {
        let mut all = vec!["/C".to_string(), exe];
        all.extend(args);
        ("cmd.exe".to_string(), all)
    } else if lower.ends_with(".ps1") {
        let mut all = vec![
            "-NoProfile".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
            exe,
        ];
        all.extend(args);
        ("powershell.exe".to_string(), all)
    } else if lower.ends_with(".sh") {
        let mut all = vec![exe];
        all.extend(args);
        (bash_path(), all)
    } else {
        (exe, args)
    }
}

/// 给人看的「实际会执行的命令行」预览：走和启动完全一样的分支（.bat/.cmd → cmd /C、
/// .ps1 → powershell、.sh → bash、其余直接执行），参数按 MSVCRT 规则引号化。
///
/// **仅供显示**：这里返回的字符串不参与启动，真正启动仍走 `build_command` + `Command`。
pub fn preview_launch(exe: &str, args: &str) -> String {
    let spec = LaunchSpec {
        exe: exe.trim().to_string(),
        args: args.to_string(),
        workdir: None,
    };
    let (program, argv) = build_command(&spec);
    let mut parts = vec![quote_arg(&program)];
    parts.extend(argv.iter().map(|arg| quote_arg(arg)));
    parts.join(" ")
}

/// Git Bash 的位置（找不到就让系统自己找 bash）。
fn bash_path() -> String {
    for p in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        if Path::new(p).exists() {
            return p.to_string();
        }
    }
    "bash".to_string()
}

/// 把一行参数拆成数组：按 MSVCRT（CommandLineToArgvW / VC++ 启动代码）的规则解析。
///
/// 覆盖：双引号包空格、空参数 `""`、反斜杠转义 `\"`、尾部反斜杠、连续引号、中文路径。
/// 引号里的空白不当分隔符；只有空格和制表符算分隔符（和 MSVCRT 一致）。
pub fn split_args(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        // 参数之间的空白
        while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let mut current = String::new();
        let mut in_quotes = false;
        while i < chars.len() {
            let ch = chars[i];
            if ch == '\\' {
                // 数连续反斜杠，看后面是不是引号
                let mut backslashes = 0usize;
                while i < chars.len() && chars[i] == '\\' {
                    backslashes += 1;
                    i += 1;
                }
                if i < chars.len() && chars[i] == '"' {
                    for _ in 0..backslashes / 2 {
                        current.push('\\');
                    }
                    if backslashes % 2 == 1 {
                        // 奇数个反斜杠 + 引号 = 字面引号
                        current.push('"');
                    } else {
                        in_quotes = !in_quotes;
                    }
                    i += 1;
                } else {
                    for _ in 0..backslashes {
                        current.push('\\');
                    }
                }
            } else if ch == '"' {
                if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                    // 引号里的连续两个引号 = 一个字面引号
                    current.push('"');
                    i += 2;
                } else {
                    in_quotes = !in_quotes;
                    i += 1;
                }
            } else if (ch == ' ' || ch == '\t') && !in_quotes {
                break;
            } else {
                current.push(ch);
                i += 1;
            }
        }
        out.push(current);
    }
    out
}

/// 把参数数组按 MSVCRT 规则拼回一行（`split_args` 的逆运算），空格分隔。
pub fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 单个参数引号化：只在必要时加引号，按 MSVCRT 规则转义反斜杠/引号，
/// 保证再经 [`split_args`] 解析能还原。
fn quote_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '"'));
    if !needs_quotes {
        return arg.to_string();
    }

    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    // 结尾的反斜杠要加倍，否则会把“收尾引号”转义掉
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

/// runner 日志文件路径：key 用服务 id（没传就用本机端口兜底）。
/// 跟服务名无关，所以改名不换日志文件。
fn log_path_for(log_dir: &Path, key: &str) -> PathBuf {
    log_dir.join(format!("service-{}.log", safe_name(key)))
}

fn safe_name(name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::NullReporter;
    use crate::probe::{ProbeOutcome, PROBE_FAILS_BEFORE_DOWN};

    fn null_reporter() -> Arc<dyn Reporter> {
        Arc::new(NullReporter)
    }

    /// 喂 n 次探测失败（模拟连续 3 次探不到端口）。
    fn feed_down(probe: &ProbeSlot, n: u32) {
        for i in 0..n {
            probe.feed(ProbeOutcome::Down {
                error: format!("第 {} 次", i + 1),
            });
        }
    }

    #[test]
    fn args_split_respects_quotes() {
        assert_eq!(
            split_args(r#"--port 8188 --dir "D:\my app\data""#),
            vec!["--port", "8188", "--dir", r"D:\my app\data"]
        );
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("a  b"), vec!["a", "b"]);
    }

    #[test]
    fn args_split_msvcrt_edge_cases() {
        // 空参数
        assert_eq!(split_args(r#"--name """#), vec!["--name", ""]);
        // 内嵌引号（反斜杠转义）
        assert_eq!(split_args(r#"a\"b"#), vec!["a\"b"]);
        // 尾部反斜杠路径（偶数个反斜杠 + 收尾引号）
        assert_eq!(split_args(r#""D:\a b\\""#), vec![r"D:\a b\"]);
        // 中文路径（空格在引号里）
        assert_eq!(
            split_args(r#"--dir "C:\中文 目录\x.txt""#),
            vec!["--dir", r"C:\中文 目录\x.txt"]
        );
        // 纯空格 = 没有参数
        assert_eq!(split_args("   "), Vec::<String>::new());
        assert_eq!(split_args(" \t "), Vec::<String>::new());
        // 未闭合的引号：宽松处理，不报错
        assert_eq!(split_args(r#"unclosed "quote here"#), vec!["unclosed", "quote here"]);
        // 引号内的连续两个引号 = 字面引号
        assert_eq!(split_args(r#""a""b""#), vec!["a\"b"]);
    }

    #[test]
    fn args_quote_round_trips() {
        let cases: Vec<Vec<&str>> = vec![
            vec!["--port", "8188", "--dir", r"D:\my app\data"],
            vec!["--name", ""],
            vec!["a\"b"],
            vec![r"D:\a b\"],
            vec![r"C:\中文 目录\x.txt"],
            vec!["a\"b\\c\"", "--x", "y\\"],
            vec!["plain"],
            vec!["two words", "\ttab\t"],
        ];
        for case in cases {
            let owned: Vec<String> = case.iter().map(|s| s.to_string()).collect();
            let line = quote_args(&owned);
            assert_eq!(split_args(&line), owned, "line={line:?}");
        }
    }

    #[test]
    fn preview_launch_shows_effective_command() {
        assert_eq!(
            preview_launch(r"D:\x\start.bat", "--fast"),
            r"cmd.exe /C D:\x\start.bat --fast"
        );
        assert_eq!(
            preview_launch(r"D:\x\run.ps1", ""),
            r"powershell.exe -NoProfile -ExecutionPolicy Bypass -File D:\x\run.ps1"
        );
        assert_eq!(
            preview_launch(r"D:\my app\comfy.exe", r#"--port 8188 --dir "D:\my app\data""#),
            r#""D:\my app\comfy.exe" --port 8188 --dir "D:\my app\data""#
        );
        let sh = preview_launch(r"D:\x\run.sh", "--fast");
        assert!(sh.ends_with(r"run.sh --fast"), "sh={sh}");
        assert!(sh.contains("bash"), "sh={sh}");
    }

    #[test]
    fn handwritten_legacy_dir_arg_keeps_trailing_literal_quote() {
        // 手写老配置 `--dir "D:\data\"`：尾随反斜杠把收尾引号转义成字面引号，
        // 解析结果末尾是字面引号而不是目录分隔符——这是既有语义，用测试锁住。
        assert_eq!(
            split_args(r#"--dir "D:\data\""#),
            vec!["--dir", r#"D:\data""#]
        );
        // 规范写法（quote_args 生成）再经 split_args 必须还原成 D:\data\，往返一致。
        let args = vec![r"D:\data\".to_string()];
        let canonical = quote_args(&args);
        assert_eq!(split_args(&canonical), args, "canonical={canonical:?}");
        // 带空格的路径：quote_args 会把尾部反斜杠加倍保住收尾引号
        let spaced = vec![r"D:\my data\".to_string()];
        let canonical = quote_args(&spaced);
        assert_eq!(canonical, r#""D:\my data\\""#);
        assert_eq!(split_args(&canonical), spaced);
    }

    #[test]
    fn command_by_file_type() {
        let bat = LaunchSpec {
            exe: r"D:\x\start.bat".into(),
            args: "--fast".into(),
            workdir: None,
        };
        let (p, a) = build_command(&bat);
        assert_eq!(p, "cmd.exe");
        assert_eq!(a, vec!["/C", r"D:\x\start.bat", "--fast"]);

        let exe = LaunchSpec {
            exe: r"D:\x\comfy.exe".into(),
            args: String::new(),
            workdir: None,
        };
        let (p, a) = build_command(&exe);
        assert_eq!(p, r"D:\x\comfy.exe");
        assert!(a.is_empty());
    }

    #[test]
    fn status_words_are_ui_friendly() {
        let spec = LocalSpec {
            name: "t".into(),
            local_port: 1,
            probe_kind: ProbeKind::Tcp,
            launch: None,
            watch: false,
        };
        let mut r = LocalRunner::new(spec, &std::env::temp_dir(), null_reporter());
        assert_eq!(r.status_word(), "stopped");
        r.user_stopped = false;
        r.phase = Phase::Running;
        r.probe.feed(ProbeOutcome::TcpOpen);
        assert_eq!(r.status_word(), "running");
        feed_down(&r.probe, PROBE_FAILS_BEFORE_DOWN);
        r.started_at = Some(Instant::now());
        assert_eq!(r.status_word(), "starting");
        r.started_at = Some(Instant::now() - STARTUP_GRACE - Duration::from_secs(1));
        assert_eq!(r.status_word(), "not_listening");
        r.phase = Phase::Failed;
        assert_eq!(r.status_word(), "failed");
    }

    #[test]
    fn claim_recognizes_existing_listener() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let spec = LocalSpec {
            name: "claim".into(),
            local_port: port,
            probe_kind: ProbeKind::Tcp,
            launch: Some(LaunchSpec {
                exe: r"D:\x\start.bat".into(),
                args: String::new(),
                workdir: None,
            }),
            watch: true,
        };
        let mut r = LocalRunner::new(spec, &std::env::temp_dir(), null_reporter());

        assert!(r.claim_if_listening(), "端口在监听时应该认领");
        assert_eq!(r.status_word(), "running");
        assert!(r.pid().is_none(), "认领的进程不是本会话自己起的");
        assert!(!r.user_stopped, "认领后看护必须继续生效");

        // 认领后 poll 不能再拉一个进程（launch 是假路径，真拉会起进程/失败）
        r.poll();
        assert!(r.pid().is_none(), "认领后绝不能重复启动");
    }

    #[test]
    fn claim_reports_false_when_port_free() {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let spec = LocalSpec {
            name: "claim-none".into(),
            local_port: port,
            probe_kind: ProbeKind::Tcp,
            launch: Some(LaunchSpec {
                exe: "x.exe".into(),
                args: String::new(),
                workdir: None,
            }),
            watch: true,
        };
        let mut r = LocalRunner::new(spec, &std::env::temp_dir(), null_reporter());

        assert!(!r.claim_if_listening());
        assert_eq!(r.status_word(), "stopped");
        assert!(r.user_stopped, "没认领时保持“用户没启动过”的状态");
    }

    /// 测试用的“长时间不退出”的假进程。
    fn dummy_child() -> Child {
        Command::new("cmd.exe")
            .args(["/C", "ping", "-n", "20", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("起测试进程")
    }

    fn free_port() -> u16 {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        port
    }

    fn launchable(port: u16, watch: bool) -> LocalSpec {
        LocalSpec {
            name: "t".into(),
            local_port: port,
            probe_kind: ProbeKind::Tcp,
            launch: Some(LaunchSpec {
                exe: "x.exe".into(),
                args: String::new(),
                workdir: None,
            }),
            watch,
        }
    }

    #[test]
    fn claim_window_waits_for_starting_service() {
        let port = free_port();
        // 测试里不跑探测线程，手动喂结果
        let slot = ProbeSlot::new();
        let mut r = LocalRunner::new_with_probe(
            launchable(port, true),
            &std::env::temp_dir(),
            null_reporter(),
            Arc::clone(&slot),
        );
        assert!(!r.claim_if_listening(), "端口没在听，第一次认不到");
        assert!(r.claim_deadline.is_some(), "要开认领窗口接着等");

        // 服务启动完了，端口通了：窗口内下一次 poll 应该认领，而不是再拉一个
        slot.feed(ProbeOutcome::TcpOpen);
        r.poll();
        assert!(r.pid().is_none(), "认领后不能重复启动");
        assert!(r.listening());
        assert_eq!(r.status_word(), "running");
    }

    #[test]
    fn autostart_waits_out_claim_window() {
        let port = free_port();
        let slot = ProbeSlot::new();
        let mut r = LocalRunner::new_with_probe(
            launchable(port, true),
            &std::env::temp_dir(),
            null_reporter(),
            Arc::clone(&slot),
        );
        assert!(!r.claim_if_listening());
        r.start_for_autostart();
        assert!(r.claim_deadline.is_some(), "autostart 不能把认领窗口清掉");
        r.poll();
        assert_eq!(r.fail_streak, 0, "认领窗口里不能抢跑启动（假 exe 也不许被执行）");
        assert!(r.pid().is_none());

        // 窗口过了才走正常启动逻辑（假 exe 会启动失败，算一次失败）
        r.claim_deadline = Some(Instant::now() - Duration::from_millis(1));
        r.poll();
        assert_eq!(r.fail_streak, 1, "窗口结束后要正常尝试启动");
        assert_eq!(
            r.last_failure().map(|f| f.code),
            Some(ReasonCode::StartFail)
        );
    }

    #[test]
    fn hang_requires_three_fails() {
        let port = free_port();
        let slot = ProbeSlot::new();
        let mut r = LocalRunner::new_with_probe(
            launchable(port, true),
            &std::env::temp_dir(),
            null_reporter(),
            Arc::clone(&slot),
        );
        r.user_stopped = false;
        r.phase = Phase::Starting;
        r.child = Some(dummy_child());
        r.started_at = Some(Instant::now() - STARTUP_GRACE - Duration::from_secs(1));

        // 只失败 1、2 次：不能当假死，更不能杀进程
        feed_down(&slot, 1);
        r.poll();
        assert_eq!(r.fail_streak, 0, "1 次失败还不算假死");
        assert!(r.pid().is_some(), "防抖期间不能杀进程");
        feed_down(&slot, 1); // 累计 2 次
        r.poll();
        assert_eq!(r.fail_streak, 0, "2 次失败还不算假死");
        assert!(r.pid().is_some());

        // 第 3 次：判假死，看护先停掉它再安排重启
        feed_down(&slot, 1); // 累计 3 次
        r.poll();
        assert_eq!(r.fail_streak, 1, "假死要算一次失败");
        assert!(r.pid().is_none(), "看护应该先停掉假死的进程");
        assert!(r.retry_at.is_some(), "看护应该安排重启");
        assert!(matches!(r.phase, Phase::Starting));
        assert_eq!(
            r.last_failure().map(|f| f.code),
            Some(ReasonCode::Hung)
        );
    }

    #[test]
    fn hang_without_watch_only_alarms() {
        let port = free_port();
        let slot = ProbeSlot::new();
        let mut r = LocalRunner::new_with_probe(
            launchable(port, false),
            &std::env::temp_dir(),
            null_reporter(),
            Arc::clone(&slot),
        );
        r.user_stopped = false;
        r.phase = Phase::Starting;
        r.child = Some(dummy_child());
        r.started_at = Some(Instant::now() - STARTUP_GRACE - Duration::from_secs(1));

        feed_down(&slot, 3);
        r.poll();
        assert_eq!(r.fail_streak, 1, "假死要算一次失败");
        assert!(r.pid().is_some(), "没勾看护不能自动杀它");
        assert_eq!(r.status_word(), "not_listening");
        assert!(r.reason().unwrap().contains("假死"), "至少要有报警");
        r.kill_child(); // 清理测试进程
    }

    #[test]
    fn probe_down_recorded_after_debounce() {
        let port = free_port();
        let slot = ProbeSlot::new();
        let mut r = LocalRunner::new_with_probe(
            launchable(port, true),
            &std::env::temp_dir(),
            null_reporter(),
            Arc::clone(&slot),
        );
        r.user_stopped = false;
        r.phase = Phase::Starting;
        r.child = Some(dummy_child());
        r.started_at = Some(Instant::now() - STARTUP_GRACE - Duration::from_secs(1));

        feed_down(&slot, 2);
        r.poll();
        assert!(r.last_failure().is_none(), "不到 3 次不记失败");

        feed_down(&slot, 1); // 累计 3 次
        r.poll();
        assert_eq!(
            r.last_failure().map(|f| f.code),
            Some(ReasonCode::Hung)
        );
        r.kill_child();
    }

    #[test]
    fn healthy_clears_last_failure() {
        let port = free_port();
        let slot = ProbeSlot::new();
        let mut r = LocalRunner::new_with_probe(
            launchable(port, false),
            &std::env::temp_dir(),
            null_reporter(),
            Arc::clone(&slot),
        );
        r.note_failure_text(ReasonCode::Exited, "程序退出了".into());
        assert!(r.last_failure().is_some());

        slot.feed(ProbeOutcome::TcpOpen);
        r.poll();
        assert!(r.last_failure().is_none(), "端口通了要清掉上次失败");
        assert!(r.listening());
    }

    #[test]
    fn stop_now_marks_user_stopped_without_failure() {
        let port = free_port();
        let mut r = LocalRunner::new(launchable(port, true), &std::env::temp_dir(), null_reporter());
        r.start_now();
        r.stop_now();
        assert!(r.is_user_stopped());
        assert!(
            r.last_failure().is_none(),
            "用户主动停不是失败，不发原因码也不记 lastFailure"
        );
        assert_eq!(r.status_word(), "stopped");
    }

    #[test]
    fn overflow_flag_set_once() {
        let dir = std::env::temp_dir().join(format!("sx-svc-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 纯函数：临时文件超限 → true
        let path = dir.join("service-x.log");
        assert!(!check_log_overflow(&path), "文件不存在不算超限");
        std::fs::write(&path, vec![b'a'; (SERVICE_LOG_MAX_BYTES + 1) as usize]).unwrap();
        assert!(check_log_overflow(&path), "超上限要报 true");

        // runner 侧：只置位一次，第二次不重复上报
        let port = free_port();
        let mut r = LocalRunner::new(launchable(port, false), &dir, null_reporter());
        r.set_id("x");
        assert_eq!(r.log_path(), path);
        r.log_check_at = None;
        r.check_log_overflow();
        assert!(r.log_overflowed(), "超限后要置位");
        // 第二次调用（内部靠标志去重，不会重复写 app.log）
        r.log_check_at = None;
        r.check_log_overflow();
        assert!(r.log_overflowed());

        // 文件变小（轮转/手删）后自动清
        std::fs::write(r.log_path(), b"small").unwrap();
        r.log_check_at = None;
        r.check_log_overflow();
        assert!(!r.log_overflowed(), "文件小了要自动清标记");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_and_rotate_needs_launch_spec() {
        let port = free_port();
        let mut spec = launchable(port, true);
        spec.launch = None; // 只监控的服务没有「重启」可言
        let mut r = LocalRunner::new(spec, &std::env::temp_dir(), null_reporter());
        r.restart_and_rotate();
        assert!(matches!(r.phase(), Phase::Stopped));
        assert!(r.pid().is_none());
    }

    #[test]
    fn set_spec_swaps_probe_on_port_change() {
        let a = free_port();
        let b = free_port();
        let slot = ProbeSlot::new();
        let mut r = LocalRunner::new_with_probe(
            launchable(a, true),
            &std::env::temp_dir(),
            null_reporter(),
            Arc::clone(&slot),
        );
        let mut next = r.spec().clone();
        next.local_port = b;
        next.probe_kind = ProbeKind::Http;
        r.set_spec(next);
        assert!(!std::ptr::eq(r.probe(), slot.as_ref()), "换端口要换探测槽");

        // 旧槽的探测结果不能影响新端口
        slot.feed(ProbeOutcome::TcpOpen);
        assert!(!r.probe().alive());
        assert_eq!(r.probe().fail_streak(), 0);
    }

    #[test]
    fn log_path_does_not_follow_rename() {
        let mut spec = launchable(65531, false);
        spec.launch = None;
        spec.name = "旧名字".into();
        let mut r = LocalRunner::new(spec, &std::env::temp_dir(), null_reporter());
        let before = r.log_path().to_path_buf();

        let mut renamed = r.spec().clone();
        renamed.name = "新名字".into();
        r.set_spec(renamed);
        assert_eq!(r.log_path(), before, "改名不能换日志文件");

        r.set_id("svc-abc123");
        let id_path = r.log_path().to_path_buf();
        assert!(
            id_path.ends_with("service-svc-abc123.log"),
            "有服务 id 时日志名用 id：{id_path:?}"
        );
        let mut renamed2 = r.spec().clone();
        renamed2.name = "又改名".into();
        r.set_spec(renamed2);
        assert_eq!(r.log_path(), id_path, "按 id 命名后改名同样不换文件");
    }

    #[test]
    fn log_line_gets_epoch_prefix() {
        let dir = std::env::temp_dir().join(format!("sx-epoch-svc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = LocalRunner::new_with_probe(
            launchable(free_port(), false),
            &dir,
            null_reporter(),
            ProbeSlot::new(),
        );
        r.set_id("epoch-test");
        r.log_line("[服务] 测试一行");

        // 落盘格式必须是 `[<epoch-ms>] ` + 原行，正文一个字节不改。
        let text = std::fs::read_to_string(r.log_path()).unwrap();
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
        assert_eq!(&line[close + 2..], "[服务] 测试一行", "正文不能被改：{line:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
