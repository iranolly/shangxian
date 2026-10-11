//! 控制通道服务端：后台线程监听本机回环端口，把 CLI / 别的程序的请求交给 Guardian。
//!
//! 跟界面用的是同一份状态（同一个 Guardian），所以命令行改完，界面立刻能看到，
//! 反过来也一样。协议见 `shangxian_core::ctl`。

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use shangxian_core::config::default_data_dir;
use shangxian_core::ctl as core_ctl;
use shangxian_core::guardian::{
    Guardian, NewTunnelInput, ServerInput, ServiceInput, UpdateTunnelInput,
};

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

        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let guardian = guardian.clone();
            let token = info.token.clone();
            std::thread::spawn(move || {
                if let Err(e) = handle(stream, &guardian, &token) {
                    eprintln!("[控制通道] 这条请求处理失败：{e}");
                }
            });
        }
    });
}

/// 一条连接 = 一条请求。
fn handle(stream: TcpStream, guardian: &Arc<Mutex<Guardian>>, token: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;

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

    let result = {
        let mut g = guardian.lock().unwrap();
        dispatch(&mut g, method, &params)
    };
    match result {
        Ok(v) => reply(&stream, &json!({"id": id, "ok": true, "result": v})),
        Err(e) => reply(&stream, &json!({"id": id, "ok": false, "error": e})),
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
    ("service_stop", "停止本机服务（只停应用自己启动的）", "{id}"),
    ("service_restart", "重启本机服务（先停再启）", "{id}"),
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
        "{id?, name, host, sshPort, user, keyPath?}",
    ),
    ("delete_server", "删除云端服务器（有隧道在用会拒绝）", "{id}"),
    (
        "setup_passwordless",
        "用密码登录一次，自动配好免密（密码不落盘）",
        "{serverId, password}",
    ),
    ("hash_password", "在服务器上生成访问密码的哈希（apr1，给 nginx 用）", "{serverId, password}"),
    ("get_nginx_snippet", "生成某条隧道的 nginx 配置片段", "{id}"),
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
            g.stop_service(&id).map(|_| json!({"ok": true}))
        }
        "service_restart" => {
            let id = need_str(params, "id")?;
            g.stop_service(&id)?;
            g.start_service(&id).map(|_| json!({"ok": true}))
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
        "setup_passwordless" => setup_passwordless(g, params),
        "hash_password" => {
            let server_id = need_str(params, "serverId")?;
            let password = need_str(params, "password")?;
            g.hash_password(&server_id, &password)
                .map(|hash| json!({"hash": hash}))
        }
        "get_nginx_snippet" => {
            let id = need_str(params, "id")?;
            g.nginx_snippet(&id).map(to_json)
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

/// 配免密：锁只拿两次（拿服务器信息、写回密钥路径），中间那段连服务器不占锁。
fn setup_passwordless(g: &mut Guardian, params: &Value) -> Result<Value, String> {
    let server_id = need_str(params, "serverId")?;
    let password = need_str(params, "password")?;
    let (host, port, user) = g.server_basics(&server_id)?;
    let key_path = shangxian_core::config::key_path_for(&default_data_dir(), &server_id);

    let result = shangxian_core::keyboot::bootstrap_key(&host, port, &user, &password, &key_path)?;
    g.set_server_key(&server_id, &result.key_path)?;
    Ok(json!({
        "keyPath": result.key_path,
        "message": format!("配好了！以后连这台服务器都走密钥，不用再输密码。密钥放在：{}", result.key_path),
    }))
}
