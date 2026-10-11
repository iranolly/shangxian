//! 上线助手命令行（cli.exe）——给 AI agent 和别的程序用的遥控器。
//!
//! 定位：应用（在托盘里跑着）才是真正干活的那个；这个命令行负责查状态、改配置、
//! 起停服务与隧道。默认输出中文人话；加 `--json` 输出机器能直接吃的 JSON。
//!
//! 应用没在跑时：只读的几个命令（status/services/tunnels/servers）会退回到直接读
//! 配置文件，状态字段标成 unknown；要改东西就先 `cli app start`。

use std::path::PathBuf;
use std::process::Command;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use shangxian_core::config::{default_data_dir, AppConfig};
use shangxian_core::ctl;

#[derive(Parser)]
#[command(
    name = "cli",
    version,
    about = "上线助手命令行：查询与配置（默认人话输出，--json 给机器看）",
    after_help = "给 AI agent 的速查：cli guide\n全部底层方法：cli methods"
)]
struct Cli {
    /// 输出 JSON（AI agent / 脚本用这个）
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 概览：应用 + 本机服务 + 发布隧道 + 云端服务器
    Status,
    /// 本机服务列表
    Services,
    /// 发布隧道列表
    Tunnels,
    /// 云端服务器列表
    Servers,
    /// 本机正在监听的端口（带进程名和程序路径）
    Ports,
    /// 读日志：隧道日志或本机服务日志
    Logs {
        id: String,
        /// tunnel（隧道日志）或 service（服务日志）
        #[arg(long, default_value = "tunnel")]
        kind: String,
        #[arg(long, default_value_t = 60)]
        lines: usize,
    },
    /// 生成 nginx 配置片段（人工贴到面板上）
    Nginx { id: String },
    /// 本机服务：新增/编辑/启停/删除
    Service {
        #[command(subcommand)]
        sub: ServiceCmd,
    },
    /// 发布隧道：新增/编辑/启停/删除
    Tunnel {
        #[command(subcommand)]
        sub: TunnelCmd,
    },
    /// 云端服务器：新增/编辑/删除/配免密
    Server {
        #[command(subcommand)]
        sub: ServerCmd,
    },
    /// 应用自己：启动、看状态
    App {
        #[command(subcommand)]
        sub: AppCmd,
    },
    /// 列出所有可调用的底层方法（给 agent 看）
    Methods,
    /// 直接调用底层方法（万能口）：cli call <方法> '{"参数":...}'
    Call {
        method: String,
        #[arg(default_value = "{}")]
        params: String,
    },
    /// 给 AI agent 的速查说明
    Guide,
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// 新增：cli service add --name X --port 8188 [--exe 程序] [--args 参数] [--workdir 目录]
    Add {
        #[arg(long)]
        name: String,
        /// 本机端口
        #[arg(long)]
        port: u16,
        /// 启动程序（不填 = 只监控，不帮你启动）
        #[arg(long)]
        exe: Option<String>,
        #[arg(long, default_value = "")]
        args: String,
        #[arg(long)]
        workdir: Option<String>,
        /// 掉线自动拉起
        #[arg(long)]
        watch: bool,
        /// 开机自动启动
        #[arg(long)]
        autostart: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// 改：只写你要改的字段，其余保持不变
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        exe: Option<String>,
        #[arg(long)]
        args: Option<String>,
        #[arg(long)]
        workdir: Option<String>,
        #[arg(long)]
        note: Option<String>,
        /// --watch true / --watch false
        #[arg(long)]
        watch: Option<bool>,
        /// --autostart true / --autostart false
        #[arg(long)]
        autostart: Option<bool>,
    },
    /// 启动
    Start { id: String },
    /// 停止（只停应用自己启动的进程）
    Stop { id: String },
    /// 重启（先停再启）
    Restart { id: String },
    /// 删除（还有隧道在用会拒绝）
    Remove { id: String },
}

