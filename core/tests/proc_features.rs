//! 真机集成测试：`proc_detail::launch_info` 读取运行进程的程序路径/参数/工作目录。
//!
//! 覆盖：自己进程（64 位 PEB + 类 60）、另一个进程（辅助进程，参数与工作目录）、
//! WOW64 进程（本机有 32 位 PowerShell 时验证必须走 32 位 PEB）、
//! 端口主人换了仍给出实际值、参数里的敏感信息告警。
//! 运行：cargo test -p shangxian-core --test proc_features

#![cfg(windows)]

mod common;

use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use shangxian_core::proc_detail::{launch_info, LaunchInfo};

fn norm_path(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn has_code(info: &LaunchInfo, code: &str) -> bool {
    info.warnings.iter().any(|w| w.code == code)
}

#[test]
fn launch_info_reads_current_process_listener() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let pid = std::process::id();

    let info = launch_info(port, Some(pid));

    assert_eq!(info.pid, Some(pid), "warnings={:?}", info.warnings);
    assert!(info.pid_matched, "warnings={:?}", info.warnings);
    let exe = info.exe.clone().expect("应该读到 exe");
    assert!(std::path::Path::new(&exe).is_file(), "exe={exe}");
    assert!(info.args.is_some(), "args 应该可读：{:?}", info.warnings);
    assert!(info.raw_command_line.is_some(), "raw 应该可读");
    let cwd = std::env::current_dir().unwrap().display().to_string();
    assert_eq!(
        info.workdir.as_deref().map(norm_path),
        Some(norm_path(&cwd)),
        "workdir 应该等于它现在的工作目录：warnings={:?}",
        info.warnings
    );
    assert!(
        !has_code(&info, "readTimeout") && !has_code(&info, "workdirDenied"),
        "不该有读取失败：{:?}",
        info.warnings
    );
}

#[test]
fn launch_info_without_expected_pid_uses_current_owner() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    let info = launch_info(port, None);

    assert_eq!(info.pid, Some(std::process::id()), "{:?}", info.warnings);
    assert!(info.pid_matched, "{:?}", info.warnings);
    assert!(!has_code(&info, "portOwnerChanged"), "{:?}", info.warnings);
    assert!(info.exe.is_some() && info.args.is_some(), "{:?}", info.warnings);
}

/// 期望的 PID 已经不在监听这个端口了：PID 必须如实报成现在的主人、pidMatched=false，
/// 但 exe/args 仍可给实际值（前端自己决定不覆盖）。
#[test]
fn launch_info_reports_owner_change_but_fills_actual_values() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    let info = launch_info(port, Some(0)); // 0 不可能是端口主人

    assert_eq!(info.pid, Some(std::process::id()), "{:?}", info.warnings);
    assert!(!info.pid_matched, "{:?}", info.warnings);
    assert!(has_code(&info, "portOwnerChanged"), "{:?}", info.warnings);
    assert!(
        info.exe.is_some() && info.args.is_some(),
        "换了主人也要给实际值：{:?}",
        info.warnings
    );
}

#[test]
fn launch_info_reports_process_gone() {
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let info = launch_info(port, Some(std::process::id()));

    assert_eq!(info.pid, None, "{:?}", info.warnings);
    assert!(!info.pid_matched);
    assert!(has_code(&info, "processGone"), "{:?}", info.warnings);
    assert!(info.exe.is_none() && info.args.is_none() && info.workdir.is_none());
}

#[test]
fn launch_info_reads_other_process_args_and_workdir() {
    let dir = std::env::temp_dir().join("sx-proc-launch-info");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut helper = common::spawn_listen_helper(&dir).expect("辅助进程没起来");
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "端口没进入 LISTENING"
    );

    let info = launch_info(helper.port, Some(helper.pid()));

    assert_eq!(info.pid, Some(helper.pid()), "warnings={:?}", info.warnings);
    assert!(info.pid_matched, "warnings={:?}", info.warnings);
    let exe = info.exe.clone().expect("应该读到 exe");
    let me = std::env::current_exe().unwrap().display().to_string();
    assert_eq!(norm_path(&exe), norm_path(&me), "exe={exe}");
    let args = info.args.clone().expect("应该读到 args");
    assert!(args.contains("common::listen_helper"), "args={args}");
    assert!(args.contains("--nocapture"), "args={args}");
    assert_eq!(
        info.workdir.as_deref().map(norm_path),
        Some(norm_path(&dir.display().to_string())),
        "workdir 应该是辅助进程被指定的工作目录：warnings={:?}",
        info.warnings
    );
    for code in [
        "readTimeout",
        "workdirDenied",
        "workdirNotExist",
        "controlChars",
        "tooLong",
        "processGone",
        "portOwnerChanged",
    ] {
        assert!(!has_code(&info, code), "不该有 {code}：{:?}", info.warnings);
    }

    assert!(helper.is_running(), "辅助进程应该还在跑");
}

/// 参数里带 password/token 时必须给 sensitiveArgs 告警（但参数仍照常返回）。
#[test]
fn launch_info_warns_on_sensitive_args() {
    let ps = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    if !Path::new(ps).exists() {
        println!("跳过：本机没有 PowerShell（{ps}）");
        return;
    }
    let dir = std::env::temp_dir().join("sx-proc-launch-sensitive");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let body = "$x='--password hunter2 --token abcdefghijklmnop'";
    let Some(helper) = common::spawn_ps_listener(Path::new(ps), &dir, body) else {
        println!("跳过：PowerShell 监听进程没起来");
        return;
    };
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "端口没进入 LISTENING"
    );

    let info = launch_info(helper.port, Some(helper.pid()));

    assert_eq!(info.pid, Some(helper.pid()), "warnings={:?}", info.warnings);
    assert!(has_code(&info, "sensitiveArgs"), "{:?}", info.warnings);
    assert!(
        info.args.as_deref().unwrap_or_default().contains("--password"),
        "args 仍应返回：{:?}",
        info.args
    );
}

/// WOW64：32 位进程的当前目录必须从 32 位 PEB 读（64 位 PEB 的 CWD 是陈旧值）。
/// 本机没有 32 位 PowerShell 时优雅跳过。
#[test]
fn launch_info_wow64_reads_32bit_peb_workdir() {
    let exe32 = r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe";
    if !Path::new(exe32).exists() {
        println!("跳过：本机没有 32 位 PowerShell（{exe32}）");
        return;
    }
    let dir = std::env::temp_dir().join("sx-proc-launch-info-wow64");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let Some(helper) = common::spawn_ps_listener(Path::new(exe32), &dir, "") else {
        println!("跳过：32 位 PowerShell 没起来");
        return;
    };
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "32 位进程的端口没进入 LISTENING"
    );

    let info = launch_info(helper.port, Some(helper.pid()));

    assert_eq!(info.pid, Some(helper.pid()), "warnings={:?}", info.warnings);
    assert!(info.pid_matched, "warnings={:?}", info.warnings);
    let workdir = info.workdir.clone().unwrap_or_default();
    assert_eq!(
        norm_path(&workdir),
        norm_path(&dir.display().to_string()),
        "WOW64 必须用 32 位 PEB 读工作目录：warnings={:?}",
        info.warnings
    );
    let exe = info.exe.clone().unwrap_or_default();
    assert!(
        norm_path(&exe).ends_with("syswow64\\windowspowershell\\v1.0\\powershell.exe"),
        "exe={exe}（应当读到 32 位 PowerShell）"
    );
}
