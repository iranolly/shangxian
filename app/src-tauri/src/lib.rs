// 上线助手桌面壳：窗口 + 托盘常驻 + 隧道守护接入。
// 核心看护逻辑在 shangxian-core；这一层负责生命周期与界面命令。

use std::sync::{Arc, Mutex};

use shangxian_core::config::{default_data_dir, AppConfig};
use shangxian_core::guardian::{
    Guardian, NewTunnelInput, ServerInput, ServerView, ServiceInput, ServiceView, TunnelView,
    UpdateTunnelInput,
};
use shangxian_core::import::ImportSuggestion;
use shangxian_core::local_ports::{list_listening, LocalPort};
use shangxian_core::nginx::NginxSnippet;
use shangxian_core::protection::AccessCheck;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, State, WindowEvent,
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

/// 停止一台本机服务（只停我们自己启动的进程）。
#[tauri::command]
fn service_stop(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.guardian.lock().unwrap().stop_service(&id)
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

/// 在服务器上生成访问密码的哈希。
#[tauri::command]
fn hash_password(
    server_id: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    state.guardian.lock().unwrap().hash_password(&server_id, &password)
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
                AppConfig::load(&config_path).unwrap_or_else(|e| {
                    eprintln!("[配置] 读取失败，先按空配置启动：{e}");
                    AppConfig::default()
                })
            } else {
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
                                    "服务掉线",
                                    format!("「{name}」连不上了，正在自动重试。"),
                                ),
                                shangxian_core::notify::Notice::Recovered { name } => (
                                    "服务已恢复",
                                    format!("「{name}」重新发布成功。"),
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
            let show_item = MenuItemBuilder::with_id("show", "打开主界面").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&show_item, &quit_item])
                .build()?;

            let mut tray = TrayIconBuilder::with_id("main")
                .tooltip("上线助手")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main(app),
                    "quit" => app.exit(0),
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
            tray.build(app)?;

            // ---------- 自绘标题栏：圆角 + 最大化按钮命中测试（贴靠布局要用） ----------
            if let Some(window) = app.get_webview_window("main") {
                titlebar::attach(&window, app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭按钮 = 缩到托盘，不退出程序（真正退出走托盘菜单）
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_service,
            remove_service,
            service_start,
            service_stop,
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

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