#[derive(Subcommand)]
enum TunnelCmd {
    /// 新增：cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000
    Add {
        #[arg(long)]
        service: String,
        #[arg(long)]
        server: String,
        /// 云端端口（写死，不会自动漂）
        #[arg(long = "remote-port")]
        remote_port: u16,
        /// 隧道名字（不填 = 用「服务名 发布」）
        #[arg(long)]
        name: Option<String>,
        /// 公网网址（可空）
        #[arg(long)]
        url: Option<String>,
        /// none / password / ip
        #[arg(long, default_value = "none")]
        access: String,
        #[arg(long = "access-user")]
        access_user: Option<String>,
        /// 访问密码（会先让服务器算出哈希再存，密码本身不落盘）
        #[arg(long = "access-password")]
        access_password: Option<String>,
        /// 只允许这些 IP（access=ip 时用，例 "1.2.3.4,5.6.7.8"）
        #[arg(long = "access-ips")]
        access_ips: Option<String>,
    },
    /// 改：只写你要改的字段，其余保持不变
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "remote-port")]
        remote_port: Option<u16>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        access: Option<String>,
        #[arg(long = "access-user")]
        access_user: Option<String>,
        #[arg(long = "access-password")]
        access_password: Option<String>,
        #[arg(long = "access-ips")]
        access_ips: Option<String>,
    },
    /// 开始发布
    Start { id: String },
    /// 暂停发布
    Stop { id: String },
    /// 立刻重连
    Restart { id: String },
    /// 删除
    Remove { id: String },
}

#[derive(Subcommand)]
enum ServerCmd {
    /// 新增：cli server add --name 香港-1 --host api.example.com --user user
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        host: String,
        #[arg(long)]
        user: String,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// 密钥文件（不填 = 用系统默认密钥）
        #[arg(long)]
        key: Option<String>,
    },
    /// 改：只写你要改的字段
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        key: Option<String>,
    },
    /// 删除（有隧道在用会拒绝）
    Remove { id: String },
    /// 用密码登录一次，自动配好免密（密码从标准输入读，不进命令行历史）
    SetupKey { id: String },
    /// 生成访问密码的哈希（apr1，给 nginx 用；密码从标准输入读）
    HashPassword { id: String },
}

#[derive(Subcommand)]
enum AppCmd {
    /// 把应用叫起来（在托盘里静默运行）
    Start,
    /// 应用在不在跑
    Status,
}

fn main() {
    let cli = Cli::parse();
    let want_json = cli.json;
    match run(&cli) {
        Ok(()) => {}
        Err(e) => {
            if want_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"ok": false, "error": e})).unwrap()
                );
            } else {
                eprintln!("✗ {e}");
            }
            std::process::exit(1);
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    match &cli.cmd {
        Cmd::Status => {
            let snap = snapshot();
            if cli.json {
                print_json(&snap);
            } else {
                print_status(&snap);
            }
        }
        Cmd::Services => {
            let snap = snapshot();
            if cli.json {
                print_json(&snap["services"]);
            } else {
                print_services(&snap);
            }
        }
        Cmd::Tunnels => {
            let snap = snapshot();
            if cli.json {
                print_json(&snap["tunnels"]);
            } else {
                print_tunnels(&snap);
            }
        }
        Cmd::Servers => {
            let snap = snapshot();
            if cli.json {
                print_json(&snap["servers"]);
            } else {
                print_servers(&snap);
            }
        }
        Cmd::Ports => {
            let v = call("list_local_ports", json!({}))?;
            if cli.json {
                print_json(&v);
            } else {
                print_ports(&v);
            }
        }
        Cmd::Logs { id, kind, lines } => {
            // 先确认这个 id 存在，免得脚本把「空输出」当成「一切正常」
            if kind == "service" {
                find_service(id)?;
            } else {
                find_tunnel(id)?;
            }
            let method = if kind == "service" {
                "get_service_logs"
            } else {
                "get_logs"
            };
            let v = call(method, json!({"id": id, "lines": lines}))?;
            if cli.json {
                print_json(&v);
            } else {
                println!("{}", v["text"].as_str().unwrap_or(""));
            }
        }
        Cmd::Nginx { id } => {
            let v = call("get_nginx_snippet", json!({"id": id}))?;
            if cli.json {
                print_json(&v);
            } else {
                println!("{}", v["snippet"].as_str().unwrap_or(""));
                if let Some(h) = v["htpasswd"].as_str() {
                    println!("\n# 密码文件内容（存到站点根目录，权限设成只有服务器能读）：\n{h}");
                }
                for step in v["steps"].as_array().cloned().unwrap_or_default() {
                    println!("\n· {}", step.as_str().unwrap_or(""));
                }
            }
        }
        Cmd::Service { sub } => service_cmd(sub, cli.json)?,
        Cmd::Tunnel { sub } => tunnel_cmd(sub, cli.json)?,
        Cmd::Server { sub } => server_cmd(sub, cli.json)?,
        Cmd::App { sub } => app_cmd(sub, cli.json)?,
        Cmd::Methods => {
            let v = call("methods", json!({}))?;
            if cli.json {
                print_json(&v);
            } else {
                println!("能调的方法（cli call <方法> '<参数JSON>'）：\n");
                for m in v.as_array().cloned().unwrap_or_default() {
                    println!(
                        "  {:<22} {}\n  {:<22} 参数：{}\n",
                        m["method"].as_str().unwrap_or(""),
                        m["desc"].as_str().unwrap_or(""),
                        "",
                        m["params"].as_str().unwrap_or("")
                    );
                }
            }
        }
        Cmd::Call { method, params } => {
            let params: Value = serde_json::from_str(params)
                .map_err(|e| format!("参数不是合法 JSON：{e}"))?;
            let v = call(method, params)?;
            print_json(&v);
        }
        Cmd::Guide => {
            println!("{GUIDE}");
        }
    }
    Ok(())
}

