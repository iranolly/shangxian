//! 控制通道服务端：后台线程监听本机回环端口，把 CLI / 别的程序的请求交给 Guardian。
//!
//! 跟界面用的是同一份状态（同一个 Guardian），所以命令行改完，界面立刻能看到，
//! 反过来也一样。协议见 `shangxian_core::ctl`。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use shangxian_core::config::default_data_dir;
use shangxian_core::ctl as core_ctl;
use shangxian_core::guardian::{
    Guardian, NewTunnelInput, ServerInput, ServiceInput, StopOutcome, UpdateTunnelInput,
};

/// 一行请求最多 64KB（防着有人塞个超大 JSON 把内存撑爆）。
const MAX_LINE_BYTES: usize = 64 * 1024;
/// 连上之后最多 10 秒要把请求发完（防着连上就不说话）。
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// 同时在处理的连接数上限（一条连接一个线程，不能让人无限开）。
const MAX_CONNECTIONS: usize = 8;

/// 连接名额：拿到就占一个，线程结束（哪怕 panic）自动还回去。
struct ConnSlot(Arc<AtomicUsize>);

impl ConnSlot {
    fn try_acquire(live: &Arc<AtomicUsize>) -> Option<Self> {
        if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            live.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(live.clone()))
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 开控制通道：绑定随机端口 → 写 ctl.json → 等连接。
pub fn spawn(guardian: Arc<Mutex<Guardian>>) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[控制通道] 端口开不出来，命令行用不了：{e}");
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(_) => return,
        };
        let data_dir = default_data_dir();
        let info = core_ctl::CtlInfo {
            port,
            token: core_ctl::new_token(),
            pid: std::process::id(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        if let Err(e) = core_ctl::write_ctl_info(&data_dir, &info) {
            eprintln!("[控制通道] 写 ctl.json 失败，命令行用不了：{e}");
            return;
        }
        eprintln!("[控制通道] 已开启：127.0.0.1:{port}（命令行/agent 可以连了）");

        let live = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // 名额满了就不接了，回一句人话把人打发走，别把线程数撑爆。
            let Some(slot) = ConnSlot::try_acquire(&live) else {
                let _ = reply(
                    &stream,
                    &json!({"id": 0, "ok": false, "error": format!("同时处理的连接太多了（最多 {MAX_CONNECTIONS} 条），稍后再试")}),
                );
                continue;
            };
            let guardian = guardian.clone();
            let token = info.token.clone();
            std::thread::spawn(move || {
                let _slot = slot; // 线程结束（哪怕 panic）自动把名额还回去
                if let Err(e) = handle(stream, &guardian, &token) {
                    eprintln!("[控制通道] 这条请求处理失败：{e}");
                }
            });
        }
    });
}

/// 一条连接 = 一条请求。
fn handle(stream: TcpStream, guardian: &Arc<Mutex<Guardian>>, token: &str) -> std::io::Result<()> {
    // 读请求既限时也限量：连上一直不发、或者发一行没边的巨型 JSON，都不能拖住应用。
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    let mut limited = reader.by_ref().take((MAX_LINE_BYTES + 1) as u64);
    match limited.read_line(&mut line) {
        Ok(0) => return Ok(()), // 连上了但什么都没发
        Ok(_) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::TimedOut
                || e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            return reply(
                &stream,
                &json!({"id": 0, "ok": false, "error": "读请求超时了（连上了但一直没把请求发过来）"}),
            );
        }
        Err(e) => return Err(e),
    }
    if line.len() > MAX_LINE_BYTES {
        return reply(
            &stream,
            &json!({"id": 0, "ok": false, "error": "请求太长了：一行最多 64KB"}),
        );
    }

    let req: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => {
            return reply(
                &stream,
                &json!({"id": 0, "ok": false, "error": format!("请求看不懂：{e}")}),
            )
        }
    };
    let id = req.get("id").cloned().unwrap_or(json!(1));
    if req.get("token").and_then(|v| v.as_str()) != Some(token) {
        return reply(
            &stream,
            &json!({"id": id, "ok": false, "error": "通行证不对（ctl.json 可能是旧的，重启应用即可）"}),
        );
    }
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    // 要联网跑 SSH 的方法全在这拦下（最长 20 秒）：短锁里只克隆连接信息，真干活放锁外。
    // 界面 800ms 快照、看护 150ms 轮询都在等这把全局锁，占着就是界面卡死。
    match method {
        "check_tunnel_access" => {
            let result = {
                let g = guardian.lock().unwrap();
                need_str(&params, "id").and_then(|tunnel_id| g.access_check_request(&tunnel_id))
            }
            .and_then(|req| shangxian_core::guardian::run_access_check(&req).map(to_json));
            return reply_result(&stream, &id, result);
        }
        "hash_password" => {
            let result = need_server_id(&params)
                .and_then(|server_id| crate::server_conn(guardian, &server_id))
                .and_then(|conn| {
                    let password = need_str(&params, "password")?;
                    shangxian_core::cleanup::hash_password(&conn.exec(), &password)
                        .map(|hash| json!({"hash": hash}))
                });
            return reply_result(&stream, &id, result);
        }
        "setup_passwordless" => {
            return reply_result(&stream, &id, setup_passwordless(guardian, &params));
        }
        "test_server" => {
            let result = need_server_id(&params)
                .and_then(|server_id| crate::server_conn(guardian, &server_id))
                .and_then(|conn| crate::run_server_test(&conn).map(|msg| json!({"message": msg})));
            return reply_result(&stream, &id, result);
        }
        _ => {}
    }

    // 其余方法都是读内存/改配置，很快，按老规矩在锁里跑。
    let result = {
        let mut g = guardian.lock().unwrap();
        dispatch(&mut g, method, &params)
    };
    reply_result(&stream, &id, result)
}

