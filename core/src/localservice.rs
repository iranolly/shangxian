//! 本机服务：启动、停止、看护、端口探测。
//!
//! 纪律（对齐 DESIGN.md §9.4）：
//! - 只停「我们自己启动的」进程：握着子进程句柄去结束，不存在 pid 复用误杀；
//!   用户自己手动开的进程绝不碰。
//! - 用户手动点了停止，看护不再拉起（除非用户再点启动）。
//! - 起来后必须端口探测通过才算「在跑」（进程活着 ≠ 服务可用）。
//! - 连续失败到上限就停下报信，不无限重试刷屏。

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::os::windows::process::CommandExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::LaunchSpec;
use crate::job::Job;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 端口探测间隔
const PROBE_EVERY: Duration = Duration::from_secs(2);
/// 启动后多久还没探测到端口，就从「正在启动」变成「进程在但端口不通」
const STARTUP_GRACE: Duration = Duration::from_secs(15);
/// 连续失败这么多次就停下（不再自动重试）
const MAX_FAILS: u32 = 5;
/// 重试退避：2 秒 → 30 秒
const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

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

/// 一台本机服务的运行时信息。
#[derive(Debug, Clone)]
pub struct LocalSpec {
    pub name: String,
    pub local_port: u16,
    /// None = 只监控不启动
    pub launch: Option<LaunchSpec>,
    pub watch: bool,
}

pub struct LocalRunner {
    spec: LocalSpec,
    child: Option<Child>,
    job: Option<Job>,
    phase: Phase,
    /// 端口探测结果
    listening: bool,
    last_probe: Option<Instant>,
    /// 本次启动的时刻（判断是否还在「正在启动」宽限期）
    started_at: Option<Instant>,
    /// 用户手动停过：看护不拉起
    user_stopped: bool,
    /// 连续失败次数（进程活不过 3 秒就算失败）
    fail_streak: u32,
    backoff: Duration,
    retry_at: Option<Instant>,
    reason: Option<String>,
    log_path: PathBuf,
}

impl LocalRunner {
    pub fn new(spec: LocalSpec, log_dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(log_dir);
        let log_path = log_dir.join(format!("service-{}.log", safe_name(&spec.name)));
        Self {
            spec,
            child: None,
            job: None,
            phase: Phase::Stopped,
            listening: false,
            last_probe: None,
            started_at: None,
            user_stopped: true, // 应用启动时不会自动拉起；开机自启由 guardian 统一触发
            fail_streak: 0,
            backoff: BACKOFF_BASE,
            retry_at: None,
            reason: None,
            log_path,
        }
    }

    pub fn spec(&self) -> &LocalSpec {
        &self.spec
    }

