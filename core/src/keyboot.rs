//! 用密码登录一次，把公钥写到服务器上，以后都走密钥（DESIGN.md 第 10 节）。
//!
//! 流程（小实验 `.setup/keyboot-spike` 验证过）：
//! 生成密钥 → 密码登录 → 幂等写入 authorized_keys → 用密钥登录验证 → 密码丢弃。
//!
//! 密码只在内存里活几秒：不落盘、不进日志、不进命令行参数。

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
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

/// 主机密钥检查：第一次连到某台服务器就把指纹记到本地文件；以后再连指纹对不上就拒绝。
/// 语义和 ssh 的 `StrictHostKeyChecking=accept-new` 一样：首连信任（TOFU），变更拒绝。
struct HostKeyCheck {
    host: String,
    port: u16,
    /// 指纹记录文件（放私钥旁边，不跟系统 known_hosts 混在一起）
    store: PathBuf,
    /// 拒绝原因（connect 失败后读出来给人话提示）
    reject: Arc<Mutex<Option<String>>>,
}

impl Handler for HostKeyCheck {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};

        let outcome = match check_known_hosts_path(&self.host, self.port, key, &self.store) {
            // 指纹一致：放行
            Ok(true) => Ok(()),
            // 第一次见这台服务器：记下指纹，放行
            Ok(false) => learn_known_hosts_path(&self.host, self.port, key, &self.store)
                .map_err(|e| format!("记录主机指纹失败（{}）：{e}", self.store.display())),
            // 指纹和上次记录的不一样：明确拒绝
            Err(russh::keys::Error::KeyChanged { .. }) => Err(format!(
                "服务器 {}:{} 的主机密钥与上次记录的不一致（记录文件：{}）。\n\
                 这可能是服务器重装或更换了机器，也可能是中间人冒充。为安全起见先停止连接。\n\
                 确认无问题后，请删除该文件中对应行，再重新配置。",
                self.host,
                self.port,
                self.store.display()
            )),
            Err(e) => Err(format!(
                "读取主机指纹记录失败（{}）：{e}",
                self.store.display()
            )),
        };

        let reject = self.reject.clone();
        async move {
            match outcome {
                Ok(()) => Ok(true),
                Err(msg) => {
                    if let Ok(mut slot) = reject.lock() {
                        *slot = Some(msg);
                    }
                    Ok(false)
                }
            }
        }
    }
}

/// 主机指纹记录文件：放在私钥旁边（通常就是 ~/.ssh）。
fn host_key_store(key_path: &Path) -> PathBuf {
    key_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("shangxian_known_hosts"))
        .unwrap_or_else(|| PathBuf::from("shangxian_known_hosts"))
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
    let known_hosts = host_key_store(key_path);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("内部错误（无法创建网络运行时）：{e}"))?;
    runtime.block_on(write_public_key(
        host,
        port,
        user,
        password,
        &public_key,
        &known_hosts,
    ))?;

    if !verify_with_key(host, port, user, key_path)? {
        return Err("公钥已写入，但使用密钥登录仍未成功。请检查服务器上 ~/.ssh 的权限（目录 700，authorized_keys 文件 600）".into());
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
        std::fs::create_dir_all(dir).map_err(|e| format!("创建密钥目录失败：{e}"))?;
    }

    let keygen = crate::ssh::default_ssh_exe().with_file_name("ssh-keygen.exe");
    let mut cmd = Command::new(&keygen);
    cmd.args([
        "-t",
        "ed25519",
        "-N",
        "",
        "-f",
        &key_path.display().to_string(),
        "-C",
        "shangxian",
    ])
    .stdin(Stdio::null())
    .creation_flags(CREATE_NO_WINDOW);
    let out = run_local_tool(&mut cmd, "调用 ssh-keygen 失败")?;
    if !out.status.success() {
        return Err(format!("生成密钥失败：{}", out.stderr.trim()));
    }

    // 刚生成的私钥权限要收紧；收不了就把这对新密钥删掉，不留隐患。
    if let Err(e) = tighten_key_permissions(key_path) {
        let _ = std::fs::remove_file(key_path);
        let _ = std::fs::remove_file(&pub_path);
        return Err(e);
    }

    std::fs::read_to_string(&pub_path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("读公钥失败：{e}"))
}

