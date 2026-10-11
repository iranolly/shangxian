//! 本机服务：启动、停止、看护、端口探测。
//!
//! 纪律（对齐 DESIGN.md §9.4）：
//! - 应用退出/重启/崩溃不结束本机服务（保活）：启动进保活作业，下次启动按端口认领；
//!   只有显式停止/删除（看护以外的路径）才结束整棵树。
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

    /// 应用启动后的认领：端口已经在监听、本会话又没有自己的子进程时，把它认成
    /// 「在跑」（可能是上个会话留下的，也可能是用户自己开的），绝不重复启动。
    ///
    /// 认领后 `user_stopped = false`：勾了看护的服务以后掉了，watch 会照常拉起。
    /// 返回是否认领成功。
    pub fn claim_if_listening(&mut self) -> bool {
        if self.child.is_some() {
            return false; // 本会话自己起的，不需要认领
        }
        if !probe(self.spec.local_port) {
            return false;
        }
        self.listening = true;
        self.phase = Phase::Running;
        self.user_stopped = false;
        self.fail_streak = 0;
        self.reason = None;
        self.retry_at = None;
        self.last_probe = Some(Instant::now());
        self.log_line("[服务] 端口已经在监听，已认领（不会重复启动）");
        true
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

        // 2) 没子进程：先看端口（可能是用户自己开的，也可能我们起的进程已经退出了）
        if self.child.is_none() {
            let due_probe = self
                .last_probe
                .map(|t| t.elapsed() >= PROBE_EVERY)
                .unwrap_or(true);
            if due_probe {
                self.last_probe = Some(Instant::now());
                let was_listening = self.listening;
                self.listening = probe(self.spec.local_port);
                if self.listening {
                    // 它在跑（不管是用户自己开的还是我们起的）：状态对齐，清掉重试计划
                    self.phase = Phase::Running;
                    self.reason = None;
                    self.fail_streak = 0;
                    self.retry_at = None;
                } else {
                    if matches!(self.phase, Phase::Running | Phase::NotListening) {
                        self.phase = Phase::Stopped;
                    }
                    // 刚刚从“在听”变成“没在听”：它掉了 → 看护负责拉起来
                    if was_listening
                        && self.spec.watch
                        && !self.user_stopped
                        && !matches!(self.phase, Phase::Failed)
                    {
                        self.log_line("[看护] 端口不通了（服务掉了），马上拉起来");
                        self.retry_at = Some(Instant::now());
                        self.phase = Phase::Starting;
                    }
                }
            }
        }

        // 3) 到点就拉起（看护 或 用户刚点了启动）
        let due = match self.retry_at {
            Some(at) => Instant::now() >= at,
            None => false,
        };
        let should_run = !self.user_stopped && (self.spec.watch || matches!(self.phase, Phase::Starting));
        if due && should_run && self.spec.launch.is_some() {
            // 先看端口：已经在听说明它早就在跑了（比如用户自己开的，或者上次留下的），
            // 这时千万别再起一个（两个进程抢同一个端口）。
            if probe(self.spec.local_port) {
                self.retry_at = None;
                self.listening = true;
                self.phase = Phase::Running;
                self.reason = None;
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

        // 先建好保活作业对象再启动进程：作业创建失败就直接返回，不启动、不泄漏子进程。
        // （放进保活作业后，应用退出/崩溃不会连带结束本机服务；显式停止时靠
        //   job.terminate() 结束整棵树，含 .bat/.ps1 的子孙进程。）
        let job = Job::new_keep_alive()?;

        let child = cmd
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("启动 {program} 失败：{e}")))?;

        if let Err(e) = job.assign_process(child.as_raw_handle()) {
            eprintln!(
                "[warn] 作业对象分配失败（显式停止时整棵树可能清不干净）：{} pid={} err={e}",
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
            let _ = writeln!(f, "{text}");
        }
    }
}

impl Drop for LocalRunner {
    fn drop(&mut self) {
        // 应用退出/重启不杀本机服务（保活）：只释放句柄，进程继续跑。
        // 显式停止（stop_now）/删除服务（guardian 先 stop）走 kill_child()。
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

    #[test]
    fn claim_recognizes_existing_listener() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let spec = LocalSpec {
            name: "claim".into(),
            local_port: port,
            launch: Some(LaunchSpec {
                exe: r"D:\x\start.bat".into(),
                args: String::new(),
                workdir: None,
            }),
            watch: true,
        };
        let mut r = LocalRunner::new(spec, &std::env::temp_dir());

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
            launch: Some(LaunchSpec {
                exe: "x.exe".into(),
                args: String::new(),
                workdir: None,
            }),
            watch: true,
        };
        let mut r = LocalRunner::new(spec, &std::env::temp_dir());

        assert!(!r.claim_if_listening());
        assert_eq!(r.status_word(), "stopped");
        assert!(r.user_stopped, "没认领时保持“用户没启动过”的状态");
    }
}
