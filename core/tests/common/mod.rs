//! 集成测试共用的小工具：spawn 一个「占住端口」的辅助进程。
//!
//! 两种辅助进程：
//! - [`spawn_listen_helper`]：把测试二进制自己再拉一份（不需要外部依赖）；
//! - [`spawn_ps_listener`]：拉一个 PowerShell 监听进程（验证 WOW64 / 敏感参数等场景）。
//!
//! 辅助进程绑一个随机端口，把端口号打到 stdout（`SX_PORT=...`），父测试读出来。

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 不弹控制台窗口（探针/测试里沿用同一常量）。
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 辅助进程本体：只由父测试通过 `--ignored` 拉起，默认不跑。
///
/// 可选环境变量 `SX_LISTEN_PORT`：指定要监听的端口（保活/认领测试要让服务监听
/// 配置里的固定端口）；不设置或 0 = 随机端口。端口号从 stdout 的 `SX_PORT=...` 读出。
#[test]
#[ignore = "辅助进程，由同目录其它测试拉起"]
pub fn listen_helper() {
    if std::env::var("SX_LISTEN_HELPER").is_err() {
        return;
    }
    let want_port: u16 = std::env::var("SX_LISTEN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", want_port)) else {
        return;
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    println!("SX_PORT={port}");
    let _ = std::io::stdout().flush();
    std::thread::sleep(Duration::from_secs(120));
    drop(listener);
}

pub struct HelperChild {
    pub child: Child,
    pub port: u16,
}

impl HelperChild {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for HelperChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 拉起辅助进程并等它报出端口；失败返回 None（测试自己决定是跳过还是失败）。
pub fn spawn_listen_helper(workdir: &Path) -> Option<HelperChild> {
    let exe = std::env::current_exe().ok()?;
    let mut command = Command::new(exe);
    command
        .args(["--exact", "common::listen_helper", "--ignored", "--nocapture"])
        .env("SX_LISTEN_HELPER", "1");
    spawn_and_read_port(command, workdir)
}

/// 拉起一个 PowerShell 监听进程（`body` 会拼在启动监听之后，可带敏感参数做测试）。
pub fn spawn_ps_listener(exe: &Path, workdir: &Path, body: &str) -> Option<HelperChild> {
    let extra = if body.is_empty() {
        String::new()
    } else {
        format!(";{body}")
    };
    let script = format!(
        "$l=[System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback,0);$l.Start();\
         [Console]::WriteLine('SX_PORT='+$l.LocalEndpoint.Port){extra};Start-Sleep -Seconds 90"
    );
    let mut command = Command::new(exe);
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    spawn_and_read_port(command, workdir)
}

fn spawn_and_read_port(mut command: Command, workdir: &Path) -> Option<HelperChild> {
    let mut child = command
        .current_dir(workdir)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
            if let Some(rest) = line.trim().strip_prefix("SX_PORT=") {
                let _ = tx.send(rest.to_string());
                return;
            }
        }
    });
    let port = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(text) => text.parse::<u16>().ok()?,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    if port == 0 {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    Some(HelperChild { child, port })
}

/// 等端口真的进入 LISTENING（netstat 有延迟），并确认主人是 `pid`。
pub fn wait_port_owner(port: u16, pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if shangxian_core::local_ports::listening_pids(port).contains(&pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
