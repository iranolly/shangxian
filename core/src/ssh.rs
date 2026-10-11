//! ssh.exe 参数构造。
//!
//! 规则来自 DESIGN.md §3.3：
//! - 写死系统 ssh 路径（本机有两份 ssh，行为不同，不能靠 PATH 碰运气）。
//! - 每服务一条独立连接，不用连接复用（实测 System32 版开复用会报错）。
//! - ExitOnForwardFailure=yes：远端端口没绑上就直接退出，不装"活着"。
//! - 双向保活：ServerAliveInterval/CountMax。
//! - BatchMode=yes：绝不弹交互提示。

use std::path::{Path, PathBuf};

/// 系统自带 ssh（固定路径，绝不从 PATH 解析）。
pub const DEFAULT_SSH_EXE: &str = r"C:\Windows\System32\OpenSSH\ssh.exe";

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
        "ConnectTimeout=10".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];

    if let Some(key) = &t.identity_file {
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
