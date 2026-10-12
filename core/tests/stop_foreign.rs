//! 真机集成测试：`guardian::stop_service` 对「外部进程」的确认 → 结束流程。
//!
//! 安全断言：
//! - 没确认（kill_foreign=false）时只提示，绝不动手；
//! - 确认了但 PID 变了 / 程序路径和配置不一样时，也必须拒绝并给人话错误；
//! - 确认且校验通过，才真的结束，并返回 ForeignStopped。
//!
//! 运行：cargo test -p shangxian-core --test stop_foreign

#![cfg(windows)]

mod common;

use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use shangxian_core::config::{AppConfig, LaunchSpec, LocalService};
use shangxian_core::guardian::{Guardian, StopOutcome};

fn make_guardian_with(tag: &str, port: u16, launch: Option<LaunchSpec>) -> Guardian {
    let dir = std::env::temp_dir().join(format!("sx-stop-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let config = AppConfig {
        version: 4,
        servers: Vec::new(),
        services: vec![LocalService {
            id: "svc-1".into(),
            name: "测试服务".into(),
            local_port: port,
            probe_kind: "tcp".into(),
            launch,
            watch: false,
            autostart: false,
            note: None,
        }],
        tunnels: Vec::new(),
        notify_enabled: true,
        settings: shangxian_core::config::Settings::default(),
    };
    Guardian::new(
        config,
        dir.join("config.json"),
        dir.join("logs"),
        std::sync::Arc::new(shangxian_core::log::NullReporter),
    )
}

fn make_guardian(tag: &str, port: u16, launch_exe: Option<String>) -> Guardian {
    make_guardian_with(
        tag,
        port,
        launch_exe.map(|exe| LaunchSpec {
            exe,
            args: String::new(),
            workdir: None,
        }),
    )
}

/// 端口上的进程是「外部进程」（Guardian 没启动过）时，第一次停止只能要确认。
/// （这里必须配了启动方式，否则是「只监控」服务，会直接给人话错误——见 P2-5。）
#[test]
fn foreign_listener_asks_for_confirmation_first() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut guardian = make_guardian(
        "confirm",
        port,
        Some(r"C:\definitely\not\me.exe".into()),
    );

    match guardian.stop_service("svc-1", false, None).unwrap() {
        StopOutcome::NeedsConfirm { pid, exe } => {
            assert_eq!(pid, std::process::id());
            assert!(exe.is_some(), "测试进程应该能读到 exe");
        }
        other => panic!("期望 NeedsConfirm，实际 {other:?}"),
    }
}

/// 即使确认了，程序路径和配置不一致也必须拒绝（这里配置指向别的程序）。
#[test]
fn kill_foreign_refuses_when_exe_does_not_match() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut guardian = make_guardian("refuse", port, Some(r"C:\definitely\not\me.exe".into()));

    let err = guardian
        .stop_service("svc-1", true, Some(std::process::id()))
        .unwrap_err();
    assert!(
        err.contains("没有结束") || err.contains("不是这台服务配置的"),
        "err={err}"
    );
    // 测试进程当然还活着（listener 还在）
    assert!(listener.local_addr().is_ok());
}

/// 只监控的服务（没配启动方式）：绝不接管，就算用户确认了 PID 也不动它。
#[test]
fn kill_foreign_refuses_monitor_only_service_even_with_pid() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut guardian = make_guardian("monitor", port, None);

    let err = guardian
        .stop_service("svc-1", true, Some(std::process::id()))
        .unwrap_err();
    assert!(err.contains("没配启动方式"), "err={err}");
    assert!(err.contains("补全"), "err={err}");
    assert!(listener.local_addr().is_ok(), "测试进程当然还活着");
}