/// 统一的回包格式：成功给 result，失败给 error，id 原样带回。
fn reply_result(stream: &TcpStream, id: &Value, result: Result<Value, String>) -> std::io::Result<()> {
    match result {
        Ok(v) => reply(stream, &json!({"id": id, "ok": true, "result": v})),
        Err(e) => reply(stream, &json!({"id": id, "ok": false, "error": e})),
    }
}

fn reply(stream: &TcpStream, value: &Value) -> std::io::Result<()> {
    let mut s = stream;
    s.write_all(value.to_string().as_bytes())?;
    s.write_all(b"\n")?;
    s.flush()
}

// ---------- 参数小工具 ----------

fn need_str(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

fn need_bool(params: &Value, key: &str) -> Result<bool, String> {
    params
        .get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("缺少参数 {key}（要 true/false）"))
}

/// 服务器 id：标准写法是 `serverId`，也兼容 `id`（各调用方写法不一，认就行）。
fn need_server_id(params: &Value) -> Result<String, String> {
    params
        .get("serverId")
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "缺少参数 serverId".to_string())
}

fn opt_usize(params: &Value, key: &str, fallback: usize) -> usize {
    params
        .get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(fallback)
}

fn to_json<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn from_params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, String> {
    serde_json::from_value(params.clone()).map_err(|e| format!("参数不对：{e}"))
}

// ---------- 方法表（也给 agent 看，cli methods 会打印它） ----------

const METHODS: &[(&str, &str, &str)] = &[
    ("ping", "看应用在不在（会校验通行证）", "{}"),
    ("status", "一次拿全：应用信息 + 本机服务 + 隧道 + 服务器", "{}"),
    ("get_snapshot", "只拿三张清单（不含应用信息）", "{}"),
    ("methods", "列出所有能调的方法（就是这张表）", "{}"),
    ("list_local_ports", "本机正在监听的端口（带进程名和程序路径）", "{}"),
    ("scan_imports", "扫描现有启动脚本，返回可导入的建议", "{}"),
    ("import_services", "把导入建议变成服务（默认暂停）", "{items: [...]}"),
    (
        "save_service",
        "新增/编辑本机服务（不填 id 就是新增）",
        "{id?, name, localPort, launchExe?, launchArgs?, launchWorkdir?, watch, autostart, note?}",
    ),
    ("remove_service", "删除本机服务（有隧道在用会拒绝）", "{id}"),
    ("service_start", "启动本机服务（用登记过的启动方式）", "{id}"),
    (
        "service_stop",
        "停止本机服务（默认只停应用自己启动的；外部进程要先拿 needsConfirm 让用户确认）",
        "{id, killForeign?, expectedPid?}",
    ),
    ("service_restart", "重启本机服务（先停再启；外部进程在听时不碰，返回 needsConfirm）", "{id}"),
    (
        "inspect_process_launch_info",
        "自动读取某端口上进程的启动方式（程序/参数/工作目录 + 警告）",
        "{port, expectedPid?}",
    ),
    (
        "preview_launch_command",
        "给人看的实际命令行预览（走和启动一样的分支，不参与启动）",
        "{exe, args}",
    ),
    ("set_service_watch", "开关看护（掉了自动拉起）", "{id, watch}"),
    ("set_service_autostart", "开关开机自启", "{id, autostart}"),
    ("get_service_logs", "读本机服务的日志尾部", "{id, lines?}"),
    (
        "add_tunnel",
        "新增发布隧道",
        "{name, serviceId, serverId, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}",
    ),
    (
        "update_tunnel",
        "编辑发布隧道（accessHash 传 null 表示不改密码）",
        "{id, name, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}",
    ),
    ("remove_tunnel", "删除发布隧道", "{id}"),
    ("tunnel_start", "开始发布", "{id}"),
    ("tunnel_stop", "暂停发布", "{id}"),
    ("tunnel_restart", "立刻重连", "{id}"),
    ("get_logs", "读隧道的日志尾部", "{id, lines?}"),
    (
        "save_server",
        "新增/编辑云端服务器（不填 id 就是新增）",
        "{id?, name, host, sshPort, user, keyPath?, vhostDir?}",
    ),
    ("delete_server", "删除云端服务器（有隧道在用会拒绝）", "{id}"),
    (
        "setup_passwordless",
        "用密码登录一次，自动配好免密（密码不落盘）",
        "{serverId, password}",
    ),
    (
        "test_server",
        "测试连接：真连一次这台服务器的 SSH",
        "{serverId}",
    ),
    ("hash_password", "生成访问密码的哈希（bcrypt，本机算好、给 nginx 用）", "{serverId, password}"),
    ("get_nginx_snippet", "生成某条隧道的 nginx 配置片段", "{id}"),
    (
        "check_tunnel_access",
        "核对云端 nginx 上这条隧道的实际访问保护",
        "{id}",
    ),
    ("get_notify_enabled", "掉线通知开着吗", "{}"),
    ("set_notify_enabled", "开关掉线通知", "{enabled}"),
    ("get_autostart", "开机自启开着吗", "{}"),
    ("set_autostart", "开关开机自启", "{enabled}"),
    ("get_data_dir", "配置文件夹在哪", "{}"),
];