    pub fn set_spec(&mut self, spec: LocalSpec) {
        self.spec = spec;
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn listening(&self) -> bool {
        self.listening
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
                if self.listening {
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
        if self.child.is_none() {
            self.retry_at = Some(Instant::now()); // 下一轮 poll 立刻拉起
        }
    }

    /// 用户点「停止」：只停我们自己启动的那个进程。
    pub fn stop_now(&mut self) {
        self.user_stopped = true;
        self.kill_child();
        self.listening = false;
        self.phase = Phase::Stopped;
        self.retry_at = None;
        self.reason = None;
        self.log_line("[服务] 已按你的要求停止");
    }

    /// 开机自启：由应用启动时统一触发。
    pub fn start_for_autostart(&mut self) {
        self.start_now();
    }

    /// 周期调用：推进状态机。
    pub fn poll(&mut self) {
        // 只监控的服务（没配启动方式）：状态完全看端口通不通——
        // 用户自己手动开的服务也要能看出“在跑”。
        if self.spec.launch.is_none() {
            let due = self
                .last_probe
                .map(|t| t.elapsed() >= PROBE_EVERY)
                .unwrap_or(true);
            if due {
                self.last_probe = Some(Instant::now());
                self.listening = probe(self.spec.local_port);
            }
            self.phase = if self.listening {
                Phase::Running
            } else {
                Phase::Stopped
            };
            return;
        }

        // 1) 有子进程：看它还在不在，顺手探测端口
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let uptime = self.started_at.map(|t| t.elapsed()).unwrap_or_default();
                    self.child = None;
                    self.job = None; // 进程已退出，作业里没有活物
                    self.listening = false;
                    self.log_line(&format!(
                        "[看护] 进程退出了（退出码 {}，活了 {} 秒）",
                        status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "未知".into()),
                        uptime.as_secs()
                    ));

                    // 活不到 3 秒算失败（启动就崩），连续失败到上限就停手
                    if uptime < Duration::from_secs(3) {
                        self.fail_streak += 1;
                        if self.fail_streak >= MAX_FAILS {
                            self.phase = Phase::Failed;
                            self.reason = Some("连续启动失败，已经停止自动重试".into());
                            self.log_line("[看护] 连续失败，停止自动重试（你可以点启动再试一次）");
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
                    // 进程活着：定期探测端口
                    let due = self
                        .last_probe
                        .map(|t| t.elapsed() >= PROBE_EVERY)
                        .unwrap_or(true);
                    if due {
                        self.last_probe = Some(Instant::now());
                        self.listening = probe(self.spec.local_port);
                    }
                    self.phase = if self.listening {
                        Phase::Running
                    } else {
                        Phase::NotListening
                    };
                    if self.listening {
                        self.fail_streak = 0;
                        self.reason = None;
                    }
                }
                Err(err) => {
                    self.reason = Some(format!("查看进程状态失败：{err}"));
                }
            }
            return;
        }

        // 2) 没子进程：到点就拉起（看护 或 用户刚点了启动）
        let due = match self.retry_at {
            Some(at) => Instant::now() >= at,
            None => false,
        };
        let should_run = !self.user_stopped && (self.spec.watch || matches!(self.phase, Phase::Starting));
        if due && should_run && self.spec.launch.is_some() {
            self.retry_at = None;
            match self.spawn() {
                Ok(pid) => {
                    self.log_line(&format!("[服务] 已启动，进程号 {pid}"));
                }
                Err(err) => {
                    self.fail_streak += 1;
                    self.reason = Some(format!("启动失败：{err}"));
                    self.log_line(&format!("[服务] 启动失败：{err}"));
                    if self.fail_streak >= MAX_FAILS {
                        self.phase = Phase::Failed;
                        self.reason = Some("连续启动失败，已经停止自动重试".into());
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

        let child = cmd
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("启动 {program} 失败：{e}")))?;

        // 放进作业对象：应用没了，它也会被系统一起结束
        let job = Job::new()?;
        if let Err(e) = job.assign_process(child.as_raw_handle()) {
            eprintln!(
                "[warn] 作业对象分配失败（本机服务孤儿保底清理失效）：{} pid={} err={e}",
                self.spec.name,
                child.id()
            );
        }

        let pid = child.id();
        self.started_at = Some(Instant::now());
        self.last_probe = None;
        self.listening = false;
        self.phase = Phase::Starting;
        self.child = Some(child);
        self.job = Some(job);
        Ok(pid)
    }

    /// 只结束我们自己启动的那个进程。
    fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.job = None;
    }

    fn log_line(&self, text: &str) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(f, "{text}");
        }
    }
}

impl Drop for LocalRunner {
    fn drop(&mut self) {
        self.kill_child();
    }
}

/// 探测本机端口通不通。
fn probe(port: u16) -> bool {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    TcpStream::connect_timeout(&addr.into(), Duration::from_millis(400)).is_ok()
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

/// 把一行参数拆成数组：双引号包住的空格不当分隔符。
fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for ch in text.chars() {
        match ch {
            '"' => in_quote = !in_quote,
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
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
            launch: None,
            watch: false,
        };
        let mut r = LocalRunner::new(spec, &std::env::temp_dir());
        assert_eq!(r.status_word(), "stopped");
        r.user_stopped = false;
        r.phase = Phase::Running;
        r.listening = true;
        assert_eq!(r.status_word(), "running");
        r.listening = false;
        r.started_at = Some(Instant::now());
        assert_eq!(r.status_word(), "starting");
        r.started_at = Some(Instant::now() - STARTUP_GRACE - Duration::from_secs(1));
        assert_eq!(r.status_word(), "not_listening");
        r.phase = Phase::Failed;
        assert_eq!(r.status_word(), "failed");
    }
}
