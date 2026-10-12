//! 真机端到端测试：本地服务保活 / 认领 / 看护复活 / 显式停止 / 退出停全部。
//!
//! 覆盖（对齐需求）：
//! - 保活：Guardian（应用）drop 后，它拉起来的本地服务继续跑（keep-alive 作业）；
//! - 认领：新 Guardian 首次 poll 认领已在监听的服务，不重复启动；
//! - 看护复活：认领的进程被杀掉后，watch 轮询把它重新拉起（新 PID）；
//! - 显式停止：stop_service 结束自家整棵树（含 .cmd 包装脚本的子孙进程）；
//! - 删除运行中服务：remove_service 同步停掉自家整棵树，端口释放、无孤儿
//!   （复审 verification_gap #1 回归：Drop 兜底杀已移除，删除是唯一清理点）；
//! - 退出停全部：stop_all_for_exit 停自家进程 + 核对过 exe 的认领进程，
//!   核对不过的（exe 不一致 / 只监控）绝不硬杀，计入「没能停」。
//!
//! 运行：cargo test -p shangxian-core --test keep_alive

#![cfg(windows)]

mod common;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use shangxian_core::config::{AppConfig, LaunchSpec, LocalService, CONFIG_VERSION};
use shangxian_core::guardian::{Guardian, ServiceInput, StopOutcome};

