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
    use std::time::{Duration, Instant};

    use shangxian_core::notify::{HealthItem, HealthKind, Notice, NotifyTracker};

    // 新接口：NotifyTracker::new(down_after, renotify_after) + update(Instant, &[HealthItem])
    let mut t = NotifyTracker::new(Duration::from_secs(60), Duration::from_secs(600));
    let t0 = Instant::now();
    let svc = |healthy: bool| {
        vec![HealthItem {
            kind: HealthKind::Service,
            id: "a".to_string(),
            name: "相册".to_string(),
            healthy,
            should_notify: true,
        }]
    };
    let is_down = |notices: &[Notice]| {
        notices
            .iter()
            .any(|n| matches!(n, Notice::Down { name, .. } if name == "相册"))
    };
    let is_recovered = |notices: &[Notice]| {
        notices
            .iter()
            .any(|n| matches!(n, Notice::Recovered { name, .. } if name == "相册"))
    };

    // 刚掉线不提醒；未到阈值也不提醒
    assert!(t.update(t0, &svc(false)).is_empty());
    assert!(t
        .update(t0 + Duration::from_secs(30), &svc(false))
        .is_empty());
    // 到阈值提醒一次
    assert!(is_down(&t.update(t0 + Duration::from_secs(61), &svc(false))));
    // 继续掉线不重复提醒（再次提醒间隔 10 分钟）
    assert!(t
        .update(t0 + Duration::from_secs(120), &svc(false))
        .is_empty());
    // 恢复时补一条
    assert!(is_recovered(&t.update(t0 + Duration::from_secs(130), &svc(true))));
    // 再次掉线：重新计时
    assert!(t
        .update(t0 + Duration::from_secs(140), &svc(false))
        .is_empty());
    assert!(is_down(&t.update(t0 + Duration::from_secs(201), &svc(false))));
    // 服务从列表消失（被暂停/删除）：不报恢复，状态清掉
    assert!(t.update(t0 + Duration::from_secs(210), &[]).is_empty());
    assert!(t.update(t0 + Duration::from_secs(220), &svc(true)).is_empty());
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
        vhost_dir: None,
    };
    let svc = Tunnel {
        id: "x".into(),
        name: "相册".into(),
        service_id: "svc-1".into(),
        server_id: "s".into(),
        remote_port: 18080,
        url: Some("album.example.com".into()),
        key_path: None,
        access: vec!["password".into()],
        access_user: Some("shangxian".into()),
        access_hash: Some("$2y$12$abc".into()),
        access_detail: None,
        enabled: true,
        preset: "web".into(),
    };

    let snip = shangxian_core::nginx::generate(&svc, 8080, &server);
    // 只给「要新增的几行」，不是整段配置
    let add = snip.snippet.as_deref().unwrap();
    assert!(add.contains("auth_basic \"需要密码\";"));
    assert!(add.contains("auth_basic_user_file /xp/panel/vhost/album.example.com.htpasswd;"));
    // D0 第 13 条：续期豁免升级为多行（注释 + 要加的行）
    let acme = snip.acme_exempt.as_deref().unwrap();
    assert!(acme.contains("auth_basic off;"), "acme={acme}");
    assert!(acme.contains("# 证书续期专用"), "acme={acme}");
    assert!(!acme.contains("allow all;"), "只开密码时不该有 allow all：{acme}");
    // 简短示意里要看得到域名和新增位置
    assert!(
        snip.preview
            .iter()
            .any(|l| !l.is_new && l.text.contains("server_name album.example.com;"))
    );
    assert!(
        snip.preview
            .iter()
            .any(|l| l.is_new && l.text.contains("auth_basic_user_file"))
    );
    assert!(
        snip.preview
            .iter()
            .any(|l| l.is_new && l.text.contains("auth_basic off;"))
    );
    assert!(snip
        .htpasswd
        .as_deref()
        .unwrap()
        .contains("shangxian:$2y$12$abc"));

    let mut open = svc.clone();
    open.access = Vec::new();
    let snip2 = shangxian_core::nginx::generate(&open, 8080, &server);
    assert!(snip2.snippet.is_none());
    assert!(snip2.acme_exempt.is_none());
    assert!(
        snip2.preview
            .iter()
            .any(|l| l.is_new && l.text.contains("没有访问保护"))
    );

    let mut ip = svc.clone();
    ip.access = vec!["ip".into()];
    ip.access_detail = Some("1.2.3.4, 5.6.7.8".into());
    let snip3 = shangxian_core::nginx::generate(&ip, 8080, &server);
    let add3 = snip3.snippet.as_deref().unwrap();
    assert!(add3.contains("allow 1.2.3.4;"));
    assert!(add3.contains("allow 5.6.7.8;"));
    assert!(add3.contains("deny all;"));
    // D0 第 13 条：白名单模式也必须生成 .well-known 的 allow all;（不再假设面板自带）
    let acme3 = snip3.acme_exempt.as_deref().unwrap();
    assert!(acme3.contains("allow all;"), "acme3={acme3}");
    assert!(!acme3.contains("auth_basic off;"), "没开密码时不该有 auth_basic off：{acme3}");
}

