//! 读取运行中进程的启动信息（程序路径 / 命令行 / 工作目录），供「添加本机服务」自动填充，
//! 以及停止外部进程前的核对。
//!
//! 真机结论来自 `core/tests/proc_probe.rs`（Win11 26100 x64）：
//! - 命令行：`NtQueryInformationProcess(ProcessCommandLineInformation = 60)`，只需要
//!   `PROCESS_QUERY_LIMITED_INFORMATION`；两阶段调用（先小缓冲探测，遇到
//!   `STATUS_INFO_LENGTH_MISMATCH` 就按 `ReturnLength` 分配整缓冲重试）；成功时缓冲前
//!   16 字节是 `UNICODE_STRING` 头，字符串内联在 `buf[16..16+Length]`，UTF-16LE。
//! - 工作目录：需要 `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`。先取 PEB 基址，再看
//!   `ProcessWow64Information`：非 0 = WOW64，必须走 32 位 PEB（`peb32+0x10 → params32`、
//!   `params32+0x24 → CurrentDirectory.DosPath`、`params32+0x40 → CommandLine`）；纯 64 位走
//!   `peb+0x20 → params`、`params+0x38 → CWD`、`params+0x70 → CommandLine`。绝不能拿 WOW64
//!   进程的 64 位 PEB CWD（实测是陈旧值）。
//! - 语义硬约束：PEB 里的 `CurrentDirectory` 是「当前目录」而不是「启动目录」；进程自己
//!   chdir 过就无法恢复启动目录。界面文案应写「它现在的工作目录」，core 不做恢复承诺。
//!
//! 纪律：
//! - 只读，不改目标进程；任何失败都降级成 warning，绝不 panic；
//! - **绝不把命令行/参数写进任何日志**（命令行里可能有密码/密钥）；
//! - PEB 读取放在独立线程里做，500ms 拿不到就返回 `readTimeout`，不卡调用方；
//! - 所有句柄 RAII 关闭，远端数据（长度、指针）严格校验后再用。

use std::mem::size_of;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
    ProcessWow64Information,
};

/// PEB 读取线程的 join 超时：超了就只给 warning，不让界面卡住。
const PEB_JOIN_TIMEOUT: Duration = Duration::from_millis(500);
/// `raw_command_line` 给前端看的上限（字符数）。
const MAX_RAW_CHARS: usize = 32 * 1024;
/// `args` 超过这个字符数就不返回（让人确认原始命令行）。
const MAX_ARGS_CHARS: usize = 8192;
/// 单个远端 UNICODE_STRING 的上限（32K 字符 ≈ 64KB）。
const MAX_STR_BYTES: usize = 64 * 1024;
/// 类 60 重试时接受的缓冲上限。
const MAX_CMD_BUF: usize = 2 * MAX_STR_BYTES + 64;
/// `STATUS_INFO_LENGTH_MISMATCH`
const STATUS_INFO_LENGTH_MISMATCH: u32 = 0xC000_0004;
/// `ERROR_INVALID_PARAMETER`（进程已经不存在时 OpenProcess 的常见错误码）
const ERROR_INVALID_PARAMETER: u32 = 87;
/// `ERROR_ACCESS_DENIED`
const ERROR_ACCESS_DENIED: u32 = 5;

/// 一条给界面看的告警：`code` 供前端特判，`text` 是人话。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchWarning {
    pub code: String,
    pub text: String,
}

/// 某个端口上进程的启动信息（「添加本机服务」自动填充用）。
///
/// JSON 字段名（serde camelCase）：
/// `port` / `pid` / `pidMatched` / `exe` / `args` / `workdir` / `rawCommandLine` / `warnings`。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchInfo {
    pub port: u16,
    /// 现在真正在监听这个端口的进程号；读不到/没有就是 None
    pub pid: Option<u32>,
    /// 收集到的进程号是否与调用方给的 `expected_pid` 一致（没给 expected_pid 时，
    /// 唯一主人也算匹配）
    pub pid_matched: bool,
    /// 程序完整路径（QueryFullProcessImageNameW，DOS 路径）
    pub exe: Option<String>,
    /// 规范化后的启动参数（MSVCRT 规则解析 → 丢掉 argv0 → 重新序列化）
    pub args: Option<String>,
    /// 它现在的工作目录（PEB 读到的「当前目录」，不是启动目录）
    pub workdir: Option<String>,
    /// 原始命令行（截断到 32K 字符，仅供只读展示）
    pub raw_command_line: Option<String>,
    pub warnings: Vec<LaunchWarning>,
}

