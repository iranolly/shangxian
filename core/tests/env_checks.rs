//! 环境相关的小验证（不依赖 Docker）：本机端口枚举、旧脚本扫描。
//! 运行：cargo test -p shangxian-core --test env_checks -- --nocapture

#[test]
fn list_local_ports_works() {
    let ports = shangxian_core::local_ports::list_listening();
    assert!(!ports.is_empty(), "本机应该至少有一个监听端口");
    println!(
        "监听到 {} 个端口，前几个：{:?}",
        ports.len(),
        &ports[..ports.len().min(8)]
    );
}

#[test]
fn scan_imports_runs() {
    let items = shangxian_core::import::scan();
    for it in &items {
        println!("导入建议：{it:?}");
    }
    // 不硬性要求数量：脚本路径取决于用户机器
}

#[test]
fn notify_tracker_rules() {
    use shangxian_core::notify::{Notice, NotifyTracker};

    let mut t = NotifyTracker::new(60_000);
    let svc = |healthy: bool| vec![("a".to_string(), "相册".to_string(), healthy)];

    // 刚掉线不提醒；未到阈值也不提醒
    assert!(t.update(0, &svc(false)).is_empty());
    assert!(t.update(30_000, &svc(false)).is_empty());
    // 到阈值提醒一次
    assert_eq!(
        t.update(60_001, &svc(false)),
        vec![Notice::Down {
            name: "相册".into()
        }]
    );
    // 继续掉线不重复提醒
    assert!(t.update(120_000, &svc(false)).is_empty());
    // 恢复时补一条
    assert_eq!(
        t.update(130_000, &svc(true)),
        vec![Notice::Recovered {
            name: "相册".into()
        }]
    );
    // 再次掉线：重新计时
    assert!(t.update(140_000, &svc(false)).is_empty());
    assert_eq!(
        t.update(200_001, &svc(false)),
        vec![Notice::Down {
            name: "相册".into()
        }]
    );
    // 服务从列表消失（被暂停/删除）：不报恢复，状态清掉
    assert!(t.update(210_000, &[]).is_empty());
    assert!(t.update(220_000, &svc(true)).is_empty());
}

#[test]
fn nginx_snippet_generation() {
    use shangxian_core::config::{ServerConfig, Tunnel};

    let server = ServerConfig {
        id: "s".into(),
        name: "s".into(),
        host: "h".into(),
        ssh_port: 22,
        user: "u".into(),
        key_path: None,
    };
    let svc = Tunnel {
        id: "x".into(),
        name: "相册".into(),
        service_id: "svc-1".into(),
        server_id: "s".into(),
        remote_port: 18080,
        url: Some("album.example.com".into()),
        key_path: None,
        access: Some("password".into()),
        access_user: Some("shangxian".into()),
        access_hash: Some("$apr1$abc".into()),
        access_detail: None,
        enabled: true,
    };

    let snip = shangxian_core::nginx::generate(&svc, 8080, &server);
    assert!(snip.snippet.contains("server_name album.example.com;"));
    assert!(snip.snippet.contains("proxy_pass http://127.0.0.1:18080;"));
    assert!(snip.snippet.contains("auth_basic"));
    assert!(snip
        .htpasswd
        .as_deref()
        .unwrap()
        .contains("shangxian:$apr1$abc"));

    let mut open = svc.clone();
    open.access = Some("none".into());
    let snip2 = shangxian_core::nginx::generate(&open, 8080, &server);
    assert!(snip2.snippet.contains("没有访问保护"));

    let mut ip = svc.clone();
    ip.access = Some("ip".into());
    ip.access_detail = Some("1.2.3.4, 5.6.7.8".into());
    let snip3 = shangxian_core::nginx::generate(&ip, 8080, &server);
    assert!(snip3.snippet.contains("allow 1.2.3.4;"));
    assert!(snip3.snippet.contains("allow 5.6.7.8;"));
    assert!(snip3.snippet.contains("deny all;"));
}

#[test]
fn service_and_tunnel_validations() {
    use shangxian_core::config::AppConfig;
    use shangxian_core::guardian::{Guardian, NewTunnelInput, ServerInput, ServiceInput};

    let dir = std::env::temp_dir().join("sx-config-test");
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.json");
    let mut g = Guardian::new(AppConfig::default(), path.clone(), dir.join("logs"));

    let sid = g
        .save_server(ServerInput {
            id: None,
            name: "测试".into(),
            host: "example.com".into(),
            ssh_port: 22,
            user: "u".into(),
            key_path: None,
        })
        .unwrap();

    // 登记两台本机服务
    let svc_a = g
        .save_service(ServiceInput {
            id: None,
            name: "服务A".into(),
            local_port: 8080,
            launch_exe: None,
            launch_args: None,
            launch_workdir: None,
            watch: false,
            autostart: false,
            note: None,
        })
        .unwrap();
    assert_eq!(g.snapshot().len(), 1);

    // 本机端口重复 → 拒绝
    assert!(
        g.save_service(ServiceInput {
            id: None,
            name: "服务B".into(),
            local_port: 8080,
            launch_exe: None,
            launch_args: None,
            launch_workdir: None,
            watch: false,
            autostart: false,
            note: None,
        })
        .is_err(),
        "本机端口重复应该被拒绝"
    );

    // 发布一条隧道
    let tun = g
        .add_tunnel(NewTunnelInput {
            name: "服务A 发布".into(),
            service_id: svc_a.clone(),
            server_id: sid.clone(),
            remote_port: 18080,
            url: None,
            access: "none".into(),
            access_user: None,
            access_hash: None,
            access_detail: None,
        })
        .unwrap();
    assert_eq!(g.tunnel_snapshot().len(), 1);
    // 服务卡片上能看到"已发布到 1 条隧道"
    assert_eq!(g.snapshot()[0].tunnel_count, 1);

    // 同一台服务器上云端端口重复 → 拒绝
    assert!(
        g.add_tunnel(NewTunnelInput {
            name: "重复".into(),
            service_id: svc_a.clone(),
            server_id: sid.clone(),
            remote_port: 18080,
            url: None,
            access: "none".into(),
            access_user: None,
            access_hash: None,
            access_detail: None,
        })
        .is_err(),
        "重复云端端口应该被拒绝"
    );

    // 还有隧道引用时删服务 → 拦住（不连带删）
    let err = g.remove_service(&svc_a).unwrap_err();
    assert!(err.contains("隧道"), "错误信息要说清是隧道在用：{err}");
    assert_eq!(g.snapshot().len(), 1);

    // 还有隧道挂着时删服务器 → 拒绝
    assert!(g.delete_server(&sid).is_err(), "有隧道引用时不能删服务器");

    // 删掉隧道后，服务和服务器都能删
    g.remove_tunnel(&tun).unwrap();
    g.remove_service(&svc_a).unwrap();
    assert!(g.snapshot().is_empty());
    g.delete_server(&sid).unwrap();
    assert!(g.servers().is_empty());
}

