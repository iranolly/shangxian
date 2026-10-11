//! 只读真机探针（默认 `#[ignore]`）：核实「读取正在运行进程的命令行/工作目录」的 Windows 细节。
//!
//! 验证目标（对应新功能 U1–U6）：
//! - U1 `NtQueryInformationProcess(ProcessCommandLineInformation=60)` 所需 access 与 NTSTATUS
//! - U2 类 60 的输出缓冲布局 / ReturnLength 语义（内联 vs 指针、缓冲怎么开）
//! - U3 x64 PEB 偏移：PEB+0x20、params+0x38(CurrentDirectory.DosPath)、params+0x70(CommandLine)
//! - U4 WOW64：32 位 PEB(+0x10)+params(+0x24 CWD / +0x40 命令行) 能否读到、读到的是启动目录还是当前目录
//! - U5 `ProcessWow64Information`(26) 对纯 64 位进程返回 0
//! - U6 工作目录权限表现：自己 / 同用户普通进程 / SYSTEM 进程，失败时记录 GetLastError/NTSTATUS
//!
//! 运行（仓库根目录）：
//! ```text
//! cargo test -p shangxian-core --test proc_probe -- --ignored --nocapture
//! ```
//!
//! 本探针只读；只 spawn 并清理自己启动的子进程（Job Object `KILL_ON_JOB_CLOSE` 兜底整棵进程树）；
//! 不需要管理员权限。探针自己 spawn `ping.exe -n 120 127.0.0.1` 作为对拍目标，工作目录设为仓库根。

#![cfg(windows)]

use std::mem::size_of;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::thread::sleep;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS,
};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, IsWow64Process2, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_BASIC_INFORMATION, PROCESS_NAME_NATIVE, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
    ProcessWow64Information,
};

/// std 没 re-export，沿用 local_ports.rs 的写法。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------------------------------------------------------------- 基础工具

fn nt_name(status: NTSTATUS) -> String {
    match status as u32 {
        0x0000_0000 => "STATUS_SUCCESS".to_string(),
        0x8000_0005 => "STATUS_BUFFER_OVERFLOW".to_string(),
        0x8000_000D => "STATUS_PARTIAL_COPY".to_string(),
        0xC000_0003 => "STATUS_INVALID_INFO_CLASS".to_string(),
        0xC000_0004 => "STATUS_INFO_LENGTH_MISMATCH".to_string(),
        0xC000_0005 => "STATUS_ACCESS_VIOLATION".to_string(),
        0xC000_000D => "STATUS_INVALID_PARAMETER".to_string(),
        0xC000_0022 => "STATUS_ACCESS_DENIED".to_string(),
        0xC000_0023 => "STATUS_BUFFER_TOO_SMALL".to_string(),
        0xC000_00BB => "STATUS_NOT_SUPPORTED".to_string(),
        other => format!("NTSTATUS(0x{other:08X})"),
    }
}

fn err_text(code: u32) -> String {
    format!(
        "GetLastError={code} ({})",
        std::io::Error::from_raw_os_error(code as i32)
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn u16s(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn path_eq(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\').eq_ignore_ascii_case(b.trim_end_matches('\\'))
}

/// 按 Windows 规则从整条命令行剥掉 argv0（argv0 只在开头）。
fn split_argv0(cmdline: &str) -> (String, String) {
    let s = cmdline.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim_start().to_string());
        }
        return (rest.to_string(), String::new());
    }
    match s.find(|c: char| c == ' ' || c == '\t') {
        Some(i) => (s[..i].to_string(), s[i..].trim_start().to_string()),
        None => (s.to_string(), String::new()),
    }
}

fn open_process(pid: u32, access: u32) -> Result<HANDLE, String> {
    unsafe {
        let h = OpenProcess(access, 0, pid);
        if h.is_null() {
            Err(err_text(GetLastError()))
        } else {
            Ok(h)
        }
    }
}

fn read_mem(h: HANDLE, addr: usize, out: &mut [u8]) -> Result<(), String> {
    unsafe {
        let mut got: usize = 0;
        let ok = ReadProcessMemory(
            h,
            addr as *const core::ffi::c_void,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut got,
        );
        if ok == 0 {
            Err(format!(
                "ReadProcessMemory(0x{addr:X}, {}B) -> {}",
                out.len(),
                err_text(GetLastError())
            ))
        } else if got != out.len() {
            Err(format!(
                "ReadProcessMemory(0x{addr:X}) 只读到 {}/{}B",
                got,
                out.len()
            ))
        } else {
            Ok(())
        }
    }
}

// ------------------------------------------------------- UNICODE_STRING 读取

/// 64 位 UNICODE_STRING：Length u16 / MaximumLength u16 / pad u32 / Buffer *mut u16
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UStr64 {
    length: u16,
    maximum_length: u16,
    _pad: u32,
    buffer: u64,
}