/// 读取 `port` 上监听进程的启动信息。
///
/// `expected_pid`：
/// - `Some(pid)`：先重新解析端口，确认现在的主人还是不是它。不是就返回
///   `portOwnerChanged` / `multipleOwners` / `processGone` 之类 warning，
///   `pid_matched = false`，由前端决定要不要覆盖用户填的内容；
/// - `None`：直接看端口现在的主人是谁。
///
/// 本函数只读，不做任何写入；失败一律走 `warnings`，不返回 `Err`。
pub fn launch_info(port: u16, expected_pid: Option<u32>) -> LaunchInfo {
    let owners = crate::local_ports::listening_pids(port);
    let (pid, pid_matched, codes) = resolve_owner(&owners, expected_pid);

    let mut info = LaunchInfo {
        port,
        pid,
        pid_matched,
        exe: None,
        args: None,
        workdir: None,
        raw_command_line: None,
        warnings: Vec::new(),
    };
    for code in &codes {
        push_warning(&mut info.warnings, code);
    }

    // 没有主人 / 分不清主人：不猜，也不去读别的进程
    let Some(target) = pid else {
        return info;
    };
    if codes.contains(&"multipleOwners") {
        return info;
    }

    let probe = match probe_with_timeout(target) {
        Ok(probe) => probe,
        Err(()) => {
            push_warning(&mut info.warnings, "readTimeout");
            return info;
        }
    };
    for code in &probe.codes {
        push_warning(&mut info.warnings, code);
    }
    // 探针发现进程已经没了（netstat 之后退出的竞态）：不返回半截信息
    if probe.codes.contains(&"processGone") {
        return info;
    }

    info.exe = probe.exe;

    // 原始命令行（交流程都用完整串解析，展示才截断）
    let argv = match probe.raw.as_deref() {
        Some(raw) => {
            let argv = crate::localservice::split_args(raw);
            info.raw_command_line = Some(truncate_chars(raw, MAX_RAW_CHARS));
            argv
        }
        None => Vec::new(),
    };

    // 工作目录：剥 NT 前缀、去尾斜杠（保留根）、目录不在就不填
    if let Some(raw_workdir) = probe.workdir.as_deref() {
        let workdir = clean_workdir(raw_workdir);
        if !workdir.is_empty() {
            if Path::new(&workdir).is_dir() {
                info.workdir = Some(workdir);
            } else {
                push_warning(&mut info.warnings, "workdirNotExist");
            }
        }
    }

    // 参数：丢掉 argv0，按 MSVCRT 序列化规则拼回一行
    if !argv.is_empty() {
        let args_text = crate::localservice::quote_args(&argv[1..]);
        if args_text.chars().count() > MAX_ARGS_CHARS {
            push_warning(&mut info.warnings, "tooLong");
        } else if args_text.contains(['\r', '\n', '\t']) {
            push_warning(&mut info.warnings, "controlChars");
        } else {
            if !args_text.is_empty() && contains_sensitive(&args_text) {
                push_warning(&mut info.warnings, "sensitiveArgs");
            }
            info.args = Some(args_text);
        }
    }

    if let Some(exe) = info.exe.as_deref() {
        if !Path::new(exe).is_file() {
            push_warning(&mut info.warnings, "exeNotFound");
        }
        if is_wrapper_exe(exe) {
            // 容器/代理类进程：接管它的参数没有意义
            push_warning(&mut info.warnings, "wrapperHint");
            info.args = None;
            info.workdir = None;
        } else if info.workdir.is_none()
            && argv.len() > 1
            && argv[1..].iter().any(|arg| looks_relative_path(arg))
        {
            push_warning(&mut info.warnings, "relativeArgNoWorkdir");
        }
        if should_warn_system_or_elevated(
            exe,
            info.raw_command_line.as_deref(),
            probe.cmd_read_failed,
        ) {
            push_warning(&mut info.warnings, "systemOrElevated");
        }
    }

    if let Some(parent) = parent_process_name(target) {
        if is_shell_name(&parent) {
            push_warning(&mut info.warnings, "parentShell");
        }
    }

    info
}

