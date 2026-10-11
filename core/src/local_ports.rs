//! 本机正在监听的端口（新建服务向导第一步用）。

use std::collections::BTreeMap;
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPort {
    pub port: u16,
    pub pid: Option<u32>,
    /// 进程名（例：python.exe），取不到就是 None
    pub process: Option<String>,
    /// 程序完整路径（添服务时自动填启动程序用）
    pub exe: Option<String>,
}

/// 列出本机 127.0.0.1 / 0.0.0.0 上正在监听的 TCP 端口。
pub fn list_listening() -> Vec<LocalPort> {
    let Ok(out) = Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let mut seen: BTreeMap<u16, Option<u32>> = BTreeMap::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || cols[0] != "TCP" || cols[3] != "LISTENING" {
            continue;
        }
        let Some((host, port)) = cols[1].rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port.parse::<u16>() else { continue };
        if port < 1024 {
            continue; // 系统保留端口，不是用户的服务
        }
        if !matches!(host, "127.0.0.1" | "0.0.0.0" | "[::]" | "[::1]") {
            continue;
        }
        let pid = cols[4].parse::<u32>().ok();
        seen.entry(port).or_insert(pid);
    }

    seen.into_iter()
        .map(|(port, pid)| {
            let exe = pid.and_then(process_path);
            let process = exe.as_deref().and_then(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            });
            LocalPort {
                port,
                pid,
                process,
                exe,
            }
        })
        .collect()
}

/// 重新跑一遍 netstat，收集这个端口上所有 LISTENING 的进程号（去重，保持 netstat 顺序）。
///
/// 和 `list_listening` 不同：这里不按主机地址/端口大小过滤——停止外部进程、核对端口主人时
/// 需要拿到真实全部监听者，任何一个都不能漏。
pub fn listening_pids(port: u16) -> Vec<u32> {
    let Ok(out) = Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let mut pids: Vec<u32> = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 || cols[0] != "TCP" || cols[3] != "LISTENING" {
            continue;
        }
        let Some((_, row_port)) = cols[1].rsplit_once(':') else {
            continue;
        };
        if row_port.parse::<u16>() != Ok(port) {
            continue;
        }
        let Ok(pid) = cols[4].parse::<u32>() else {
            continue;
        };
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    pids
}

/// 由进程号拿到程序完整路径。
pub(crate) fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        let _ = CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}
