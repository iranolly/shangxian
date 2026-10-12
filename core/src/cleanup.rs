//! 远端操作：ssh 执行、残留端口精确清理、密码哈希。
//!
//! 清理纪律（对齐 DESIGN.md §3.2）：
//! - 只清理「占住了本服务注册端口、而且归属能核实的 sshd 会话进程」；
//! - 归属核实 = 进程的属主 uid 就是当前登录账号；核不上只报告、绝不动手；
//! - 绝不按用户名全杀、绝不清理端口段、绝不碰其他服务的端口；
//! - 遇到非 sshd 的占用者，只报告不动手。

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

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
///
/// 走 [`crate::proc::run_capture`]：stdout/stderr 并发读，避免远端命令输出超过管道
/// 缓冲时假超时（D1 §11）。
pub fn run(exec: &SshExec, script: &str, timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(&exec.ssh_exe);
    cmd.args(exec.args_for(script))
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);

    let captured = crate::proc::run_capture(&mut cmd, timeout, crate::proc::DEFAULT_OUTPUT_LIMIT)
        .map_err(|e| crate::proc::spawn_error_with_context(e, "无法启动 ssh"))?;

    if !captured.status.success() && captured.stdout.trim().is_empty() {
        return Err(format!(
            "ssh 退出码 {:?}：{}",
            captured.status.code(),
            captured.stderr.trim()
        ));
    }
    Ok(captured.stdout)
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
    /// 是 sshd，但属主不是当前登录账号：归属核不上，只报告不动手
    OwnerUnverified { pid: u32, comm: String, owner: String },
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
            Self::OwnerUnverified { pid, comm, owner } => format!(
                "端口被 sshd 占着（pid {pid} {comm}，账号 {owner}），但不是当前登录账号的会话，归属核不上，没有动它"
            ),
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
            // 脚本输出：OWNER_UNVERIFIED <pid> <账号> <comm>
            Some("OWNER_UNVERIFIED") if parts.len() >= 3 => {
                return CleanupOutcome::OwnerUnverified {
                    pid: parts[1].parse().unwrap_or(0),
                    owner: parts[2].to_string(),
                    comm: parts[3..].join(" "),
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

/// 远端清理脚本：按端口找持有者，确认是 sshd 会话、而且属主就是当前登录账号，才结束它。
/// 核不上归属（别的账号的会话、uid 读不到）只报告，不动手。
fn cleanup_script(port: u16) -> String {
    format!(
        r#"p={port}
hex=$(printf '%04X' "$p")
inode=$(awk -v h=":$hex" '$2 ~ h "$" && $4 == "0A" {{ print $10; exit }}' /proc/net/tcp /proc/net/tcp6 2>/dev/null)
if [ -z "$inode" ]; then echo PORT_FREE; exit 0; fi
me_uid=$(id -u 2>/dev/null || echo '')
for d in /proc/[0-9]*; do
  if ls -l "$d/fd" 2>/dev/null | grep -q "socket:\[$inode\]"; then
    pid=${{d#/proc/}}
    comm=$(cat "$d/comm" 2>/dev/null)
    case "$comm" in
      sshd*)
        owner_uid=$(stat -c %u "/proc/$pid" 2>/dev/null || echo '?')
        owner_name=$(stat -c %U "/proc/$pid" 2>/dev/null || echo '?')
        if [ -n "$me_uid" ] && [ "$owner_uid" = "$me_uid" ]; then
          if kill "$pid" 2>/dev/null; then echo "KILLED $pid $comm"; else echo "KILL_FAILED $pid"; fi
        else
          echo "OWNER_UNVERIFIED $pid $owner_name $comm"
        fi ;;
      *)
        echo "OWNER_NOT_SSHD $pid $comm" ;;
    esac
    exit 0
  fi
done
echo NO_OWNER"#
    )
}

/// 在本地生成 nginx 用的访问密码哈希（bcrypt），密码不落命令行、不上服务器。
///
/// `exec` 参数保留是为了兼容现有调用方（guardian / app / cli），现在不再使用。
pub fn hash_password(_exec: &SshExec, password: &str) -> Result<String, String> {
    if password.is_empty() {
        return Err("访问密码不能是空的".into());
    }
    if password.len() > 72 {
        return Err("访问密码太长了：bcrypt 只认前 72 个字节，请换短一点的密码".into());
    }
    let hash = bcrypt::hash(password, bcrypt::DEFAULT_COST)
        .map_err(|e| format!("本地生成密码哈希失败：{e}"))?;
    // bcrypt 库默认输出 $2b$；htpasswd / nginx 更常用 $2y$，两者算法一致，只换前缀。
    Ok(hash.replacen("$2b$", "$2y$", 1))
}

/// 把一段文本安全地包进单引号里（给远端 shell 用；域名等外部输入必须过这一道）。
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_exec() -> SshExec {
        SshExec {
            ssh_exe: PathBuf::from("ssh"),
            target: "u@h".into(),
            ssh_port: 22,
            identity_file: None,
        }
    }

    #[test]
    fn hash_password_makes_bcrypt_hash() {
        let hash = hash_password(&fake_exec(), "正确的horse电池").unwrap();
        assert!(hash.starts_with("$2y$"), "应该是 $2y$ bcrypt 哈希：{hash}");
        assert!(bcrypt::verify("正确的horse电池", &hash).unwrap());
        assert!(!bcrypt::verify("别的密码", &hash).unwrap());
    }

    #[test]
    fn hash_password_rejects_empty_and_too_long() {
        assert!(hash_password(&fake_exec(), "").is_err());
        assert!(hash_password(&fake_exec(), &"a".repeat(73)).is_err());
    }

    #[test]
    fn run_reports_missing_ssh_in_plain_words() {
        let exec = SshExec {
            ssh_exe: PathBuf::from("definitely-missing-shangxian-ssh.exe"),
            target: "u@h".into(),
            ssh_port: 22,
            identity_file: None,
        };
        let err = run(&exec, "echo hi", Duration::from_secs(1)).unwrap_err();
        assert!(err.starts_with("无法启动 ssh："), "错误文案：{err}");
    }

    #[test]
    fn parses_owner_unverified() {
        match parse_outcome("OWNER_UNVERIFIED 123 zhangsan sshd\n") {
            CleanupOutcome::OwnerUnverified { pid, comm, owner } => {
                assert_eq!(pid, 123);
                assert_eq!(owner, "zhangsan");
                assert_eq!(comm, "sshd");
            }
            other => panic!("解析错了：{other:?}"),
        }
    }

    #[test]
    fn cleanup_script_checks_owner_and_is_not_mangled() {
        let s = cleanup_script(18080);
        assert!(s.contains("me_uid=$(id -u"), "要先拿到当前 uid：{s}");
        assert!(s.contains("owner_uid=$(stat -c %u"), "要查进程属主：{s}");
        assert!(s.contains("${d#/proc/}"), "花括号不能被误写：{s}");
        assert!(s.contains("OWNER_UNVERIFIED"));
    }
}
