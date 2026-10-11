//! 单条隧道的看护状态机。
//!
//! 行为（对齐 DESIGN.md §3.3「重连守护」）：
//! - 启动：spawn ssh.exe → 放进独立作业对象（防孤儿）。
//! - 退出检测：轮询 `try_wait()`；退出后按退避表安排重试。
//! - 退避：2s 起步，翻倍，封顶 30s；跑满 `stable_after`（默认 60s）后重置为 2s。
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
use std::time::{Duration, Instant};

use crate::job::Job;

/// 创建进程时不弹控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一条隧道的启动参数。
#[derive(Debug, Clone)]
pub struct TunnelSpec {
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
}

/// 状态机产生的事件（日志 / 通知 / 界面刷新用）。
#[derive(Debug, Clone)]
pub enum Event {
    Started { attempt: u32, pid: u32 },
    /// 本次进程退出，以及多久后重试。
    Exited {
        attempt: u32,
        exit_code: Option<u32>,
        uptime: Duration,
        retry_in: Duration,
        reset_backoff: bool,
    },
    /// 启动失败（例如 ssh.exe 找不到）。
    SpawnFailed { error: String, retry_in: Duration },
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
    /// 本次启动时日志文件的大小（只扫这之后的新内容）
    log_scan_from: u64,
    /// 需要做一次远端端口清理
    cleanup_pending: bool,
    /// 正在跑的清理任务
    cleanup_rx: Option<std::sync::mpsc::Receiver<crate::cleanup::CleanupOutcome>>,
    /// 上次清理时间（限流：每分钟最多一次）
    last_cleanup_at: Option<Instant>,
    phase: Phase,
}

impl Tunnel {
    pub fn new(spec: TunnelSpec, log_dir: impl Into<PathBuf>) -> Self {
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
            log_scan_from: 0,
            cleanup_pending: false,
            cleanup_rx: None,
            last_cleanup_at: None,
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

    /// 周期调用（建议 100–250ms）：推进状态机并返回发生的事件。
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if matches!(self.phase, Phase::Stopped) {
            return events;
        }

        // 1) 有子进程：检查是否退出。
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let uptime = self.started_at.map(|t| t.elapsed()).unwrap_or_default();
                    let reset_backoff = uptime >= self.spec.stable_after;
                    let previous_attempt = self.attempt;

                    self.child = None;
                    self.job = None; // 句柄关闭；进程已退出，作业里没有活物

                    // 若退出原因是"远端端口被占"，安排一次精确清理（每分钟最多一次）
                    if self.spec.cleanup.is_some()
                        && self.read_log_tail().contains("remote port forwarding failed")
                    {
                        let due = self
                            .last_cleanup_at
                            .map(|t| t.elapsed() >= Duration::from_secs(60))
                            .unwrap_or(true);
                        if due {
                            self.cleanup_pending = true;
                        }
                    }

                    if reset_backoff {
                        self.backoff = self.spec.retry_base;
                    }
                    let retry_in = self.backoff;
                    self.retry_at = Some(Instant::now() + retry_in);
                    self.phase = Phase::Backoff {
                        until: Instant::now() + retry_in,
                    };

                    events.push(Event::Exited {
                        attempt: previous_attempt,
                        exit_code: status.code().map(|c| c as u32),
                        uptime,
                        retry_in,
                        reset_backoff,
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
                    }
                }
                Err(err) => {
                    events.push(Event::Exited {
                        attempt: self.attempt,
                        exit_code: None,
                        uptime: self.started_at.map(|t| t.elapsed()).unwrap_or_default(),
                        retry_in: self.backoff,
                        reset_backoff: false,
                    });
                    self.child = None;
                    self.job = None;
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
                    self.log_line(&format!("[清理] {}", outcome.human()));
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

        match self.spawn() {
            Ok(pid) => {
                self.attempt += 1;
                events.push(Event::Started {
                    attempt: self.attempt,
                    pid,
                });
            }
            Err(err) => {
                let retry_in = self.backoff;
                self.retry_at = Some(Instant::now() + retry_in);
                self.phase = Phase::Backoff {
                    until: Instant::now() + retry_in,
                };
                self.backoff = std::cmp::min(self.backoff * 2, self.spec.retry_max);
                self.spawn_failed = true;
                events.push(Event::SpawnFailed {
                    error: err.to_string(),
                    retry_in,
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
        self.phase = Phase::Stopped;
    }

    fn spawn(&mut self) -> io::Result<u32> {
        let job = Job::new()?;

        let log_path = self.log_path();
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| io::Error::new(e.kind(), format!("打开日志失败: {e}")))?;
        // 只扫本次启动之后的新日志（避免把上次连接的成功当成本次）
        self.log_scan_from = log.metadata().map(|m| m.len()).unwrap_or(0);
        self.connected = false;
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
            eprintln!(
                "[warn] 作业对象分配失败（孤儿保底清理失效）：{name} pid={pid} err={e}",
                name = self.spec.name,
                pid = child.id()
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
        service_log_path(&self.log_dir, &self.spec.name)
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
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 某个服务的 ssh 日志文件路径（界面看日志、清理读原因都用它）。
pub fn service_log_path(log_dir: &std::path::Path, name: &str) -> PathBuf {
    log_dir.join(format!("{}.ssh.log", sanitize(name)))
}

/// 方便调用方判断某路径是不是默认 ssh。
pub fn is_default_ssh(path: &Path) -> bool {
    path.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(crate::ssh::DEFAULT_SSH_EXE)
}
