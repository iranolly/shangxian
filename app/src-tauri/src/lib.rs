// 上线助手桌面壳：窗口 + 托盘常驻 + 隧道守护接入。
// 核心看护逻辑在 shangxian-core；这一层负责生命周期与界面命令。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use shangxian_core::config::{self, AppConfig, Settings, default_data_dir};
use shangxian_core::events::{EventLine, EventLog, EventSubject};
use shangxian_core::guardian::{
    Guardian, NewTunnelInput, ServerInput, ServerView, ServiceInput, ServiceView, StopOutcome,
    TunnelView, UpdateTunnelInput,
};
use shangxian_core::import::ImportSuggestion;
use shangxian_core::local_ports::{LocalPort, list_listening};
use shangxian_core::log::{
    TAG_CONFIG, TAG_CRASH, TAG_GUARD, TAG_NOTIFY, TAG_POWER, TAG_START, TAG_TUNNEL, log_error,
    log_info, log_warn,
};
use shangxian_core::nginx::NginxSnippet;
use shangxian_core::proc_detail::LaunchInfo;
use shangxian_core::reason::ReasonCode;
use shangxian_core::state::{StateStore, UpdateInfo};
use tauri::{
    Emitter, Manager, State, WindowEvent,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO, MessageBoxW,
};

mod ctl;
mod logging;
mod mainwindow;
mod notify;
mod power;
mod recovery;
mod singleton;
mod titlebar;

/// 给界面的一次完整快照（v2：多了 `app`）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    app: AppView,
    /// 本机服务（这台电脑上的东西）
    services: Vec<ServiceView>,
    /// 发布隧道（服务 → 服务器）
    tunnels: Vec<TunnelView>,
    servers: Vec<ServerView>,
}

/// 应用自己的状态（D2 §6.1 冻结）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppView {
    snapshot_version: u32,
    pid: u32,
    app_version: String,
    data_dir: String,
    started_at: i64,
    job_guard_ok: bool,
    job_guard_error: Option<String>,
    config_trusted: bool,
    update_info: UpdateInfo,
    settings: Settings,
    // 下面两个来自 ctl `status.app` 现状，必须保留（旧 CLI 读它们）
    notify_enabled: bool,
    autostart: bool,
}

/// 从共享上下文拼一份 `AppView`（Tauri 快照与控制通道 `status` 用同一份）。
pub(crate) fn build_app_view(ctx: &ctl::Ctx) -> AppView {
    let (settings, notify_enabled) = match ctx.lock_guardian() {
        Ok(g) => (g.settings(), g.notify_enabled()),
        Err(_) => (Settings::default(), true),
    };
    AppView {
        snapshot_version: 2,
        pid: std::process::id(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        data_dir: ctx.data_dir.display().to_string(),
        started_at: ctx.started_at,
        job_guard_ok: ctx.job_guard_ok,
        job_guard_error: ctx.job_guard_error.clone(),
        config_trusted: ctx.config_trusted(),
        update_info: ctx.state.update_info(),
        settings,
        notify_enabled,
        autostart: autostart_enabled(),
    }
}

/// 应用共享状态：Tauri 命令从这里拿 Ctx。
pub(crate) struct AppState {
    pub(crate) ctx: ctl::Ctx,
}

// ---------- 启动自检：子进程保底清理 ----------

static JOB_GUARD_OK: OnceLock<bool> = OnceLock::new();

fn job_guard_ok() -> bool {
    *JOB_GUARD_OK.get_or_init(shangxian_core::job::self_check)
}

fn job_guard_error() -> Option<String> {
    if job_guard_ok() {
        None
    } else {
        Some(
            "子进程保底清理没装配好：创建或指派作业对象失败；会有残留进程的风险。建议先重启电脑；还不行就检查安全软件。"
                .to_string(),
        )
    }
}

// ---------- 一次性系统提示（sys-notice） ----------

/// sticky 的提示先在内存里记一份：界面刚建出来时事件可能比页面先到，加载完补发。
static PENDING_SYS_NOTICES: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());

/// 发一条一次性系统提示给界面（D1 §14.8）：`{level, text, sticky}`。
pub(crate) fn sys_notice(app: &tauri::AppHandle, level: &str, text: &str, sticky: bool) {
    let payload = serde_json::json!({ "level": level, "text": text, "sticky": sticky });
    log_info(TAG_CONFIG, text);
    if sticky {
        PENDING_SYS_NOTICES.lock().unwrap().push(payload.clone());
    }
    if let Err(e) = app.emit("sys-notice", payload) {
        log_warn(TAG_CONFIG, &format!("界面还没起来，提示先记着：{e}"));
    }
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
    let g = guardian
        .lock()
        .map_err(|_| "内部状态锁坏了：重启一下应用".to_string())?;
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
    let out =
        shangxian_core::cleanup::run(&conn.exec(), "echo shangxian-ok", Duration::from_secs(15))
            .map_err(|e| ssh_error_human(&e, conn))?;
    if !out.contains("shangxian-ok") {
        return Err(format!(
            "连上了，但服务器回的话看不懂（{}）。看看这台服务器的默认 shell 是不是被改过。",
            out.trim()
        ));
    }
    Ok(format!(
        "连接成功，用时 {:.1} 秒",
        started.elapsed().as_secs_f64()
    ))
}

