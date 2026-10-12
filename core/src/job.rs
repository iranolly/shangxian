//! Windows 作业对象（Job Object）封装。
//!
//! 两种用法：
//! - [`Job::new`]：设置 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，给 ssh 隧道用。无论主程序
//!   正常退出、被任务管理器强杀、还是崩溃，作业句柄一关闭，里面的子进程全部被系统结束，
//!   不会留下占用云端端口的孤儿 ssh 进程。
//! - [`Job::new_keep_alive`]：不带 kill-on-close，给本机服务用。应用退出/重启/崩溃后
//!   服务继续跑，下次启动按端口认领；要停时用 [`Job::terminate`] 显式结束整棵树。

use std::io;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

use crate::log::{log_warn, TAG_GUARD};

/// 创建进程时不弹控制台窗口。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一个作业对象。
pub struct Job {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// HANDLE 本质是一个内核句柄数值，跨线程传递是安全的；
// 我们保证同一句柄只被关闭一次（Drop）。
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// kill-on-close 作业：释放句柄时系统结束作业内全部进程（ssh 隧道用）。
    pub fn new() -> io::Result<Self> {
        Self::create(true)
    }

    /// 保活作业：释放句柄不会结束里面的进程（本机服务用）。
    ///
    /// 应用退出/崩溃不会连带结束本机服务；要停时用 [`Job::terminate`] 显式结束整棵树。
    pub fn new_keep_alive() -> io::Result<Self> {
        Self::create(false)
    }

    fn create(kill_on_close: bool) -> io::Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }

            if kill_on_close {
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

    /// 结束作业内的整棵进程树（`TerminateJobObject`）。
    ///
    /// 显式停止本机服务时用：`cmd.exe` / `powershell.exe` 包装脚本再拉起的子孙进程
    /// 也一起结束。失败只打警告，调用方仍可退回 `Child::kill`。
    pub fn terminate(&self) {
        unsafe {
            let ok = TerminateJobObject(self.handle, 1);
            if ok == 0 {
                log_warn(
                    TAG_GUARD,
                    &format!("结束进程树失败：{}", io::Error::last_os_error()),
                );
            }
        }
    }
}

/// 启动自检：作业对象能建、子进程能放进去、能整树结束。约 20–80ms（D1 §3.6）。
///
/// 注意：绝不把当前进程放进作业（kill-on-close 会把应用自己带走）。
pub fn self_check() -> bool {
    let job = match Job::new() {
        Ok(job) => job,
        Err(e) => {
            log_warn(TAG_GUARD, &format!("启动自检没过：作业对象建不出来（{e}）"));
            return false;
        }
    };

    let mut child = match Command::new("cmd.exe")
        .args(["/C", "exit", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            log_warn(TAG_GUARD, &format!("启动自检没过：试跑的探针进程起不来（{e}）"));
            return false;
        }
    };

    if let Err(e) = job.assign_process(child.as_raw_handle()) {
        log_warn(TAG_GUARD, &format!("启动自检没过：探针进程放不进作业（{e}）"));
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }

    // 释放作业句柄 = kill-on-close 结束探针；等它退出，确认系统真的执行了。
    drop(job);
    match child.wait() {
        Ok(_) => true,
        Err(e) => {
            log_warn(TAG_GUARD, &format!("启动自检没过：探针进程收不了尾（{e}）"));
            false
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    /// 起一个活 60 秒左右的子进程（`ping` 在 Windows 上是同步子进程，到点自己退）。
    fn spawn_sleeper() -> Child {
        Command::new("cmd.exe")
            .args(["/C", "ping", "127.0.0.1", "-n", "60", ">NUL"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleeper")
    }

    fn wait_exit(child: &mut Child, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Err(_) => return true,
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn self_check_passes_on_this_machine() {
        assert!(
            self_check(),
            "本机作业对象自检应该通过（受限 CI 可改成 #[ignore]）"
        );
    }

    #[test]
    fn keep_alive_job_drop_leaves_child_running() {
        let mut child = spawn_sleeper();
        let job = Job::new_keep_alive().expect("new_keep_alive");
        job.assign_process(child.as_raw_handle())
            .expect("assign_process");
        drop(job); // 模拟应用退出：关句柄，保活作业不杀进程

        std::thread::sleep(Duration::from_millis(300));
        assert!(
            matches!(child.try_wait(), Ok(None)),
            "保活作业释放后子进程不能被杀"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn kill_on_close_job_drop_kills_child() {
        let mut child = spawn_sleeper();
        let job = Job::new().expect("new");
        job.assign_process(child.as_raw_handle())
            .expect("assign_process");
        drop(job);

        assert!(
            wait_exit(&mut child, Duration::from_secs(5)),
            "kill-on-close 作业释放后子进程应该被系统结束"
        );
    }

    #[test]
    fn terminate_kills_whole_process_tree() {
        let ps = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        if !Path::new(ps).exists() {
            println!("跳过：本机没有 PowerShell（{ps}）");
            return;
        }

        let dir = std::env::temp_dir().join(format!("sx-job-tree-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let marker = dir.join("grandchild.marker");
        let _ = std::fs::remove_file(&marker);
        let script = dir.join("marker.ps1");
        std::fs::write(
            &script,
            format!(
                "Start-Sleep -Seconds 3\r\nSet-Content -LiteralPath '{}' -Value 'alive'\r\n",
                marker.display()
            ),
        )
        .expect("write marker.ps1");

        // 直接子进程 cmd.exe → 孙进程 powershell；terminate 必须把孙进程也结束。
        let mut child = Command::new("cmd.exe")
            .args(["/C", ps, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd -> powershell");

        let job = Job::new_keep_alive().expect("new_keep_alive");
        job.assign_process(child.as_raw_handle())
            .expect("assign_process");
        std::thread::sleep(Duration::from_millis(800)); // 等孙进程起来
        job.terminate();

        assert!(
            wait_exit(&mut child, Duration::from_secs(5)),
            "terminate 后直接子进程应该立刻退出"
        );
        // 孙进程要 3 秒后才写标记；如果 terminate 只杀了直接子进程，标记就会出现
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !marker.exists(),
            "terminate 必须连孙进程一起结束（否则它会写下 {}）",
            marker.display()
        );

        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_dir(&dir);
    }
}