/// P2-5：只监控的服务 + `kill_foreign=false`，也必须直接给人话错误（提示端口上现在
/// 是谁在听），不能返回 NeedsConfirm——确认后反正也一定被拒，是死路。
#[test]
fn monitor_only_service_refuses_stop_without_kill_foreign() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut guardian = make_guardian("monitor-no-kill", port, None);

    let err = guardian.stop_service("svc-1", false, None).unwrap_err();
    assert!(err.contains("没配启动方式"), "err={err}");
    assert!(err.contains("端口上现在是"), "err={err}");
    assert!(err.contains("补全"), "err={err}");
    assert!(listener.local_addr().is_ok(), "外部进程不能被动");
}

/// 确认的 PID 和端口现在的主人不一样 → 拒绝。
#[test]
fn kill_foreign_refuses_stale_pid() {
    let _listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = _listener.local_addr().unwrap().port();
    // 配了启动方式（排除「只监控直接拒绝」那条分支，专门测 PID 过期）
    let mut guardian = make_guardian("stale", port, Some(r"C:\definitely\not\me.exe".into()));

    let err = guardian.stop_service("svc-1", true, Some(0)).unwrap_err();
    assert!(err.contains("不是 PID") || err.contains("重新确认"), "err={err}");
}

/// 端口上没有任何进程 → Stopped{pid:None}。
#[test]
fn stop_without_listener_reports_stopped() {
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let mut guardian = make_guardian("empty", port, None);
    match guardian.stop_service("svc-1", false, None).unwrap() {
        StopOutcome::Stopped { pid: None } => {}
        other => panic!("期望 Stopped{{pid:None}}，实际 {other:?}"),
    }
}

/// 完整闭环：确认后真的结束外部进程。
#[test]
fn confirmed_foreign_process_is_terminated() {
    let dir = std::env::temp_dir().join("sx-stop-kill");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut helper = common::spawn_listen_helper(&dir).expect("辅助进程没起来");
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "端口没进入 LISTENING"
    );

    let exe = std::env::current_exe().unwrap().display().to_string();
    let mut guardian = make_guardian("kill", helper.port, Some(exe));

    // 第一次：只确认，不能动手
    match guardian
        .stop_service("svc-1", false, Some(helper.pid()))
        .unwrap()
    {
        StopOutcome::NeedsConfirm { pid, exe } => {
            assert_eq!(pid, helper.pid());
            assert!(
                exe.as_deref()
                    .map(|e| std::path::Path::new(e).is_file())
                    .unwrap_or(false),
                "exe={exe:?}"
            );
        }
        other => panic!("期望 NeedsConfirm，实际 {other:?}"),
    }
    assert!(helper.is_running(), "确认前不能结束进程");

    // 用户确认：带着同一个 PID 再来
    match guardian
        .stop_service("svc-1", true, Some(helper.pid()))
        .unwrap()
    {
        StopOutcome::ForeignStopped { pid } => assert_eq!(pid, helper.pid()),
        other => panic!("期望 ForeignStopped，实际 {other:?}"),
    }
    assert!(
        helper.wait_exit(Duration::from_secs(10)),
        "确认后应该真的结束这个进程"
    );
}

/// launch 存在时，`kill_foreign=true` 即使省略 expectedPid，也要通过 exe 校验后才能结束。
#[test]
fn kill_foreign_without_expected_pid_still_validates_exe() {
    let dir = std::env::temp_dir().join("sx-stop-no-pid");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut helper = common::spawn_listen_helper(&dir).expect("辅助进程没起来");
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "端口没进入 LISTENING"
    );

    let exe = std::env::current_exe().unwrap().display().to_string();
    let mut guardian = make_guardian("no-pid", helper.port, Some(exe));

    match guardian.stop_service("svc-1", true, None).unwrap() {
        StopOutcome::ForeignStopped { pid } => assert_eq!(pid, helper.pid()),
        other => panic!("期望 ForeignStopped，实际 {other:?}"),
    }
    assert!(helper.wait_exit(Duration::from_secs(10)), "校验通过就应该真的结束");
}