// ---------- 各命令实现 ----------

fn service_cmd(sub: &ServiceCmd, want_json: bool) -> Result<(), String> {
    match sub {
        ServiceCmd::Add {
            name,
            port,
            exe,
            args,
            workdir,
            watch,
            autostart,
            note,
        } => {
            let v = call(
                "save_service",
                json!({
                    "id": null,
                    "name": name,
                    "localPort": port,
                    "launchExe": exe,
                    "launchArgs": args,
                    "launchWorkdir": workdir,
                    "watch": watch,
                    "autostart": autostart,
                    "note": note,
                }),
            )?;
            done(want_json, &v, &format!("✓ 已登记本机服务「{name}」（id={}）", id_of(&v)));
        }
        ServiceCmd::Edit {
            id,
            name,
            port,
            exe,
            args,
            workdir,
            note,
            watch,
            autostart,
        } => {
            let old = find_service(id)?;
            let v = call(
                "save_service",
                json!({
                    "id": id,
                    "name": name.clone().unwrap_or_else(|| str_of(&old, "name")),
                    "localPort": port.unwrap_or_else(|| num_of(&old, "localPort") as u16),
                    "launchExe": exe.clone().or_else(|| opt_str(&old, "launchExe")),
                    "launchArgs": args.clone().or_else(|| opt_str(&old, "launchArgs")).unwrap_or_default(),
                    "launchWorkdir": workdir.clone().or_else(|| opt_str(&old, "launchWorkdir")),
                    "watch": watch.unwrap_or_else(|| bool_of(&old, "watch")),
                    "autostart": autostart.unwrap_or_else(|| bool_of(&old, "autostart")),
                    "note": note.clone().or_else(|| opt_str(&old, "note")),
                }),
            )?;
            done(want_json, &v, &format!("✓ 已改好本机服务「{id}」"));
        }
        ServiceCmd::Start { id } => {
            let v = call("service_start", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已让「{id}」启动（几秒后 cli services 看结果）"));
        }
        ServiceCmd::Stop { id } => {
            let v = call("service_stop", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已停止「{id}」"));
        }
        ServiceCmd::Restart { id } => {
            let v = call("service_restart", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已重启「{id}」"));
        }
        ServiceCmd::Remove { id } => {
            let v = call("remove_service", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已删除本机服务「{id}」"));
        }
    }
    Ok(())
}

fn tunnel_cmd(sub: &TunnelCmd, want_json: bool) -> Result<(), String> {
    match sub {
        TunnelCmd::Add {
            service,
            server,
            remote_port,
            name,
            url,
            access,
            access_user,
            access_password,
            access_ips,
        } => {
            let svc = find_service(service)?;
            let svc_name = str_of(&svc, "name");
            let name = name
                .clone()
                .unwrap_or_else(|| format!("{svc_name} 发布"));
            let hash = match access_password {
                Some(pw) => Some(hash_password(server, pw)?),
                None => None,
            };
            let v = call(
                "add_tunnel",
                json!({
                    "name": name,
                    "serviceId": service,
                    "serverId": server,
                    "remotePort": remote_port,
                    "url": url,
                    "access": access,
                    "accessUser": access_user,
                    "accessHash": hash,
                    "accessDetail": access_ips,
                }),
            )?;
            done(
                want_json,
                &v,
                &format!("✓ 已建好隧道「{name}」并开始发布（id={}）", id_of(&v)),
            );
        }
        TunnelCmd::Edit {
            id,
            name,
            remote_port,
            url,
            access,
            access_user,
            access_password,
            access_ips,
        } => {
            let old = find_tunnel(id)?;
            let access_new = access
                .clone()
                .or_else(|| opt_str(&old, "access"))
                .unwrap_or_else(|| "none".into());
            let server_id = str_of(&old, "serverId");
            let hash = match access_password {
                Some(pw) => Some(hash_password(&server_id, pw)?),
                None => None,
            };
            let v = call(
                "update_tunnel",
                json!({
                    "id": id,
                    "name": name.clone().unwrap_or_else(|| str_of(&old, "name")),
                    "remotePort": remote_port.unwrap_or_else(|| num_of(&old, "remotePort") as u16),
                    "url": url.clone().or_else(|| opt_str(&old, "url")),
                    "access": access_new,
                    "accessUser": access_user.clone().or_else(|| opt_str(&old, "accessUser")),
                    "accessHash": hash,
                    "accessDetail": access_ips.clone().or_else(|| opt_str(&old, "accessDetail")),
                }),
            )?;
            done(want_json, &v, &format!("✓ 已改好隧道「{id}」（会断一下再自动连上）"));
        }
        TunnelCmd::Start { id } => {
            let v = call("tunnel_start", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已开始发布「{id}」"));
        }
        TunnelCmd::Stop { id } => {
            let v = call("tunnel_stop", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已暂停「{id}」"));
        }
        TunnelCmd::Restart { id } => {
            let v = call("tunnel_restart", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已让「{id}」重连"));
        }
        TunnelCmd::Remove { id } => {
            let v = call("remove_tunnel", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已删除隧道「{id}」"));
        }
    }
    Ok(())
}

fn server_cmd(sub: &ServerCmd, want_json: bool) -> Result<(), String> {
    match sub {
        ServerCmd::Add {
            name,
            host,
            user,
            port,
            key,
        } => {
            let v = call(
                "save_server",
                json!({"id": null, "name": name, "host": host, "user": user, "sshPort": port, "keyPath": key}),
            )?;
            done(
                want_json,
                &v,
                &format!(
                    "✓ 已添加服务器「{name}」（id={}）\n  没配密钥的话：cli server setup-key {}",
                    id_of(&v),
                    id_of(&v)
                ),
            );
        }
        ServerCmd::Edit {
            id,
            name,
            host,
            user,
            port,
            key,
        } => {
            let old = find_server(id)?;
            let v = call(
                "save_server",
                json!({
                    "id": id,
                    "name": name.clone().unwrap_or_else(|| str_of(&old, "name")),
                    "host": host.clone().unwrap_or_else(|| str_of(&old, "host")),
                    "user": user.clone().unwrap_or_else(|| str_of(&old, "user")),
                    "sshPort": port.unwrap_or_else(|| num_of(&old, "sshPort") as u16),
                    "keyPath": key.clone().or_else(|| opt_str(&old, "keyPath")),
                }),
            )?;
            done(want_json, &v, &format!("✓ 已改好服务器「{id}」"));
        }
        ServerCmd::Remove { id } => {
            let v = call("delete_server", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已删除服务器「{id}」"));
        }
        ServerCmd::SetupKey { id } => {
            let pw = read_secret("请输入这台服务器的登录密码（不会存下来、不会进日志）：")?;
            let v = call(
                "setup_passwordless",
                json!({"serverId": id, "password": pw}),
            )?;
            done(
                want_json,
                &v,
                v["message"].as_str().unwrap_or("✓ 配好了"),
            );
        }
        ServerCmd::HashPassword { id } => {
            let pw = read_secret("请输入要设置的访问密码：")?;
            let v = call("hash_password", json!({"serverId": id, "password": pw}))?;
            if want_json {
                print_json(&v);
            } else {
                println!("{}", v["hash"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(())
}

fn app_cmd(sub: &AppCmd, want_json: bool) -> Result<(), String> {
    let dir = default_data_dir();
    match sub {
        AppCmd::Status => {
            let running = ctl::app_running(&dir);
            let v = match &running {
                Some(info) => json!({"running": true, "port": info.port, "pid": info.pid, "appVersion": info.app_version}),
                None => json!({"running": false}),
            };
            if want_json {
                print_json(&v);
            } else if running.is_some() {
                println!("✓ 应用在跑（进程号 {}，控制端口 {}）", v["pid"], v["port"]);
            } else {
                println!("✗ 应用没在跑。要叫起来：cli app start");
            }
        }
        AppCmd::Start => {
            if let Some(info) = ctl::app_running(&dir) {
                if want_json {
                    print_json(&json!({"running": true, "pid": info.pid}));
                } else {
                    println!("✓ 应用本来就在跑（进程号 {}）", info.pid);
                }
                return Ok(());
            }
            let exe = app_exe()?;
            Command::new(&exe)
                .arg("--hidden")
                .spawn()
                .map_err(|e| format!("启动失败（{}）：{e}", exe.display()))?;
            // 等它把控制通道开起来（最多 30 秒）
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if let Some(info) = ctl::app_running(&dir) {
                    if want_json {
                        print_json(&json!({"running": true, "pid": info.pid}));
                    } else {
                        println!("✓ 应用已启动（在托盘里，进程号 {}）", info.pid);
                    }
                    return Ok(());
                }
            }
            return Err("应用起来了但控制通道没通（看托盘图标，或手动打开应用）".into());
        }
    }
    Ok(())
}

// ---------- 跟应用说话 ----------

/// 发一条请求给运行中的应用。
fn call(method: &str, params: Value) -> Result<Value, String> {
    let dir = default_data_dir();
    let info = ctl::read_ctl_info(&dir)
        .ok_or_else(|| format!("{}（找不到 ctl.json）", ctl::APP_NOT_RUNNING))?;
    ctl::request(&info, method, &params)
}

/// 拿三张清单：应用在跑就用应用的实时状态；没在跑就读配置文件（状态标 unknown）。
fn snapshot() -> Value {
    if let Ok(v) = call("status", json!({})) {
        return v;
    }
    let dir = default_data_dir();
    let cfg = AppConfig::load(&dir.join("config.json")).unwrap_or_default();
    let service_name = |id: &str| {
        cfg.services
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let server_name = |id: &str| {
        cfg.servers
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let services: Vec<Value> = cfg
        .services
        .iter()
        .map(|s| {
            let tunnels: Vec<&str> = cfg
                .tunnels
                .iter()
                .filter(|t| t.service_id == s.id)
                .map(|t| t.name.as_str())
                .collect();
            json!({
                "id": s.id,
                "name": s.name,
                "localPort": s.local_port,
                "status": "unknown",
                "watch": s.watch,
                "autostart": s.autostart,
                "launchExe": s.launch.as_ref().map(|l| l.exe.clone()),
                "launchArgs": s.launch.as_ref().map(|l| l.args.clone()),
                "launchWorkdir": s.launch.as_ref().and_then(|l| l.workdir.clone()),
                "note": s.note,
                "tunnelCount": tunnels.len(),
                "tunnelNames": tunnels,
            })
        })
        .collect();
    let tunnels: Vec<Value> = cfg
        .tunnels
        .iter()
        .map(|t| {
            let local_port = cfg
                .services
                .iter()
                .find(|s| s.id == t.service_id)
                .map(|s| s.local_port)
                .unwrap_or(0);
            json!({
                "id": t.id,
                "name": t.name,
                "serviceId": t.service_id,
                "serviceName": service_name(&t.service_id),
                "localPort": local_port,
                "serverId": t.server_id,
                "serverName": server_name(&t.server_id),
                "remotePort": t.remote_port,
                "url": t.url,
                "status": "unknown",
                "enabled": t.enabled,
                "access": t.access,
                "accessUser": t.access_user,
                "hasPassword": t.access_hash.is_some(),
            })
        })
        .collect();
    let servers: Vec<Value> = cfg
        .servers
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "name": s.name, "host": s.host, "user": s.user,
                "sshPort": s.ssh_port, "keyPath": s.key_path,
            })
        })
        .collect();
    json!({
        "app": {"pid": null, "appVersion": null, "dataDir": dir.display().to_string(), "running": false},
        "services": services,
        "tunnels": tunnels,
        "servers": servers,
    })
}

/// 从实时/离线快照里找一台本机服务。
fn find_service(id: &str) -> Result<Value, String> {
    let snap = snapshot();
    snap["services"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == id))
        .cloned()
        .ok_or_else(|| format!("没找到本机服务「{id}」（cli services 看 id）"))
}

fn find_tunnel(id: &str) -> Result<Value, String> {
    let snap = snapshot();
    snap["tunnels"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["id"] == id))
        .cloned()
        .ok_or_else(|| format!("没找到隧道「{id}」（cli tunnels 看 id）"))
}

fn find_server(id: &str) -> Result<Value, String> {
    let snap = snapshot();
    snap["servers"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == id))
        .cloned()
        .ok_or_else(|| format!("没找到服务器「{id}」（cli servers 看 id）"))
}

/// 让服务器算出访问密码的哈希（密码本身不落盘）。
fn hash_password(server_id: &str, password: &str) -> Result<String, String> {
    let v = call(
        "hash_password",
        json!({"serverId": server_id, "password": password}),
    )?;
    v["hash"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "服务器没给出哈希".into())
}

/// 读密码：从标准输入读一行（不进命令行历史、不进日志）。
fn read_secret(prompt: &str) -> Result<String, String> {
    eprint!("{prompt}");
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .map_err(|e| format!("读密码失败：{e}"))?;
    let s = s.trim().to_string();
    if s.is_empty() {
        return Err("密码是空的，什么都没做".into());
    }
    Ok(s)
}

/// 找应用本体：跟 cli.exe 同目录 → 注册表里开机自启记的路径 → 几个常见安装位置。
fn app_exe() -> Result<PathBuf, String> {
    for p in app_exe_candidates() {
        if p.exists() {
            return Ok(p);
        }
    }
    Err("没找到『上线助手』主程序。\n  办法：手动点图标把应用打开，命令行照样能连上（cli app status 确认）。".into())
}

/// 候选位置（按可靠程度排）。
fn app_exe_candidates() -> Vec<PathBuf> {
    let names = ["app.exe", "Shangxian.exe", "上线助手.exe"];
    let mut out = Vec::new();

    // 1) 跟 cli.exe 同一个文件夹（安装包会把两个装在一起）
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            for name in names {
                out.push(dir.join(name));
            }
        }
    }

    // 2) 注册表：开机自启那一项记着应用的真实路径
    if let Some(p) = app_exe_from_registry() {
        out.push(p);
    }

    // 3) 常见安装位置（以防上面都没命中）
    for base in ["LOCALAPPDATA", "PROGRAMFILES", "ProgramFiles(x86)"] {
        let Ok(dir) = std::env::var(base) else { continue };
        if dir.is_empty() {
            continue;
        }
        for sub in ["Programs\\Shangxian", "Shangxian"] {
            for name in names {
                out.push(PathBuf::from(&dir).join(sub).join(name));
            }
        }
    }
    out
}

/// 从注册表里问应用在哪（HKCU Run 键的值形如："C:\...\Shangxian.exe" --hidden）。
fn app_exe_from_registry() -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;
    let out = Command::new("reg")
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            "/v",
            "Shangxian",
        ])
        .creation_flags(0x0800_0000) // 别闪黑框
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("REG_SZ"))?;
    let after = line.split("REG_SZ").nth(1)?.trim();
    let path_str = if let Some(rest) = after.strip_prefix('"') {
        rest.split('"').next().unwrap_or("").to_string()
    } else {
        after.split(" --hidden").next().unwrap_or(after).trim().to_string()
    };
    if path_str.is_empty() {
        return None;
    }
    let path = PathBuf::from(path_str);
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

// ---------- 打印 ----------

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn done(want_json: bool, v: &Value, human: &str) {
    if want_json {
        print_json(&json!({"ok": true, "result": v}));
    } else {
        println!("{human}");
    }
}

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap_or("").to_string()
}

fn str_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().map(|s| s.to_string())
}

fn bool_of(v: &Value, key: &str) -> bool {
    v[key].as_bool().unwrap_or(false)
}

fn num_of(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or(0)
}

/// 字符串显示宽度（中文按 2 格算，表格才能对齐）。
fn disp_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) > 0x2E7F { 2 } else { 1 })
        .sum()
}

