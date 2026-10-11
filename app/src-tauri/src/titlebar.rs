//! 自绘标题栏的 Windows 侧支持。
//!
//! 标题栏画在网页里，这里只做系统层面的两件事：
//! - 光标落在最大化按钮上时，命中测试返回 HTMAXBUTTON（官方文档要求这么做，
//!   Windows 11 的贴靠布局弹层需要这个答案）；
//! - 这块区域的鼠标事件进不了网页层，所以点击由这里处理、悬停状态回传给界面。
//!
//! 另外顺手把窗口四角设成圆角（Windows 11）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Emitter};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, HTCLIENT, HTMAXBUTTON, IsZoomed, SW_MAXIMIZE, SW_RESTORE, ShowWindow,
    WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
};

type SubclassProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM, usize, usize) -> LRESULT;

#[link(name = "comctl32")]
unsafe extern "system" {
    fn SetWindowSubclass(hwnd: HWND, proc: SubclassProc, id: usize, data: usize) -> i32;
    fn DefSubclassProc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
}

/// 最大化按钮的位置（网页坐标：CSS 像素，客户区原点）。由界面上报。
static MAX_BTN: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
/// 当前是否悬停在最大化按钮上（只在状态变化时发事件）。
static HOVER: AtomicBool = AtomicBool::new(false);
/// 给窗口过程回传事件用的句柄。
static APP: OnceLock<AppHandle> = OnceLock::new();

/// 界面调用：上报最大化按钮的位置（窗口尺寸/缩放变化后会重新上报）。
#[tauri::command]
pub fn set_maximize_rect(x: f64, y: f64, w: f64, h: f64) {
    *MAX_BTN.lock().unwrap() = Some((x, y, w, h));
}

/// 窗口建好后调用：圆角 + 命中测试子类化。
pub fn attach(window: &tauri::WebviewWindow, app: &AppHandle) {
    let _ = APP.set(app.clone());
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let hwnd = hwnd.0 as *mut core::ffi::c_void;
    unsafe {
        // Windows 11 圆角；老系统不支持，失败就算了
        let pref: i32 = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &pref as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        // 命中测试：光标在最大化按钮上时回 HTMAXBUTTON
        SetWindowSubclass(hwnd, window_proc, 0x5348, 0);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
            if result == HTCLIENT as LRESULT && over_max_button_screen(hwnd, lparam) {
                set_hover(true);
                return HTMAXBUTTON as LRESULT;
            }
            set_hover(false);
            result
        }
        WM_NCLBUTTONDOWN if wparam == HTMAXBUTTON as WPARAM => 0,
        WM_NCLBUTTONUP if wparam == HTMAXBUTTON as WPARAM => {
            if over_max_button_cursor(hwnd) {
                toggle_maximize(hwnd);
            }
            0
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// 命中测试给的是屏幕坐标（lparam 里）。
fn over_max_button_screen(hwnd: HWND, lparam: LPARAM) -> bool {
    let x = (lparam & 0xFFFF) as u16 as i16 as i32;
    let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
    point_in_button(hwnd, x, y)
}

/// 鼠标松开时用当前光标位置判断（按下后在别处松开就不算点击）。
fn over_max_button_cursor(hwnd: HWND) -> bool {
    let mut pt = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut pt) } == 0 {
        return false;
    }
    point_in_button(hwnd, pt.x, pt.y)
}

fn point_in_button(hwnd: HWND, screen_x: i32, screen_y: i32) -> bool {
    let Some((bx, by, bw, bh)) = *MAX_BTN.lock().unwrap() else {
        return false;
    };
    let mut pt = POINT {
        x: screen_x,
        y: screen_y,
    };
    if unsafe { ScreenToClient(hwnd, &mut pt) } == 0 {
        return false;
    }
    let scale = unsafe { GetDpiForWindow(hwnd) } as f64 / 96.0;
    if scale <= 0.0 {
        return false;
    }
    let x = pt.x as f64 / scale;
    let y = pt.y as f64 / scale;
    x >= bx && x <= bx + bw && y >= by && y <= by + bh
}

fn toggle_maximize(hwnd: HWND) {
    unsafe {
        let cmd = if IsZoomed(hwnd) != 0 {
            SW_RESTORE
        } else {
            SW_MAXIMIZE
        };
        ShowWindow(hwnd, cmd);
    }
}

/// 悬停状态变化时通知界面（这块区域的鼠标事件进不了网页层）。
fn set_hover(hover: bool) {
    if HOVER.swap(hover, Ordering::SeqCst) == hover {
        return;
    }
    if let Some(app) = APP.get() {
        let _ = app.emit("titlebar-max-hover", hover);
    }
}