/// 一个方法名一个分支，全部落到 Guardian（跟界面走的是同一条路）。
fn dispatch(g: &mut Guardian, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "ping" => Ok(json!({"pong": true})),
        "methods" => Ok(json!(METHODS
            .iter()
            .map(|(name, desc, shape)| json!({"method": name, "desc": desc, "params": shape}))
            .collect::<Vec<_>>())),
        "status" => Ok(json!({
            "app": app_info(g),
            "services": to_json(g.snapshot()),
            "tunnels": to_json(g.tunnel_snapshot()),
            "servers": to_json(g.servers()),
        })),
        "get_snapshot" => Ok(json!({
            "services": to_json(g.snapshot()),
            "tunnels": to_json(g.tunnel_snapshot()),
            "servers": to_json(g.servers()),
        })),

        // ----- 本机服务 -----
        "list_local_ports" => Ok(to_json(shangxian_core::local_ports::list_listening())),
        "scan_imports" => Ok(to_json(shangxian_core::import::scan())),
        "import_services" => {
            let items = params
                .get("items")
                .cloned()
                .ok_or_else(|| "缺少参数 items".to_string())?;
            let items: Vec<shangxian_core::import::ImportSuggestion> =
                serde_json::from_value(items).map_err(|e| format!("items 不对：{e}"))?;
            g.import_services(items).map(|n| json!({"imported": n}))
        }
        "save_service" => {
            let input: ServiceInput = from_params(params)?;
            g.save_service(input).map(|id| json!({"id": id}))
        }
        "remove_service" => {
            let id = need_str(params, "id")?;
            g.remove_service(&id).map(|_| json!({"ok": true}))
        }
        "service_start" => {
            let id = need_str(params, "id")?;
            g.start_service(&id).map(|_| json!({"ok": true}))
        }
        "service_stop" => {
            let id = need_str(params, "id")?;
            let kill_foreign = params
                .get("killForeign")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let expected_pid = params
                .get("expectedPid")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            g.stop_service(&id, kill_foreign, expected_pid).map(to_json)
        }
        "service_restart" => {
            let id = need_str(params, "id")?;
            // 外部进程在听：不碰它，把 needsConfirm 原样交给调用方（CLI 会提示先手动停）
            let outcome = g.stop_service(&id, false, None)?;
            if matches!(outcome, StopOutcome::NeedsConfirm { .. }) {
                return Ok(to_json(outcome));
            }
            g.start_service(&id).map(|_| json!({"ok": true}))
        }
        "inspect_process_launch_info" => {
            let port = params
                .get("port")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| "缺少参数 port".to_string())?;
            let port = u16::try_from(port)
                .map_err(|_| "端口要填 1~65535 的数字".to_string())?;
            let expected_pid = params
                .get("expectedPid")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            Ok(to_json(shangxian_core::proc_detail::launch_info(
                port,
                expected_pid,
            )))
        }
        "preview_launch_command" => {
            let exe = need_str(params, "exe")?;
            let args = params
                .get("args")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(json!({
                "command": shangxian_core::localservice::preview_launch(&exe, &args)
            }))
        }
        "set_service_watch" => {
            let id = need_str(params, "id")?;
            let watch = need_bool(params, "watch")?;
            g.set_service_watch(&id, watch).map(|_| json!({"ok": true}))
        }
        "set_service_autostart" => {
            let id = need_str(params, "id")?;
            let on = need_bool(params, "autostart")?;
            g.set_service_autostart(&id, on).map(|_| json!({"ok": true}))
        }
        "get_service_logs" => {
            let id = need_str(params, "id")?;
            let lines = opt_usize(params, "lines", 200);
            Ok(json!({"text": g.service_log_tail(&id, lines)}))
        }

        // ----- 发布隧道 -----
        "add_tunnel" => {
            let input: NewTunnelInput = from_params(params)?;
            g.add_tunnel(input).map(|id| json!({"id": id}))
        }
        "update_tunnel" => {
            let input: UpdateTunnelInput = from_params(params)?;
            g.update_tunnel(input).map(|_| json!({"ok": true}))
        }
        "remove_tunnel" => {
            let id = need_str(params, "id")?;
            g.remove_tunnel(&id).map(|_| json!({"ok": true}))
        }
        "tunnel_start" => {
            let id = need_str(params, "id")?;
            g.set_enabled(&id, true).map(|_| json!({"ok": true}))
        }
        "tunnel_stop" => {
            let id = need_str(params, "id")?;
            g.set_enabled(&id, false).map(|_| json!({"ok": true}))
        }
        "tunnel_restart" => {
            let id = need_str(params, "id")?;
            g.restart(&id).map(|_| json!({"ok": true}))
        }
        "get_logs" => {
            let id = need_str(params, "id")?;
            let lines = opt_usize(params, "lines", 200);
            Ok(json!({"text": g.log_tail(&id, lines)}))
        }

        // ----- 云端服务器 -----
        "save_server" => {
            let input: ServerInput = from_params(params)?;
            g.save_server(input).map(|id| json!({"id": id}))
        }
        "delete_server" => {
            let id = need_str(params, "id")?;
            g.delete_server(&id).map(|_| json!({"ok": true}))
        }
        "get_nginx_snippet" => {
            let id = need_str(params, "id")?;
            g.nginx_snippet(&id).map(to_json)
        }
        // 这几条都要联网跑 SSH，统一在 handle 里走「短锁 + 锁外执行」那条路。
        // 万一有调用方绕过那条路走到这儿，直接拒绝：占着全局锁跑 SSH 最长 20 秒，
        // 界面会卡死——不能再从这个口子回去。
        "check_tunnel_access" | "hash_password" | "setup_passwordless" | "test_server" => {
            Err("这条方法要联网跑 SSH，得走锁外那条路，不能占着全局锁跑".to_string())
        }

        // ----- 应用 -----
        "get_notify_enabled" => Ok(json!({"enabled": g.notify_enabled()})),
        "set_notify_enabled" => {
            let enabled = need_bool(params, "enabled")?;
            g.set_notify_enabled(enabled).map(|_| json!({"ok": true}))
        }
        "get_autostart" => Ok(json!({"enabled": crate::autostart_enabled()})),
        "set_autostart" => {
            let enabled = need_bool(params, "enabled")?;
            crate::apply_autostart(enabled).map(|_| json!({"ok": true}))
        }
        "get_data_dir" => Ok(json!({"path": default_data_dir().display().to_string()})),

        other => Err(format!(
            "没有这个方法：{other}（能用的都在这儿：cli methods）"
        )),
    }
}

