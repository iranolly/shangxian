//! 开发用截图工具（不随应用发布）。
//! 找到标题匹配的窗口，截图为 BMP（read 工具可直接查看）。
//! 用法：capture [窗口标题] [输出路径]

use std::env;
use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::mem::zeroed;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};

/// PrintWindow 的 PW_RENDERFULLCONTENT：让 WebView2 这类合成内容也能截到。
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let title = args.get(1).map(String::as_str).unwrap_or("上线助手");
    let out = args.get(2).map(String::as_str).unwrap_or(".setup/ui-shot.bmp");

    unsafe {
        // 声明 DPI 感知：否则在 125%/150% 缩放下只能截到窗口的一部分
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let hwnd: HWND = FindWindowW(std::ptr::null(), wide(title).as_ptr());
        if hwnd.is_null() {
            eprintln!("找不到窗口：{title}（窗口可能还没打开，或标题不一致）");
            std::process::exit(1);
        }

        let mut rect: RECT = zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            eprintln!("GetWindowRect 失败");
            std::process::exit(1);
        }
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 {
            eprintln!("窗口尺寸异常：{w}x{h}");
            std::process::exit(1);
        }

        let hdc = GetDC(hwnd);
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);

        let printed = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT);
        if printed == 0 {
            // 回退：直接从屏幕对应区域拷贝（窗口需在前台）
            let screen = GetDC(std::ptr::null_mut());
            let _ = windows_sys::Win32::Graphics::Gdi::BitBlt(
                mem,
                0,
                0,
                w,
                h,
                screen,
                rect.left,
                rect.top,
                windows_sys::Win32::Graphics::Gdi::SRCCOPY,
            );
            ReleaseDC(std::ptr::null_mut(), screen);
        }

        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; // 负高度 = 自顶向下
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        let mut buf = vec![0u8; (w * 4) as usize * h as usize];
        let got = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut info,
            DIB_RGB_COLORS,
        );
        if got == 0 {
            eprintln!("GetDIBits 失败");
            std::process::exit(1);
        }

        if let Err(e) = write_bmp(out, &buf, w as usize, h as usize) {
            eprintln!("写文件失败：{e}");
            std::process::exit(1);
        }

        SelectObject(mem, old);
        DeleteObject(bmp as HGDIOBJ);
        DeleteDC(mem);
        ReleaseDC(hwnd, hdc);
    }

    println!("saved {out}");
}

/// 写 24bpp BMP（自下而上、BGR）。
fn write_bmp(path: &str, bgra: &[u8], w: usize, h: usize) -> std::io::Result<()> {
    let row_pad = (4 - (w * 3) % 4) % 4;
    let data_size = (w * 3 + row_pad) * h;
    let file_size = 14 + 40 + data_size;
    let mut f = File::create(path)?;

    // BITMAPFILEHEADER
    f.write_all(b"BM")?;
    f.write_all(&(file_size as u32).to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?;
    // BITMAPINFOHEADER
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&(w as i32).to_le_bytes())?;
    f.write_all(&(h as i32).to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&(data_size as u32).to_le_bytes())?;
    f.write_all(&2835u32.to_le_bytes())?;
    f.write_all(&2835u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;

    for y in (0..h).rev() {
        let row = &bgra[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let p = &row[x * 4..x * 4 + 4];
            f.write_all(&[p[0], p[1], p[2]])?;
        }
        for _ in 0..row_pad {
            f.write_all(&[0u8])?;
        }
    }
    Ok(())
}