/// 按显示宽度补空格。
fn pad(s: &str, width: usize) -> String {
    format!("{}{}", s, " ".repeat(width.saturating_sub(disp_width(s))))
}

/// 本机服务状态词（跟界面完全一致）。
fn service_word(status: &str) -> &'static str {
    match status {
        "running" => "在跑",
        "starting" => "正在启动",
        "not_listening" => "进程在但端口不通",
        "failed" => "起不来",
        "stopped" => "没启动",
        _ => "不知道（应用没在跑）",
    }
}

/// 隧道状态词（跟界面完全一致）。
fn tunnel_word(status: &str) -> &'static str {
    match status {
        "running" => "正在发布",
        "connecting" => "连接中",
        "disconnected" => "掉线重连中",
        "error" => "出错",
        "paused" => "已暂停",
        _ => "不知道（应用没在跑）",
    }
}

fn access_word(access: &str) -> &'static str {
    match access {
        "password" => "要密码",
        "ip" => "只允许指定 IP",
        _ => "谁都能进",
    }
}

fn print_status(snap: &Value) {
    let app = &snap["app"];
    if app["running"].as_bool() == Some(false) || app["pid"].is_null() {
        println!("应用：没在跑（下面是配置文件里的内容，状态看不出来；cli app start 可以叫起来）\n");
    } else {
        println!(
            "应用：在跑（进程号 {}，配置文件夹 {}）\n",
            app["pid"],
            app["dataDir"].as_str().unwrap_or("")
        );
    }
    print_services(snap);
    println!();
    print_tunnels(snap);
    println!();
    print_servers(snap);
}

