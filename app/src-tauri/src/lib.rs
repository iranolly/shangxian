// 上线助手桌面壳：窗口 + 托盘常驻 + 隧道守护接入。
// 核心看护逻辑在 shangxian-core；这一层负责生命周期与界面命令。

use std::sync::{Arc, Mutex};

use shangxian_core::config::{default_data_dir, AppConfig};
use shangxian_core::guardian::{
    Guardian, NewTunnelInput, ServerInput, ServerView, ServiceInput, ServiceView, StopOutcome,
    TunnelView, UpdateTunnelInput,
};
use shangxian_core::import::ImportSuggestion;
use shangxian_core::local_ports::{list_listening, LocalPort};
use shangxian_core::nginx::NginxSnippet;
use shangxian_core::proc_detail::LaunchInfo;
use shangxian_core::protection::AccessCheck;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, State, WindowEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW,
};

mod ctl;
mod titlebar;

/// 给界面的一次完整快照。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    /// 本机服务（这台电脑上的东西）
    services: Vec<ServiceView>,
    /// 发布隧道（服务 → 服务器）
    tunnels: Vec<TunnelView>,
    servers: Vec<ServerView>,
}

struct AppState {
    guardian: Arc<Mutex<Guardian>>,
}

// ---------- 短锁 + 锁外跑 SSH ----------
// 全局锁后面等的是看护轮询（150ms 一次）和界面快照（800ms 一次）。
// 任何要联网的 SSH 操作都得：短锁里克隆连接信息 → 放锁 → 跑完再短锁写回。

/// 一台服务器跑 SSH 需要的连接信息（从 Guardian 里克隆出来，不占着锁）。
pub(crate) struct ServerConn {
    pub(crate) host: String,
    pub(crate) ssh_port: u16,
    pub(crate) user: String,
    pub(crate) identity_file: Option<std::path::PathBuf>,
}

impl ServerConn {
    /// ssh 的目标写法：`user@host`。
    pub(crate) fn target(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// 装成 core 的 ssh 执行器（参数跟隧道走的是同一套）。
    pub(crate) fn exec(&self) -> shangxian_core::cleanup::SshExec {
        shangxian_core::cleanup::SshExec {
            ssh_exe: shangxian_core::ssh::default_ssh_exe(),
            target: self.target(),
            ssh_port: self.ssh_port,
            identity_file: self.identity_file.clone(),
        }
    }
}

/// 短锁里克隆一台服务器的连接信息（不联网）。
pub(crate) fn server_conn(
    guardian: &Arc<Mutex<Guardian>>,
    server_id: &str,
) -> Result<ServerConn, String> {
    let g = guardian.lock().unwrap();
    let s = g
        .servers()
        .into_iter()
        .find(|s| s.id == server_id)
        .ok_or_else(|| format!("找不到云端服务器：{server_id}"))?;
    Ok(ServerConn {
        host: s.host,
        ssh_port: s.ssh_port,
        user: s.user,
        identity_file: s.key_path.map(std::path::PathBuf::from),
    })
}

/// 「测试连接」真正干活的部分：跑一次 SSH，成功返回人话 + 用时。
/// 必须在锁外调（Tauri 那边放 `spawn_blocking`，控制通道那边放连接线程里）。
pub(crate) fn run_server_test(conn: &ServerConn) -> Result<String, String> {
    let started = std::time::Instant::now();
    let out = shangxian_core::cleanup::run(
        &conn.exec(),
        "echo shangxian-ok",
        std::time::Duration::from_secs(15),
    )
    .map_err(|e| ssh_error_human(&e, conn))?;
    if !out.contains("shangxian-ok") {
        return Err(format!(
            "连上了，但服务器回的话看不懂（{}）。看看这台服务器的默认 shell 是不是被改过。",
            out.trim()
        ));
    }
    Ok(format!("连接成功，用时 {:.1} 秒", started.elapsed().as_secs_f64()))
}

/// 把 ssh 的报错翻成用户能懂的话（原始报错附在后面，对不上时好排查）。
fn ssh_error_human(raw: &str, conn: &ServerConn) -> String {
    let low = raw.to_ascii_lowercase();
    let target = conn.target();
    if low.contains("无法启动 ssh") || low.contains("cannot find the file") {
        return "这台电脑上找不到 ssh 命令：去「设置 → 应用 → 可选功能」里装上「OpenSSH 客户端」再试".into();
    }
    if low.contains("timed out") || low.contains("timeout") {
        return format!(
            "连不上 {target}（端口 {}）：超时了。检查地址、SSH 端口填得对不对，服务器防火墙有没有放行",
            conn.ssh_port
        );
    }
    if low.contains("permission denied") || low.contains("authentication failed") {
        return "登录被拒绝了：这台服务器还没配免密，或者密钥不对。先在这台服务器上用密码配一次免密，再点测试".into();
    }
    if low.contains("host key verification failed") {
        return "服务器指纹对不上：可能服务器重装过、或者换成另一台机器了。核对清楚再连".into();
    }
    if low.contains("connection refused") {
        return format!(
            "{target}（端口 {}）拒绝连接：SSH 端口可能填错了，或者服务器上 sshd 没开",
            conn.ssh_port
        );
    }
    if low.contains("could not resolve") || low.contains("name or service not known") {
        return format!("找不到服务器地址：{target} 解析不了，检查主机名/IP 有没有写错");
    }
    if low.contains("no such file") || low.contains("identity file") {
        let key = conn
            .identity_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "（还没配）".to_string());
        return format!("密钥文件用不了（{key}）：重新用密码配一次免密，或者手动核对这个路径");
    }
    format!("连不上 {target}：{}", raw.trim())
}