/// 32 位（WOW64）UNICODE_STRING：Length u16 / MaximumLength u16 / Buffer u32
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UStr32 {
    length: u16,
    maximum_length: u16,
    buffer: u32,
}

fn read_ustr64(h: HANDLE, base: usize, off: usize) -> Result<(UStr64, Option<String>), String> {
    let mut raw = [0u8; 16];
    read_mem(h, base + off, &mut raw)?;
    let us: UStr64 = unsafe { ptr::read_unaligned(raw.as_ptr() as *const UStr64) };
    let text = if us.length == 0 || us.buffer == 0 {
        None
    } else {
        let mut sb = vec![0u8; us.length as usize];
        read_mem(h, us.buffer as usize, &mut sb)?;
        Some(String::from_utf16_lossy(&u16s(&sb)))
    };
    Ok((us, text))
}

fn read_ustr32(h: HANDLE, base: usize, off: usize) -> Result<(UStr32, Option<String>), String> {
    let mut raw = [0u8; 8];
    read_mem(h, base + off, &mut raw)?;
    let us: UStr32 = unsafe { ptr::read_unaligned(raw.as_ptr() as *const UStr32) };
    let text = if us.length == 0 || us.buffer == 0 {
        None
    } else {
        let mut sb = vec![0u8; us.length as usize];
        read_mem(h, us.buffer as usize, &mut sb)?;
        Some(String::from_utf16_lossy(&u16s(&sb)))
    };
    Ok((us, text))
}

// ------------------------------------------------------- 类 60（命令行）调用

struct Cmd60 {
    status: NTSTATUS,
    ret_len: u32,
    length: u16,
    max: u16,
    bufptr: u64,
    /// 按“字符串内联在头部之后”的假设解出来的文本
    inline_text: Option<String>,
}

fn set_uni_header(buf: &mut [u8], max_len: u16, buffer_ptr: u64) {
    assert!(buf.len() >= 16);
    buf[0..2].copy_from_slice(&0u16.to_le_bytes()); // Length = 0（入参）
    buf[2..4].copy_from_slice(&max_len.to_le_bytes()); // MaximumLength = 可用容量
    buf[4..8].fill(0); // padding（x64）
    buf[8..16].copy_from_slice(&buffer_ptr.to_le_bytes()); // Buffer = 落点
}

fn class60_query(h: HANDLE, buf: &mut [u8], param_len: u32) -> Cmd60 {
    let mut ret_len: u32 = 0xDEAD_BEEF;
    let status = unsafe {
        NtQueryInformationProcess(
            h,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            param_len,
            &mut ret_len,
        )
    };
    let length = u16::from_le_bytes([buf[0], buf[1]]);
    let max = u16::from_le_bytes([buf[2], buf[3]]);
    let bufptr = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    let inline_text = if length > 0 && buf.len() >= 16 + length as usize {
        Some(String::from_utf16_lossy(&u16s(&buf[16..16 + length as usize])))
    } else {
        None
    };
    Cmd60 {
        status,
        ret_len,
        length,
        max,
        bufptr,
        inline_text,
    }
}

fn class60_case(h: HANDLE, cap: usize, max_len: u16, param_len: u32) -> (Cmd60, Vec<u8>) {
    let mut buf = vec![0u8; cap];
    let p = buf.as_ptr() as u64 + 16;
    set_uni_header(&mut buf, max_len, p);
    let r = class60_query(h, &mut buf, param_len);
    (r, buf)
}

/// 按真机实测约定读类 60（两阶段）：
/// 1) 先拿一个小缓冲探测；STATUS_INFO_LENGTH_MISMATCH 时 ReturnLength = 头部+字符串+NUL 的总字节数；
/// 2) 按需分配后重试；成功时缓冲头部是 UNICODE_STRING，字符串内联在 +16 处。
fn cmdline60(h: HANDLE) -> (NTSTATUS, u32, Option<String>) {
    let mut probe = vec![0u8; 64];
    let probe_len = probe.len() as u32;
    let r1 = class60_query(h, &mut probe, probe_len);
    if r1.status == 0 {
        return (r1.status, r1.ret_len, r1.inline_text);
    }
    if r1.status as u32 != 0xC000_0004 || r1.ret_len == 0 || r1.ret_len > (1 << 20) {
        return (r1.status, r1.ret_len, None);
    }
    let mut buf = vec![0u8; r1.ret_len as usize];
    let buf_len = buf.len() as u32;
    let r2 = class60_query(h, &mut buf, buf_len);
    (r2.status, r2.ret_len, r2.inline_text)
}