/// 从空闲端口探测里挑一个真实的空闲端口（先占住再释放）。
fn free_port() -> u16 {
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

fn listen_pids(port: u16) -> Vec<u32> {
    shangxian_core::local_ports::listening_pids(port)
}

/// 等到端口上出现监听者，返回它的 PID。
fn wait_listener(port: u16, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = listen_pids(port).first().copied() {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 等到端口彻底没人监听。
fn wait_port_clear(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if listen_pids(port).is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn make_config(port: u16, launch: Option<LaunchSpec>, watch: bool) -> AppConfig {
    AppConfig {
        version: CONFIG_VERSION,
        servers: Vec::new(),
        services: vec![LocalService {
            id: "svc-1".into(),
            name: "保活测试服务".into(),
            local_port: port,
            probe_kind: "tcp".into(),
            launch,
            watch,
            autostart: false,
            note: None,
        }],
        tunnels: Vec::new(),
        notify_enabled: true,
        settings: shangxian_core::config::Settings::default(),
    }
}

fn test_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sx-keep-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn make_guardian(dir: &Path, config: AppConfig) -> Guardian {
    Guardian::new(
        config,
        dir.join("config.json"),
        dir.join("logs"),
        std::sync::Arc::new(shangxian_core::log::NullReporter),
    )
}

/// 写一个 `.cmd` 包装脚本：设置 `SX_LISTEN_PORT` 后拉起测试二进制里的 helper。
///
/// 和服务真实启动链一致（`.cmd` → `cmd.exe` → 监听子进程）：显式停止必须能清掉
/// 整棵树（作业对象 terminate），认领看护也能在复活时重建同样的链路。
fn write_listen_wrapper(dir: &Path, port: u16) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let script = dir.join(format!("listen-{port}.cmd"));
    std::fs::write(
        &script,
        format!(
            "@echo off\r\nset \"SX_LISTEN_HELPER=1\"\r\nset \"SX_LISTEN_PORT={port}\"\r\n\"{}\" --exact common::listen_helper --ignored --nocapture\r\n",
            exe.display()
        ),
    )
    .expect("write listen wrapper");
    script
}

/// 进程还活着吗（句柄能打开且还没退出）。
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        let _ = CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

/// 进程 `root` 及其全部后代进程号（ToolHelp 快照，只读）。
/// 删除运行中服务后用来逐个核对整棵树都退出了（无孤儿）。
fn process_tree_pids(root: u32) -> Vec<u32> {
    use std::collections::{HashMap, HashSet};
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut tree: HashSet<u32> = HashSet::new();
    tree.insert(root);
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return tree.into_iter().collect();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut ok = Process32FirstW(snapshot, &mut entry);
        while ok != 0 {
            children
                .entry(entry.th32ParentProcessID)
                .or_default()
                .push(entry.th32ProcessID);
            ok = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);

        let mut pending: Vec<u32> = vec![root];
        while let Some(pid) = pending.pop() {
            if let Some(kids) = children.get(&pid) {
                for &kid in kids {
                    if tree.insert(kid) {
                        pending.push(kid);
                    }
                }
            }
        }
    }
    tree.into_iter().collect()
}

/// 用 `TerminateProcess` 杀掉一个进程（模拟「被认领的服务掉了」）。
fn terminate(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        assert!(!handle.is_null(), "OpenProcess({pid}) 失败");
        let ok = TerminateProcess(handle, 1);
        let err = if ok == 0 {
            Some(std::io::Error::last_os_error())
        } else {
            None
        };
        let _ = CloseHandle(handle);
        assert!(err.is_none(), "TerminateProcess({pid}) 失败：{err:?}");
    }
}

/// T1：保活 → 认领 → 编辑不动旧进程 → 看护复活 → 显式停止。
#[test]
fn keep_alive_claim_watch_revive_and_explicit_stop() {
    let dir = test_dir("t1");
    let port = free_port();
    let wrapper = write_listen_wrapper(&dir, port);
    let launch = Some(LaunchSpec {
        exe: wrapper.display().to_string(),
        args: String::new(),
        workdir: None,
    });
    let config = make_config(port, launch, true);

    // 1) Guardian（应用）启动服务，等到端口真的在监听
    let mut g = make_guardian(&dir, config.clone());
    g.start_service("svc-1").unwrap();
    let first_pid = wait_listener(port, Duration::from_secs(30)).expect("服务应该起来监听");
    assert!(
        g.service_pid("svc-1").is_some(),
        "应用应该握着自己拉起的子进程"
    );

    // 2) 保活：drop 整个 Guardian（模拟应用退出）→ 服务继续跑、PID 不变
    drop(g);
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(
        listen_pids(port),
        vec![first_pid],
        "应用退出后本地服务必须继续跑（保活）"
    );

    // 3) 认领：同一配置目录新建 Guardian，首次 poll 认领，不重复启动
    let mut g2 = make_guardian(&dir, config.clone());
    g2.poll();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        g2.poll();
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(
            listen_pids(port),
            vec![first_pid],
            "认领后绝不能重复启动"
        );
    }
    assert!(
        g2.service_pid("svc-1").is_none(),
        "认领的进程不是本会话自己起的"
    );
    assert_eq!(
        g2.snapshot()
            .into_iter()
            .find(|s| s.id == "svc-1")
            .unwrap()
            .status,
        "running",
        "认领后界面应该显示运行中"
    );
    assert!(
        g2.service_log_tail("svc-1", 50).contains("已认领"),
        "认领要写入日志"
    );

    // 3.5) 编辑启动方式：不能杀正在跑的旧进程、也不能把认领状态打回
    g2.save_service(ServiceInput {
        id: Some("svc-1".into()),
        name: "保活测试服务".into(),
        local_port: port,
        launch_exe: Some(wrapper.display().to_string()),
        launch_args: Some(String::new()),
        launch_workdir: Some(dir.display().to_string()),
        watch: true,
        autostart: false,
        note: None,
    })
    .unwrap();
    g2.poll();
    assert_eq!(
        listen_pids(port),
        vec![first_pid],
        "编辑启动方式绝不能动正在跑的旧进程"
    );
    assert!(
        g2.service_pid("svc-1").is_none(),
        "认领状态不能被编辑打回成“自己启动”"
    );
    assert_eq!(
        g2.snapshot()
            .into_iter()
            .find(|s| s.id == "svc-1")
            .unwrap()
            .status,
        "running"
    );

    // 4) 看护复活：杀掉被认领的进程 → watch 重新拉起 → 出现新的监听 PID
    terminate(first_pid);
    let new_pid = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            g2.poll();
            if let Some(pid) = listen_pids(port).first().copied() {
                if pid != first_pid {
                    break pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "看护应该把掉了的（被认领）服务重新拉起"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    assert_ne!(new_pid, first_pid);
    assert!(
        g2.service_pid("svc-1").is_some(),
        "复活后应该是本会话自己拉起的进程"
    );
    assert!(
        g2.service_log_tail("svc-1", 80).contains("看护"),
        "应该留下看护日志"
    );

    // 5) 显式停止：结束自家整棵树，端口释放
    let own_pid = g2.service_pid("svc-1");
    match g2.stop_service("svc-1", false, None).unwrap() {
        StopOutcome::Stopped { pid } => assert_eq!(pid, own_pid),
        other => panic!("期望 Stopped，实际 {other:?}"),
    }
    assert!(
        wait_port_clear(port, Duration::from_secs(10)),
        "显式停止后端口 {port} 必须释放（整棵树清掉）"
    );
    assert!(g2.service_pid("svc-1").is_none());
}

/// 退出停全部 A：被认领的进程 exe 能核对上 → 结束它，返回 (1, 0)。
#[test]
fn stop_all_for_exit_kills_verified_claimed_process() {
    let dir = test_dir("stop-claimed");
    let mut helper = common::spawn_listen_helper(&dir).expect("辅助监听进程没起来");
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "端口没进入 LISTENING"
    );

    // 配置的启动程序 = 测试二进制本身（和 helper 是同一个 exe）→ fresh 核对能过
    let exe = std::env::current_exe().unwrap().display().to_string();
    let launch = Some(LaunchSpec {
        exe,
        args: String::new(),
        workdir: None,
    });
    let config = make_config(helper.port, launch, false);
    let mut g = make_guardian(&dir, config);
    g.poll(); // 认领
    assert_eq!(
        g.snapshot()[0].status,
        "running",
        "应该认领已在监听的进程"
    );

    let (stopped, failed) = g.stop_all_for_exit();
    assert_eq!((stopped, failed), (1, 0), "核对通过的认领进程应该被停掉");
    assert!(
        helper.wait_exit(Duration::from_secs(10)),
        "认领进程应该真的被结束"
    );
    assert!(
        wait_port_clear(helper.port, Duration::from_secs(10)),
        "端口应该释放"
    );
}

/// 退出停全部 B：exe 核对不上 → 绝不硬杀，计入「没能停」。
#[test]
fn stop_all_for_exit_refuses_unverified_exe() {
    let dir = test_dir("stop-refuse-exe");
    let mut helper = common::spawn_listen_helper(&dir).expect("辅助监听进程没起来");
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "端口没进入 LISTENING"
    );

    let launch = Some(LaunchSpec {
        exe: r"C:\definitely\not\me.exe".into(),
        args: String::new(),
        workdir: None,
    });
    let config = make_config(helper.port, launch, false);
    let mut g = make_guardian(&dir, config);
    g.poll(); // 认领

    let (stopped, failed) = g.stop_all_for_exit();
    assert_eq!(
        (stopped, failed),
        (0, 1),
        "核对不过的必须计入没能停"
    );
    assert!(helper.is_running(), "核对不过的外部进程绝不能被碰");
}