// ---------- 配置读坏时的红条 ----------

/// 配置读坏的原因（界面加载完会由 `on_page_load` 补发一次，免得事件比界面先到）。
static CONFIG_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// 告诉界面「配置文件读不出来」：事件名 `config-error`，payload `{ message: string }`。
fn report_config_error(app: &tauri::AppHandle, message: &str) {
    *CONFIG_ERROR.lock().unwrap() = Some(message.to_string());
    if let Err(e) = app.emit("config-error", serde_json::json!({ "message": message })) {
        eprintln!("[配置] 通知界面失败（界面可能还没起来）：{e}");
    }
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    let guardian = state.guardian.lock().unwrap();
    Snapshot {
        services: guardian.snapshot(),
        tunnels: guardian.tunnel_snapshot(),
        servers: guardian.servers(),
    }
}

// ---------- 本机服务 ----------

/// 新增/编辑一台本机服务。
#[tauri::command]
fn save_service(input: ServiceInput, state: State<'_, AppState>) -> Result<String, String> {
    state.guardian.lock().unwrap().save_service(input)
}

/// 删除一台本机服务（还有隧道在用时会拒绝）。
#[tauri::command]
fn remove_service(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().remove_service(&id)
}

/// 启动一台本机服务。
#[tauri::command]
fn service_start(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().start_service(&id)
}

/// 停止一台本机服务。
///
/// 默认只停我们自己启动的进程；端口上是外部进程时先返回 `needsConfirm`（带 pid/exe），
/// 界面问过用户后带上 `killForeign=true` 和同一个 `expectedPid` 再调一次才会真的结束它。
///
/// 停止过程要走 netstat / OpenProcess / TerminateProcess，会阻塞一会儿，
/// 所以和 `inspect_process_launch_info` 一样放 `spawn_blocking` 里，不占 Tauri 主线程。
#[tauri::command]
async fn service_stop(
    id: String,
    kill_foreign: Option<bool>,
    expected_pid: Option<u32>,
    state: State<'_, AppState>,
) -> Result<StopOutcome, String> {
    let guardian = state.guardian.clone();
    tauri::async_runtime::spawn_blocking(move || {
        guardian
            .lock()
            .unwrap()
            .stop_service(&id, kill_foreign.unwrap_or(false), expected_pid)
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))?
}

/// 自动读取某个端口上进程的启动方式（程序/参数/工作目录 + 警告）。
///
/// 读进程要走 PEB/ReadProcessMemory，可能卡住几十毫秒，放 `spawn_blocking` 里不堵界面。
#[tauri::command]
async fn inspect_process_launch_info(
    port: u16,
    expected_pid: Option<u32>,
) -> Result<LaunchInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        shangxian_core::proc_detail::launch_info(port, expected_pid)
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))
}

