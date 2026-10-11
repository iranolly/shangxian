//! Windows 作业对象（Job Object）封装。
//!
//! 作用：把每个 ssh.exe 子进程放进一个作业里，并设置
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`。这样无论主程序正常退出、
//! 被任务管理器强杀、还是崩溃，作业句柄一关闭，里面的子进程全部被系统结束，
//! 不会留下占用云端端口的孤儿 ssh 进程。

use std::io;
use std::os::windows::io::RawHandle;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// 一个作业对象。持有它 = 子进程活着；释放它 = 子进程被系统结束。
pub struct Job {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// HANDLE 本质是一个内核句柄数值，跨线程传递是安全的；
// 我们保证同一句柄只被关闭一次（Drop）。
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> io::Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let err = io::Error::last_os_error();
                let _ = CloseHandle(handle);
                return Err(err);
            }

            Ok(Self { handle })
        }
    }

    /// 把已启动的子进程放进本作业。传入 `std::process::Child::as_raw_handle()`。
    pub fn assign_process(&self, raw: RawHandle) -> io::Result<()> {
        unsafe {
            let ok = AssignProcessToJobObject(
                self.handle,
                raw as windows_sys::Win32::Foundation::HANDLE,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