#[test]
fn nginx_snippet_password_and_ip_together() {
    use shangxian_core::config::{ServerConfig, Tunnel};

    let server = ServerConfig {
        id: "s".into(),
        name: "s".into(),
        host: "h".into(),
        ssh_port: 22,
        user: "u".into(),
        key_path: None,
        vhost_dir: None,
    };
    let tun = Tunnel {
        id: "x".into(),
        name: "相册".into(),
        service_id: "svc-1".into(),
        server_id: "s".into(),
        remote_port: 18080,
        url: Some("album.example.com".into()),
        key_path: None,
        // 两道门同时开：密码 + 只允许指定 IP
        access: vec!["password".into(), "ip".into()],
        access_user: Some("shangxian".into()),
        access_hash: Some("$2y$12$abc".into()),
        access_detail: Some("1.2.3.4, 5.6.7.8".into()),
        enabled: true,
        preset: "web".into(),
    };

    let snip = shangxian_core::nginx::generate(&tun, 8080, &server);
    let add = snip.snippet.as_deref().unwrap();
    // 两块都要出现：密码块在前，IP 块在后
    assert!(add.contains("auth_basic \"需要密码\";"), "add={add}");
    assert!(add.contains("auth_basic_user_file /xp/panel/vhost/album.example.com.htpasswd;"));
    assert!(add.contains("allow 1.2.3.4;"));
    assert!(add.contains("allow 5.6.7.8;"));
    assert!(add.contains("deny all;"));
    let password_at = add.find("auth_basic").unwrap();
    let ip_at = add.find("allow 1.2.3.4;").unwrap();
    assert!(password_at < ip_at, "密码块应该在 IP 块前面：\n{add}");
    // 密码开着时，证书续期目录里要保留 auth_basic off;；两道门都开时两行都有
    let acme = snip.acme_exempt.as_deref().unwrap();
    assert!(acme.contains("auth_basic off;") && acme.contains("allow all;"), "acme={acme}");
    assert!(
        snip.preview
            .iter()
            .any(|l| l.is_new && l.text.contains("auth_basic off;")),
        "示意图里要让用户看到 .well-known 里的 auth_basic off;"
    );
    // 人话说明里要讲清「两道门都要过」
    assert!(
        snip.steps.iter().any(|s| s.contains("白名单和密码两道门都要过")),
        "steps={:?}",
        snip.steps
    );
}