/// 把 ssh 的报错翻成用户能懂的话。
///
/// 关键字表只有 `core::reason` 一份（[`ReasonCode::from_ssh_exit`] 内部走
/// `from_ssh_text`），这里只补上连接地址和原始报错，方便对不上时排查。
fn ssh_error_human(raw: &str, conn: &ServerConn) -> String {
    // 没有退出码的场合按 255 兜底：认不出关键字就是 exitUnknown，和隧道退出同一套规则。
    let code = ReasonCode::from_ssh_exit(raw, 255);
    let text = code.render(&[]);
    if matches!(code, ReasonCode::SshMissing) {
        // 本机根本没装 ssh：提服务器地址没意义
        return text;
    }
    format!(
        "连不上 {}（端口 {}）：{text}。原始报错：{}",
        conn.target(),
        conn.ssh_port,
        raw.trim()
    )
}

// ---------- 配置读坏时的红条 ----------

/// 配置读坏的原因（界面加载完会由 `on_page_load` 补发一次，免得事件比界面先到）。
static CONFIG_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// 告诉界面「配置文件读不出来」：事件名 `config-error`，payload `{ message: string }`。
fn report_config_error(app: &tauri::AppHandle, message: &str) {
    *CONFIG_ERROR.lock().unwrap() = Some(message.to_string());
    if let Err(e) = app.emit("config-error", serde_json::json!({ "message": message })) {
        log_warn(
            TAG_CONFIG,
            &format!("通知界面失败：{e}（页面加载后会补发）"),
        );
    }
}

/// 配置读坏的事件（D2 §16）：core::events 目前没有同名的发射器，先在这里按同样的形状发。
fn emit_config_corrupt(events: &EventLog, message: &str) {
    events.append(EventLine::new(
        "app",
        "error",
        Some(ReasonCode::ConfigCorrupt),
        Some(EventSubject::app()),
        message,
    ));
}

// ---------- 崩溃自动恢复开关（标记文件实现，见 recovery.rs 注释） ----------

/// 开关崩溃自动恢复：先注册/注销，成功再写开关文件；失败不动开关。
pub(crate) fn apply_crash_recovery(
    data_dir: &std::path::Path,
    enabled: bool,
) -> Result<(), String> {
    recovery::install(enabled)?;
    recovery::set_enabled_marker(data_dir, enabled)?;
    if enabled {
        recovery::clear_recovery_state(data_dir);
        log_info(TAG_CRASH, "用户已重新开启崩溃自动恢复");
    } else {
        log_info(TAG_CRASH, "用户已关闭崩溃自动恢复");
    }
    Ok(())
}

// ============================================================
// Tauri 命令
// ============================================================

/// 直接调控制通道分发器（新命令都走这条路，界面/CLI 共用同一份实现）。
macro_rules! ctl_params {
    ($($key:literal => $arg:ident),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut map = serde_json::Map::new();
        $(map.insert($key.to_string(), serde_json::json!($arg));)*
        serde_json::Value::Object(map)
    }};
}

macro_rules! ctl_sync {
    ($name:ident, $method:literal $(, $key:literal => $arg:ident : $ty:ty)*) => {
        #[tauri::command]
        fn $name(state: State<'_, AppState> $(, $arg: $ty)*) -> Result<serde_json::Value, String> {
            let params = ctl_params!($($key => $arg),*);
            ctl::dispatch(&state.ctx, $method, &params)
        }
    };
}

macro_rules! ctl_async {
    ($name:ident, $method:literal $(, $key:literal => $arg:ident : $ty:ty)*) => {
        #[tauri::command]
        async fn $name(state: State<'_, AppState> $(, $arg: $ty)*) -> Result<serde_json::Value, String> {
            let ctx = state.ctx.clone();
            let params = ctl_params!($($key => $arg),*);
            tauri::async_runtime::spawn_blocking(move || ctl::dispatch(&ctx, $method, &params))
                .await
                .map_err(|e| format!("内部错误：{e}"))?
        }
    };
}

#[tauri::command]
fn get_snapshot(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let ctx = &state.ctx;
    let app = build_app_view(ctx);
    let g = ctx.lock_guardian()?;
    Ok(Snapshot {
        app,
        services: g.snapshot(),
        tunnels: g.tunnel_snapshot(),
        servers: g.servers(),
    })
}

// ---------- 本机服务 ----------

/// 新增/编辑一台本机服务。
#[tauri::command]
fn save_service(input: ServiceInput, state: State<'_, AppState>) -> Result<String, String> {
    state.ctx.lock_guardian()?.save_service(input)
}

/// 删除一台本机服务（还有隧道在用时会拒绝；返回可撤销的令牌）。
#[tauri::command]
fn remove_service(id: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let info = state.ctx.lock_guardian()?.remove_service(&id)?;
    Ok(ctl::dispatch_delete_result("service", &info))
}

/// 启动一台本机服务。
#[tauri::command]
fn service_start(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.ctx.lock_guardian()?.start_service(&id)
}

/// 停止一台本机服务。
///
/// 默认只停我们自己启动的进程；端口上是外部进程时先返回 `needsConfirm`（带 pid/exe），
/// 界面问过用户后带上 `killForeign=true` 和同一个 `expectedPid` 再调一次才会真的结束它。
#[tauri::command]
async fn service_stop(
    id: String,
    kill_foreign: Option<bool>,
    expected_pid: Option<u32>,
    state: State<'_, AppState>,
) -> Result<StopOutcome, String> {
    let guardian = state.ctx.guardian.clone();
    tauri::async_runtime::spawn_blocking(move || {
        guardian
            .lock()
            .map_err(|_| "内部状态锁坏了：重启一下应用".to_string())?
            .stop_service(&id, kill_foreign.unwrap_or(false), expected_pid)
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))?
}

