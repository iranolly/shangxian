//! 用密码登录一次，把公钥写到服务器上，以后都走密钥（DESIGN.md 第 10 节）。
//!
//! 流程（小实验 `.setup/keyboot-spike` 验证过）：
//! 生成密钥 → 密码登录 → 幂等写入 authorized_keys → 用密钥登录验证 → 密码丢弃。
//!
//! 密码只在内存里活几秒：不落盘、不进日志、不进命令行参数。

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Handler};
use russh::ChannelMsg;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 配免密的结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    /// 生成/使用的私钥路径
    pub key_path: String,
    /// 公钥那一行（给界面展示）
    pub public_key: String,
    /// 用密钥登录验证是否通过
    pub verified: bool,
}

/// 接受任何主机密钥：等价于 ssh 第一次连接时的 "accept-new"
/// （服务器指纹变了系统 ssh 后面会拦，这里只负责这一次握手）。
struct AcceptAll;

impl Handler for AcceptAll {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        async { Ok(true) }
    }
}

/// 用密码登录一次，把公钥写到服务器上，并用密钥登录验证。
pub fn bootstrap_key(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    key_path: &Path,
) -> Result<Bootstrap, String> {
    let public_key = ensure_key(key_path)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("内部错误（起不了网络运行时）：{e}"))?;
    runtime.block_on(write_public_key(host, port, user, password, &public_key))?;

    if !verify_with_key(host, port, user, key_path)? {
        return Err("公钥写进去了，但用密钥登录还是没成功。请检查服务器上 ~/.ssh 的权限（目录 700、authorized_keys 文件 600）".into());
    }

    Ok(Bootstrap {
        key_path: key_path.display().to_string(),
        public_key,
        verified: true,
    })
}

/// 密钥不存在就生成一把（存在就直接用现成的）。
fn ensure_key(key_path: &Path) -> Result<String, String> {
    let pub_path = PathBuf::from(format!("{}.pub", key_path.display()));
    if key_path.exists() && pub_path.exists() {
        return std::fs::read_to_string(&pub_path)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("读公钥失败：{e}"));
    }
    if let Some(dir) = key_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建密钥目录失败：{e}"))?;
    }

    let keygen = crate::ssh::default_ssh_exe().with_file_name("ssh-keygen.exe");
    let out = Command::new(&keygen)
        .args([
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            &key_path.display().to_string(),
            "-C",
            "shangxian",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("调用 ssh-keygen 失败：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "生成密钥失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    std::fs::read_to_string(&pub_path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("读公钥失败：{e}"))
}

async fn write_public_key(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    pubkey: &str,
) -> Result<(), String> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let mut session = client::connect(config, (host, port), AcceptAll)
        .await
        .map_err(|e| format!("连不上服务器 {host}:{port}（{e}）"))?;

    let auth = session
        .authenticate_password(user, password)
        .await
        .map_err(|e| format!("登录过程中出错：{e}"))?;

    if !auth.success() {
        // russh 没公开这个类型，只能从它的调试输出里读服务器接受哪些方式；
        // 读不到就退回一句通用提示，不影响主流程。
        let allowed = allowed_methods(&format!("{auth:?}"));
        let password_ok = allowed.contains("Password") || allowed.contains("KeyboardInteractive");
        return Err(if password_ok {
            "密码不对，再试一次（或者检查一下登录账号）".into()
        } else if allowed.is_empty() {
            "服务器不接受密码登录。请在服务器上手动配好密钥，或者让管理员打开密码登录".into()
        } else {
            format!(
                "这台服务器不允许用密码登录（它接受的方式：{allowed}）。请在服务器上手动配好密钥"
            )
        });
    }

    // 幂等写入：已经有这一行就不重复加
    let cmd = "sh -c 'umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; \
               KEY=$(cat); grep -qxF \"$KEY\" ~/.ssh/authorized_keys || \
               printf \"%s\\n\" \"$KEY\" >> ~/.ssh/authorized_keys; echo shangxian-wrote'";
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("打不开服务器会话：{e}"))?;
    channel
        .exec(true, cmd)
        .await
        .map_err(|e| format!("执行命令失败：{e}"))?;
    channel
        .data(pubkey.as_bytes())
        .await
        .map_err(|e| format!("发送公钥失败：{e}"))?;
    channel
        .eof()
        .await
        .map_err(|e| format!("结束输入失败：{e}"))?;

    let mut output = String::new();
    let mut exit = None;
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => output.push_str(&String::from_utf8_lossy(&data)),
            ChannelMsg::ExtendedData { data, .. } => {
                output.push_str(&String::from_utf8_lossy(&data))
            }
            ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
            _ => {}
        }
    }
    if exit != Some(0) || !output.contains("shangxian-wrote") {
        return Err(format!(
            "往服务器写公钥没成功（退出码 {exit:?}）：{}",
            output.trim()
        ));
    }

    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "zh")
        .await;
    Ok(())
}

/// 用密钥登录验证一次（走系统 ssh，跟正式隧道同一个程序）。
fn verify_with_key(host: &str, port: u16, user: &str, key_path: &Path) -> Result<bool, String> {
    let out = Command::new(crate::ssh::default_ssh_exe())
        .args([
            "-i",
            &key_path.display().to_string(),
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ConnectTimeout=10",
            "-p",
            &port.to_string(),
            &format!("{user}@{host}"),
            "echo shangxian-ok",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("调用 ssh 验证失败：{e}"))?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("shangxian-ok"))
}

/// 从 russh 的调试输出里抠出「服务器接受哪些登录方式」。
fn allowed_methods(debug: &str) -> String {
    debug
        .split("MethodSet([")
        .nth(1)
        .and_then(|rest| rest.split("])").next())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_allowed_methods_from_debug_output() {
        // 这是靶机上实测到的真实输出
        let debug = "Failure { remaining_methods: MethodSet([PublicKey, KeyboardInteractive]), partial_success: false }";
        assert_eq!(allowed_methods(debug), "PublicKey, KeyboardInteractive");

        let wrong_password = "Failure { remaining_methods: MethodSet([PublicKey, Password, KeyboardInteractive]), partial_success: false }";
        assert_eq!(
            allowed_methods(wrong_password),
            "PublicKey, Password, KeyboardInteractive"
        );

        // 认不出来的格式不要炸
        assert_eq!(allowed_methods("Success"), "");
    }
}