/// 给人看的「实际会执行的命令行」预览（走和启动完全一样的分支，仅供显示）。
#[tauri::command]
fn preview_launch_command(exe: String, args: String) -> String {
    shangxian_core::localservice::preview_launch(&exe, &args)
}

/// 看护开关（进程掉了自动拉起）。
#[tauri::command]
fn set_service_watch(id: String, watch: bool, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().set_service_watch(&id, watch)
}

/// 开机自启开关。
#[tauri::command]
fn set_service_autostart(
    id: String,
    autostart: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.guardian.lock().unwrap().set_service_autostart(&id, autostart)
}

/// 本机服务自己的输出（程序日志 + 看护记录）。
#[tauri::command]
fn get_service_logs(id: String, lines: Option<usize>, state: State<'_, AppState>) -> String {
    state
        .guardian
        .lock()
        .unwrap()
        .service_log_tail(&id, lines.unwrap_or(200))
}

// ---------- 发布隧道 ----------

/// 新建一条隧道。
#[tauri::command]
fn add_tunnel(input: NewTunnelInput, state: State<'_, AppState>) -> Result<String, String> {
    state.guardian.lock().unwrap().add_tunnel(input)
}

/// 编辑一条隧道。
#[tauri::command]
fn update_tunnel(input: UpdateTunnelInput, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().update_tunnel(input)
}

/// 停止并删除一条隧道。
#[tauri::command]
fn remove_tunnel(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().remove_tunnel(&id)
}

/// 开始发布。
#[tauri::command]
fn tunnel_start(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().set_enabled(&id, true)
}

/// 暂停发布。
#[tauri::command]
fn tunnel_stop(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().set_enabled(&id, false)
}

/// 立刻重连。
#[tauri::command]
fn tunnel_restart(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().restart(&id)
}

/// 本机正在监听的端口（添加本机服务时自动填表用）。
#[tauri::command]
fn list_local_ports() -> Vec<LocalPort> {
    list_listening()
}

/// 用密码登录一次，自动配好免密登录（生成密钥、把公钥写到服务器、用密钥验证）。
/// 密码只在内存里活几秒，不落盘、不进日志。
#[tauri::command]
async fn setup_passwordless(
    server_id: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (host, port, user) = state.guardian.lock().unwrap().server_basics(&server_id)?;
    let key_path = shangxian_core::config::key_path_for(&default_data_dir(), &server_id);

    let result = tauri::async_runtime::spawn_blocking(move || {
        shangxian_core::keyboot::bootstrap_key(&host, port, &user, &password, &key_path)
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))??;

    state
        .guardian
        .lock()
        .unwrap()
        .set_server_key(&server_id, &result.key_path)?;
    Ok(format!(
        "配好了！以后连这台服务器都走密钥，不用再输密码。密钥放在：{}",
        result.key_path
    ))
}

/// 掉线通知开关。
#[tauri::command]
fn get_notify_enabled(state: State<'_, AppState>) -> bool {
    state.guardian.lock().unwrap().notify_enabled()
}

#[tauri::command]
fn set_notify_enabled(enabled: bool, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().set_notify_enabled(enabled)
}

/// 立刻发一条测试通知（设置页“试一下”）。
#[tauri::command]
fn test_notification(app: tauri::AppHandle) -> Result<(), String> {
    send_test_notification(&app)
}

fn send_test_notification(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    let result = app
        .notification()
        .builder()
        .title("上线助手")
        .body("通知长这样：服务掉线或恢复时，我会在这里提醒你。")
        .show()
        .map_err(|e| format!("发送通知失败：{e}"));
    match &result {
        Ok(()) => eprintln!("[通知] 测试通知已发送"),
        Err(e) => eprintln!("[通知] 测试通知发送失败：{e}"),
    }
    result
}