/// 自动读取某个端口上进程的启动方式（程序/参数/工作目录 + 警告）。
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
    state.ctx.lock_guardian()?.set_service_watch(&id, watch)
}

/// 开机自启开关。
#[tauri::command]
fn set_service_autostart(
    id: String,
    autostart: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .ctx
        .lock_guardian()?
        .set_service_autostart(&id, autostart)
}

/// 本机服务自己的输出（程序日志 + 看护记录）。
#[tauri::command]
fn get_service_logs(id: String, lines: Option<usize>, state: State<'_, AppState>) -> String {
    state
        .ctx
        .lock_guardian()
        .map(|g| g.service_log_tail(&id, lines.unwrap_or(200)))
        .unwrap_or_default()
}

// 重启本机服务并轮转日志（日志太大时的手动入口）。
ctl_sync!(restart_service_rotate_logs, "restart_service_rotate_logs", "id" => id: String);

// ---------- 发布隧道 ----------

/// 新建一条隧道。
#[tauri::command]
fn add_tunnel(input: NewTunnelInput, state: State<'_, AppState>) -> Result<String, String> {
    state.ctx.lock_guardian()?.add_tunnel(input)
}

/// 编辑一条隧道。
#[tauri::command]
fn update_tunnel(input: UpdateTunnelInput, state: State<'_, AppState>) -> Result<(), String> {
    state.ctx.lock_guardian()?.update_tunnel(input)
}

/// 停止并删除一条隧道（返回可撤销的令牌）。
#[tauri::command]
fn remove_tunnel(id: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let info = state.ctx.lock_guardian()?.remove_tunnel(&id)?;
    Ok(ctl::dispatch_delete_result("tunnel", &info))
}

/// 开始发布。
#[tauri::command]
fn tunnel_start(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.ctx.lock_guardian()?.set_enabled(&id, true)
}

/// 暂停发布。
#[tauri::command]
fn tunnel_stop(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.ctx.lock_guardian()?.set_enabled(&id, false)
}

/// 立刻重连。
#[tauri::command]
fn tunnel_restart(id: String, state: State<'_, AppState>) -> Result<(), String> {
    state.ctx.lock_guardian()?.restart(&id)
}

// 清掉这台服务器的 SSH 指纹记录（服务器重装/换机器后连不上时用）。
ctl_sync!(forget_host_key, "forget_host_key", "id" => id: String);

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
    let ctx = state.ctx.clone();
    let guardian = ctx.guardian.clone();
    let key_path = shangxian_core::config::key_path_for(&default_data_dir(), &server_id);

    let task_server_id = server_id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let conn = server_conn(&guardian, &task_server_id)?;
        shangxian_core::keyboot::bootstrap_key(
            &conn.host,
            conn.ssh_port,
            &conn.user,
            &password,
            &key_path,
        )
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))??;

    ctx.lock_guardian()?
        .set_server_key(&server_id, &result.key_path)?;
    Ok(format!(
        "配好了！以后连这台服务器都走密钥，不用再输密码。密钥放在：{}",
        result.key_path
    ))
}

/// 掉线通知开关。
#[tauri::command]
fn get_notify_enabled(state: State<'_, AppState>) -> bool {
    state
        .ctx
        .lock_guardian()
        .map(|g| g.notify_enabled())
        .unwrap_or(true)
}

#[tauri::command]
fn set_notify_enabled(enabled: bool, state: State<'_, AppState>) -> Result<(), String> {
    state.ctx.lock_guardian()?.set_notify_enabled(enabled)
}

// 免打扰时段（读）。
ctl_sync!(get_notify_quiet, "get_notify_quiet");

/// 免打扰时段（设置页直接给三块字段）。

/// 立刻发一条测试通知（设置页“试一下”）。
#[tauri::command]
fn test_notification(app: tauri::AppHandle) -> Result<(), String> {
    send_test_notification(&app)
}

fn send_test_notification(app: &tauri::AppHandle) -> Result<(), String> {
    if notify::send_toast(
        app,
        "上线助手",
        "通知长这样：服务掉线或恢复时，我会在这里提醒你。",
    ) {
        Ok(())
    } else {
        Err("通知发不出去；可以到系统设置里看看通知权限".to_string())
    }
}

/// 托盘「退出并停止全部本地服务」的结果提示。
///
/// release 版没有控制台，`eprintln` 用户看不到；走统一的 `send_toast`
/// （发不出去时它自己会写 app.log + 给界面发 sys-notice）。
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
    notify::send_toast(app, "上线助手", &body);
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
    state.ctx.lock_guardian()?.import_services(items)
}

/// 读服务日志尾部。
#[tauri::command]
fn get_logs(id: String, lines: Option<usize>, state: State<'_, AppState>) -> String {
    state
        .ctx
        .lock_guardian()
        .map(|g| g.log_tail(&id, lines.unwrap_or(200)))
        .unwrap_or_default()
}

/// 新增/编辑服务器。
#[tauri::command]
fn save_server(input: ServerInput, state: State<'_, AppState>) -> Result<String, String> {
    state.ctx.lock_guardian()?.save_server(input)
}

