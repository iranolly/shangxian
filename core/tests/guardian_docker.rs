//! 可选集成测试：对本地 Docker sshd 靶机做一次「开始发布 → 暂停发布」闭环。
//! 运行：cargo test -p shangxian-core --test guardian_docker -- --ignored
//! 前置：靶机容器在 127.0.0.1:2222，测试密钥在 .setup/target_key。

use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use shangxian_core::config::{AppConfig, LocalService, ServerConfig, Tunnel};
use shangxian_core::guardian::Guardian;

#[test]
#[ignore = "需要本地 Docker sshd 靶机（127.0.0.1:2222）"]
fn start_stop_roundtrip() {
    let key = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../.setup/target_key");
    assert!(key.exists(), "缺少测试密钥：{key:?}");

    let config = AppConfig {
        version: 4,
        servers: vec![ServerConfig {
            id: "t".into(),
            name: "靶机".into(),
            host: "127.0.0.1".into(),
            ssh_port: 2222,
            user: "tun".into(),
            key_path: Some(key.display().to_string()),
        }],
        services: vec![LocalService {
            id: "svc-local".into(),
            name: "测试服务".into(),
            local_port: 18084,
            launch: None,
            watch: false,
            autostart: false,
            note: None,
        }],
        tunnels: vec![Tunnel {
            id: "svc".into(),
            name: "测试服务".into(),
            service_id: "svc-local".into(),
            server_id: "t".into(),
            remote_port: 19104,
            url: None,
            key_path: None,
            access: Vec::new(),
            access_user: None,
            access_hash: None,
            access_detail: None,
            enabled: false,
        }],
        notify_enabled: true,
    };

    let dir = std::env::temp_dir().join("sx-guardian-test");
    let path = dir.join("config.json");
    let mut g = Guardian::new(config, path.clone(), dir.join("logs"));

    // 开始发布 → 等它变成"正在发布"
    g.set_enabled("svc", true).unwrap();
    let mut running = false;
    for _ in 0..40 {
        g.poll();
        if g.tunnel_snapshot()[0].status == "running" {
            running = true;
            break;
        }
        sleep(Duration::from_millis(200));
    }
    assert!(running, "8 秒内没到 running：{:?}", g.tunnel_snapshot()[0]);

    // 暂停发布 → 状态变"已暂停"
    g.set_enabled("svc", false).unwrap();
    for _ in 0..10 {
        g.poll();
        sleep(Duration::from_millis(100));
    }
    assert_eq!(g.tunnel_snapshot()[0].status, "paused");
}

