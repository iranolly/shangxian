//! 主窗口的延迟创建与销毁、窗口记忆。
//!
//! `tauri.conf.json` 里不再自动建窗（`"windows": []`），窗口按需创建：
//! - 正常启动：建窗显示；
//! - `--hidden` / `--test-notify` / `--recovered`：只进托盘，不建窗；
//! - 托盘左键 / 「打开主界面」/ 第二实例 / 通知点击：`show_or_create`；
//! - 点关闭 = 真正销毁窗口，WebView2 进程随之释放（托盘态不再常驻内存）。
//!
//! 窗口位置/大小/最大化状态存 `<数据目录>/window-state.json`（不用插件）。
//! 重开时先校验位置还在不在任何显示器上，不在就居中。

use std::path::Path;

use shangxian_core::config::default_data_dir;
use shangxian_core::log::{TAG_WINDOW, log_error, log_warn};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 主窗口标签（能力文件 capabilities/default.json 里的 `"windows": ["main"]` 认它）。
pub const MAIN_LABEL: &str = "main";
pub const DEFAULT_WIDTH: f64 = 1040.0;
pub const DEFAULT_HEIGHT: f64 = 700.0;
pub const MIN_WIDTH: f64 = 960.0;
pub const MIN_HEIGHT: f64 = 640.0;
pub const WINDOW_STATE_FILE: &str = "window-state.json";

/// 窗口记忆（坏文件按默认值处理，不阻塞启动）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub maximized: bool,
}

/// 有窗口就 show + unminimize + set_focus；没有就创建。任何线程可调。
pub fn show_or_create(app: &AppHandle) {
    if focus_existing(app) {
        return;
    }
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        // 排队期间可能已经有别的入口把窗口建出来了，这里再确认一次。
        if focus_existing(&handle) {
            return;
        }
        if let Err(e) = create(&handle) {
            log_error(TAG_WINDOW, &format!("主窗口打开失败：{e}"));
        }
    });
    if let Err(e) = queued {
        log_error(TAG_WINDOW, &format!("主窗口创建请求排队失败：{e}"));
    }
}

/// 已有窗口就显示并聚焦；返回是否处理了。
fn focus_existing(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window(MAIN_LABEL) else {
        return false;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    true
}

/// 只在主线程调：建窗 → 挂自绘标题栏 → 恢复记忆 → show/focus。
pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let state = load_state(&default_data_dir());
    let mut builder =
        WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
            .title("上线助手")
            .inner_size(
                state.width.unwrap_or(DEFAULT_WIDTH as u32) as f64,
                state.height.unwrap_or(DEFAULT_HEIGHT as u32) as f64,
            )
            .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
            .decorations(false)
            .shadow(true)
            .visible(false);

    // 保存的位置还在任何显示器上就直接用；否则居中，免得窗口跑到不存在的屏幕外面。
    if let (Some(x), Some(y)) = (state.x, state.y) {
        if position_is_visible(app, &state) {
            builder = builder.position(x as f64, y as f64);
        } else {
            builder = builder.center();
        }
    } else {
        builder = builder.center();
    }

    let window = builder.build()?;
    // 自绘标题栏（圆角 + 最大化按钮命中测试）跟窗口绑定，每次建窗都要挂一次。
    crate::titlebar::attach(&window, app);
    if state.maximized {
        let _ = window.maximize();
    }
    let _ = window.show();
    let _ = window.set_focus();
    Ok(window)
}

/// `CloseRequested` 时调：把当前窗口位置/大小/最大化写进 `<数据目录>/window-state.json`。
pub fn save_state(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_LABEL) else {
        return;
    };
    let position = window.outer_position().ok();
    let size = window.inner_size().ok();
    let state = WindowState {
        x: position.map(|p| p.x),
        y: position.map(|p| p.y),
        width: size.map(|s| s.width),
        height: size.map(|s| s.height),
        maximized: window.is_maximized().unwrap_or(false),
    };
    let path = default_data_dir().join(WINDOW_STATE_FILE);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(&state) {
        Ok(text) => {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text.as_bytes()).is_ok()
                && std::fs::rename(&tmp, &path).is_err()
            {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        Err(e) => log_warn(TAG_WINDOW, &format!("窗口位置记录失败：{e}")),
    }
}

/// 读状态（文件不在/读坏 → 默认值）。
pub fn load_state(data_dir: &Path) -> WindowState {
    let Ok(text) = std::fs::read_to_string(data_dir.join(WINDOW_STATE_FILE)) else {
        return WindowState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 坐标校验：保存的窗口矩形还和某个显示器有交集吗（第二显示器被拔掉就返回 false）。
pub fn position_is_visible(app: &AppHandle, state: &WindowState) -> bool {
    let (Some(x), Some(y)) = (state.x, state.y) else {
        return false;
    };
    let w = state.width.unwrap_or(DEFAULT_WIDTH as u32) as i32;
    let h = state.height.unwrap_or(DEFAULT_HEIGHT as u32) as i32;
    let monitor_list = match app.available_monitors() {
        Ok(list) => list,
        Err(_) => return false,
    };
    monitor_list.iter().any(|m| {
        let mp = m.position();
        let ms = m.size();
        let mx2 = mp.x.saturating_add(ms.width as i32);
        let my2 = mp.y.saturating_add(ms.height as i32);
        // 窗口至少要有 80px × 40px 落在显示器里，才算“看得见”。
        let overlap_x = (x + w).min(mx2) - x.max(mp.x);
        let overlap_y = (y + h).min(my2) - y.max(mp.y);
        overlap_x >= 80 && overlap_y >= 40
    })
}