/// 退出停全部 C：只监控（没配启动方式）→ 没有归属可核对，绝不硬杀。
#[test]
fn stop_all_for_exit_refuses_monitor_only_service() {
    let dir = test_dir("stop-monitor");
    let mut helper = common::spawn_listen_helper(&dir).expect("辅助监听进程没起来");
    assert!(
        common::wait_port_owner(helper.port, helper.pid(), Duration::from_secs(20)),
        "端口没进入 LISTENING"
    );

    let config = make_config(helper.port, None, false);
    let mut g = make_guardian(&dir, config);
    g.poll(); // 只监控：探测到端口在听，显示 running

    let (stopped, failed) = g.stop_all_for_exit();
    assert_eq!((stopped, failed), (0, 1), "只监控的服务没有归属可核对");
    assert!(helper.is_running(), "只监控的服务绝不能被硬杀");
}

/// 退出停全部 D：自家子进程 → 借保活作业 terminate 结束整棵树。
#[test]
fn stop_all_for_exit_stops_own_process_tree() {
    let dir = test_dir("stop-own");
    let port = free_port();
    let wrapper = write_listen_wrapper(&dir, port);
    let launch = Some(LaunchSpec {
        exe: wrapper.display().to_string(),
        args: String::new(),
        workdir: None,
    });
    let config = make_config(port, launch, true);
    let mut g = make_guardian(&dir, config);

    g.start_service("svc-1").unwrap();
    wait_listener(port, Duration::from_secs(30)).expect("服务应该起来监听");
    assert!(g.service_pid("svc-1").is_some());

    let (stopped, failed) = g.stop_all_for_exit();
    assert_eq!((stopped, failed), (1, 0), "自家服务应该被停掉");
    assert!(
        wait_port_clear(port, Duration::from_secs(10)),
        "退出停全部后端口必须释放（包装脚本的子孙进程一起清掉）"
    );
    assert!(g.service_pid("svc-1").is_none());
}

/// T2 回归（复审 verification_gap #1）：运行中的服务（`.cmd` 包装脚本 → 孙子进程监听）
/// 直接 `remove_service` 删除 → 必须同步停掉自家整棵树：端口释放、树内每个进程
/// 都退出（无孤儿）。Drop 兜底杀已移除，删除是这条路径唯一的清理点。
#[test]
fn remove_running_service_releases_port_and_kills_whole_tree() {
    let dir = test_dir("remove-running");
    let port = free_port();
    let wrapper = write_listen_wrapper(&dir, port);
    let launch = Some(LaunchSpec {
        exe: wrapper.display().to_string(),
        args: String::new(),
        workdir: None,
    });
    let config = make_config(port, launch, true);

    let mut g = make_guardian(&dir, config);
    g.start_service("svc-1").unwrap();
    let listener_pid = wait_listener(port, Duration::from_secs(30)).expect("服务应该起来监听");
    let child_pid = g.service_pid("svc-1").expect("应用应该握着直接子进程");
    assert_ne!(
        listener_pid, child_pid,
        "监听者必须是包装脚本的孙子进程，才能测出整棵树是否清干净"
    );

    // 记下删除前的整棵树（直接子进程 + 孙子进程），删除后逐个核对都退出了
    let tree = process_tree_pids(child_pid);
    assert!(
        tree.contains(&child_pid) && tree.contains(&listener_pid),
        "树快照应该同时包含直接子进程和监听孙子进程：{tree:?}"
    );

    g.remove_service("svc-1").expect("没有隧道引用，删除应该成功");

    assert!(
        wait_port_clear(port, Duration::from_secs(10)),
        "删除服务后端口 {port} 必须释放"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let alive: Vec<u32> = tree.iter().copied().filter(|pid| pid_alive(*pid)).collect();
        if alive.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "删除运行中服务后仍有进程树成员活着（孤儿）：{alive:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(g.service_pid("svc-1").is_none(), "删除后不应再留下运行时");
}