fn print_services(snap: &Value) {
    let list = snap["services"].as_array().cloned().unwrap_or_default();
    println!("本机服务 {} 台：", list.len());
    for s in list {
        println!(
            "  [{}] {} 本机端口 {}   {}{}",
            pad(service_word(s["status"].as_str().unwrap_or("")), 12),
            pad(&format!("{}（{}）", str_of(&s, "name"), str_of(&s, "id")), 26),
            pad(&num_of(&s, "localPort").to_string(), 6),
            if s["launchExe"].is_null() {
                "只监控，不帮你启动".to_string()
            } else {
                format!("启动程序 {}", str_of(&s, "launchExe"))
            },
            if num_of(&s, "tunnelCount") > 0 {
                format!("，已发布到 {} 条隧道", num_of(&s, "tunnelCount"))
            } else {
                String::new()
            }
        );
        if let Some(reason) = s["reason"].as_str() {
            if !reason.is_empty() {
                println!("        ↳ {reason}");
            }
        }
    }
}

fn print_tunnels(snap: &Value) {
    let list = snap["tunnels"].as_array().cloned().unwrap_or_default();
    println!("发布隧道 {} 条：", list.len());
    for t in list {
        // 服务器名字：优先用清单里的名字（实时快照里只有 id）
        let server_name = t["serverName"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| {
                let id = t["serverId"].as_str()?;
                snap["servers"]
                    .as_array()?
                    .iter()
                    .find(|s| s["id"] == id)
                    .map(|s| str_of(s, "name"))
            })
            .unwrap_or_else(|| str_of(&t, "serverId"));
        println!(
            "  [{}] {} {} → {} · 云端 {} · {}",
            pad(tunnel_word(t["status"].as_str().unwrap_or("")), 10),
            pad(&format!("{}（{}）", str_of(&t, "name"), str_of(&t, "id")), 26),
            pad(&str_of(&t, "serviceName"), 16),
            pad(&server_name, 12),
            pad(&num_of(&t, "remotePort").to_string(), 6),
            access_word(t["access"].as_str().unwrap_or("none"))
        );
        if let Some(url) = t["url"].as_str() {
            if !url.is_empty() {
                println!("        网址 https://{url}");
            }
        }
        if let Some(reason) = t["reason"].as_str() {
            if !reason.is_empty() {
                println!("        ↳ {reason}");
            }
        }
    }
}