#[test]
fn service_and_tunnel_validations() {
    use shangxian_core::config::AppConfig;
    use shangxian_core::guardian::{Guardian, NewTunnelInput, ServerInput, ServiceInput};

    let dir = std::env::temp_dir().join("sx-config-test");
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.json");
    let mut g = Guardian::new(
        AppConfig::default(),
        path.clone(),
        dir.join("logs"),
        std::sync::Arc::new(shangxian_core::log::NullReporter),
    );

    let sid = g
        .save_server(ServerInput {
            id: None,
            name: "测试".into(),
            host: "example.com".into(),
            ssh_port: 22,
            user: "u".into(),
            key_path: None,
            vhost_dir: None,
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
            access: Vec::new(),
            access_user: None,
            access_hash: None,
            access_detail: None,
            preset: None,
        })
        .unwrap();
    assert_eq!(g.tunnel_snapshot().len(), 1);

    // 「要密码」但没给哈希 → 报错
    let err = g
        .add_tunnel(NewTunnelInput {
            name: "缺密码".into(),
            service_id: svc_a.clone(),
            server_id: sid.clone(),
            remote_port: 18081,
            url: None,
            access: vec!["password".into()],
            access_user: Some("u".into()),
            access_hash: None,
            access_detail: None,
            preset: None,
        })
        .unwrap_err();
    assert!(err.contains("密码"), "err={err}");

    // 「只允许指定 IP」但没填 IP → 报错
    let err = g
        .add_tunnel(NewTunnelInput {
            name: "缺 IP".into(),
            service_id: svc_a.clone(),
            server_id: sid.clone(),
            remote_port: 18082,
            url: None,
            access: vec!["ip".into()],
            access_user: None,
            access_hash: None,
            access_detail: None,
            preset: None,
        })
        .unwrap_err();
    assert!(err.contains("IP"), "err={err}");
    assert_eq!(g.tunnel_snapshot().len(), 1, "没写成的隧道不应该落盘");
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
            access: Vec::new(),
            access_user: None,
            access_hash: None,
            access_detail: None,
            preset: None,
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
    let mut g = Guardian::new(
        AppConfig::default(),
        dir.join("config.json"),
        dir.join("logs"),
        std::sync::Arc::new(shangxian_core::log::NullReporter),
    );

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
    g.stop_service(&id, false, None).unwrap();
    // 探测有防抖（3 次失败约 6 秒）：等它彻底静下来再断言
    for _ in 0..40 {
        g.poll();
        if g.snapshot()[0].status == "stopped" {
            break;
        }
        sleep(Duration::from_millis(200));
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
    let mut g = Guardian::new(
        AppConfig::default(),
        dir.join("config.json"),
        dir.join("logs"),
        std::sync::Arc::new(shangxian_core::log::NullReporter),
    );

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
    // （探测有防抖：连续 3 次失败才判 down，2 秒一轮 → 最多约 6 秒）
    drop(listener);
    let mut stopped = false;
    for _ in 0..60 {
        g.poll();
        if g.snapshot()[0].status == "stopped" {
            stopped = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(stopped, "端口关了就该显示没启动：{:?}", g.snapshot()[0]);
}

/// P3-3：update_tunnel 的 IP 校验/写回要用「合并后的 detail」
/// （新值优先，没传就用旧的），别把旧的 IP 列表误清空或误报错。
#[test]
fn update_tunnel_merges_access_detail() {
    use shangxian_core::config::AppConfig;
    use shangxian_core::guardian::{
        Guardian, NewTunnelInput, ServerInput, ServiceInput, UpdateTunnelInput,
    };

    let dir = std::env::temp_dir().join("sx-update-tunnel-test");
    let _ = std::fs::remove_dir_all(&dir);
    let mut g = Guardian::new(
        AppConfig::default(),
        dir.join("config.json"),
        dir.join("logs"),
        std::sync::Arc::new(shangxian_core::log::NullReporter),
    );

    let sid = g
        .save_server(ServerInput {
            id: None,
            name: "测试".into(),
            host: "example.com".into(),
            ssh_port: 22,
            user: "u".into(),
            key_path: None,
            vhost_dir: None,
        })
        .unwrap();
    let svc = g
        .save_service(ServiceInput {
            id: None,
            name: "服务A".into(),
            local_port: 18094,
            launch_exe: None,
            launch_args: None,
            launch_workdir: None,
            watch: false,
            autostart: false,
            note: None,
        })
        .unwrap();

    let tun = g
        .add_tunnel(NewTunnelInput {
            name: "相册".into(),
            service_id: svc,
            server_id: sid,
            remote_port: 18094,
            url: Some("album.example.com".into()),
            access: vec!["password".into(), "ip".into()],
            access_user: Some("u".into()),
            access_hash: Some("$2y$12$old".into()),
            access_detail: Some("1.2.3.4".into()),
            preset: None,
        })
        .unwrap();

    let update = |g: &mut Guardian,
                  access: Option<Vec<String>>,
                  hash: Option<String>,
                  detail: Option<String>|
     -> Result<(), String> {
        g.update_tunnel(UpdateTunnelInput {
            id: tun.clone(),
            name: "相册".into(),
            remote_port: 18094,
            url: Some("album.example.com".into()),
            access,
            access_user: Some("u".into()),
            access_hash: hash,
            access_detail: detail,
            preset: None,
        })
    };

    // 1) access=None：保持原模式，detail 和旧哈希都不能丢
    update(&mut g, None, None, None).unwrap();
    let view = g.tunnel_snapshot().remove(0);
    assert_eq!(view.access, vec!["password", "ip"], "access 没传要保持原模式");
    assert_eq!(view.access_detail.as_deref(), Some("1.2.3.4"), "没传 detail 要合并旧值");
    assert!(view.has_password, "没传新哈希要保留旧哈希");

    // 2) Some([])：取消保护，不报错，旧的 IP 列表一起清掉；hash 和 user 也一并清空
    update(&mut g, Some(Vec::new()), None, None).unwrap();
    let view = g.tunnel_snapshot().remove(0);
    assert!(view.access.is_empty(), "空数组应该清掉保护");
    assert!(!view.has_password, "关掉密码保护要清掉旧哈希，不能让旧密码复活");
    assert_eq!(view.access_user, None, "关掉密码保护时 user 也要清掉");
    assert_eq!(view.access_detail, None, "不要 IP 保护了就该清掉 detail");

    // 3) ip 模式但合并后 detail 为空 → 中文人话报错（不能静默写入）
    let err = update(&mut g, Some(vec!["ip".into()]), None, None).unwrap_err();
    assert!(err.contains("IP"), "err={err}");
    assert!(err.contains("只允许指定 IP"), "err={err}");
    assert_eq!(g.tunnel_snapshot()[0].access, Vec::<String>::new(), "报错后不能改配置");

    // 4) ip 模式：新 detail 优先；不传就用旧的
    update(&mut g, Some(vec!["ip".into()]), None, Some("5.6.7.8".into())).unwrap();
    assert_eq!(g.tunnel_snapshot()[0].access_detail.as_deref(), Some("5.6.7.8"));
    update(&mut g, Some(vec!["ip".into()]), None, None).unwrap();
    assert_eq!(
        g.tunnel_snapshot()[0].access_detail.as_deref(),
        Some("5.6.7.8"),
        "ip 模式不传 detail 要合并保留旧值"
    );

    // 5) 关过密码后旧哈希已被清掉：重开密码不传新哈希 → 报错，配置不变（旧密码不能复活）
    let err = update(&mut g, Some(vec!["password".into()]), None, None).unwrap_err();
    assert!(err.contains("访问密码"), "err={err}");
    let view = g.tunnel_snapshot().remove(0);
    assert_eq!(view.access, vec!["ip".to_string()], "报错后不能改配置");
    assert!(!view.has_password, "旧哈希已经清掉，重开时必须给新哈希");

    // 6) 重开密码：显式带新哈希才通过（user 显式给则覆盖）
    update(
        &mut g,
        Some(vec!["password".into()]),
        Some("$2y$12$new".into()),
        None,
    )
    .unwrap();
    let view = g.tunnel_snapshot().remove(0);
    assert_eq!(view.access, vec!["password".to_string()]);
    assert!(view.has_password, "显式给了新哈希要写进去");
    assert_eq!(view.access_user.as_deref(), Some("u"), "显式给 user 要覆盖");
    assert_eq!(view.access_detail, None, "不想要 IP 时 detail 清掉");
}

/// 关闭密码保护要清空 hash + user；重开必须给新哈希；
/// 只改名称/端口（access 与 accessUser 都不传）时原 hash 与 user 保持不变。
#[test]
fn update_tunnel_password_toggle_semantics() {
    use shangxian_core::config::AppConfig;
    use shangxian_core::guardian::{
        Guardian, NewTunnelInput, ServerInput, ServiceInput, UpdateTunnelInput,
    };

    let dir = std::env::temp_dir().join("sx-update-tunnel-password-test");
    let _ = std::fs::remove_dir_all(&dir);
    let mut g = Guardian::new(
        AppConfig::default(),
        dir.join("config.json"),
        dir.join("logs"),
        std::sync::Arc::new(shangxian_core::log::NullReporter),
    );

    let sid = g
        .save_server(ServerInput {
            id: None,
            name: "测试".into(),
            host: "example.com".into(),
            ssh_port: 22,
            user: "u".into(),
            key_path: None,
            vhost_dir: None,
        })
        .unwrap();
    let svc = g
        .save_service(ServiceInput {
            id: None,
            name: "服务B".into(),
            local_port: 18096,
            launch_exe: None,
            launch_args: None,
            launch_workdir: None,
            watch: false,
            autostart: false,
            note: None,
        })
        .unwrap();

    let tun = g
        .add_tunnel(NewTunnelInput {
            name: "相册".into(),
            service_id: svc,
            server_id: sid,
            remote_port: 18096,
            url: None,
            access: vec!["password".into()],
            access_user: Some("u".into()),
            access_hash: Some("$2y$12$old".into()),
            access_detail: None,
            preset: None,
        })
        .unwrap();

    let update = |g: &mut Guardian,
                  name: &str,
                  port: u16,
                  access: Option<Vec<String>>,
                  user: Option<String>,
                  hash: Option<String>|
     -> Result<(), String> {
        g.update_tunnel(UpdateTunnelInput {
            id: tun.clone(),
            name: name.into(),
            remote_port: port,
            url: None,
            access,
            access_user: user,
            access_hash: hash,
            access_detail: None,
            preset: None,
        })
    };

    // 1) 只改名称/端口：access 与 accessUser 都不传 → 原密码模式、hash、user 全部保持
    update(&mut g, "改过名", 18097, None, None, None).unwrap();
    let view = g.tunnel_snapshot().remove(0);
    assert_eq!(view.name, "改过名");
    assert_eq!(view.remote_port, 18097);
    assert_eq!(view.access, vec!["password".to_string()], "没传 access 要保持原模式");
    assert!(view.has_password, "只改名称/端口不能丢旧哈希");
    assert_eq!(view.access_user.as_deref(), Some("u"), "没传 accessUser 要保持旧 user");

    // 2) 关闭密码保护：不传新哈希也要把旧 hash 和 user 清空
    update(&mut g, "改过名", 18097, Some(Vec::new()), None, None).unwrap();
    let view = g.tunnel_snapshot().remove(0);
    assert!(view.access.is_empty(), "空数组应该关掉保护");
    assert!(!view.has_password, "关掉密码后 has_password 必须为 false");
    assert_eq!(view.access_user, None, "关掉密码后 user 必须清空");

    // 3) 重新开启密码但没给新哈希 → 报错（旧哈希已清掉，不能复活）
    let err = update(
        &mut g,
        "改过名",
        18097,
        Some(vec!["password".into()]),
        None,
        None,
    )
    .unwrap_err();
    assert!(err.contains("访问密码"), "err={err}");
    let view = g.tunnel_snapshot().remove(0);
    assert!(view.access.is_empty(), "报错后 access 不能变");
    assert!(!view.has_password, "报错后不能凭空冒出旧哈希");
    assert_eq!(view.access_user, None, "报错后 user 仍为空");

    // 4) 重新开启密码 + 新哈希：通过；user 不传则保持（当前为空）
    update(
        &mut g,
        "改过名",
        18097,
        Some(vec!["password".into()]),
        None,
        Some("$2y$12$new1".into()),
    )
    .unwrap();
    let view = g.tunnel_snapshot().remove(0);
    assert!(view.has_password, "显式给了新哈希要写进去");
    assert_eq!(view.access_user, None, "没传 accessUser 要保持现值（空）");

    // 5) 密码模式不传新哈希 → 保留当前哈希；显式给 user → 覆盖
    update(
        &mut g,
        "改过名",
        18097,
        Some(vec!["password".into()]),
        Some("u2".into()),
        None,
    )
    .unwrap();
    let view = g.tunnel_snapshot().remove(0);
    assert!(view.has_password, "没传新哈希要保持当前哈希");
    assert_eq!(view.access_user.as_deref(), Some("u2"), "显式给 accessUser 要覆盖");
}
