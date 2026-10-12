//! 第 0 步验证程序（spike）。
//!
//! 用途：对本地 Docker sshd 靶机跑两条真实反向隧道，验证：
//! 1. 启动两条隧道，本地假服务通过隧道可从"服务器"访问；
//! 2. 强杀其中一条的 ssh 进程（模拟断线），看护器自动按退避重连；
//! 3. 本进程被强杀后，作业对象保证不留任何孤儿 ssh.exe。
//!
//! 用法：
//!   spike <ssh_target> <ssh_port> <key_path>
//! 例：
//!   spike tun@127.0.0.1 2222 .setup/target_key

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use shangxian_core::ssh::{self, TunnelTarget};
use shangxian_core::supervisor::{Event, Tunnel, TunnelSpec};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("用法: spike <ssh_target> <ssh_port> <key_path>");
        eprintln!("例:   spike tun@127.0.0.1 2222 .setup/target_key");
        std::process::exit(2);
    }
    let ssh_target = args[1].clone();
    let ssh_port: u16 = args[2].parse().expect("ssh_port 必须是数字");
    let key = PathBuf::from(&args[3]);

    let log_dir = std::env::current_dir().unwrap().join(".setup/tunnel-logs");
    std::fs::create_dir_all(&log_dir).unwrap();

    // 两条假的本机服务（真实 HTTP 应答，方便从"服务器"里 curl 验证）。
    let hits_a = spawn_fake_service(18081, "hi from local service A");
    let hits_b = spawn_fake_service(18082, "hi from local service B");
    println!("[spike] 本机模拟服务已启动：18081(A)、18082(B)");

    let mk = |name: &str, remote: u16, local: u16| {
        let target = TunnelTarget {
            ssh_target: ssh_target.clone(),
            ssh_port,
            remote_port: remote,
            local_port: local,
            identity_file: Some(key.clone()),
        };
        TunnelSpec::new(name, ssh::default_ssh_exe(), ssh::tunnel_args(&target))
    };

    let mut t_a = Tunnel::new(
        mk("tunnel-A", 19101, 18081),
        &log_dir,
        Arc::new(shangxian_core::log::NullReporter),
    );
    let mut t_b = Tunnel::new(
        mk("tunnel-B", 19102, 18082),
        &log_dir,
        Arc::new(shangxian_core::log::NullReporter),
    );

    let start = Instant::now();
    let mut killed_a = false;
    let mut killed_b = false;
    let mut last_report = Instant::now();

    println!("[spike] 开始看护；8 秒后强制结束 A 的 ssh 进程以模拟断线，24 秒后强制结束 B。");
    loop {
        let now = Instant::now();

        for (name, events) in [("A", t_a.poll()), ("B", t_b.poll())] {
            for e in events {
                match e {
                    Event::Started { attempt, pid } => {
                        println!(
                            "[{:>6.2}s] [{name}] 隧道已启动（第 {attempt} 次，pid={pid}）",
                            (now - start).as_secs_f32()
                        );
                    }
                    Event::Exited {
                        attempt,
                        exit_code,
                        uptime,
                        retry_in,
                        reset_backoff,
                        code: _,
                    } => {
                        println!(
                            "[{:>6.2}s] [{name}] 隧道断开（第 {attempt} 次退出，code={exit_code:?}，存活 {:.1}s），{:.0}s 后重试{}",
                            (now - start).as_secs_f32(),
                            uptime.as_secs_f32(),
                            retry_in.as_secs_f32(),
                            if reset_backoff { "（退避已重置）" } else { "" }
                        );
                    }
                    Event::SpawnFailed { error, retry_in, .. } => {
                        println!(
                            "[{:>6.2}s] [{name}] 启动失败：{error}；{:.0}s 后重试",
                            (now - start).as_secs_f32(),
                            retry_in.as_secs_f32()
                        );
                    }
                    Event::SystemWake { .. } | Event::CleanupFinished { .. } => {}
                }
            }
        }

        // 模拟断线：强杀 ssh 进程（作业对象不会杀它，这是"崩溃"路径）。
        if !killed_a && now - start > Duration::from_secs(8) {
            if t_a.kill_child() {
                println!("[{:>6.2}s] [A] === 已强制结束 A 的 ssh 进程，等待自动重连 ===", (now - start).as_secs_f32());
            }
            killed_a = true;
        }
        if !killed_b && now - start > Duration::from_secs(24) {
            if t_b.kill_child() {
                println!("[{:>6.2}s] [B] === 已强制结束 B 的 ssh 进程，等待自动重连 ===", (now - start).as_secs_f32());
            }
            killed_b = true;
        }

        if now - last_report > Duration::from_secs(6) {
            println!(
                "[{:>6.2}s] 状态：A={:?} pid={:?} / B={:?} pid={:?}（本地模拟服务共被访问 A:{} 次、B:{} 次）",
                (now - start).as_secs_f32(),
                t_a.phase(),
                t_a.pid(),
                t_b.phase(),
                t_b.pid(),
                hits_a.load(Ordering::Relaxed),
                hits_b.load(Ordering::Relaxed)
            );
            last_report = now;
        }

        if now - start > Duration::from_secs(45) {
            println!("[spike] 已运行 45 秒，结束退出。");
            break;
        }
        thread::sleep(Duration::from_millis(120));
    }

    t_a.stop();
    t_b.stop();
    println!("[spike] 已停止全部隧道。作业对象关闭后 ssh 进程应全部结束。");
}

/// 起一个本地假 HTTP 服务，返回被访问次数的共享计数。
fn spawn_fake_service(port: u16, body: &'static str) -> Arc<AtomicU32> {
    let hits = Arc::new(AtomicU32::new(0));
    let hits_thread = hits.clone();
    thread::spawn(move || {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("绑定模拟服务端口失败");
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            hits_thread.fetch_add(1, Ordering::Relaxed);
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    hits
}