/// 应用自己拉起来的进程：不用确认，直接停，返回 Stopped{pid}。
#[test]
fn own_child_stop_returns_stopped_with_pid() {
    let ps = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    if !Path::new(ps).exists() {
        println!("跳过：本机没有 PowerShell（{ps}）");
        return;
    }
    // 先占一个空闲端口再放开，让服务固定监听这个端口
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let script = format!(
        "$l=[System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback,{port});\
         $l.Start();Start-Sleep -Seconds 90"
    );
    let launch = LaunchSpec {
        exe: ps.into(),
        args: format!("-NoProfile -NonInteractive -Command \"{script}\""),
        workdir: None,
    };
    let mut guardian = make_guardian_with("own", port, Some(launch));

    guardian.start_service("svc-1").unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut pid = None;
    while Instant::now() < deadline {
        guardian.poll();
        if let Some(candidate) = guardian.service_pid("svc-1") {
            if common::wait_port_owner(port, candidate, Duration::from_secs(5)) {
                pid = Some(candidate);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let pid = pid.expect("服务应该起来并监听端口");

    match guardian.stop_service("svc-1", false, None).unwrap() {
        StopOutcome::Stopped { pid: stopped } => assert_eq!(stopped, Some(pid)),
        other => panic!("期望 Stopped，实际 {other:?}"),
    }
    assert!(guardian.service_pid("svc-1").is_none(), "自己的进程应该已结束");
}

/// P1 回归：`.cmd` 包装脚本再拉起的孙子进程监听端口时，第一次 `stop_service(false)`
/// 必须返回 `Stopped`（自家整棵树），不能把自家后代误判成「外部进程」弹假确认框。
#[test]
fn own_wrapper_grandchild_stop_returns_stopped_without_confirmation() {
    let ps = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    if !Path::new(ps).exists() {
        println!("跳过：本机没有 PowerShell（{ps}）");
        return;
    }

    let dir = std::env::temp_dir().join("sx-stop-wrapper");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    // 先占一个空闲端口再放开；随后让脚本里的孙子进程固定监听这个端口
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    // .cmd 直接子进程（cmd.exe）→ PowerShell 孙子进程监听端口
    let inner = format!(
        "$l=[System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback,{port});\
         $l.Start();Start-Sleep -Seconds 120"
    );
    let script_path = dir.join("wrap.cmd");
    let launch = LaunchSpec {
        exe: script_path.display().to_string(),
        args: String::new(),
        workdir: None,
    };
    // 注意：make_guardian_with 会清空并重建测试目录，脚本必须在它之后写
    let mut guardian = make_guardian_with("wrapper", port, Some(launch));
    std::fs::write(
        &script_path,
        format!("@echo off\r\n\"{ps}\" -NoProfile -NonInteractive -Command \"{inner}\"\r\n"),
    )
    .unwrap();

    guardian.start_service("svc-1").unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut listener_pid = None;
    while Instant::now() < deadline {
        guardian.poll();
        if let Some(pid) = shangxian_core::local_ports::listening_pids(port)
            .first()
            .copied()
        {
            listener_pid = Some(pid);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let listener_pid = listener_pid.expect("孙子进程应该起来并监听端口");
    let child_pid = guardian.service_pid("svc-1").expect("直接子进程应该在");
    assert_ne!(
        listener_pid, child_pid,
        "监听者必须是孙子进程，才能测出「只排除直接子进程」的误判"
    );

    // 第一次停止（没确认）：必须是 Stopped，而不是 NeedsConfirm
    match guardian.stop_service("svc-1", false, None).unwrap() {
        StopOutcome::Stopped { pid } => assert_eq!(pid, Some(child_pid)),
        other => panic!("期望 Stopped，实际 {other:?}（自家后代被误判成外部进程）"),
    }

    // 调用后监听进程确实消失（自家进程树被 Job 整体结束），不留孤儿
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let alive = shangxian_core::local_ports::listening_pids(port);
        if alive.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "停止后端口 {port} 上仍有监听进程：{alive:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(guardian.service_pid("svc-1").is_none(), "自家进程应该已结束");
}