/// 托盘「退出并停止全部本地服务」的结果提示。
///
/// release 版没有控制台，`eprintln` 用户看不到；有系统通知就发一条，通知发不出去
/// （系统不支持/权限被关）时退化为写应用日志（数据目录 `logs/app.log`）。
fn notify_exit_stop_result(app: &tauri::AppHandle, stopped: usize, remaining: usize) {
    let body = if remaining == 0 {
        if stopped == 0 {
            "没有正在跑的本地服务需要停止。".to_string()
        } else {
            format!("已停止 {stopped} 个本地服务。")
        }
    } else if stopped == 0 {
        format!("{remaining} 个本地服务没能停（可能是脚本启动的服务，需要手动处理）。")
    } else {
        format!(
            "已停止 {stopped} 个本地服务；{remaining} 个没能停（可能是脚本启动的服务，需要手动处理）。"
        )
    };

    use tauri_plugin_notification::NotificationExt;
    match app
        .notification()
        .builder()
        .title("上线助手")
        .body(&body)
        .show()
    {
        Ok(()) => eprintln!("[通知] 已发送：{body}"),
        Err(e) => {
            eprintln!("[通知] 发送失败：{e}");
            append_app_log(&format!("[退出] {body}"));
        }
    }
}

/// 通知发不出去时的兜底：把一行结果追加进应用日志（release 版没有控制台）。
fn append_app_log(line: &str) {
    use std::io::Write;
    let dir = default_data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
    {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "[{millis}] {line}");
    }
}

/// 扫描现有启动脚本，返回导入建议。
#[tauri::command]
fn scan_imports() -> Vec<ImportSuggestion> {
    shangxian_core::import::scan()
}

/// 导入选中的服务（默认是"已暂停"，由用户手动开始发布）。
#[tauri::command]
fn import_services(
    items: Vec<ImportSuggestion>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    state.guardian.lock().unwrap().import_services(items)
}

/// 读服务日志尾部。
#[tauri::command]
fn get_logs(id: String, lines: Option<usize>, state: State<'_, AppState>) -> String {
    state.guardian.lock().unwrap().log_tail(&id, lines.unwrap_or(200))
}

/// 新增/编辑服务器。
#[tauri::command]
fn save_server(input: ServerInput, state: State<'_, AppState>) -> Result<String, String> {
    state.guardian.lock().unwrap().save_server(input)
}

/// 删除服务器（还有服务挂着时会拒绝）。
#[tauri::command]
fn delete_server(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().delete_server(&id)
}

/// 在本机生成访问密码的哈希（bcrypt `$2y$…`，给 nginx 用）；密码只在本机算，不发给服务器。
///
/// 生成走本机 bcrypt（不再把密码发到服务器上跑 openssl），很快；但也不占全局锁：
/// 短锁里拿连接信息，计算放锁外（Tauri 线程池里跑），别卡住看护轮询和界面快照。
#[tauri::command]
async fn hash_password(
    server_id: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let guardian = state.guardian.clone();
    let conn = server_conn(&guardian, &server_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        shangxian_core::cleanup::hash_password(&conn.exec(), &password)
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))?
}

/// 测试连接：真连一次这台服务器（SSH 跑一句 echo），给服务器页的「测试连接」按钮用。
///
/// 参数 `serverId`；也兼容 `id`（前端 api.ts 两个都传，怕后端认另一个名字）。
/// 不占全局锁（短锁拿连接信息 → 锁外跑），界面不会卡；最长 15 秒。
#[tauri::command]
async fn test_server(
    server_id: Option<String>,
    id: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let server_id = server_id
        .or(id)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "缺少参数 serverId（要测哪台服务器的 id）".to_string())?;
    let guardian = state.guardian.clone();
    let conn = server_conn(&guardian, &server_id)?;
    tauri::async_runtime::spawn_blocking(move || run_server_test(&conn))
        .await
        .map_err(|e| format!("内部错误：{e}"))?
}

/// 生成服务的 nginx 配置片段（人工应用到面板）。
#[tauri::command]
fn get_nginx_snippet(id: String, state: State<'_, AppState>) -> Result<NginxSnippet, String> {
    state.guardian.lock().unwrap().nginx_snippet(&id)
}

/// 核对云端 nginx 上这条隧道的实际访问保护（走 SSH 只读读配置，不改服务器）。
///
/// 核对要跑 SSH（最长 20 秒），不能占着看护轮询/界面快照的全局锁：
/// 短锁里把连接信息克隆出来，放锁后再放 `spawn_blocking` 里跑（同 `service_stop`）。
#[tauri::command]
async fn check_tunnel_access(id: String, state: State<'_, AppState>) -> Result<AccessCheck, String> {
    let guardian = state.guardian.clone();
    let req = {
        let g = guardian.lock().unwrap();
        g.access_check_request(&id)?
    };
    tauri::async_runtime::spawn_blocking(move || shangxian_core::guardian::run_access_check(&req))
        .await
        .map_err(|e| format!("内部错误：{e}"))?
}