/// 端到端验证「远端残留端口自动清理」：
/// 先用一个被冻结的客户端占住远端端口（服务器察觉不到），
/// 再让看护器去连同一个端口——它应该清掉残留、重连成功。
#[test]
#[ignore = "需要本地 Docker 靶机（127.0.0.1:2222）与 instrumentisto/rsync-ssh 镜像"]
fn cleanup_kills_stale_holder() {
    let key = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.setup/target_key");
    assert!(key.exists(), "缺少测试密钥：{key:?}");
    let key_arg = format!("{}:/key:ro", key.display().to_string().replace('\\', "/"));

    // 1) 制造一个"被冻住"的残留连接，占住远端 19105
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", "stale-client"])
        .output();
    let run = std::process::Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            "stale-client",
            "-v",
            &key_arg,
            "instrumentisto/rsync-ssh",
            "sh",
            "-c",
            "cp /key /tmp/key && chmod 600 /tmp/key && exec ssh -i /tmp/key -o StrictHostKeyChecking=no -o BatchMode=yes -p 2222 -N -R 127.0.0.1:19105:127.0.0.1:9 tun@host.docker.internal",
        ])
        .output()
        .expect("docker run 失败");
    assert!(
        run.status.success(),
        "docker run 失败：{}",
        String::from_utf8_lossy(&run.stderr)
    );
    sleep(Duration::from_secs(4));
    let pause = std::process::Command::new("docker")
        .args(["pause", "stale-client"])
        .output()
        .expect("docker pause 失败");
    assert!(pause.status.success());

    // 确认残留确实占住了端口（否则测试前提不成立）
    let holder = std::process::Command::new("docker")
        .args(["exec", "-u", "tun", "sshd-test", "find_port_owner", "19105"])
        .output()
        .expect("检查残留失败");
    let holder_pid = String::from_utf8_lossy(&holder.stdout).trim().to_string();
    assert!(
        holder_pid.chars().all(|c| c.is_ascii_digit()) && !holder_pid.is_empty(),
        "残留连接没占住 19105：{holder_pid:?}"
    );

    // 2) 启动我们的隧道：应经历"端口被占 → 清理 → 重连成功"
    let config = AppConfig {
        version: 4,
        servers: vec![ServerConfig {
            id: "t".into(),
            name: "靶机".into(),
            host: "127.0.0.1".into(),
            ssh_port: 2222,
            user: "tun".into(),
            key_path: Some(key.display().to_string()),
        }],
        services: vec![LocalService {
            id: "svc-local".into(),
            name: "清理测试".into(),
            local_port: 18085,
            launch: None,
            watch: false,
            autostart: false,
            note: None,
        }],
        tunnels: vec![Tunnel {
            id: "svc".into(),
            name: "清理测试".into(),
            service_id: "svc-local".into(),
            server_id: "t".into(),
            remote_port: 19105,
            url: None,
            key_path: None,
            access: Vec::new(),
            access_user: None,
            access_hash: None,
            access_detail: None,
            enabled: false,
        }],
        notify_enabled: true,
    };
    let dir = std::env::temp_dir().join("sx-cleanup-test");
    let mut g = Guardian::new(config, dir.join("config.json"), dir.join("logs"));
    g.set_enabled("svc", true).unwrap();

    // 成功标准：状态连续 5 秒保持在跑（刚启动的 ssh 还没暴露端口冲突，不能算数）
    let mut running_since: Option<std::time::Instant> = None;
    let mut running = false;
    for _ in 0..160 {
        g.poll();
        if g.tunnel_snapshot()[0].status == "running" {
            let since = running_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= Duration::from_secs(5) {
                running = true;
                break;
            }
        } else {
            running_since = None;
        }
        sleep(Duration::from_millis(250));
    }
    let log = g.log_tail("svc", 80);
    println!("--- 服务日志 ---\n{log}");
    assert!(running, "清理后仍没稳定连上：{:?}", g.tunnel_snapshot()[0]);
    assert!(log.contains("清理"), "日志里应该能看到清理记录");

    // 3) 收尾
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", "stale-client"])
        .output();
}

/// 密码登录一次 → 自动配好免密（DESIGN.md 第 10 节）。
/// 需要靶机打开密码登录（linuxserver 镜像：PASSWORD_ACCESS=true）。
/// 密码从环境变量 SSHD_TEST_PASSWORD 读（不写进代码）：
///   SSHD_TEST_PASSWORD=你的靶机密码 cargo test -p shangxian-core --test guardian_docker -- --ignored
#[test]
#[ignore]
fn keyboot_passwordless_flow() {
    let Ok(password) = std::env::var("SSHD_TEST_PASSWORD") else {
        println!("跳过：没设 SSHD_TEST_PASSWORD（本机靶机容器的密码，不进代码库）");
        return;
    };
    let dir = std::env::temp_dir().join("sx-keyboot-test");
    let _ = std::fs::remove_dir_all(&dir);
    let key_path = dir.join("id_ed25519");

    // 第一次：生成密钥 → 密码登录 → 写公钥 → 密钥登录验证
    let first = shangxian_core::keyboot::bootstrap_key(
        "127.0.0.1",
        2222,
        "tun",
        &password,
        &key_path,
    )
    .expect("第一次配免密应该成功");
    assert!(first.verified, "密钥登录验证应该通过");
    assert!(key_path.exists(), "应该生成了私钥文件");
    println!("公钥：{}", first.public_key);

    // 再跑一遍：密钥已存在，不重复写入，仍然验证通过
    let second = shangxian_core::keyboot::bootstrap_key(
        "127.0.0.1",
        2222,
        "tun",
        &password,
        &key_path,
    )
    .expect("重复配免密也要成功（幂等）");
    assert!(second.verified);

    // 密码错：要给出人话提示（不能是原始英文报错）
    let err = shangxian_core::keyboot::bootstrap_key(
        "127.0.0.1",
        2222,
        "tun",
        "WrongPass",
        &key_path,
    )
    .unwrap_err();
    println!("密码错时的提示：{err}");
    assert!(
        err.contains("密码") || err.contains("不允许"),
        "密码错时要说人话：{err}"
    );
}
