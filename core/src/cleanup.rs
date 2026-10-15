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
                format!("已清理占用端口的残留会话（pid {pid} {comm}）")
            }
            Self::NotSshd { pid, comm } => {
                format!("端口被非 ssh 进程占用（pid {pid} {comm}），未做处理")
            }
            Self::OwnerUnverified { pid, comm, owner } => format!(
                "端口被 sshd 占用（pid {pid} {comm}，账号 {owner}），但不属于当前登录账号的会话，无法确认归属，未做处理"
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
/// 服务器的 nginx 验不了 bcrypt 时改用 [`hash_password_apr1`]（自适应入口见 app 的
/// `hash_password_adaptive`）。
pub fn hash_password(_exec: &SshExec, password: &str) -> Result<String, String> {
    if password.is_empty() {
        return Err("访问密码不能为空".into());
    }
    if password.len() > 72 {
        return Err("访问密码过长：bcrypt 只识别前 72 个字节，请更换更短的密码".into());
    }
    let hash = bcrypt::hash(password, bcrypt::DEFAULT_COST)
        .map_err(|e| format!("本地生成密码哈希失败：{e}"))?;
    // bcrypt 库默认输出 $2b$；htpasswd / nginx 更常用 $2y$，两者算法一致，只换前缀。
    Ok(hash.replacen("$2b$", "$2y$", 1))
}

/// apr1 的字符表（md5crypt / nginx 内置实现用的同一套）。
const APR1_ITOA64: &[u8; 64] =
    b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 在本地生成 nginx 用的 apr1 兼容哈希（`$apr1$…`），密码同样只在本机算、不落盘。
///
/// 用在「服务器的 nginx 验不了 bcrypt」的场合：apr1 是 nginx **内置**支持的格式，
/// 不依赖服务器上的加密库，任何版本都能读（小皮这类自带旧库的服务器就靠它兜底）。
pub fn hash_password_apr1(password: &str) -> Result<String, String> {
    if password.is_empty() {
        return Err("访问密码不能为空".into());
    }
    // 8 位随机盐（256 % 64 == 0，取模无偏）
    let raw: [u8; 8] = rand::random();
    let salt: String = raw
        .iter()
        .map(|b| APR1_ITOA64[*b as usize % APR1_ITOA64.len()] as char)
        .collect();
    Ok(apr1_hash(password, &salt))
}

/// 按 md5crypt/apr1 算法算哈希（盐由调用方给出；nginx 与 openssl 的算法一致）。
fn apr1_hash(password: &str, salt: &str) -> String {
    use md5::Context;

    let pw = password.as_bytes();
    let salt = &salt.as_bytes()[..salt.len().min(8)];

    let mut ctx = Context::new();
    ctx.consume(pw);
    ctx.consume(&b"$apr1$"[..]);
    ctx.consume(salt);

    let mut ctx1 = Context::new();
    ctx1.consume(pw);
    ctx1.consume(salt);
    ctx1.consume(pw);
    let mut digest = ctx1.compute().0;

    let mut left = pw.len();
    while left > 0 {
        let take = left.min(16);
        ctx.consume(&digest[..take]);
        left -= take;
    }
    let mut bits = pw.len();
    while bits > 0 {
        if bits & 1 != 0 {
            ctx.consume([0u8]);
        } else {
            ctx.consume(&pw[..1]);
        }
        bits >>= 1;
    }
    digest = ctx.compute().0;

    for round in 0..1000u32 {
        let mut c = Context::new();
        if round & 1 != 0 {
            c.consume(pw);
        } else {
            c.consume(&digest[..]);
        }
        if round % 3 != 0 {
            c.consume(salt);
        }
        if round % 7 != 0 {
            c.consume(pw);
        }
        if round & 1 != 0 {
            c.consume(&digest[..]);
        } else {
            c.consume(pw);
        }
        digest = c.compute().0;
    }

    fn to64(out: &mut String, mut v: u32, n: usize) {
        for _ in 0..n {
            out.push(APR1_ITOA64[(v & 0x3f) as usize] as char);
            v >>= 6;
        }
    }

    let mut out = String::from("$apr1$");
    out.push_str(&String::from_utf8_lossy(salt));
    out.push('$');
    to64(&mut out, (digest[0] as u32) << 16 | (digest[6] as u32) << 8 | digest[12] as u32, 4);
    to64(&mut out, (digest[1] as u32) << 16 | (digest[7] as u32) << 8 | digest[13] as u32, 4);
    to64(&mut out, (digest[2] as u32) << 16 | (digest[8] as u32) << 8 | digest[14] as u32, 4);
    to64(&mut out, (digest[3] as u32) << 16 | (digest[9] as u32) << 8 | digest[15] as u32, 4);
    to64(&mut out, (digest[4] as u32) << 16 | (digest[10] as u32) << 8 | digest[5] as u32, 4);
    to64(&mut out, digest[11] as u32, 2);
    out
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

    /// 用 openssl passwd -apr1 生成的固定向量，防止算法实现走样。
    #[test]
    fn apr1_matches_openssl_vectors() {
        assert_eq!(
            apr1_hash("test123", "abcdefgh"),
            "$apr1$abcdefgh$tbBaQr8bvJpOyzoypIHIO0"
        );
        assert_eq!(
            apr1_hash("test123", "ABCDEF12"),
            "$apr1$ABCDEF12$i44n9x.YOEegkvf5K63x3/"
        );
        assert_eq!(
            apr1_hash("test123", "12345678"),
            "$apr1$12345678$1u9kBHN3v4nFFNZAEKFnT1"
        );
        // 非 ASCII 按 UTF-8 字节算（和浏览器提交/nginx 校验一致；向量在 Linux UTF-8 环境用 openssl 生成）
        assert_eq!(
            apr1_hash("正确的horse电池", "xyZ12345"),
            "$apr1$xyZ12345$jtp1DXjccNdD/Ljgyvuvd."
        );
    }

    #[test]
    fn hash_password_apr1_is_well_formed_and_rejects_empty() {
        let hash = hash_password_apr1("正确的horse电池").unwrap();
        assert!(hash.starts_with("$apr1$"), "应该是 apr1 哈希：{hash}");
        assert_eq!(hash.len(), "$apr1$".len() + 8 + 1 + 22, "长度不对：{hash}");
        // 盐是随机生成的：连算两次大概率不同
        let again = hash_password_apr1("正确的horse电池").unwrap();
        assert_ne!(hash, again);
        assert!(hash_password_apr1("").is_err());
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