/// 删除服务器（还有服务挂着时会拒绝；返回可撤销的令牌）。
#[tauri::command]
fn delete_server(id: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let info = state.ctx.lock_guardian()?.delete_server(&id)?;
    Ok(ctl::dispatch_delete_result("server", &info))
}

/// 在本机生成访问密码的哈希（bcrypt `$2y$…`，给 nginx 用）；密码只在本机算。
#[tauri::command]
async fn hash_password(
    server_id: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let ctx = state.ctx.clone();
    let guardian = ctx.guardian.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = server_conn(&guardian, &server_id)?;
        shangxian_core::cleanup::hash_password(&conn.exec(), &password)
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))?
}

/// 测试连接：真连一次这台服务器（SSH 跑一句 echo）。
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
    let guardian = state.ctx.guardian.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = server_conn(&guardian, &server_id)?;
        run_server_test(&conn)
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))?
}

/// 生成服务的 nginx 配置片段（人工应用到面板）。
#[tauri::command]
fn get_nginx_snippet(id: String, state: State<'_, AppState>) -> Result<NginxSnippet, String> {
    state.ctx.lock_guardian()?.nginx_snippet(&id)
}

/// 核对云端 nginx 上这条隧道的实际访问保护（走 SSH 只读读配置，不改服务器）。
/// 结果会顺手存进运行记录（protectionCheck）。
#[tauri::command]
async fn check_tunnel_access(
    id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let ctx = state.ctx.clone();
    tauri::async_runtime::spawn_blocking(move || {
        ctl::dispatch(
            &ctx,
            "check_tunnel_access",
            &serde_json::json!({ "id": id }),
        )
    })
    .await
    .map_err(|e| format!("内部错误：{e}"))?
}

// ---------- 本轮新增：统计 / 日志 / 体检 / 诊断 ----------

ctl_sync!(get_stats, "stats");
ctl_async!(
    query_logs,
    "logs_query",
    "source" => source: Option<String>,
    "id" => id: Option<String>,
    "lines" => lines: Option<usize>,
    "archive" => archive: Option<u8>,
    "format" => format: Option<String>
);
ctl_async!(
    run_doctor,
    "doctor",
    "mode" => mode: Option<String>,
    "id" => id: Option<String>
);
ctl_async!(
    build_diag,
    "build_diag",
    "out" => out: Option<String>,
    "deep" => deep: Option<bool>
);

// ---------- 本轮新增：删除撤销 / 配置 / 设置 / 更新 ----------

ctl_sync!(list_deleted, "list_deleted");
ctl_sync!(restore_deleted, "restore_deleted", "token" => token: String);
ctl_sync!(purge_deleted, "purge_deleted", "token" => token: String);
ctl_async!(
    export_config,
    "export_config",
    "out" => out: Option<String>,
    "redacted" => redacted: Option<bool>
);
ctl_async!(
    import_config,
    "import_config",
    "doc" => doc: Option<String>,
    "path" => path: Option<String>,
    "mode" => mode: Option<String>,
    "dryRun" => dry_run: Option<bool>
);
ctl_async!(check_update, "check_update");
ctl_sync!(get_settings, "get_settings");

/// 设置项整份保存（D2 §9.2：三块全给，未知字段拒绝）。
#[tauri::command]
fn set_settings(
    settings: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    ctl::dispatch(&state.ctx, "set_settings", &settings)
}

/// 免打扰时段（三个字段一起给）。
#[tauri::command]
fn set_notify_quiet(
    enabled: bool,
    start: String,
    end: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let params = serde_json::json!({ "enabled": enabled, "start": start, "end": end });
    ctl::dispatch(&state.ctx, "set_notify_quiet", &params)
}

// ---------- 本轮新增：崩溃自恢复 ----------

ctl_sync!(get_recovery_state, "get_recovery_state");
ctl_sync!(clear_recovery_state, "clear_recovery_state");

#[tauri::command]
fn set_crash_recovery(enabled: bool, state: State<'_, AppState>) -> Result<(), String> {
    apply_crash_recovery(&state.ctx.data_dir, enabled)
}

// ---------- 本轮新增：云端完整检查（check_tunnel_cloud / 环境识别 / 访问审计 / 公网 IP） ----------

ctl_async!(
    check_tunnel_cloud,
    "check_tunnel_cloud",
    "id" => id: String,
    "mode" => mode: Option<String>
);
ctl_async!(scan_server_env, "scan_server_env", "serverId" => server_id: String);
ctl_async!(run_access_audit, "run_access_audit", "id" => id: String);
ctl_async!(get_public_ip, "get_public_ip");

// ---------- 本轮新增：Tauri 专用帮手 ----------