/// 应用自己的信息（版本、进程号、数据目录、开关状态）。
fn app_info(g: &Guardian) -> Value {
    json!({
        "pid": std::process::id(),
        "appVersion": env!("CARGO_PKG_VERSION"),
        "dataDir": default_data_dir().display().to_string(),
        "notifyEnabled": g.notify_enabled(),
        "autostart": crate::autostart_enabled(),
    })
}

/// 配免密：锁只拿两次（拿连接信息、写回密钥路径），中间那段 SSH（最长十几秒）不占锁。
fn setup_passwordless(guardian: &Arc<Mutex<Guardian>>, params: &Value) -> Result<Value, String> {
    let server_id = need_server_id(params)?;
    let password = need_str(params, "password")?;
    let key_path = shangxian_core::config::key_path_for(&default_data_dir(), &server_id);
    let conn = crate::server_conn(guardian, &server_id)?;

    let result = shangxian_core::keyboot::bootstrap_key(
        &conn.host,
        conn.ssh_port,
        &conn.user,
        &password,
        &key_path,
    )?;
    guardian.lock().unwrap().set_server_key(&server_id, &result.key_path)?;
    Ok(json!({
        "keyPath": result.key_path,
        "message": format!("配好了！以后连这台服务器都走密钥，不用再输密码。密钥放在：{}", result.key_path),
    }))
}
