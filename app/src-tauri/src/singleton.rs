//! 多会话守卫：同一台电脑只允许一个「上线助手」在跑。
//!
//! 背景：`tauri-plugin-single-instance` 的互斥量是**会话级**的（名字没有 `Global\`
//! 前缀），远程桌面另开一个账号会话就能再起一个实例，两边会按各自的配置去抢云端端口。
//!
//! 三层防线（D1 §10）：
//! 1. 全局命名互斥量 `Global\Shangxian.App.v1`（主机制）；
//! 2. 数据目录文件锁 `<数据目录>/app.lock`（系统策略不让建 Global 对象时回落）；
//! 3. 云端清理的既有保护（端口被别人占时只报错、绝不动手）。
//!
//! 插件保留不动：同一个会话里再点一次图标，体验仍然是「把已有窗口叫出来」。

use std::path::Path;

/// 全局互斥量的名字（冻结，改动等于换了把锁）。
pub const GLOBAL_MUTEX: &str = r"Global\Shangxian.App.v1";

/// 启动时的占用判定结果。
pub enum Singleton {
    /// 拿到锁了；token 活着期间独占，进程退出自动释放。
    Acquired(SingletonToken),
    /// 已经有一个实例在跑。
    AlreadyRunning {
        /// true = 在另一个 Windows 会话（比如另一账号的远程桌面）里跑着。
        other_session: bool,
    },
}

/// 独占凭据。Drop 时关句柄（系统随即释放互斥量/文件锁）。
pub struct SingletonToken {
    handle: windows_sys::Win32::Foundation::HANDLE,
    /// 文件锁回落时用它把锁一直握到进程退出（Drop 即关闭）。
    #[allow(dead_code)]
    fallback_lock: Option<std::fs::File>,
}

// 句柄本身是个不透明的内核对象指针；凭据只在主线程创建、在进程生命周期内存活。
unsafe impl Send for SingletonToken {}
unsafe impl Sync for SingletonToken {}

impl SingletonToken {
    /// 两层防线都没成功时的「放行」凭据：宁可放行，也不把用户卡在启动界面。
    fn detached() -> Self {
        Self {
            handle: std::ptr::null_mut(),
            fallback_lock: None,
        }
    }
}

impl Drop for SingletonToken {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

/// 取独占权。数据目录必须已经解析好（便携模式同样适用）。
pub fn acquire(data_dir: &Path) -> Singleton {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError,
        };
        use windows_sys::Win32::System::Threading::CreateMutexW;

        let name: Vec<u16> = GLOBAL_MUTEX
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // bInitialOwner = FALSE：只持有句柄、不抢所有权，免得引入跨线程 ReleaseMutex 的坑。
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if !handle.is_null() {
            let err = unsafe { GetLastError() };
            if err == ERROR_ALREADY_EXISTS {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                return Singleton::AlreadyRunning {
                    other_session: true,
                };
            }
            return Singleton::Acquired(SingletonToken {
                handle,
                fallback_lock: None,
            });
        }
        if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
            return Singleton::AlreadyRunning {
                other_session: true,
            };
        }
    }

    // 第 2 层：数据目录文件锁。同一 Windows 账号跨会话共享数据目录，能拦住同账号的第二个会话。
    match open_exclusive(data_dir) {
        Ok(file) => Singleton::Acquired(SingletonToken {
            handle: std::ptr::null_mut(),
            fallback_lock: Some(file),
        }),
        Err(locked) => {
            if locked {
                Singleton::AlreadyRunning {
                    other_session: false,
                }
            } else {
                // 两个都拿不到：记录一笔，继续启动（不卡死用户）。
                shangxian_core::log::log_warn(
                    shangxian_core::log::TAG_SESSION,
                    "未能获取独占锁（系统策略或目录权限），本次不会阻止重复启动",
                );
                Singleton::Acquired(SingletonToken::detached())
            }
        }
    }
}

/// 独占打开 `<数据目录>/app.lock`。
/// 返回 `Ok(file)` = 拿到；`Err(true)` = 已被别的实例占着；`Err(false)` = 其它 IO 错误。
fn open_exclusive(data_dir: &Path) -> Result<std::fs::File, bool> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let path = data_dir.join("app.lock");
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .share_mode(0) // 不给别人共享读写：第二个打开者会拿到 ERROR_SHARING_VIOLATION
            .open(&path)
        {
            Ok(file) => Ok(file),
            Err(e) if e.raw_os_error() == Some(32) => Err(true), // ERROR_SHARING_VIOLATION
            Err(_) => Err(false),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = data_dir;
        Err(false)
    }
}

/// 同一个会话里已经有实例：提示去看托盘（不弹“另一个 Windows 会话”那种话）。
pub fn show_same_session_dialog() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
        };
        let text = "上线助手已在运行：请查看右下角托盘中的图标（可能已收起在箭头中）。";
        let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let caption_w: Vec<u16> = "上线助手"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text_w.as_ptr(),
                caption_w.as_ptr(),
                MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
            );
        }
    }
}

/// 检测到已有实例时弹的人话对话框（阻塞，当前会话可见）。由调用方决定何时调。
pub fn show_already_running_dialog() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
        };
        let text = "它已在另一个 Windows 会话中运行（例如另一个远程桌面）。\
                   请到该会话中操作；如需在当前会话中使用，请先在该会话的托盘中退出。";
        let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let caption_w: Vec<u16> = "上线助手"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text_w.as_ptr(),
                caption_w.as_ptr(),
                MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shangxian-singleton-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn second_acquire_is_blocked() {
        // 全局互斥量是进程级固定名字：同进程第二次 acquire 必须被拦。
        let dir = temp_dir("global");
        let first = acquire(&dir);
        assert!(matches!(first, Singleton::Acquired(_)));
        let second = acquire(&dir);
        assert!(matches!(second, Singleton::AlreadyRunning { .. }));
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_lock_blocks_second_open() {
        let dir = temp_dir("file");
        let first = open_exclusive(&dir);
        assert!(first.is_ok());
        assert_eq!(open_exclusive(&dir).err(), Some(true));
        drop(first);
        assert!(open_exclusive(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