#[test]
fn local_service_start_stop_and_watch() {
    use std::process::Command;
    use std::thread::sleep;
    use std::time::Duration;

    use shangxian_core::config::AppConfig;
    use shangxian_core::guardian::{Guardian, ServiceInput};

    // 找一个 python 当"本机服务"（起个只监听本机的静态站点）
    let python = ["python", "python3"]
        .into_iter()
        .find(|p| Command::new(p).arg("--version").output().is_ok());
    let Some(python) = python else {
        eprintln!("这台机器没有 python，跳过本机服务测试");
        return;
    };

    let dir = std::env::temp_dir().join("sx-localsvc-test");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut g = Guardian::new(AppConfig::default(), dir.join("config.json"), dir.join("logs"));

    let port = 18091;
    let id = g
        .save_service(ServiceInput {
            id: None,
            name: "本机站点".into(),
            local_port: port,
            launch_exe: Some(python.to_string()),
            launch_args: Some(format!("-m http.server {port} --bind 127.0.0.1")),
            launch_workdir: Some(dir.display().to_string()),
            watch: true,
            autostart: false,
            note: None,
        })
        .unwrap();

    // 启动 → 端口探测通过才算"在跑"
    g.start_service(&id).unwrap();
    let mut running = false;
    for _ in 0..60 {
        g.poll();
        if g.snapshot()[0].status == "running" {
            running = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(running, "服务应该起来并且端口通：{:?}", g.snapshot()[0]);
    let pid1 = g.service_pid(&id).expect("应该有进程号");

    // 看护：从外面把进程杀掉（不是用户点停止）→ 应该自动拉起来
    let _ = Command::new("taskkill")
        .args(["/PID", &pid1.to_string(), "/F"])
        .output();
    let mut restarted = false;
    for _ in 0..80 {
        g.poll();
        if let Some(pid2) = g.service_pid(&id) {
            if pid2 != pid1 && g.snapshot()[0].status == "running" {
                restarted = true;
                break;
            }
        }
        sleep(Duration::from_millis(250));
    }
    assert!(restarted, "进程被杀后应该被看护自动拉起来：{:?}", g.snapshot()[0]);

    // 用户点停止 → 停掉且不再自动拉起
    g.stop_service(&id).unwrap();
    for _ in 0..10 {
        g.poll();
        sleep(Duration::from_millis(100));
    }
    assert_eq!(g.snapshot()[0].status, "stopped");
    assert!(g.service_pid(&id).is_none(), "用户停了就不该有进程");

    // 服务日志里能看到看护记录
    let log = g.service_log_tail(&id, 100);
    assert!(log.contains("看护"), "服务日志里应该有看护记录：{log}");
}

#[test]
fn monitor_only_service_reports_port_state() {
    use std::net::TcpListener;
    use std::thread::sleep;
    use std::time::Duration;

    use shangxian_core::config::AppConfig;
    use shangxian_core::guardian::{Guardian, ServiceInput};

    let dir = std::env::temp_dir().join("sx-monitor-test");
    let _ = std::fs::remove_dir_all(&dir);
    let mut g = Guardian::new(AppConfig::default(), dir.join("config.json"), dir.join("logs"));

    // 占一个端口当"用户自己开的服务"（没配启动方式，只监控）
    let listener = TcpListener::bind("127.0.0.1:18093").unwrap();
    let id = g
        .save_service(ServiceInput {
            id: None,
            name: "手动开的服务".into(),
            local_port: 18093,
            launch_exe: None,
            launch_args: None,
            launch_workdir: None,
            watch: false,
            autostart: false,
            note: None,
        })
        .unwrap();

    // 端口有人监听 → 状态应该是"在跑"
    let mut running = false;
    for _ in 0..20 {
        g.poll();
        if g.snapshot()[0].status == "running" {
            running = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(running, "只监控的服务，端口通就该显示在跑：{:?}", g.snapshot()[0]);

    // 没配启动方式 → 不让点启动（要给一句人话）
    let err = g.start_service(&id).unwrap_err();
    assert!(err.contains("启动方式"), "错误提示要说清没配启动方式：{err}");

    // 端口关掉 → 状态回到"没启动"
    drop(listener);
    let mut stopped = false;
    for _ in 0..20 {
        g.poll();
        if g.snapshot()[0].status == "stopped" {
            stopped = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(stopped, "端口关了就该显示没启动：{:?}", g.snapshot()[0]);
}