/// 根据「端口上的主人」和调用方给的期望 PID 决定要读哪个进程。
/// 返回 (pid, pid_matched, warning codes)。
fn resolve_owner(owners: &[u32], expected_pid: Option<u32>) -> (Option<u32>, bool, Vec<&'static str>) {
    if owners.is_empty() {
        return (None, false, vec!["processGone"]);
    }
    if owners.len() > 1 {
        let contained = expected_pid.map(|e| owners.contains(&e)).unwrap_or(false);
        let pid = if contained { expected_pid } else { None };
        let mut codes = Vec::new();
        if expected_pid.is_some() && !contained {
            codes.push("portOwnerChanged");
        }
        codes.push("multipleOwners");
        return (pid, contained, codes);
    }
    let owner = owners[0];
    match expected_pid {
        None => (Some(owner), true, Vec::new()),
        Some(expected) if expected == owner => (Some(owner), true, Vec::new()),
        Some(_) => (Some(owner), false, vec!["portOwnerChanged"]),
    }
}

fn warning_text(code: &str) -> &'static str {
    match code {
        "sensitiveArgs" => {
            "启动参数中疑似包含密码或密钥，请确认是否需要填入配置（配置以明文保存）"
        }
        "permissionDenied" => {
            "无法读取该进程的信息（可能是受保护的系统进程，或以管理员身份启动的进程）"
        }
        "systemOrElevated" => {
            "这是 Windows 系统目录中的程序，权限不足，无法读取其命令行"
        }
        "processGone" => "该端口上已无法看到监听进程（可能刚退出）",
        "portOwnerChanged" => {
            "该端口当前的占用进程不是所选进程，未覆盖已填写的内容，请核对后再使用"
        }
        "multipleOwners" => "该端口同时被多个进程监听，无法确定应读取哪一个，已跳过自动填充",
        "workdirDenied" => "无法读取该进程的工作目录（权限不足）",
        "workdirUnread" => {
            "未能读取该进程的工作目录（可能刚启动、刚退出，或系统未按常规返回），未自动填充"
        }
        "workdirNotExist" => "读取到的工作目录在本机已不存在，未自动填充",
        "readTimeout" => {
            "未能及时读取该进程的信息（可能已卡住，或系统繁忙），本次未读取到参数和工作目录"
        }
        "unsupported" => "系统未按常规返回该进程的命令行，已改用兼容方式读取",
        "controlChars" => {
            "参数包含换行/制表符，填入一行配置会变形，未自动填充；可在下方查看原始命令行"
        }
        "tooLong" => "参数过长（超过 8192 个字符），未自动填充；可在下方查看原始命令行",
        "parentShell" => {
            "该进程由命令行窗口（cmd/PowerShell/bash）启动，环境变量可能无法传递；建议将启动程序填写为脚本本身"
        }
        "wrapperHint" => {
            "这似乎是容器/代理类进程（docker/wsl/cloudflared 等），接管其参数没有意义，已跳过自动填充"
        }
        "exeNotFound" => "程序路径在本机无法找到（可能已被删除或移动）",
        "relativeArgNoWorkdir" => {
            "参数中似乎包含相对路径，但未读取到工作目录，直接填入可能导致启动失败"
        }
        _ => "读取进程信息时出错",
    }
}

fn push_warning(warnings: &mut Vec<LaunchWarning>, code: &str) {
    if warnings.iter().any(|w| w.code == code) {
        return;
    }
    warnings.push(LaunchWarning {
        code: code.to_string(),
        text: warning_text(code).to_string(),
    });
}

/// 独立线程里读进程（类 60 + PEB），主线程只等 500ms。
fn probe_with_timeout(pid: u32) -> Result<Probe, ()> {
    let (tx, rx) = mpsc::channel();
    // 线程创建失败不再 panic：降级成「没读到」，调用方按 readTimeout 处理。
    let spawned = std::thread::Builder::new()
        .name("sx-proc-probe".into())
        .spawn(move || {
            // 线程里所有句柄都是 RAII；超时后线程自己跑完、句柄自己关，不受调用方影响
            let _ = tx.send(probe_process(pid));
        });
    if spawned.is_err() {
        return Err(());
    }
    rx.recv_timeout(PEB_JOIN_TIMEOUT).map_err(|_| ())
}

#[derive(Default)]
struct Probe {
    exe: Option<String>,
    raw: Option<String>,
    workdir: Option<String>,
    codes: Vec<&'static str>,
    /// 读命令行失败过（类 60 或 OpenProcess）：和 "真正读不到命令行" 一起决定是否报
    /// `systemOrElevated`，不能拿无关原因（工作目录没读到）凑数。
    cmd_read_failed: bool,
}