fn print_cmd60(label: &str, r: &Cmd60, buf: &[u8]) {
    println!(
        "[{label}] status=0x{:08X} ({}) returnLength={} out.header=(Length={}, MaximumLength={}, Buffer=0x{:X})",
        r.status as u32,
        nt_name(r.status),
        if r.ret_len == 0xDEAD_BEEF {
            "未改动".to_string()
        } else {
            r.ret_len.to_string()
        },
        r.length,
        r.max,
        r.bufptr
    );
    println!("[{label}] out[0..32]={}", hex(&buf[..buf.len().min(32)]));
    println!("[{label}] 内联解码（假设字符串在头部+16 处）={:?}", r.inline_text);
}

// ------------------------------------------------------- PEB / 权限 / 枚举

fn class26(h: HANDLE) -> (NTSTATUS, usize) {
    let mut value: usize = 0;
    let mut ret: u32 = 0;
    let st = unsafe {
        NtQueryInformationProcess(
            h,
            ProcessWow64Information,
            (&mut value as *mut usize).cast(),
            size_of::<usize>() as u32,
            &mut ret,
        )
    };
    (st, value)
}

fn is_wow64_2(h: HANDLE) -> Option<(u16, u16)> {
    let mut pm: IMAGE_FILE_MACHINE = 0xFFFF;
    let mut nm: IMAGE_FILE_MACHINE = 0xFFFF;
    let ok = unsafe { IsWow64Process2(h, &mut pm, &mut nm) };
    if ok == 0 { None } else { Some((pm, nm)) }
}

/// 走 x64 PEB：class0 → PEB+0x20 → params+0x38(CWD)/+0x60(ImagePathName)/+0x70(CommandLine)。
/// 返回 (命令行, 工作目录)。
fn peb64_dump(h: HANDLE, tag: &str) -> (Option<String>, Option<String>) {
    let mut pbi: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut ret: u32 = 0;
    let st = unsafe {
        NtQueryInformationProcess(
            h,
            ProcessBasicInformation,
            (&mut pbi as *mut PROCESS_BASIC_INFORMATION).cast(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut ret,
        )
    };
    if st != 0 {
        println!(
            "{tag} class0(ProcessBasicInformation) 失败 status=0x{:08X} ({}) ret={ret}",
            st as u32,
            nt_name(st)
        );
        return (None, None);
    }
    let peb = pbi.PebBaseAddress as usize;
    println!("{tag} class0=ok peb=0x{peb:X}");
    let mut b8 = [0u8; 8];
    if let Err(e) = read_mem(h, peb + 0x20, &mut b8) {
        println!("{tag} PEB+0x20(ProcessParameters) 读失败: {e}");
        return (None, None);
    }
    let params = u64::from_le_bytes(b8) as usize;
    println!("{tag} peb+0x20 -> ProcessParameters=0x{params:X}");

    let cmd = match read_ustr64(h, params, 0x70) {
        Ok((us, text)) => {
            println!(
                "{tag} params+0x70(CommandLine): Length={} MaximumLength={} Buffer=0x{:X} text={text:?}",
                us.length, us.maximum_length, us.buffer
            );
            text
        }
        Err(e) => {
            println!("{tag} params+0x70(CommandLine) 读失败: {e}");
            None
        }
    };
    let cwd = match read_ustr64(h, params, 0x38) {
        Ok((us, text)) => {
            println!(
                "{tag} params+0x38(CurrentDirectory.DosPath): Length={} Buffer=0x{:X} text={text:?}",
                us.length, us.buffer
            );
            text
        }
        Err(e) => {
            println!("{tag} params+0x38(CurrentDirectory.DosPath) 读失败: {e}");
            None
        }
    };
    match read_ustr64(h, params, 0x60) {
        Ok((us, text)) => println!(
            "{tag} params+0x60(ImagePathName) 交叉验证: Length={} text={text:?}",
            us.length
        ),
        Err(e) => println!("{tag} params+0x60(ImagePathName) 读失败: {e}"),
    }
    (cmd, cwd)
}

/// 走 WOW64 的 32 位 PEB：class26 → PEB32+0x10 → params32+0x24(CWD)/+0x40(CommandLine)。
/// 返回 (命令行, 工作目录)。
fn peb32_dump(h: HANDLE, tag: &str) -> (Option<String>, Option<String>) {
    let (st, peb32) = class26(h);
    println!(
        "{tag} class26(ProcessWow64Information) status=0x{:08X} ({}) peb32=0x{:X}",
        st as u32,
        nt_name(st),
        peb32
    );
    if st != 0 || peb32 == 0 {
        return (None, None);
    }
    let mut b4 = [0u8; 4];
    if let Err(e) = read_mem(h, peb32 + 0x10, &mut b4) {
        println!("{tag} PEB32+0x10(ProcessParameters) 读失败: {e}");
        return (None, None);
    }
    let params = u32::from_le_bytes(b4) as usize;
    println!("{tag} peb32+0x10 -> params32=0x{params:X}");

    let cmd = match read_ustr32(h, params, 0x40) {
        Ok((us, text)) => {
            println!(
                "{tag} params32+0x40(CommandLine): Length={} MaximumLength={} Buffer=0x{:X} text={text:?}",
                us.length, us.maximum_length, us.buffer
            );
            text
        }
        Err(e) => {
            println!("{tag} params32+0x40(CommandLine) 读失败: {e}");
            None
        }
    };
    let cwd = match read_ustr32(h, params, 0x24) {
        Ok((us, text)) => {
            println!(
                "{tag} params32+0x24(CurrentDirectory.DosPath): Length={} Buffer=0x{:X} text={text:?}",
                us.length, us.buffer
            );
            text
        }
        Err(e) => {
            println!("{tag} params32+0x24(CurrentDirectory.DosPath) 读失败: {e}");
            None
        }
    };
    (cmd, cwd)
}

fn qfp_name(h: HANDLE, flags: u32, cap: usize) -> Result<String, String> {
    let mut buf = vec![0u16; cap];
    let mut len = cap as u32;
    unsafe {
        if QueryFullProcessImageNameW(h, flags, buf.as_mut_ptr(), &mut len) == 0 {
            return Err(format!("失败 {}；返回 len={len}", err_text(GetLastError())));
        }
    }
    Ok(String::from_utf16_lossy(&buf[..len as usize]))
}

fn find_pids(exe_name: &str) -> Vec<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut pids = Vec::new();
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let n = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..n]);
                if name.eq_ignore_ascii_case(exe_name) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        pids
    }
}

