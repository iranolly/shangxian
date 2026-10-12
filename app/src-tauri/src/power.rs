//! 电源 / 网络系统回调：电脑睡眠唤醒、换网断网时，第一时间给 core 发信号。
//!
//! 两个回调都**只置一个原子位**（core::wake::signal_*），绝不在系统回调线程里抢
//! Guardian 锁、更不做任何耗时动作。看护线程每 150ms 取一次位，按 §1 的分档处理。
//!
//! 特意不用「message-only 窗口收 WM_POWERBROADCAST」：延迟建窗之后应用经常根本
//! 没有顶层窗口，回调 API 更省事也更可靠。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

#[cfg(windows)]
use windows_sys::Win32::System::Power::DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS;

/// 已经注册过了吗（重复调用按 D1 语义报错，不重复注册）。
static INSTALLED: OnceLock<()> = OnceLock::new();
/// 电源回调登记句柄（进程生命周期内一直有效；保留是为了能注销/自测）。
static POWER_HANDLE: AtomicIsize = AtomicIsize::new(0);
/// 网卡变化通知句柄。
static NET_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// `DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS` 里带裸指针，不能自动 Sync；
/// 它是进程级只读常量，跨线程访问安全。
#[cfg(windows)]
struct PowerParams(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS);
#[cfg(windows)]
unsafe impl Sync for PowerParams {}

#[cfg(windows)]
static POWER_PARAMS: PowerParams = PowerParams(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
    Callback: Some(power_callback),
    Context: std::ptr::null_mut(),
});

/// 注册电源 + 网络回调；两个回调都只置位。
///
/// - 电源回调失败算致命（返回 Err，调用方写日志、给顶栏警告）；
/// - 网络回调失败只记一条 WARN：唤醒兜底仍然在，网络变化最坏退回 45 秒保活感知。
/// - 重复调用返回 `Err("已经注册过了")`。
pub fn install() -> Result<(), String> {
    if INSTALLED.get().is_some() {
        return Err("已经注册过了".to_string());
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
        use windows_sys::Win32::NetworkManagement::IpHelper::NotifyIpInterfaceChange;
        use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;
        use windows_sys::Win32::System::Power::RegisterSuspendResumeNotification;
        use windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

        let h = unsafe {
            RegisterSuspendResumeNotification(
                &POWER_PARAMS.0 as *const _ as HANDLE,
                DEVICE_NOTIFY_CALLBACK,
            )
        };
        if h == 0 {
            let err = unsafe { GetLastError() };
            let msg = format!(
                "电源回调注册失败（错误码 {err}）：睡眠唤醒后的自动重连会慢一些，其它功能不受影响"
            );
            shangxian_core::log::log_warn(shangxian_core::log::TAG_POWER, &msg);
            return Err(msg);
        }
        POWER_HANDLE.store(h, Ordering::SeqCst);

        let mut net_handle: HANDLE = std::ptr::null_mut();
        let code = unsafe {
            NotifyIpInterfaceChange(
                AF_UNSPEC,
                Some(ip_change_callback),
                std::ptr::null(),
                false, // 不要初始通知：刚启动不需要立刻“重连”
                &mut net_handle,
            )
        };
        if code != 0 {
            shangxian_core::log::log_warn(
                shangxian_core::log::TAG_POWER,
                &format!("网卡变化通知注册失败（错误码 {code}）：换网后重连会慢一些"),
            );
        } else {
            NET_HANDLE.store(net_handle as isize, Ordering::SeqCst);
        }
    }

    let _ = INSTALLED.set(());
    shangxian_core::log::log_info(shangxian_core::log::TAG_POWER, "电源 / 网络回调已注册");
    Ok(())
}

/// 注销回调（正常退出/自测用；不调也不影响进程退出）。
pub fn uninstall() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::NetworkManagement::IpHelper::CancelMibChangeNotify2;
        use windows_sys::Win32::System::Power::UnregisterSuspendResumeNotification;
        let h = POWER_HANDLE.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe {
                UnregisterSuspendResumeNotification(h);
            }
        }
        let n = NET_HANDLE.swap(0, Ordering::SeqCst);
        if n != 0 {
            unsafe {
                CancelMibChangeNotify2(n as *mut core::ffi::c_void);
            }
        }
    }
}

/// 电源回调：只回一个 u32（0 = ERROR_SUCCESS），不阻塞、不加锁。
#[cfg(windows)]
unsafe extern "system" fn power_callback(
    _context: *const core::ffi::c_void,
    event_type: u32,
    _setting: *const core::ffi::c_void,
) -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
    };
    if event_type == PBT_APMRESUMEAUTOMATIC || event_type == PBT_APMRESUMESUSPEND {
        shangxian_core::wake::signal_resume();
    }
    0
}

/// 网卡/地址变化回调：只置位，让看护线程把正在退避等待的隧道提前叫醒。
#[cfg(windows)]
unsafe extern "system" fn ip_change_callback(
    _caller_context: *const core::ffi::c_void,
    _row: *const windows_sys::Win32::NetworkManagement::IpHelper::MIB_IPINTERFACE_ROW,
    _notification_type: windows_sys::Win32::NetworkManagement::IpHelper::MIB_NOTIFICATION_TYPE,
) {
    shangxian_core::wake::signal_network_change();
}