fn probe_process(pid: u32) -> Probe {
    let mut probe = Probe::default();
    let mut want_peb_command_line = false;

    match OwnedHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
        Ok(handle) => {
            if let Ok(path) = query_image_path(handle.raw()) {
                probe.exe = Some(path);
            }
            match query_command_line_60(handle.raw()) {
                Ok(text) => probe.raw = text,
                Err(_) => {
                    // 类 60 返回了意外状态：回退到 PEB 里的命令行。
                    // 只有 PEB 真读到命令行时，才保留「已改用兼容方式读取」的告警。
                    probe.codes.push("unsupported");
                    probe.cmd_read_failed = true;
                    want_peb_command_line = true;
                }
            }
        }
        Err(code) => {
            probe.codes.push(if code == ERROR_INVALID_PARAMETER {
                "processGone"
            } else {
                "permissionDenied"
            });
            if code != ERROR_INVALID_PARAMETER {
                probe.cmd_read_failed = true;
            }
            want_peb_command_line = true;
        }
    }

    // 工作目录需要更高的权限：只有权限明确被拒才说「权限不够」，
    // 其它原因（刚退出 / 结构读不出来）用中性文案。
    let mut workdir_permission_denied = false;
    match OwnedHandle::open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) {
        Ok(handle) => {
            if let Ok(strings) = read_peb_strings(handle.raw()) {
                if probe.raw.is_none() && want_peb_command_line {
                    probe.raw = strings.command_line;
                }
                probe.workdir = strings.workdir;
            }
        }
        Err(code) => {
            if code == ERROR_ACCESS_DENIED {
                workdir_permission_denied = true;
            }
        }
    }

    drop_unsupported_without_fallback(&mut probe.codes, probe.raw.as_deref());
    if probe.workdir.is_none() && !probe.codes.contains(&"processGone") {
        probe.codes.push(workdir_warning_code(workdir_permission_denied));
    }

    probe
}

/// 工作目录没读到时的告警码：权限明确被拒才用 `workdirDenied`（文案「权限不够」），
/// 其它原因（刚退出 / 结构读不出来）用中性的 `workdirUnread`。
fn workdir_warning_code(permission_denied: bool) -> &'static str {
    if permission_denied {
        "workdirDenied"
    } else {
        "workdirUnread"
    }
}

/// 类 60 失败、PEB 回退也没读到命令行时，去掉「已改用兼容方式读取」这条假告警。
fn drop_unsupported_without_fallback(
    codes: &mut Vec<&'static str>,
    raw_command_line: Option<&str>,
) {
    if raw_command_line.is_none() {
        codes.retain(|code| *code != "unsupported");
    }
}

/// `systemOrElevated` 只在「程序在 Windows 系统目录 + 确实读不到命令行」时成立，
/// 不能因为工作目录读不到（或目录已被删）就误报。
fn should_warn_system_or_elevated(
    exe: &str,
    raw_command_line: Option<&str>,
    cmd_read_failed: bool,
) -> bool {
    cmd_read_failed && raw_command_line.is_none() && under_windows_dir(exe)
}

// ---------------------------------------------------------------- 句柄 RAII

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn open(pid: u32, access: u32) -> Result<Self, u32> {
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            Err(unsafe { GetLastError() })
        } else {
            Ok(OwnedHandle(handle))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct SnapshotHandle(HANDLE);

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

// ---------------------------------------------------------------- 类 60

/// 读命令行：先小缓冲探测，`STATUS_INFO_LENGTH_MISMATCH` 时按 ReturnLength 重试。
/// 返回 `Ok(None)` = 进程没有命令行；`Err(status)` = 类 60 没按约定返回。
fn query_command_line_60(handle: HANDLE) -> Result<Option<String>, u32> {
    let mut probe = [0u8; 64];
    let mut return_length: u32 = 0;
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessCommandLineInformation,
            probe.as_mut_ptr().cast(),
            probe.len() as u32,
            &mut return_length,
        )
    };
    if status == 0 {
        return Ok(decode_command_line(&probe));
    }
    if status as u32 != STATUS_INFO_LENGTH_MISMATCH {
        return Err(status as u32);
    }
    let needed = return_length as usize;
    if needed < 16 || needed > MAX_CMD_BUF {
        return Err(status as u32);
    }
    let mut buf = vec![0u8; needed];
    let mut return2: u32 = 0;
    let status2 = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut return2,
        )
    };
    if status2 != 0 {
        return Err(status2 as u32);
    }
    Ok(decode_command_line(&buf))
}