fn print_servers(snap: &Value) {
    let list = snap["servers"].as_array().cloned().unwrap_or_default();
    println!("云端服务器 {} 台：", list.len());
    for s in list {
        println!(
            "  {:<16} {}@{}:{} · {}",
            format!("{}（{}）", str_of(&s, "name"), str_of(&s, "id")),
            str_of(&s, "user"),
            str_of(&s, "host"),
            num_of(&s, "sshPort"),
            match s["keyPath"].as_str() {
                Some(k) if !k.is_empty() => format!("密钥 {k}"),
                _ => "默认密钥".to_string(),
            }
        );
    }
}

fn print_ports(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    println!("本机正在监听的端口 {} 个：", list.len());
    for p in list {
        println!(
            "  {} {} {}",
            pad(&num_of(&p, "port").to_string(), 6),
            pad(&str_of(&p, "process"), 28),
            str_of(&p, "exe")
        );
    }
}

/// 给 AI agent 的速查说明（`cli guide` 打印这个）。
const GUIDE: &str = r#"# 上线助手命令行速查（给 AI agent）

这个命令行是「上线助手」桌面应用的遥控器。应用才是真正看护隧道的那个，
它常驻在系统托盘里；命令行只负责查询和改配置。

## 三条铁律（别踩）
1. **不许碰 pi-web**：本机 pi-web 服务 + 它的隧道是用户正在用的通道，暂停/重启它会直接断掉会话。
2. 隧道暂停后别人就访问不了了；不确定要不要停就先问。
3. 删除本机服务/服务器前，先看它有没有被隧道引用（有引用时应用会拒绝并告诉你）。