/// 是否已设置开机自启（HKCU Run 键，用户级、无需管理员）。
pub(crate) fn autostart_enabled() -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg")
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            "/v",
            "Shangxian",
        ])
        .creation_flags(0x0800_0000)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 设置/取消开机自启（写 HKCU Run 键；登录后带 --hidden 静默进托盘）。
pub(crate) fn apply_autostart(enabled: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let output = if enabled {
        let exe = std::env::current_exe().map_err(|e| format!("找不到程序路径：{e}"))?;
        std::process::Command::new("reg")
            .args([
                "add",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "Shangxian",
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\" --hidden", exe.display()),
                "/f",
            ])
            .creation_flags(0x0800_0000)
            .output()
    } else {
        std::process::Command::new("reg")
            .args([
                "delete",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "Shangxian",
                "/f",
            ])
            .creation_flags(0x0800_0000)
            .output()
    }
    .map_err(|e| format!("调用 reg 失败：{e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(if msg.is_empty() {
            "操作失败（可能是系统策略限制）".into()
        } else {
            msg
        })
    }
}

/// 界面调用的两个薄壳，真正干活的是上面两个函数（命令行也走同一份）。
#[tauri::command]
fn get_autostart() -> bool {
    autostart_enabled()
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    apply_autostart(enabled)
}

/// 配置文件夹路径（设置页展示）。
#[tauri::command]
fn get_data_dir() -> String {
    default_data_dir().display().to_string()
}