/// 类 60 输出缓冲：前 16 字节 `UNICODE_STRING`，字符串内联在 +16。
fn decode_command_line(buf: &[u8]) -> Option<String> {
    if buf.len() < 16 {
        return None;
    }
    let length = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    if length == 0 || length % 2 != 0 || length > buf.len() - 16 {
        return None;
    }
    let text = String::from_utf16_lossy(&u16s(&buf[16..16 + length]));
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn query_image_path(handle: HANDLE) -> Result<String, u32> {
    // 固定大缓冲：QFPI 在缓冲不够时不会回填所需长度（GetLastError=122），只能给足
    let mut buf = vec![0u16; 32 * 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) };
    if ok == 0 {
        return Err(unsafe { GetLastError() });
    }
    let n = (len as usize).min(buf.len());
    Ok(String::from_utf16_lossy(&buf[..n]))
}

// ---------------------------------------------------------------- PEB

struct PebStrings {
    command_line: Option<String>,
    workdir: Option<String>,
}

fn read_peb_strings(handle: HANDLE) -> Result<PebStrings, ()> {
    if usize::BITS < 64 {
        // 本模块按 64 位调用方设计（ReadProcessMemory 地址是 64 位）
        return Err(());
    }
    let mut basic: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut return_length: u32 = 0;
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessBasicInformation,
            (&mut basic as *mut PROCESS_BASIC_INFORMATION).cast(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut return_length,
        )
    };
    if status != 0 {
        return Err(());
    }
    let peb = basic.PebBaseAddress as usize;
    if peb == 0 {
        return Err(());
    }

    let mut wow_peb: usize = 0;
    let mut return26: u32 = 0;
    let status26 = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessWow64Information,
            (&mut wow_peb as *mut usize).cast(),
            size_of::<usize>() as u32,
            &mut return26,
        )
    };
    if status26 == 0 && wow_peb != 0 {
        // WOW64：必须走 32 位 PEB，64 位 PEB 的 CWD 是陈旧值
        read_peb32_strings(handle, wow_peb)
    } else {
        read_peb64_strings(handle, peb)
    }
}

fn read_peb64_strings(handle: HANDLE, peb: usize) -> Result<PebStrings, ()> {
    let params = read_u64(handle, peb.checked_add(0x20).ok_or(())?).ok_or(())?;
    if params == 0 {
        return Err(());
    }
    Ok(PebStrings {
        command_line: read_ustr64(handle, params, 0x70),
        workdir: read_ustr64(handle, params, 0x38),
    })
}

fn read_peb32_strings(handle: HANDLE, peb32: usize) -> Result<PebStrings, ()> {
    let params = read_u32(handle, peb32.checked_add(0x10).ok_or(())?).ok_or(())? as usize;
    if params == 0 {
        return Err(());
    }
    Ok(PebStrings {
        command_line: read_ustr32(handle, params, 0x40),
        workdir: read_ustr32(handle, params, 0x24),
    })
}

fn read_mem(handle: HANDLE, addr: usize, out: &mut [u8]) -> Option<()> {
    if out.is_empty() {
        return Some(());
    }
    let mut read: usize = 0;
    let ok = unsafe {
        ReadProcessMemory(
            handle,
            addr as *const core::ffi::c_void,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut read,
        )
    };
    if ok == 0 || read != out.len() {
        None
    } else {
        Some(())
    }
}

fn read_u64(handle: HANDLE, addr: usize) -> Option<usize> {
    let mut bytes = [0u8; 8];
    read_mem(handle, addr, &mut bytes)?;
    Some(u64::from_le_bytes(bytes) as usize)
}

fn read_u32(handle: HANDLE, addr: usize) -> Option<u32> {
    let mut bytes = [0u8; 4];
    read_mem(handle, addr, &mut bytes)?;
    Some(u32::from_le_bytes(bytes))
}

/// x64 UNICODE_STRING：Length u16 / MaximumLength u16 / pad u32 / Buffer u64
fn read_ustr64(handle: HANDLE, base: usize, offset: usize) -> Option<String> {
    let addr = base.checked_add(offset)?;
    let mut head = [0u8; 16];
    read_mem(handle, addr, &mut head)?;
    let length = u16::from_le_bytes([head[0], head[1]]) as usize;
    let buffer = u64::from_le_bytes(head[8..16].try_into().ok()?) as usize;
    read_remote_string(handle, length, buffer)
}