## 最常用的十条
    cli status --json                       # 一次看全：应用 + 服务 + 隧道 + 服务器
    cli services --json                     # 本机服务清单（id、端口、状态、启动方式）
    cli tunnels --json                      # 发布隧道清单（服务→服务器、云端端口、状态）
    cli servers --json                      # 云端服务器清单
    cli ports --json                        # 本机正在监听的端口（带进程名和程序路径）
    cli tunnel start <隧道id>               # 开始发布
    cli tunnel stop <隧道id>                # 暂停发布
    cli service start <服务id>              # 拉起本机服务（用它登记的启动方式）
    cli logs <隧道id> --lines 80            # 看隧道日志（--kind service 看服务日志）
    cli nginx <隧道id>                      # 生成要贴到云端面板的 nginx 配置片段

## 配置类
    cli service add --name 我的网站 --port 8188 --exe C:\path\python.exe --args "main.py" --watch
    cli service edit <服务id> --port 8189 --watch true
    cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000 --url 我的域名.com
    cli tunnel add ... --access password --access-user 用户名 --access-password 密码
    cli server add --name 香港-1 --host api.example.com --user user
    echo 服务器密码 | cli server setup-key <服务器id>     # 用密码登录一次，自动配好免密

## 应用没在跑怎么办
    cli app status        # 看它在不在
    cli app start         # 叫起来（静默进托盘）
只读命令（status/services/tunnels/servers）在没有应用时也能用——直接读配置文件，
但状态字段会是 unknown；要改东西必须先有应用在跑。

## 想调更底层的东西
    cli methods                       # 列出所有能调的方法 + 参数格式
    cli call <方法> '{"参数":...}'    # 直接调（万能口）

## 输出约定
- 默认中文人话；加 `--json` 输出机器格式（agent 一律加）。
- 失败时 `--json` 会输出 {"ok": false, "error": "人话原因"}，退出码 1。
- 成功时 `--json` 输出 {"ok": true, "result": ...}。
"#;