/// 打开配置文件夹。
#[tauri::command]
fn open_data_dir() -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(default_data_dir())
        .spawn()
        .map_err(|e| format!("打开文件夹失败：{e}"))?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == "--test-notify") {
                let _ = send_test_notification(app);
                return;
            }
            if args.iter().any(|a| a == "--hidden") {
                return;
            }
            show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // 开机自启时带 --hidden：只进托盘，不弹窗口
            if std::env::args().any(|a| a == "--hidden") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            // ---------- 隧道守护：加载配置 + 看护线程 ----------
            let data_dir = default_data_dir();
            std::fs::create_dir_all(&data_dir)?;
            let config_path = data_dir.join("config.json");
            let config = if config_path.exists() {
                match AppConfig::load(&config_path) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        // 坏文件留底：新版 core 的 load 会自己改名；万一没有（旧版 core），
                        // 我们兜一手，别让后面的保存把用户原来那份配置盖掉。
                        let bad_path = data_dir.join("config.json.bad");
                        if config_path.exists() {
                            let _ = std::fs::remove_file(&bad_path);
                            let _ = std::fs::rename(&config_path, &bad_path);
                        }
                        let message = if bad_path.exists() {
                            format!(
                                "配置文件读不出来：{e}。已把原文件保留为 config.json.bad，请检查或恢复。现在先用空配置跑（本机服务和隧道都不会自动起来）。"
                            )
                        } else {
                            format!(
                                "配置文件读不出来：{e}，而且备份成 config.json.bad 也没成功。请先手动把 config.json 拷一份出来再改配置。现在先用空配置跑。"
                            )
                        };
                        eprintln!("[配置] {message}");
                        report_config_error(app.handle(), &message);
                        AppConfig::default()
                    }
                }
            } else if config_path.with_extension("json.bad").exists() {
                // config.json 不在，但同目录留着 config.json.bad：说明上次打开时配置就读坏了，
                // core 已经把原文件留底、删掉原文件。这次**不能**再建空配置：不然
                // config_save_allowed 一看「文件在、能解析」就放行自动保存，保护链断了，
                // 用户也再看不到红条。内存里拿默认配置继续跑（Guardian 会自动禁止保存），
                // 等用户把原文件修回来。
                let message = "上次的配置文件读坏了，已经保留成 config.json.bad；这次先用空白配置启动，本机服务和隧道都不会自动起来，也不会把空白配置写回磁盘。修好原文件、把 .bad 改回 config.json 再打开就能恢复。".to_string();
                eprintln!("[配置] {message}");
                report_config_error(app.handle(), &message);
                AppConfig::default()
            } else {
                // 第一次用：既没有 config.json 也没有 .bad，才允许建一份空配置。
                let cfg = AppConfig::default();
                let _ = cfg.save(&config_path);
                cfg
            };
            eprintln!("[配置] {config_path:?}");

            let guardian = Arc::new(Mutex::new(Guardian::new(
                config,
                config_path,
                data_dir.join("logs"),
            )));
            app.manage(AppState {
                guardian: guardian.clone(),
            });

            // ---------- 控制通道：命令行 / AI agent 从这儿连进来 ----------
            ctl::spawn(guardian.clone());

            let guardian_poll = guardian.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(150));
                let mut g = guardian_poll.lock().unwrap();
                for (id, event) in g.poll() {
                    eprintln!("[隧道 {id}] {event:?}");
                }
            });

            // ---------- 掉线通知：每 5 秒看一次状态 ----------
            {
                let guardian = guardian.clone();
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    use tauri_plugin_notification::NotificationExt;
                    // 连续异常 3 分钟才提醒一次（防重连抖动轰炸）
                    let mut tracker = shangxian_core::notify::NotifyTracker::new(3 * 60 * 1000);
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        let (enabled, health) = {
                            let g = guardian.lock().unwrap();
                            (g.notify_enabled(), g.health_snapshot())
                        };
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        let notices = tracker.update(now, &health);
                        if !enabled {
                            continue;
                        }
                        for notice in notices {
                            let (title, body) = match &notice {
                                shangxian_core::notify::Notice::Down { name } => (
                                    format!("发布掉线：{name}"),
                                    "连不上了，正在自动重试。".to_string(),
                                ),
                                shangxian_core::notify::Notice::Recovered { name } => (
                                    format!("发布恢复：{name}"),
                                    "重新发布成功。".to_string(),
                                ),
                            };
                            match handle.notification().builder().title(title).body(&body).show()
                            {
                                Ok(()) => eprintln!("[通知] 已发送：{body}"),
                                Err(e) => eprintln!("[通知] 发送失败：{e}"),
                            }
                        }
                    }
                });
            }

            // ---------- 托盘：图标 + 右键菜单 ----------
            // 「退出（会停止发布）」会把发布（隧道）停掉，有隧道开着时先弹确认，
            // 跟下面那个明确说「停止本地服务」的选项拉开差别；两者都不动对方管的东西。
            // 「退出并停止全部本地服务」先停掉应用自己启的本地服务（整棵进程树），再停
            // 能按程序路径核对出归属的认领进程；核对不过的（比如配置是 .bat/.cmd 包装脚本、
            // 实际监听者是子进程）不硬杀，计入「没能停」并把结果提示给用户，然后退出。
            let show_item = MenuItemBuilder::with_id("show", "打开主界面").build(app)?;
            let quit_stop_item =
                MenuItemBuilder::with_id("quit_stop_services", "退出并停止全部本地服务")
                    .build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出（会停止发布）").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&show_item, &quit_stop_item, &quit_item])
                .build()?;

            let mut tray = TrayIconBuilder::with_id("main")
                .tooltip("上线助手")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main(app),
                    // 有隧道开着发布时先问一句：退出 = 发布停掉、公网访问中断
                    "quit" => {
                        if confirm_quit_with_publishing(app) {
                            app.exit(0);
                        }
                    }
                    // 先把应用启动/认领的本地服务停掉，再退出
                    "quit_stop_services" => {
                        let state = app.state::<AppState>();
                        let (stopped, remaining) =
                            state.guardian.lock().unwrap().stop_all_for_exit();
                        notify_exit_stop_result(app, stopped, remaining);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                });

            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            let tray_icon = tray.build(app)?;

            // 托盘 tooltip 跟着发布状态走：鼠标一悬停就能看到几条在发布、几条掉线。
            {
                let tray = tray_icon.clone();
                let guardian = guardian.clone();
                std::thread::spawn(move || loop {
                    let _ = tray.set_tooltip(Some(tray_tooltip(&guardian)));
                    std::thread::sleep(std::time::Duration::from_secs(2));
                });
            }

            // ---------- 自绘标题栏：圆角 + 最大化按钮命中测试（贴靠布局要用） ----------
            if let Some(window) = app.get_webview_window("main") {
                titlebar::attach(&window, app.handle());
            }
            Ok(())
        })
        .on_page_load(|webview, payload| {
            // 界面加载好了：把启动时没送到的配置错误补发一次
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                let message = CONFIG_ERROR.lock().unwrap().clone();
                if let Some(message) = message {
                    let _ = webview.emit("config-error", serde_json::json!({ "message": message }));
                }
            }
        })
        .on_window_event(|window, event| {
            // 关闭按钮 = 缩到托盘，不退出程序（真正退出走托盘菜单）
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                // 头一回缩到托盘时说一声，别让人以为程序被关掉了（只在数据目录记一次）
                notify_tray_hint_once(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_service,
            remove_service,
            service_start,
            service_stop,
            inspect_process_launch_info,
            preview_launch_command,
            set_service_watch,
            set_service_autostart,
            get_service_logs,
            add_tunnel,
            update_tunnel,
            remove_tunnel,
            tunnel_start,
            tunnel_stop,
            tunnel_restart,
            list_local_ports,
            setup_passwordless,
            test_server,
            scan_imports,
            import_services,
            get_logs,
            save_server,
            delete_server,
            hash_password,
            get_nginx_snippet,
            check_tunnel_access,
            get_notify_enabled,
            set_notify_enabled,
            test_notification,
            get_autostart,
            set_autostart,
            get_data_dir,
            open_data_dir,
            titlebar::set_maximize_rect
        ])
        .run(tauri::generate_context!());

    // 退出时把控制通道的通行证文件删掉（命令行就不会再连一个已经关了的应用）
    shangxian_core::ctl::remove_ctl_info(&default_data_dir());
    result.expect("error while running tauri application");
}