/// 把一段文本存成文件（日志「导出文件…」用）。
/// 固定落 `<数据目录>/exports/`；`name` 只当文件名用，路径分隔符会被清掉。
#[tauri::command]
fn save_text_file(
    name: String,
    content: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    if content.len() > 20 * 1024 * 1024 {
        return Err("内容太大了（超过 20MB）：分段复制，或者先用诊断包".to_string());
    }
    let safe: String = name
        .chars()
        .map(|c| {
            if c == '/'
                || c == '\\'
                || c == ':'
                || c == '*'
                || c == '?'
                || c == '"'
                || c == '<'
                || c == '>'
                || c == '|'
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = safe.trim().trim_matches('.').to_string();
    let safe = if safe.is_empty() {
        "导出.txt".to_string()
    } else {
        safe
    };
    let dir = state.ctx.data_dir.join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("存文件失败：目录建不出来（{e}）"))?;
    let mut path = dir.join(&safe);
    if path.exists() {
        let stem = std::path::Path::new(&safe)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "导出".to_string());
        let ext = std::path::Path::new(&safe)
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()))
            .unwrap_or_default();
        for n in 2..1000 {
            let candidate = dir.join(format!("{stem}-{n}{ext}"));
            if !candidate.exists() {
                path = candidate;
                break;
            }
        }
    }
    std::fs::write(&path, content.as_bytes()).map_err(|e| format!("存文件失败：{e}"))?;
    Ok(serde_json::json!({ "path": path.display().to_string() }))
}

/// 打开日志文件夹。优先走已装 opener 插件的 reveal；不可用就退回 explorer。
#[tauri::command]
fn open_log_folder(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let dir = state.ctx.data_dir.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("日志文件夹建不出来：{e}"))?;
    use tauri_plugin_opener::OpenerExt;
    match app.opener().reveal_item_in_dir(&dir) {
        Ok(()) => Ok(()),
        Err(e) => {
            log_warn(
                TAG_CONFIG,
                &format!("用系统文件管理器打开失败（{e}），换 explorer 再试"),
            );
            std::process::Command::new("explorer")
                .arg(&dir)
                .spawn()
                .map_err(|e| format!("打开文件夹失败：{e}"))?;
            Ok(())
        }
    }
}

/// 列出日志归档文件（当前 `.1` / `.2` 备份）的元信息，给界面的「归档」下拉用。
#[tauri::command]
fn get_log_archives(state: State<'_, AppState>) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    let mut push = |path: &std::path::Path, origin: &str, archive: u8| {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        items.push(serde_json::json!({
            "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            "path": path.display().to_string(),
            "origin": origin,
            "archive": archive,
            "bytes": meta.len(),
            "modifiedAt": modified,
        }));
    };

    let logs = state.ctx.data_dir.join("logs");
    for archive in 1..=2u8 {
        push(
            &std::path::PathBuf::from(format!("{}.{}", logs.join("app.log").display(), archive)),
            "appLog",
            archive,
        );
        push(
            &std::path::PathBuf::from(format!(
                "{}.{}",
                state.ctx.data_dir.join("events.jsonl").display(),
                archive
            )),
            "events",
            archive,
        );
    }
    if let Ok(dir) = std::fs::read_dir(&logs) {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_backup = name.ends_with(".1") || name.ends_with(".2");
            if !is_backup || !name.contains(".log") {
                continue;
            }
            let archive = if name.ends_with(".1") { 1 } else { 2 };
            let origin = if name.contains(".ssh.log") {
                "tunnel"
            } else if name.starts_with("service-") {
                "service"
            } else {
                continue;
            };
            push(&entry.path(), origin, archive);
        }
    }
    items.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
    items
}

/// 数据目录的路径和来源（便携模式展示用）。
#[tauri::command]
fn get_data_dir_info() -> serde_json::Value {
    let (path, kind) = config::data_dir_info();
    serde_json::json!({ "path": path.display().to_string(), "kind": kind })
}

/// 配置文件夹路径（设置页展示；保持返回字符串不动）。
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

// ============================================================
// 启动 / 退出
// ============================================================