// ------------------------------------------------------- Job Object + 子进程兜底

struct JobGuard(HANDLE);
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) }; // KILL_ON_JOB_CLOSE：兜底杀掉整棵进程树
    }
}

fn make_job() -> Option<JobGuard> {
    unsafe {
        let h = CreateJobObjectW(ptr::null(), ptr::null());
        if h.is_null() {
            eprintln!("[job] CreateJobObjectW 失败: {}", err_text(GetLastError()));
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            h,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            eprintln!("[job] SetInformationJobObject 失败: {}", err_text(GetLastError()));
            CloseHandle(h);
            return None;
        }
        println!("[job] KILL_ON_JOB_CLOSE 已启用");
        Some(JobGuard(h))
    }
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn spawn(mut cmd: Command, job: Option<HANDLE>) -> std::io::Result<Self> {
        let child = cmd.spawn()?;
        let assigned = match job {
            Some(j) => unsafe { AssignProcessToJobObject(j, child.as_raw_handle() as HANDLE) != 0 },
            None => false,
        };
        println!("[spawn] pid={} 已加入Job={assigned}", child.id());
        Ok(Self { child })
    }
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        println!("[cleanup] TerminateProcess pid={}", self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ------------------------------------------------------- U6 单进程权限探针

fn probe_target(name: &str, pid: u32) {
    println!("---- [U6] target={name} pid={pid} ----");

    // 1) 最小权限：PROCESS_QUERY_LIMITED_INFORMATION，试类 60 / 类 26 / 路径
    match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(h) => {
            let (st, ret, text) = cmdline60(h);
            println!(
                "[U6 {name}] limited+class60: status=0x{:08X} ({}) returnLength={ret} text={text:?}",
                st as u32,
                nt_name(st)
            );
            let (st26, v26) = class26(h);
            println!(
                "[U6 {name}] limited+class26: status=0x{:08X} ({}) wow64_peb=0x{:X}",
                st26 as u32,
                nt_name(st26),
                v26
            );
            match qfp_name(h, 0, 1024) {
                Ok(s) => println!("[U6 {name}] QueryFullProcessImageNameW(flags=0) -> {s:?}"),
                Err(e) => println!("[U6 {name}] QueryFullProcessImageNameW(flags=0) -> {e}"),
            }
            unsafe { CloseHandle(h) };
        }
        Err(e) => println!("[U6 {name}] OpenProcess(QUERY_LIMITED) 失败: {e}"),
    }

    // 2) 完整权限：PROCESS_QUERY_INFORMATION|PROCESS_VM_READ，试 PEB
    match open_process(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) {
        Ok(h) => {
            println!("[U6 {name}] OpenProcess(QUERY_INFORMATION|VM_READ)=ok");
            let (cmd, cwd) = peb64_dump(h, &format!("U6 {name}"));
            println!("[U6 {name}] x64 PEB 结果: cmd={cmd:?} cwd={cwd:?}");
            unsafe { CloseHandle(h) };
        }
        Err(e) => {
            println!("[U6 {name}] OpenProcess(QUERY_INFORMATION|VM_READ) 失败: {e}");
            // 只拿 QUERY_INFORMATION，看 PEB 读取被拒的具体错误
            match open_process(pid, PROCESS_QUERY_INFORMATION) {
                Ok(h) => {
                    println!("[U6 {name}] OpenProcess(QUERY_INFORMATION)=ok，试 PEB 读取");
                    let (cmd, cwd) = peb64_dump(h, &format!("U6 {name} query-only"));
                    println!("[U6 {name}] query-only PEB 结果: cmd={cmd:?} cwd={cwd:?}");
                    unsafe { CloseHandle(h) };
                }
                Err(e2) => println!("[U6 {name}] OpenProcess(QUERY_INFORMATION) 也失败: {e2}"),
            }
        }
    }
}

// ---------------------------------------------------------------- 主探针

#[test]
#[ignore = "只读真机探针：枚举/读取本机进程并 spawn 自己的受控子进程，默认不跑"]
fn windows_proc_commandline_probe() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let self_pid = unsafe { GetCurrentProcessId() };
    let mut failures: Vec<String> = Vec::new();

    println!("====[ENV]==== repo={} self_pid={self_pid}", repo.display());
    {
        let h = open_process(self_pid, PROCESS_QUERY_LIMITED_INFORMATION).expect("open self");
        match is_wow64_2(h) {
            Some((pm, nm)) => println!(
                "[U4] IsWow64Process2(self) ok: processMachine=0x{pm:04X} nativeMachine=0x{nm:04X} (AMD64=0x{:04X}, I386=0x{:04X}, UNKNOWN=0)",
                IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386
            ),
            None => println!("[U4] IsWow64Process2(self) 调用失败: {}", err_text(unsafe { GetLastError() })),
        }
        unsafe { CloseHandle(h) };
    }

    // Job 先声明，后声明的子进程 guard 先 drop，最后关 job 兜底
    let job = make_job();
    let job_h = job.as_ref().map(|j| j.0);

    // ---------- 受控 64 位目标：System32\ping.exe，工作目录=仓库根 ----------
    let ping64_exe = r"C:\Windows\System32\ping.exe";
    let mut c = Command::new(ping64_exe);
    c.args(["-n", "120", "127.0.0.1"])
        .current_dir(&repo)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let ping64 = ChildGuard::spawn(c, job_h).expect("spawn ping64");
    let pid64 = ping64.pid();
    sleep(Duration::from_millis(500));
    println!(
        "====[TARGET64]==== pid={pid64} exe={ping64_exe} cwd(期望)={} args=-n 120 127.0.0.1",
        repo.display()
    );

    // ---------- U1/U2：类 60 的 access 与缓冲 ----------
    let mut class60_text: Option<String> = None;
    println!("====[U1] class60 需要的 access（两阶段推荐调用） ====");
    for (label, access) in [
        ("QUERY_LIMITED", PROCESS_QUERY_LIMITED_INFORMATION),
        ("QUERY", PROCESS_QUERY_INFORMATION),
    ] {
        match open_process(pid64, access) {
            Ok(h) => {
                let (st, ret, text) = cmdline60(h);
                println!(
                    "[U1 {label}] status=0x{:08X} ({}) returnLength={ret} text={text:?}",
                    st as u32,
                    nt_name(st)
                );
                if label == "QUERY_LIMITED" {
                    class60_text = text;
                }
                if st != 0 {
                    failures.push(format!("class60 在 {label} 下失败: {}", nt_name(st)));
                }
                unsafe { CloseHandle(h) };
            }
            Err(e) => println!("[U1] {label}: OpenProcess 失败 {e}"),
        }
    }

    println!("====[U2] class60 缓冲布局 / returnLength（原始调用探查约定） ====");
    if let Ok(h) = open_process(pid64, PROCESS_QUERY_LIMITED_INFORMATION) {
        // (a) 只传 UNICODE_STRING 头的大小：内核按“整个 ProcessInformationLength”判断 → 失败并给出所需总大小
        let (r, buf) = class60_case(h, 4096, 4080, 16);
        print_cmd60("U2a paramLen=16(只有头)", &r, &buf);
        if r.status as u32 != 0xC000_0004 {
            failures.push("U2a: paramLen=16 竟未返回 STATUS_INFO_LENGTH_MISMATCH".into());
        }
        // (b) 传整个缓冲长度：成功；观察返回头 Buffer 是否被改写成 缓冲+16
        let (r, buf) = class60_case(h, 4096, 4080, 4096);
        print_cmd60("U2b paramLen=4096(预填头)", &r, &buf);
        if r.status != 0 {
            failures.push("U2b: paramLen=4096 失败".into());
        }
        // (c) 入参头完全清零（Buffer=NULL, MaximumLength=0），paramLen=4096
        let mut zero = vec![0u8; 4096];
        let zero_len = zero.len() as u32;
        let r = class60_query(h, &mut zero, zero_len);
        print_cmd60("U2c paramLen=4096(头全零)", &r, &zero);
        // (d) 入参头 MaximumLength=4、Buffer 合法，paramLen=4096（判断容量看谁）
        let (r, buf) = class60_case(h, 4096, 4, 4096);
        print_cmd60("U2d paramLen=4096/maxLen=4", &r, &buf);
        // (e) 边界：paramLen=111（比所需 112 少 1）与 112
        let mut b = vec![0u8; 4096];
        let r = class60_query(h, &mut b, 111);
        print_cmd60("U2e paramLen=111", &r, &b);
        let mut b = vec![0u8; 112];
        let r = class60_query(h, &mut b, 112);
        print_cmd60("U2e2 paramLen=112", &r, &b);
        // (f) 两阶段推荐用法
        let (st, ret, text) = cmdline60(h);
        println!(
            "[U2f] 两阶段推荐用法: status=0x{:08X} ({}) returnLength={ret} text={text:?}",
            st as u32,
            nt_name(st)
        );
        if st != 0 {
            failures.push("U2f: 两阶段推荐用法失败".into());
        }
        unsafe { CloseHandle(h) };
    }

    // ---------- U3b：只拿了 QUERY_LIMITED（无 VM_READ）时 PEB 回退的表现 ----------
    println!("====[U3b] 只有 QUERY_LIMITED 时 PEB 读取表现 ====");
    if let Ok(h) = open_process(pid64, PROCESS_QUERY_LIMITED_INFORMATION) {
        peb64_dump(h, "U3b limited-only");
        unsafe { CloseHandle(h) };
    }

    // ---------- U3：x64 PEB 偏移 ----------
    println!("====[U3] x64 PEB 偏移对拍 ====");
    if let Ok(h) = open_process(pid64, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) {
        let (cmd, cwd) = peb64_dump(h, "U3 ping64");
        match (&cmd, &cwd) {
            (Some(c), Some(w)) => {
                let (argv0, rest) = split_argv0(c);
                println!("[U3] argv0 拆分: argv0={argv0:?} args={rest:?}");
                if !path_eq(&argv0, ping64_exe) {
                    failures.push(format!("argv0 与预期 exe 不一致: {argv0:?}"));
                }
                if rest != "-n 120 127.0.0.1" {
                    failures.push(format!("剥掉 argv0 后的 args 与预期不一致: {rest:?}"));
                }
                if !path_eq(w, &repo.to_string_lossy()) {
                    failures.push(format!("x64 PEB CWD 与预期不一致: {w:?}"));
                }
            }
            _ => failures.push("x64 PEB 读取失败".to_string()),
        }
        if let (Some(c60), Some(cpeb)) = (&class60_text, &cmd) {
            let same = c60 == cpeb;
            println!("[U3] class60 与 x64 PEB 命令行一致={same}");
            if !same {
                failures.push(format!("class60 与 PEB 命令行不一致: {c60:?} vs {cpeb:?}"));
            }
        }
        unsafe { CloseHandle(h) };
    } else {
        failures.push("OpenProcess(QUERY|VM_READ) 读 ping64 PEB 失败".to_string());
    }

    // ---------- QueryFullProcessImageNameW 格式 ----------
    println!("====[QFPI] QueryFullProcessImageNameW 格式 ====");
    if let Ok(h) = open_process(pid64, PROCESS_QUERY_LIMITED_INFORMATION) {
        let mut normal = None;
        let mut native = None;
        for (label, flags, cap) in [
            ("flags=0 cap=1024", 0u32, 1024usize),
            ("flags=1(NATIVE) cap=1024", PROCESS_NAME_NATIVE, 1024),
            ("flags=0 cap=8(故意过小)", 0, 8),
        ] {
            match qfp_name(h, flags, cap) {
                Ok(s) => {
                    println!("[QFPI] {label} -> {s:?}");
                    if label == "flags=0 cap=1024" {
                        normal = Some(s);
                    } else if label.starts_with("flags=1") {
                        native = Some(s);
                    }
                }
                Err(e) => println!("[QFPI] {label} -> {e}"),
            }
        }
        if !normal.as_deref().map(|s| path_eq(s, ping64_exe)).unwrap_or(false) {
            failures.push(format!("QFPI flags=0 不是 DOS 路径/与预期不符: {normal:?}"));
        }
        if !native.as_deref().map(|s| s.starts_with(r"\Device\")).unwrap_or(false) {
            failures.push(format!("QFPI flags=1 不是 NT 原生路径: {native:?}"));
        }
        unsafe { CloseHandle(h) };
    }

    // ---------- U4/U5：WOW64 受控 32 位目标 ----------
    println!("====[U4/U5] WOW64 受控目标 ====");
    let wow64_exe = r"C:\Windows\SysWOW64\ping.exe";
    if !Path::new(wow64_exe).exists() {
        println!("[U4] 本机没有 {wow64_exe}，跳过 WOW64 验证（保守实现建议见文末）");
    } else {
        let mut c = Command::new(wow64_exe);
        c.args(["-n", "120", "127.0.0.1"])
            .current_dir(&repo)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match ChildGuard::spawn(c, job_h) {
            Ok(ping32) => {
                let pid32 = ping32.pid();
                sleep(Duration::from_millis(500));
                println!(
                    "====[TARGET32]==== pid={pid32} exe={wow64_exe} cwd(期望)={}",
                    repo.display()
                );

                let mut class60_wow64: Option<String> = None;
                // class26/IsWow64Process2 判断位数（U5）
                if let Ok(h) = open_process(pid32, PROCESS_QUERY_LIMITED_INFORMATION) {
                    let (st26, v26) = class26(h);
                    println!(
                        "[U5] ping32 limited+class26: status=0x{:08X} ({}) wow64_peb=0x{:X} (非0=WOW64)",
                        st26 as u32,
                        nt_name(st26),
                        v26
                    );
                    match is_wow64_2(h) {
                        Some((pm, nm)) => println!(
                            "[U4] IsWow64Process2(ping32): processMachine=0x{pm:04X} nativeMachine=0x{nm:04X}"
                        ),
                        None => println!(
                            "[U4] IsWow64Process2(ping32) 失败: {}",
                            err_text(unsafe { GetLastError() })
                        ),
                    }
                    let (st, ret, text) = cmdline60(h);
                    println!(
                        "[U4 ping32] limited+class60 两阶段: status=0x{:08X} ({}) returnLength={ret} text={text:?}",
                        st as u32,
                        nt_name(st)
                    );
                    if st != 0 {
                        failures.push(format!("WOW64 class60 失败: {}", nt_name(st)));
                    }
                    class60_wow64 = text;
                    unsafe { CloseHandle(h) };
                }

                // 64 位 PEB（WOW64 进程同样有 64 位 PEB）与 32 位 PEB 对拍
                if let Ok(h) = open_process(pid32, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) {
                    let (cmd64, cwd64) = peb64_dump(h, "U4 ping32 x64PEB");
                    let (cmd32, cwd32) = peb32_dump(h, "U4 ping32 x86PEB");
                    let repo_s = repo.to_string_lossy().to_string();
                    println!("[U4] 对拍: 64位PEB cmd={cmd64:?} cwd={cwd64:?}");
                    println!("[U4] 对拍: 32位PEB cmd={cmd32:?} cwd={cwd32:?}");
                    if let (Some(a), Some(b)) = (&cmd64, &cmd32) {
                        if a != b {
                            failures.push(format!("WOW64 两种 PEB 命令行不一致: {a:?} vs {b:?}"));
                        }
                    } else {
                        failures.push("WOW64 有 PEB 命令行读不到".to_string());
                    }
                    if let Some(c60) = &class60_wow64 {
                        if Some(c60) != cmd64.as_ref() {
                            failures.push(format!(
                                "WOW64 class60 与 64位 PEB 命令行不一致: {c60:?} vs {cmd64:?}"
                            ));
                        }
                    }
                    // 实测关键点：WOW64 下 32 位 PEB 的 CWD 才是启动目录；64 位 PEB 的 CWD 是陈旧的
                    if !cwd32.as_deref().map(|w| path_eq(w, &repo_s)).unwrap_or(false) {
                        failures.push(format!("WOW64 32位 PEB CWD 与启动目录不一致: {cwd32:?}"));
                    }
                    if !cwd64.as_deref().map(|w| path_eq(w, &repo_s)).unwrap_or(false) {
                        println!(
                            "[U4] 注意：WOW64 的 64位 PEB cwd={cwd64:?} 不是启动目录（实测为陈旧值，勿用）"
                        );
                    }
                    unsafe { CloseHandle(h) };
                } else {
                    failures.push("OpenProcess(QUERY|VM_READ) 读 ping32 PEB 失败".to_string());
                }
            }
            Err(e) => println!("[U4] spawn {wow64_exe} 失败: {e}"),
        }
    }
    // U5：纯 64 位进程 class26 必须是 0
    if let Ok(h) = open_process(pid64, PROCESS_QUERY_LIMITED_INFORMATION) {
        let (st26, v26) = class26(h);
        println!(
            "[U5] ping64 limited+class26: status=0x{:08X} ({}) wow64_peb=0x{:X} (期望=0)",
            st26 as u32,
            nt_name(st26),
            v26
        );
        if st26 != 0 || v26 != 0 {
            failures.push(format!("class26 对纯 64 位进程返回了 0x{v26:X}"));
        }
        unsafe { CloseHandle(h) };
    }

    // ---------- U4b：WOW64 的 CWD 是「启动目录」还是「当前目录」 ----------
    println!("====[U4b] WOW64 cwd 语义：进程 chdir 到 C:\\Windows\\Temp 后再读 ====");
    let cmd32_exe = r"C:\Windows\SysWOW64\cmd.exe";
    let chdir_to = r"C:\Windows\Temp";
    if Path::new(cmd32_exe).exists() {
        let mut c = Command::new(cmd32_exe);
        // raw_arg：命令行原样拼进 CreateProcess，避免 std 对引号的转义干扰 cmd
        c.raw_arg(r#"/s /c "cd /d C:\Windows\Temp && ping.exe -n 120 127.0.0.1""#)
            .current_dir(&repo)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match ChildGuard::spawn(c, job_h) {
            Ok(cmd32) => {
                let pid = cmd32.pid();
                sleep(Duration::from_millis(1500));
                if let Ok(h) = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ) {
                    let (cmdline, cwd64) = peb64_dump(h, "U4b cmd32 x64PEB");
                    let (_, cwd32) = peb32_dump(h, "U4b cmd32 x86PEB");
                    let (argv0, rest) = split_argv0(cmdline.as_deref().unwrap_or(""));
                    println!("[U4b] argv0 拆分: argv0={argv0:?} args={rest:?}");
                    println!("[U4b] 创建时启动目录={:?}", repo.to_string_lossy());
                    println!("[U4b] chdir 后 64位PEB cwd={cwd64:?} / 32位PEB cwd={cwd32:?}");
                    if !cwd32.as_deref().map(|w| path_eq(w, chdir_to)).unwrap_or(false) {
                        failures.push(format!(
                            "U4b: chdir 后 32位 PEB cwd 应为 {chdir_to}，实际 {cwd32:?}"
                        ));
                    } else {
                        println!("[U4b] 结论：PEB 的 CurrentDirectory 是“当前目录”而非“启动目录”（chdir 后启动值不可从 PEB 恢复）");
                    }
                    unsafe { CloseHandle(h) };
                } else {
                    println!("[U4b] cmd32 可能已退出（命令没跑起来）: {}", err_text(unsafe { GetLastError() }));
                }
            }
            Err(e) => println!("[U4b] spawn {cmd32_exe} 失败: {e}"),
        }
    } else {
        println!("[U4b] 没有 {cmd32_exe}，跳过");
    }

    // ---------- U6：权限表现 ----------
    println!("====[U6] 权限表现 ====");
    let mut targets: Vec<(String, u32)> = vec![("self".to_string(), self_pid)];
    for name in ["explorer.exe", "winlogon.exe", "services.exe"] {
        if let Some(pid) = find_pids(name).into_iter().next() {
            targets.push((name.to_string(), pid));
        } else {
            println!("[U6] 未找到 {name}");
        }
    }
    // SYSTEM 里挑一个可能允许 QUERY_LIMITED 的 svchost，验证跨权限边界的成功路径
    for (i, pid) in find_pids("svchost.exe").into_iter().take(2).enumerate() {
        targets.push((format!("svchost.exe#{i}"), pid));
    }
    for (name, pid) in targets {
        probe_target(&name, pid);
    }

    // ---------- 汇总 ----------
    println!("====[SUMMARY]==== 真机实测结论（Win11 26100，64 位调用方）");
    println!("[SUMMARY] U1 class60 在 PROCESS_QUERY_LIMITED_INFORMATION 与 PROCESS_QUERY_INFORMATION 下均 STATUS_SUCCESS");
    println!("[SUMMARY] U2 类60：ProcessInformationLength = 整个缓冲字节数；入参 UNICODE_STRING 头被忽略（可为全零）；");
    println!("[SUMMARY] U2 返回头在 buf[0..16]，字符串内联在 buf[16..16+Length]；所需总字节数 = 16+Length+2，在 ReturnLength 里（失败/成功均给）");
    println!("[SUMMARY] U3 x64：PEB+0x20→ProcessParameters，params+0x38→CurrentDirectory.DosPath，+0x60→ImagePathName，+0x70→CommandLine");
    println!("[SUMMARY] U4 WOW64：PEB32+0x10→params32，+0x24→CWD，+0x40→CommandLine；64位PEB的CWD是陈旧值，不可用");
    println!("[SUMMARY] U4b PEB 的 CurrentDirectory 是当前目录；进程 chdir 后启动目录无法从 PEB 恢复");
    println!("[SUMMARY] U5 class26 对纯 64 位进程返回 0（STATUS_SUCCESS），对 WOW64 进程返回 32 位 PEB 地址");
    println!("[SUMMARY] U6 同用户进程 QUERY_LIMITED+class60 可读；受保护 SYSTEM 进程 OpenProcess 即 GetLastError=5");
    println!("[SUMMARY] QFPI flags=0→DOS 路径，flags=1→\\Device\\...；cap 过小返回 ERROR_INSUFFICIENT_BUFFER(122)");
    println!("====[CHECK]==== 受控目标断言汇总");
    if failures.is_empty() {
        println!("[CHECK] 全部通过");
    } else {
        for f in &failures {
            println!("[CHECK] FAIL: {f}");
        }
    }
    println!("====[PROBE DONE]==== 子进程由 guard/job 清理");
    assert!(
        failures.is_empty(),
        "受控探针有 {failures:?} 条失败（详见 --nocapture 输出）"
    );
}
