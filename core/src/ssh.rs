//! ssh.exe / ssh-keygen.exe 参数构造。
//!
//! 规则来自 DESIGN.md §3.3 与 D1 §9.7：
//! - 写死系统 ssh 路径（本机有两份 ssh，行为不同，不能靠 PATH 碰运气）。
//! - 每服务一条独立连接，不用连接复用（实测 System32 版开复用会报错）。
//! - ExitOnForwardFailure=yes：远端端口没绑上就直接退出，不装"活着"。
//! - 双向保活：ServerAliveInterval/CountMax + TCPKeepAlive=yes。
//! - BatchMode=yes：绝不弹交互提示。
//! - 只走密钥（PreferredAuthentications=publickey）；指定了密钥文件时再加
//!   IdentitiesOnly=yes，让那把钥匙成为唯一候选（没指定时不能加，否则会禁掉 agent 钥匙）。
//! - `ssh-keygen -R` 忘掉旧指纹（服务器重装/换机器后用）。

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 系统自带 ssh（固定路径，绝不从 PATH 解析）。
pub const DEFAULT_SSH_EXE: &str = r"C:\Windows\System32\OpenSSH\ssh.exe";
/// 重置指纹的执行超时。
pub const FORGET_HOST_KEY_TIMEOUT: Duration = Duration::from_secs(15);
/// 超时后轮询子进程的步长。
const WAIT_STEP: Duration = Duration::from_millis(50);
/// 创建进程时不弹控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一条反向隧道的描述（本机端口 → 云端回环端口）。
#[derive(Debug, Clone)]
pub struct TunnelTarget {
    /// 例：`user@api.example.com` 或 `tun@127.0.0.1`
    pub ssh_target: String,
    /// 服务器 SSH 端口，默认 22。
    pub ssh_port: u16,
    /// 云端回环端口（服务器上监听的那个）。
    pub remote_port: u16,
    /// 本机服务端口。
    pub local_port: u16,
    /// 指定私钥文件（None = 用 ssh 默认密钥）。
    pub identity_file: Option<PathBuf>,
}

/// 生成 ssh.exe 的完整参数列表。
pub fn tunnel_args(t: &TunnelTarget) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-N".into(),
        "-T".into(),
        // 打开调试输出：日志里会出现 "remote forward success"，用它判定“真的连上了”
        "-v".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        "-o".into(),
        "TCPKeepAlive=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        // 隧道只走密钥：避免服务器关掉密码认证时的多余往返和误导性报错
        "-o".into(),
        "PreferredAuthentications=publickey".into(),
    ];

    if let Some(key) = &t.identity_file {
        // 只在指定了密钥文件时加：让这把钥匙成为唯一候选，
        // 避免 ssh-agent 里别的钥匙被反复试导致认证被拒。
        args.push("-o".into());
        args.push("IdentitiesOnly=yes".into());
        args.push("-i".into());
        args.push(key.display().to_string());
    }

    if t.ssh_port != 22 {
        args.push("-p".into());
        args.push(t.ssh_port.to_string());
    }

    args.push("-R".into());
    args.push(format!(
        "127.0.0.1:{}:127.0.0.1:{}",
        t.remote_port, t.local_port
    ));
    args.push(t.ssh_target.clone());
    args
}

/// 默认 ssh 路径（允许被调用方覆盖，例如测试环境）。
pub fn default_ssh_exe() -> PathBuf {
    Path::new(DEFAULT_SSH_EXE).to_path_buf()
}

/// 和 ssh.exe 同目录的 ssh-keygen.exe。
pub fn ssh_keygen_exe() -> PathBuf {
    default_ssh_exe().with_file_name("ssh-keygen.exe")
}