/// WOW64 UNICODE_STRING：Length u16 / MaximumLength u16 / Buffer u32
fn read_ustr32(handle: HANDLE, base: usize, offset: usize) -> Option<String> {
    let addr = base.checked_add(offset)?;
    let mut head = [0u8; 8];
    read_mem(handle, addr, &mut head)?;
    let length = u16::from_le_bytes([head[0], head[1]]) as usize;
    let buffer = u32::from_le_bytes(head[4..8].try_into().ok()?) as usize;
    read_remote_string(handle, length, buffer)
}

fn read_remote_string(handle: HANDLE, length: usize, buffer: usize) -> Option<String> {
    // 远端长度必须严格校验：偶数、非 0、不超上限
    if length == 0 || length % 2 != 0 || length > MAX_STR_BYTES || buffer == 0 {
        return None;
    }
    let mut bytes = vec![0u8; length];
    read_mem(handle, buffer, &mut bytes)?;
    Some(String::from_utf16_lossy(&u16s(&bytes)))
}

// ---------------------------------------------------------------- 文本处理

fn u16s(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// 工作目录规范化：剥 `\??\` / `\\?\` 前缀，去掉尾部的反斜杠（根目录 `C:\` 保留）。
fn clean_workdir(raw: &str) -> String {
    let trimmed_input = raw.trim();
    let mut text = trimmed_input.to_string();
    let lower = text.to_ascii_lowercase();
    for prefix in ["\\\\??\\", "\\??\\", "\\\\?\\"] {
        if lower.starts_with(prefix) {
            text = text[prefix.len()..].to_string();
            break;
        }
    }
    let trimmed = text.trim_end_matches(['\\', '/']).to_string();
    if trimmed.is_empty() {
        return text;
    }
    let mut out = trimmed;
    if out.len() == 2 && out.as_bytes()[1] == b':' {
        out.push('\\');
    }
    out
}

/// 疑似密钥扫描（大小写不敏感，手写关键词，不引依赖，也绝不写日志）。
fn contains_sensitive(text: &str) -> bool {
    const KEYWORDS: [&str; 10] = [
        "password",
        "passwd",
        "token",
        "secret",
        "api-key",
        "api_key",
        "apikey",
        "bearer",
        "authorization",
        "credential",
    ];
    let lower = text.to_ascii_lowercase();
    if KEYWORDS.iter().any(|keyword| lower.contains(keyword)) {
        return true;
    }
    if url_has_credentials(&lower) {
        return true;
    }
    has_sk_key(&lower)
}

/// `scheme://user:password@host` 形态
fn url_has_credentials(lower: &str) -> bool {
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find("://") {
        let start = from + pos + 3;
        let rest = &lower[start..];
        let end = rest
            .find(|c: char| matches!(c, '/' | ' ' | '"' | '\'' | ',' | ';'))
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        if let Some(at) = authority.rfind('@') {
            let userinfo = &authority[..at];
            if let Some(colon) = userinfo.rfind(':') {
                if colon + 1 < userinfo.len() {
                    return true;
                }
            }
        }
        from = start;
    }
    false
}

/// `sk-` 前缀的长串（OpenAI 之类的密钥）
fn has_sk_key(lower: &str) -> bool {
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find("sk-") {
        let start = from + pos + 3;
        let count = lower[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .count();
        if count >= 16 {
            return true;
        }
        from = start;
    }
    false
}

/// 看着像相对路径的参数（有路径分隔符、又不是绝对路径/开关）。
fn looks_relative_path(token: &str) -> bool {
    if token.starts_with('-') {
        return false;
    }
    if !token.contains('\\') && !token.contains('/') {
        return false;
    }
    let bytes = token.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    let unc = token.starts_with("\\\\") || token.starts_with("//");
    let unix_absolute = token.starts_with('/');
    !(drive_absolute || unc || unix_absolute)
}

/// 容器/代理类进程：接管它的启动参数没有意义。
fn is_wrapper_exe(exe: &str) -> bool {
    let path = exe.replace('/', "\\").to_ascii_lowercase();
    let name = path.rsplit('\\').next().unwrap_or(path.as_str());
    matches!(
        name,
        "docker-proxy.exe" | "wslrelay.exe" | "com.docker.backend.exe" | "cloudflared.exe"
    ) || path.contains("_npx\\")
        || path.contains("npm-cache\\")
}

/// exe 是否在 Windows 系统目录下（默认 `C:\Windows`，取 SystemRoot 更准）。
fn under_windows_dir(exe: &str) -> bool {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let root = root
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase();
    if root.is_empty() {
        return false;
    }
    let path = exe.replace('/', "\\").to_ascii_lowercase();
    path.starts_with(&(root + "\\"))
}

fn is_shell_name(name: &str) -> bool {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        stem.as_str(),
        "cmd" | "powershell" | "pwsh" | "bash" | "sh" | "zsh"
    )
}

/// 目标进程的父进程名（ToolHelp 快照，不需要额外权限）。
fn parent_process_name(pid: u32) -> Option<String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let snapshot = SnapshotHandle(snapshot);

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut parent: u32 = 0;
        let mut ok = Process32FirstW(snapshot.0, &mut entry);
        while ok != 0 {
            if entry.th32ProcessID == pid {
                parent = entry.th32ParentProcessID;
                break;
            }
            ok = Process32NextW(snapshot.0, &mut entry);
        }
        if parent == 0 || parent == pid {
            return None;
        }

        entry = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        ok = Process32FirstW(snapshot.0, &mut entry);
        while ok != 0 {
            if entry.th32ProcessID == parent {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                return Some(String::from_utf16_lossy(&entry.szExeFile[..end]));
            }
            ok = Process32NextW(snapshot.0, &mut entry);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_resolution_covers_all_cases() {
        // 端口上没人
        assert_eq!(resolve_owner(&[], Some(7)), (None, false, vec!["processGone"]));
        assert_eq!(resolve_owner(&[], None), (None, false, vec!["processGone"]));
        // 一个主人：没给期望 PID → 匹配；给了且一致 → 匹配
        assert_eq!(resolve_owner(&[7], None), (Some(7), true, vec![]));
        assert_eq!(resolve_owner(&[7], Some(7)), (Some(7), true, vec![]));
        // 一个主人但不是期望的那个 → 报换主人，仍然给出实际值供前端参考
        assert_eq!(
            resolve_owner(&[7], Some(9)),
            (Some(7), false, vec!["portOwnerChanged"])
        );
        // 多个主人：不知道读哪个
        assert_eq!(
            resolve_owner(&[7, 9], None),
            (None, false, vec!["multipleOwners"])
        );
        assert_eq!(
            resolve_owner(&[7, 9], Some(9)),
            (Some(9), true, vec!["multipleOwners"])
        );
        assert_eq!(
            resolve_owner(&[7, 9], Some(5)),
            (None, false, vec!["portOwnerChanged", "multipleOwners"])
        );
    }

    #[test]
    fn workdir_is_cleaned_conservatively() {
        assert_eq!(clean_workdir(r"\??\C:\Windows\Temp\"), r"C:\Windows\Temp");
        assert_eq!(clean_workdir(r"\\?\D:\x\y\\"), r"D:\x\y");
        assert_eq!(clean_workdir(r"C:\"), r"C:\");
        assert_eq!(clean_workdir(r"D:\a\b\"), r"D:\a\b");
        assert_eq!(clean_workdir(r"  D:\a\b  "), r"D:\a\b");
        assert_eq!(clean_workdir(r"\\server\share\"), r"\\server\share");
    }

    #[test]
    fn sensitive_args_are_detected() {
        for text in [
            "--password hunter2",
            "--PASSWD=hunter2",
            "token=hunter2",
            "--api-key hunter2",
            "My_Secret_Value",
            "Authorization: Bearer hunter2",
            "credential=hunter2",
            "https://user:hunter2@example.com/api",
            "mysql://root:hunter2@127.0.0.1/db",
            "sk-hunter2hunter2hunter2",
        ] {
            assert!(contains_sensitive(text), "应该认出来：{text}");
        }
        for text in [
            "--port 8188 --dir D:\\my app\\data",
            "--model sd-v1-5",
            "--listen 127.0.0.1:8188",
            "sk-short",
        ] {
            assert!(!contains_sensitive(text), "不该误报：{text}");
        }
    }

    #[test]
    fn relative_path_arguments_are_detected() {
        assert!(looks_relative_path(r"models\sd.safetensors"));
        assert!(looks_relative_path(r".\run.bat"));
        assert!(looks_relative_path("sub/dir"));
        assert!(!looks_relative_path(r"D:\models\sd.safetensors"));
        assert!(!looks_relative_path(r"\\server\share\x"));
        assert!(!looks_relative_path("--config=value"));
        assert!(!looks_relative_path("plain"));
        assert!(!looks_relative_path("/home/user/x"));
    }

    #[test]
    fn wrapper_and_shell_names_match() {
        assert!(is_wrapper_exe(r"D:\Docker\docker-proxy.exe"));
        assert!(is_wrapper_exe(r"C:\Users\a\AppData\Local\npm-cache\_npx\abc\node.exe"));
        assert!(is_wrapper_exe(r"C:\Program Files\cloudflared.exe"));
        assert!(!is_wrapper_exe(r"D:\Python\python.exe"));
        assert!(is_shell_name("cmd.exe"));
        assert!(is_shell_name(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"));
        assert!(is_shell_name("bash.exe"));
        assert!(!is_shell_name("fish.exe"));
        assert!(!is_shell_name("python.exe"));
    }

    #[test]
    fn system_or_elevated_only_when_command_line_really_unreadable() {
        // 真的读不到命令行 + 系统目录 → 报
        assert!(should_warn_system_or_elevated(
            r"C:\Windows\System32\svchost.exe",
            None,
            true
        ));
        // 命令行读到了 → 不报（哪怕工作目录没读到）
        assert!(!should_warn_system_or_elevated(
            r"C:\Windows\System32\svchost.exe",
            Some(r"svchost.exe -k netsvcs"),
            true
        ));
        // 命令行是空的、但读取没失败 → 不报
        assert!(!should_warn_system_or_elevated(
            r"C:\Windows\System32\svchost.exe",
            None,
            false
        ));
        // 不在系统目录 → 不报（由 permissionDenied 的中性文案负责）
        assert!(!should_warn_system_or_elevated(
            r"D:\tools\other.exe",
            None,
            true
        ));
    }

    #[test]
    fn unsupported_is_dropped_when_fallback_read_nothing() {
        let mut codes = vec!["unsupported"];
        drop_unsupported_without_fallback(&mut codes, None);
        assert!(
            codes.is_empty(),
            "PEB 也没读到时不该说“已改用兼容方式读取”：{codes:?}"
        );

        let mut codes = vec!["unsupported"];
        drop_unsupported_without_fallback(&mut codes, Some(r"app.exe --x"));
        assert_eq!(codes, vec!["unsupported"], "PEB 读到了就该保留");
    }

    #[test]
    fn workdir_warning_matches_reason() {
        assert_eq!(workdir_warning_code(true), "workdirDenied");
        assert_eq!(
            warning_text(workdir_warning_code(true)),
            "无法读取该进程的工作目录（权限不足）"
        );
        assert_eq!(workdir_warning_code(false), "workdirUnread");
        let neutral = warning_text(workdir_warning_code(false));
        assert!(
            !neutral.contains("权限不足"),
            "非权限原因不能用「权限不足」：{neutral}"
        );
    }

    #[test]
    fn every_warning_code_has_human_text() {
        for code in [
            "sensitiveArgs",
            "permissionDenied",
            "systemOrElevated",
            "processGone",
            "portOwnerChanged",
            "multipleOwners",
            "workdirDenied",
            "workdirUnread",
            "workdirNotExist",
            "readTimeout",
            "unsupported",
            "controlChars",
            "tooLong",
            "parentShell",
            "wrapperHint",
            "exeNotFound",
            "relativeArgNoWorkdir",
        ] {
            let text = warning_text(code);
            assert!(!text.is_empty() && text != "读取进程信息时出错", "{code}");
        }
    }

    #[test]
    fn launch_info_json_shape_is_camel_case() {
        let info = LaunchInfo {
            port: 8080,
            pid: Some(42),
            pid_matched: true,
            exe: Some(r"D:\x\a.exe".into()),
            args: Some("--port 8080".into()),
            workdir: Some(r"D:\x".into()),
            raw_command_line: Some(r#""D:\x\a.exe" --port 8080"#.into()),
            warnings: vec![LaunchWarning {
                code: "processGone".into(),
                text: "人话".into(),
            }],
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["port"], 8080);
        assert_eq!(value["pid"], 42);
        assert_eq!(value["pidMatched"], true);
        assert_eq!(value["rawCommandLine"], r#""D:\x\a.exe" --port 8080"#);
        assert_eq!(value["warnings"][0]["code"], "processGone");
        assert_eq!(value["warnings"][0]["text"], "人话");
    }
}