/// 托盘 tooltip：几条在发布、几条掉线（没开发布就还是原来那句）。
fn tray_tooltip(guardian: &Arc<Mutex<Guardian>>) -> String {
    let health = guardian.lock().unwrap().health_snapshot();
    let publishing = health.iter().filter(|(_, _, ok)| *ok).count();
    let down = health.len() - publishing;
    if health.is_empty() {
        "上线助手".to_string()
    } else {
        format!("{publishing} 条发布中 · {down} 条掉线")
    }
}

/// 退出前确认：有隧道开着发布时弹个原生确认框（模态，弹在主窗口上）。
/// 没有在发布的隧道就直接放行，不打扰。
fn confirm_quit_with_publishing(app: &tauri::AppHandle) -> bool {
    let publishing = app
        .state::<AppState>()
        .guardian
        .lock()
        .unwrap()
        .health_snapshot()
        .len();
    if publishing == 0 {
        return true;
    }
    let text = format!(
        "现在有 {publishing} 条隧道开着发布。\n\n退出后发布就停了，公网访问会中断；本机服务不受影响。\n\n确定要退出吗？"
    );
    let owner = app
        .get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as *mut core::ffi::c_void)
        .unwrap_or(std::ptr::null_mut());
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let caption_w: Vec<u16> = "上线助手"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let answer = unsafe {
        MessageBoxW(
            owner,
            text_w.as_ptr(),
            caption_w.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    answer == IDYES
}

/// 头一回点关闭缩到托盘时提醒一次：怎么找回窗口、怎么真正退出。
/// 只在数据目录留个小标记文件，提示过就不再打扰（不碰 config.json）。
fn notify_tray_hint_once(app: &tauri::AppHandle) {
    let marker = default_data_dir().join("tray-hint-shown");
    if marker.exists() {
        return;
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 先把标记写上：通知发不出去也不再反复弹
    let _ = std::fs::write(&marker, b"1");

    use tauri_plugin_notification::NotificationExt;
    let body = "已缩到托盘，右键图标可退出；左键点图标可以打开主界面。";
    match app.notification().builder().title("上线助手").body(body).show() {
        Ok(()) => eprintln!("[通知] 已发送：{body}"),
        Err(e) => {
            eprintln!("[通知] 发送失败：{e}");
            append_app_log(&format!("[托盘] {body}"));
        }
    }
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