/// 启动失败时的人话弹窗（进不了界面也要让用户看到原因）。
fn fatal_dialog(message: &str) {
    eprintln!("[启动] {message}");
    let _ = log_error(TAG_START, message);
    let text_w: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let caption_w: Vec<u16> = "上线助手"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            caption_w.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

/// 退出前的收尾：事件、落盘、删通行证、注销系统回调。
fn on_exit(ctx: &ctl::Ctx) {
    ctx.events.append(EventLine::new(
        "app",
        "info",
        Some(ReasonCode::AppExited),
        Some(EventSubject::app()),
        "应用已退出",
    ));
    ctx.state.flush();
    power::uninstall();
    shangxian_core::ctl::remove_ctl_info(&ctx.data_dir);
    log_info(TAG_START, "应用已退出");
}

/// Tauri `setup`：配置、看护、托盘、窗口策略都在这里。
fn setup(
    app: &mut tauri::App,
    data_dir: std::path::PathBuf,
    logging_error: Option<String>,
    registration_error: Option<String>,
    recovery_paused: bool,
    recovered: bool,
    hidden: bool,
    test_notify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    log_info(TAG_START, "应用已启动");

    // ---------- 运行状态 / 事件日志 ----------
    // 先建 state / events：读配置的迁移、归一化、读坏、降级都要能立刻记事件（D2 §16）。
    let state = Arc::new(StateStore::load(&data_dir));
    let events = Arc::new(EventLog::new(&data_dir));

    // ---------- 配置 ----------
    let config_path = data_dir.join("config.json");
    let mut config_warning: Option<String> = None;
    let config = match AppConfig::load_detailed(&config_path) {
        Ok((cfg, outcome)) => {
            if let Some(from) = outcome.migrated_from {
                log_info(TAG_CONFIG, &format!("旧版设置已从 v{from} 升级"));
            }
            if !outcome.normalized.is_empty() {
                log_warn(
                    TAG_CONFIG,
                    &format!(
                        "设置里有不认识的取值，已经按默认值处理：{}",
                        outcome.normalized.join("、")
                    ),
                );
            }
            // 迁移 / 归一化各记一条事件（文案由 core 定死）。
            shangxian_core::events::emit_config_load_events(&events, &outcome);
            cfg
        }
        Err(e) => {
            let message = e.to_string();
            log_error(TAG_CONFIG, &message);
            if let Some(found) = config::config_version_too_new(&config_path) {
                // 降级拦截：设置版本比程序新，原文件一字没动（D2 §13.2）。
                shangxian_core::events::emit_config_unsupported(&events, found);
            } else {
                // 真读坏：原文件已经留底成 config.json.bad（D2 §13.1）。
                emit_config_corrupt(&events, &message);
            }
            config_warning = Some(message);
            AppConfig::default()
        }
    };
    // 第一次运行（既没有 config.json 也没有读坏留底）才允许建一份空配置。
    if !config_path.exists() && !config_path.with_extension("json.bad").exists() {
        let _ = config.save(&config_path);
    }

    if let Some(reason) = state.reset_reason() {
        log_warn(TAG_CONFIG, reason.message());
        events.append(EventLine::new(
            "app",
            "warn",
            Some(reason.code()),
            Some(EventSubject::app()),
            reason.message(),
        ));
    }
    events.append(EventLine::new(
        "app",
        "info",
        Some(ReasonCode::AppStarted),
        Some(EventSubject::app()),
        "应用已启动",
    ));

    // ---------- 守护进程 ----------
    let reporter = shangxian_core::log::reporter();
    let guardian = Arc::new(Mutex::new(Guardian::new(
        config,
        config_path,
        data_dir.join("logs"),
        reporter,
    )));
    {
        let mut g = guardian
            .lock()
            .map_err(|_| "内部状态锁坏了：重启一下应用")?;
        g.attach_state(state.clone());
        g.attach_events(events.clone());
    }

    let ctx = ctl::Ctx {
        guardian: guardian.clone(),
        state: state.clone(),
        events: events.clone(),
        data_dir: data_dir.clone(),
        started_at: shangxian_core::state::now_ms(),
        job_guard_ok: job_guard_ok(),
        job_guard_error: job_guard_error(),
    };
    app.manage(AppState { ctx: ctx.clone() });

    if let Some(message) = config_warning {
        report_config_error(app.handle(), &message);
    }
    if let Some(e) = logging_error {
        sys_notice(app.handle(), "warn", &e, false);
    }
    if let Some(e) = registration_error {
        sys_notice(
            app.handle(),
            "warn",
            &format!("这台电脑不允许登记崩溃恢复；崩溃后需要你手动打开应用。（{e}）"),
            true,
        );
    }
    if let Some(err) = job_guard_error() {
        sys_notice(
            app.handle(),
            "warn",
            "这台电脑限制了应用的进程管理功能：隧道可能留下没关干净的连接。建议先重启电脑；还不行就检查安全软件。",
            true,
        );
        log_warn(TAG_GUARD, &err);
    }

    // ---------- 控制通道 ----------
    ctl::spawn(ctx.clone());

    // 自动漂移复检已下线（用户要求轻量）：云端核对只保留手动「检查云端」。

    // ---------- 看护轮询：事件日志 + 唤醒提示 ----------
    {
        let ctx = ctx.clone();
        let handle = app.handle().clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(150));
                let events = {
                    let mut g = match ctx.guardian.lock() {
                        Ok(g) => g,
                        Err(_) => continue,
                    };
                    g.poll()
                };
                for (id, event) in events {
                    match &event {
                        shangxian_core::supervisor::Event::SystemWake { kind, .. } => {
                            let text = match kind {
                                shangxian_core::wake::WakeKind::Resumed { .. } => {
                                    "电脑刚醒过来，正在马上重新连上服务器…"
                                }
                                shangxian_core::wake::WakeKind::NetworkChanged => {
                                    "网络刚恢复，正在马上重连…"
                                }
                                shangxian_core::wake::WakeKind::ClockJump { .. } => {
                                    "系统时间变了，正在重新连…"
                                }
                            };
                            log_info(TAG_POWER, text);
                            sys_notice(&handle, "info", text, false);
                        }
                        other => log_info(TAG_TUNNEL, &format!("隧道 {id}：{other:?}")),
                    }
                }
            }
        });
    }

    // ---------- 运行状态落盘调度（每秒 tick；跨日每分钟一次） ----------
    {
        let state = state.clone();
        std::thread::spawn(move || {
            let mut last_rollover = shangxian_core::state::now_ms();
            loop {
                std::thread::sleep(Duration::from_secs(1));
                state.tick();
                let now = shangxian_core::state::now_ms();
                if now.saturating_sub(last_rollover) >= 60_000 {
                    last_rollover = now;
                    state.day_rollover(now);
                }
            }
        });
    }

    // ---------- 通知线程 ----------
    notify::spawn_guard(guardian.clone(), app.handle().clone());
    if let Err(e) = notify::install_click_handler(app.handle()) {
        log_warn(TAG_NOTIFY, &e);
    }

    // ---------- 电源 / 网络回调 ----------
    // 注册成功时 install() 自己会记一条「已注册」；这里只管失败提示，不重复记。
    if let Err(e) = power::install() {
        sys_notice(app.handle(), "warn", &e, false);
    }

    // ---------- 启动 30 秒后静默检查更新 ----------
    ctl::spawn_update_guard(ctx.clone(), app.handle().clone());

    // ---------- 托盘：图标 + 右键菜单 ----------
    let show_item = MenuItemBuilder::with_id("show", "打开主界面").build(app)?;
    let quit_stop_item =
        MenuItemBuilder::with_id("quit_stop_services", "退出并停止全部本地服务").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "退出（会停止发布）").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&show_item, &quit_stop_item, &quit_item])
        .build()?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("上线助手")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => mainwindow::show_or_create(app),
            // 有隧道开着发布时先问一句：退出 = 发布停掉、公网访问中断
            "quit" => {
                if confirm_quit_with_publishing(app) {
                    app.exit(0);
                }
            }
            // 先把应用启动/认领的本地服务停掉，再退出
            "quit_stop_services" => {
                let state = app.state::<AppState>();
                let (stopped, remaining) = state
                    .ctx
                    .lock_guardian()
                    .map(|mut g| g.stop_all_for_exit())
                    .unwrap_or((0, 0));
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
                mainwindow::show_or_create(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    let tray_icon = match tray.build(app) {
        Ok(icon) => icon,
        Err(e) => {
            // 托盘建不出来又没窗口 → 应用会“看不见但活着”；立刻建窗口把错误给用户看。
            log_error("托盘", &format!("托盘图标建不出来：{e}"));
            mainwindow::show_or_create(app.handle());
            sys_notice(
                app.handle(),
                "error",
                &format!("托盘图标没建出来（{e}）：窗口关了就得重新打开应用"),
                true,
            );
            return Ok(());
        }
    };

    // 托盘 tooltip 跟着发布状态走：鼠标一悬停就能看到几条在发布、几条掉线。
    {
        let tray = tray_icon.clone();
        let guardian = guardian.clone();
        std::thread::spawn(move || {
            loop {
                let _ = tray.set_tooltip(Some(tray_tooltip(&guardian)));
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }

    // ---------- 启动矩阵：什么时候建窗口（D1 §13.4） ----------
    if test_notify {
        let _ = send_test_notification(app.handle());
    }
    let show_window = if test_notify {
        false
    } else if recovered {
        recovery_paused
    } else {
        !hidden
    };
    if show_window {
        mainwindow::show_or_create(app.handle());
    }
    if recovery_paused {
        sys_notice(
            app.handle(),
            "error",
            "程序刚才连续崩溃了 3 次，已经暂停自动恢复。服务还在跑；想看原因就去「设置 → 打开日志文件夹」。",
            true,
        );
    }

    Ok(())
}

/// 托盘 tooltip：几条在发布、几条掉线（没开发布就还是原来那句）。
fn tray_tooltip(guardian: &Arc<Mutex<Guardian>>) -> String {
    let health = match guardian.lock() {
        Ok(g) => g.health_snapshot(),
        Err(_) => return "上线助手".to_string(),
    };
    let (publishing, down) = notify::tunnel_health(&health);
    if publishing + down == 0 {
        "上线助手".to_string()
    } else {
        format!("{publishing} 条发布中 · {down} 条掉线")
    }
}

/// 退出前确认：有隧道开着发布时弹个原生确认框（模态，弹在主窗口上）。
/// 没有在发布的隧道就直接放行，不打扰。
fn confirm_quit_with_publishing(app: &tauri::AppHandle) -> bool {
    let total = app
        .state::<AppState>()
        .ctx
        .lock_guardian()
        .map(|g| {
            let health = g.health_snapshot();
            let (publishing, down) = notify::tunnel_health(&health);
            publishing + down
        })
        .unwrap_or(0);
    if total == 0 {
        return true;
    }
    let text = format!(
        "现在有 {total} 条隧道开着发布。\n\n退出后发布就停了，公网访问会中断；本机服务不受影响。\n\n确定要退出吗？"
    );
    let owner = app
        .get_webview_window(mainwindow::MAIN_LABEL)
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

/// 头一回点关闭（窗口销毁）时提醒一次：窗口只是关了，程序还在托盘里。
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
    notify::send_toast(
        app,
        "上线助手",
        "窗口关了，程序还在托盘里看着服务；右键托盘图标可以退出。",
    );
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ① 数据目录：便携解析最先（失败即中止，绝不静默回退到别处）
    let data_dir = match config::try_default_data_dir() {
        Ok(dir) => dir,
        Err(e) => {
            fatal_dialog(&e.to_string());
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        fatal_dialog(&format!(
            "数据目录建不出来（{e}）：{}。把程序挪到能写的文件夹，或者删掉便携标记再用普通模式。",
            data_dir.display()
        ));
        std::process::exit(1);
    }

    // ② 单实例 / 多会话守卫（Tauri builder 之前）
    let _singleton = match singleton::acquire(&data_dir) {
        singleton::Singleton::Acquired(token) => token,
        singleton::Singleton::AlreadyRunning { other_session } => {
            if other_session {
                singleton::show_already_running_dialog();
            } else {
                singleton::show_same_session_dialog();
            }
            std::process::exit(1);
        }
    };

    // ③ 日志初始化 + panic hook（能记日志的最早时机）
    let logging_error = logging::init(&data_dir).err();

    // ④ 崩溃自恢复：进程一开始就注册（启动阶段崩也能被 WER 接住）
    let args: Vec<String> = std::env::args().collect();
    let recovered = args.iter().any(|a| a == recovery::RECOVER_FLAG);
    let hidden = args.iter().any(|a| a == "--hidden");
    let test_notify = args.iter().any(|a| a == "--test-notify");

    let mut registration_error = None;
    if let Err(e) = recovery::install(true) {
        registration_error = Some(e);
    }
    let mut recovery_paused = false;
    if recovered {
        let st = recovery::note_recovered_start(&data_dir);
        recovery_paused = st.paused;
        if st.paused {
            let _ = recovery::install(false);
            log_warn(
                TAG_CRASH,
                &format!(
                    "10 分钟内连续崩溃 {} 次，已暂停自动恢复（注册已注销）",
                    st.restarts_in_window
                ),
            );
        }
    }
    if !recovery::crash_recovery_enabled(&data_dir) {
        let _ = recovery::install(false);
    }

    // 调试自测：--crash-test 5 秒后强退（release 构建里不存在）。
    #[cfg(debug_assertions)]
    if args.iter().any(|a| a == "--crash-test") {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(5));
            std::process::abort();
        });
    }

    log_info(TAG_START, &format!("数据目录：{}", data_dir.display()));
    // setup 闭包会拿走 data_dir，这份留着给“起不来”时的收尾用。
    let exit_data_dir = data_dir.clone();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == "--test-notify") {
                let _ = send_test_notification(app);
                return;
            }
            if args.iter().any(|a| a == "--hidden") {
                return;
            }
            mainwindow::show_or_create(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            setup(
                app,
                data_dir.clone(),
                logging_error.clone(),
                registration_error.clone(),
                recovery_paused,
                recovered,
                hidden,
                test_notify,
            )
        })
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                // 界面加载好了：把启动时没送到的一次性提示补发一次
                let message = CONFIG_ERROR.lock().unwrap().clone();
                if let Some(message) = message {
                    let _ = webview.emit("config-error", serde_json::json!({ "message": message }));
                }
                let pending = PENDING_SYS_NOTICES.lock().unwrap().clone();
                for notice in pending {
                    let _ = webview.emit("sys-notice", notice);
                }
            }
        })
        .on_window_event(|window, event| {
            // 关闭 = 真正销毁窗口（释放 WebView2）；托盘继续看护。
            if let WindowEvent::CloseRequested { .. } = event {
                mainwindow::save_state(window.app_handle());
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
            restart_service_rotate_logs,
            add_tunnel,
            update_tunnel,
            remove_tunnel,
            tunnel_start,
            tunnel_stop,
            tunnel_restart,
            forget_host_key,
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
            get_notify_quiet,
            set_notify_quiet,
            test_notification,
            get_autostart,
            set_autostart,
            get_data_dir,
            open_data_dir,
            get_data_dir_info,
            // 本轮新增
            get_stats,
            query_logs,
            run_doctor,
            build_diag,
            list_deleted,
            restore_deleted,
            purge_deleted,
            export_config,
            import_config,
            check_update,
            get_settings,
            set_settings,
            get_recovery_state,
            clear_recovery_state,
            set_crash_recovery,
            check_tunnel_cloud,
            scan_server_env,
            run_access_audit,
            get_public_ip,
            save_text_file,
            open_log_folder,
            get_log_archives,
            titlebar::set_maximize_rect
        ]);

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(e) => {
            log_error(TAG_START, &format!("界面框架起不来：{e}"));
            shangxian_core::ctl::remove_ctl_info(&exit_data_dir);
            std::process::exit(1);
        }
    };
    // 注意：`Builder::build()` 不会执行 setup（`setup` 在 `app.run()` 里才跑），
    // 而 AppState 是 setup 里 manage 的；这里提前取会 panic「state() called before manage()」。
    // 所以退出收尾在 Exit 事件里现取（那时 setup 早已完成），try_state 兜底以免极端路径再炸。
    app.run(move |handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                on_exit(&state.ctx);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn(host: &str, port: u16) -> ServerConn {
        ServerConn {
            host: host.to_string(),
            ssh_port: port,
            user: "root".to_string(),
            identity_file: None,
        }
    }

    /// 关键字表只有 core::reason 一份：这里只验证映射结果和地址/原文拼接。
    #[test]
    fn ssh_errors_use_the_single_reason_table() {
        let conn = test_conn("example.com", 2222);
        let timeout = ssh_error_human(
            "ssh: connect to host example.com port 2222: Connection timed out",
            &conn,
        );
        assert!(timeout.contains("example.com"), "{timeout}");
        assert!(timeout.contains("2222"), "{timeout}");
        assert!(timeout.contains("超时"), "{timeout}");

        let auth = ssh_error_human("Permission denied (publickey).", &conn);
        assert!(auth.contains("登录被服务器拒绝了"), "{auth}");
        assert!(auth.contains("原始报错"), "{auth}");

        // 本机没装 ssh：不提地址，直接用 reason 的文案
        let missing = ssh_error_human("无法启动 ssh：找不到文件", &conn);
        assert!(missing.contains("OpenSSH"), "{missing}");
        assert!(!missing.contains("原始报错"), "{missing}");

        let dns = ssh_error_human("ssh: Could not resolve hostname nope.invalid", &conn);
        assert!(dns.contains("解析不了"), "{dns}");
    }
}