/// `known_hosts` 里这条服务器记录的名字：22 端口是 `host`，其他端口是 `[host]:port`。
pub fn known_hosts_target(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// 纯函数：`ssh-keygen -R` 的参数（方便单测）。
pub fn forget_host_key_args(host: &str, port: u16, known_hosts: &Path) -> Vec<String> {
    vec![
        "-R".into(),
        known_hosts_target(host, port),
        "-f".into(),
        known_hosts.display().to_string(),
    ]
}

/// `%USERPROFILE%\.ssh\known_hosts`。
fn known_hosts_path() -> Result<PathBuf, String> {
    let home = std::env::var("USERPROFILE")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .ok_or_else(|| "无法读取当前用户目录（%USERPROFILE%）；可手动打开 .ssh\\known_hosts 删除对应行。".to_string())?;
    Ok(Path::new(&home).join(".ssh").join("known_hosts"))
}

/// 执行 `ssh-keygen -R`，把这条服务器从 known_hosts 里删掉（哈希过的条目也能删）。
/// 返回人话结果。
pub fn forget_host_key(host: &str, port: u16) -> Result<String, String> {
    let known_hosts = known_hosts_path()?;
    let target = known_hosts_target(host, port);
    let manual_hint = format!(
        "可手动打开 {} 删除 {} 对应行。",
        known_hosts.display(),
        target
    );
    let exe = ssh_keygen_exe();
    if !exe.exists() {
        return Err(format!(
            "找不到 {}。{manual_hint}",
            exe.display()
        ));
    }

    let mut child = Command::new(&exe)
        .args(forget_host_key_args(host, port, &known_hosts))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|err| format!("重置失败：启动 {} 失败（{err}）。{manual_hint}", exe.display()))?;

    // 手动等：std::process 没有 wait_timeout，卡住的 ssh-keygen 不能留成僵尸。
    let deadline = Instant::now() + FORGET_HOST_KEY_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "重置失败：等待 {} 秒后仍未结束，已将其停止。{manual_hint}",
                        FORGET_HOST_KEY_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(WAIT_STEP);
            }
            Err(err) => {
                return Err(format!("重置失败：查看命令结果出错（{err}）。{manual_hint}"));
            }
        }
    };

    // 进程已退出，这里只是把剩余输出读干净（不会阻塞）。
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    if !status.success() {
        let detail = first_non_empty_line(&stderr)
            .or_else(|| first_non_empty_line(&stdout))
            .unwrap_or_else(|| format!("退出码 {}", status.code().unwrap_or(-1)));
        return Err(format!("重置失败：{detail}。{manual_hint}"));
    }

    Ok("已清除旧主机指纹，下次连接会重新记录。".into())
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(identity: Option<&str>) -> TunnelTarget {
        TunnelTarget {
            ssh_target: "user@example.com".into(),
            ssh_port: 22,
            remote_port: 18080,
            local_port: 8188,
            identity_file: identity.map(PathBuf::from),
        }
    }

    #[test]
    fn tunnel_args_include_auth_and_keepalive() {
        let with_key = tunnel_args(&target(Some(r"C:\keys\id_ed25519")));
        assert!(
            with_key.iter().any(|a| a == "PreferredAuthentications=publickey"),
            "默认要带只走密钥：{with_key:?}"
        );
        assert!(
            with_key.iter().any(|a| a == "TCPKeepAlive=yes"),
            "默认要带 TCP 保活：{with_key:?}"
        );
        assert!(
            with_key.iter().any(|a| a == "IdentitiesOnly=yes"),
            "配了密钥时要加 IdentitiesOnly：{with_key:?}"
        );

        let without_key = tunnel_args(&target(None));
        assert!(
            without_key
                .iter()
                .any(|a| a == "PreferredAuthentications=publickey")
        );
        assert!(without_key.iter().any(|a| a == "TCPKeepAlive=yes"));
        assert!(
            !without_key.iter().any(|a| a == "IdentitiesOnly=yes"),
            "没指定密钥时不能加 IdentitiesOnly，否则会禁掉 agent 钥匙：{without_key:?}"
        );
    }

    #[test]
    fn tunnel_args_keep_existing_contract() {
        let args = tunnel_args(&target(Some("key")));
        for want in [
            "-N",
            "-T",
            "-v",
            "BatchMode=yes",
            "ExitOnForwardFailure=yes",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
            "ConnectTimeout=10",
            "StrictHostKeyChecking=accept-new",
            "127.0.0.1:18080:127.0.0.1:8188",
            "user@example.com",
        ] {
            assert!(args.iter().any(|a| a == want), "缺参数 {want}：{args:?}");
        }
    }

    #[test]
    fn forget_host_key_args_quotes_port() {
        let path = Path::new(r"C:\Users\me\.ssh\known_hosts");
        assert_eq!(
            forget_host_key_args("api.example.com", 22, path),
            vec![
                "-R",
                "api.example.com",
                "-f",
                r"C:\Users\me\.ssh\known_hosts"
            ]
        );
        assert_eq!(
            forget_host_key_args("api.example.com", 2222, path),
            vec![
                "-R",
                "[api.example.com]:2222",
                "-f",
                r"C:\Users\me\.ssh\known_hosts"
            ]
        );
    }

    /// 真跑一次 `ssh-keygen -R`（需要系统装好 OpenSSH 客户端）。
    /// 默认不跑，靠 `cargo test -- --ignored` 或现场验收触发。
    #[test]
    #[ignore = "需要系统自带的 ssh-keygen"]
    fn forget_host_key_on_temp_file() {
        let exe = ssh_keygen_exe();
        if !exe.exists() {
            eprintln!("跳过：{} 不存在", exe.display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("sx-known-hosts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let known_hosts = dir.join("known_hosts");
        std::fs::write(
            &known_hosts,
            "10.0.0.9 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITEST\n\
             [10.0.0.9]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITEST\n",
        )
        .unwrap();

        let arg_file = known_hosts.clone();
        let mut child = Command::new(&exe)
            .args(forget_host_key_args("10.0.0.9", 22, &arg_file))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());

        let left = std::fs::read_to_string(&known_hosts).unwrap();
        assert!(!left.contains("10.0.0.9 ssh-ed25519"), "22 端口那条要没了：{left}");
        assert!(
            left.contains("[10.0.0.9]:2222"),
            "别的端口那条不许动：{left}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