/// 生成的私钥只给当前 Windows 用户读：去掉继承来的权限，只保留当前用户可读。
fn tighten_key_permissions(key_path: &Path) -> Result<(), String> {
    let user = std::env::var("USERNAME")
        .map_err(|_| "无法读取当前 Windows 用户名（USERNAME），无法收紧私钥权限".to_string())?;
    // 写死系统目录里的 icacls，避免从 PATH 里碰到同名程序
    let icacls = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("icacls.exe"))
        .unwrap_or_else(|| PathBuf::from("icacls.exe"));

    let mut cmd = Command::new(&icacls);
    cmd.arg(key_path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:R"))
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let out = run_local_tool(&mut cmd, "调用 icacls 失败")?;

    if !out.status.success() {
        return Err(format!(
            "新私钥权限未收紧（icacls 退出码 {:?}）：{}。请手动执行：icacls \"{}\" /inheritance:r /grant:r {user}:R，然后重试",
            out.status.code(),
            out.stderr.trim(),
            key_path.display()
        ));
    }
    Ok(())
}

async fn write_public_key(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    pubkey: &str,
    known_hosts: &Path,
) -> Result<(), String> {
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let reject: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let handler = HostKeyCheck {
        host: host.to_string(),
        port,
        store: known_hosts.to_path_buf(),
        reject: reject.clone(),
    };
    let mut session = match client::connect(config, (host, port), handler).await {
        Ok(session) => session,
        Err(e) => {
            // 主机指纹不对时，把 check_server_key 里写好的原因原样给用户。
            let reason = reject.lock().ok().and_then(|slot| slot.clone());
            return Err(reason.unwrap_or_else(|| format!("无法连接服务器 {host}:{port}（{e}）")));
        }
    };

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
            "密码不正确，请重试（或检查登录账号）".into()
        } else if allowed.is_empty() {
            "服务器不接受密码登录。请在服务器上手动配置密钥，或让管理员开启密码登录".into()
        } else {
            format!(
                "该服务器不允许密码登录（支持的方式：{allowed}）。请在服务器上手动配置密钥"
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
        .map_err(|e| format!("无法打开服务器会话：{e}"))?;
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
            "向服务器写入公钥失败（退出码 {exit:?}）：{}",
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
    let mut cmd = Command::new(crate::ssh::default_ssh_exe());
    cmd.args([
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
    .stdin(Stdio::null())
    .creation_flags(CREATE_NO_WINDOW);
    let out = run_local_tool(&mut cmd, "调用 ssh 验证失败")?;
    Ok(out.status.success() && out.stdout.contains("shangxian-ok"))
}

/// 本地小工具（ssh-keygen / icacls / ssh 验证）统一走带超时的执行：15 秒就结束，
/// 起不来或超时都给一句人话（D1 §11/§16-2）。
fn run_local_tool(cmd: &mut Command, context: &str) -> Result<crate::proc::CapturedOutput, String> {
    crate::proc::run_capture(cmd, Duration::from_secs(15), crate::proc::DEFAULT_OUTPUT_LIMIT)
        .map_err(|e| crate::proc::spawn_error_with_context(e, context))
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

    #[test]
    fn host_key_store_sits_next_to_key() {
        let p = Path::new("C:/Users/x/.ssh/shangxian_id_ed25519");
        assert_eq!(
            host_key_store(p),
            PathBuf::from("C:/Users/x/.ssh/shangxian_known_hosts")
        );
    }

    #[test]
    fn learns_host_key_then_detects_change() {
        use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};
        use russh::keys::ssh_key::PublicKey;

        let dir = std::env::temp_dir().join(format!("sx-known-hosts-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = dir.join("shangxian_known_hosts");
        let _ = std::fs::remove_file(&store);

        // 两把公开的测试 ed25519 公钥（russh 自带测试里就用它们；不带注释，指纹比对才不受注释影响）
        let key1 = PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ",
        )
        .unwrap();
        let key2 = PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X",
        )
        .unwrap();

        // 第一次见：不认识 → 记下来
        assert!(!check_known_hosts_path("example.com", 22, &key1, &store).unwrap());
        learn_known_hosts_path("example.com", 22, &key1, &store).unwrap();
        // 再连：指纹一致 → 放行
        assert!(check_known_hosts_path("example.com", 22, &key1, &store).unwrap());
        // 指纹变了 → 明确报错
        let err = check_known_hosts_path("example.com", 22, &key2, &store).unwrap_err();
        assert!(
            matches!(err, russh::keys::Error::KeyChanged { .. }),
            "应该报 KeyChanged：{err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tightens_key_permissions() {
        let path = std::env::temp_dir().join(format!("sx-key-acl-test-{}.key", std::process::id()));
        std::fs::write(&path, b"dummy").unwrap();
        tighten_key_permissions(&path).unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
