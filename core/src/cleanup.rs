//! 远端操作：ssh 执行、残留端口精确清理、密码哈希。
//!
//! 清理纪律（对齐 DESIGN.md §3.2）：
//! - 只清理「占住了本服务注册端口的那个 sshd 会话进程」；
//! - 绝不按用户名全杀、绝不清理端口段、绝不碰其他服务的端口；
//! - 遇到非 sshd 的占用者，只报告不动手。

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一条远端 ssh 执行请求。
#[derive(Debug, Clone)]
pub struct SshExec {
    pub ssh_exe: PathBuf,
    pub target: String,
    pub ssh_port: u16,
    pub identity_file: Option<PathBuf>,
}

impl SshExec {
    fn args_for(&self, script: &str) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-T".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=10".into(),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
        ];
        if let Some(key) = &self.identity_file {
            args.push("-i".into());
            args.push(key.display().to_string());
        }
        if self.ssh_port != 22 {
            args.push("-p".into());
            args.push(self.ssh_port.to_string());
        }
        args.push(self.target.clone());
        args.push(script.to_string());
        args
    }
}

/// 执行一段远端脚本，返回 stdout。超时/失败返回 Err。
pub fn run(exec: &SshExec, script: &str, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(&exec.ssh_exe)
        .args(exec.args_for(script))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("无法启动 ssh：{e}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("远端命令超时".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("等待 ssh 失败：{e}")),
        }
    }

    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() && out.trim().is_empty() {
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut err);
        }
        return Err(format!(
            "ssh 退出码 {:?}：{}",
            status.code(),
            err.trim()
        ));
    }
    Ok(out)
}

/// 残留清理请求。
#[derive(Debug, Clone)]
pub struct CleanupSpec {
    pub exec: SshExec,
    pub remote_port: u16,
}

#[derive(Debug)]
pub enum CleanupOutcome {
    /// 已清理占着端口的 sshd 会话
    Killed { pid: u32, comm: String },
    /// 占用者不是 sshd：只报告，不动手
    NotSshd { pid: u32, comm: String },
    /// 端口本来就是空的
    PortFree,
    /// 端口有监听但找不到持有进程
    NoOwner,
    Failed(String),
}

impl CleanupOutcome {
    pub fn human(&self) -> String {
        match self {
            Self::Killed { pid, comm } => {
                format!("已清理占着端口的残留会话（pid {pid} {comm}）")
            }
            Self::NotSshd { pid, comm } => {
                format!("端口被非 ssh 进程占着（pid {pid} {comm}），没有动它")
            }
            Self::PortFree => "远端端口本来就是空闲的".into(),
            Self::NoOwner => "远端端口有监听但找不到持有进程".into(),
            Self::Failed(e) => format!("清理失败：{e}"),
        }
    }
}

pub fn run_cleanup(spec: &CleanupSpec) -> CleanupOutcome {
    match run(
        &spec.exec,
        &cleanup_script(spec.remote_port),
        Duration::from_secs(30),
    ) {
        Ok(out) => parse_outcome(&out),
        Err(e) => CleanupOutcome::Failed(e),
    }
}

fn parse_outcome(out: &str) -> CleanupOutcome {
    for line in out.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.first().copied() {
            Some("KILLED") if parts.len() >= 3 => {
                return CleanupOutcome::Killed {
                    pid: parts[1].parse().unwrap_or(0),
                    comm: parts[2..].join(" "),
                }
            }
            Some("OWNER_NOT_SSHD") if parts.len() >= 3 => {
                return CleanupOutcome::NotSshd {
                    pid: parts[1].parse().unwrap_or(0),
                    comm: parts[2..].join(" "),
                }
            }
            Some("KILL_FAILED") => {
                return CleanupOutcome::Failed(format!(
                    "kill 失败（pid {}）",
                    parts.get(1).unwrap_or(&"?")
                ))
            }
            Some("PORT_FREE") => return CleanupOutcome::PortFree,
            Some("NO_OWNER") => return CleanupOutcome::NoOwner,
            _ => {}
        }
    }
    CleanupOutcome::Failed("清理输出无法识别".into())
}

/// 远端清理脚本：按端口找持有者，确认是 sshd 会话才结束它。
fn cleanup_script(port: u16) -> String {
    format!(
        r#"p={port}
hex=$(printf '%04X' "$p")
inode=$(awk -v h=":$hex" '$2 ~ h "$" && $4 == "0A" {{ print $10; exit }}' /proc/net/tcp /proc/net/tcp6 2>/dev/null)
if [ -z "$inode" ]; then echo PORT_FREE; exit 0; fi
for d in /proc/[0-9]*; do
  if ls -l "$d/fd" 2>/dev/null | grep -q "socket:\[$inode\]"; then
    pid=${{d#/proc/}}
    comm=$(cat "$d/comm" 2>/dev/null)
    case "$comm" in
      sshd*)
        if kill "$pid" 2>/dev/null; then echo "KILLED $pid $comm"; else echo "KILL_FAILED $pid"; fi ;;
      *)
        echo "OWNER_NOT_SSHD $pid $comm" ;;
    esac
    exit 0
  fi
done
echo NO_OWNER"#
    )
}

/// 在服务器上生成 nginx 用的 apr1 密码哈希（密码走 stdin，不落命令行）。
pub fn hash_password(exec: &SshExec, password: &str) -> Result<String, String> {
    let script = format!(
        "printf '%s' {} | openssl passwd -apr1 -stdin",
        shell_quote(password)
    );
    let out = run(exec, &script, Duration::from_secs(20))?;
    let hash = out.trim().to_string();
    if hash.starts_with("$apr1$") {
        Ok(hash)
    } else {
        Err(format!("服务器返回了意外的结果：{hash}"))
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
