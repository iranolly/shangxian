//! 上线助手命令行（cli.exe）——给 AI agent 和别的程序用的遥控器。
//!
//! 定位：应用（在托盘里跑着）才是真正干活的那个；这个命令行负责查状态、改配置、
//! 起停服务与隧道。默认输出中文人话；加 `--json` 输出机器能直接吃的 JSON。
//!
//! 应用没在跑时：只读的几个命令（status/services/tunnels/servers）会退回到直接读
//! 配置文件，状态字段标成 unknown；要改东西就先 `cli app start`。

use std::io::{IsTerminal, Write};
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
    after_help = "给 AI agent 的速查：cli guide\n全部底层方法：cli methods\n危险操作（删除 / 暂停）在非交互环境里必须加 --yes；在交互终端会问你 y/N"
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
    #[command(after_help = "例：
  cli logs <隧道id> --lines 80
  cli logs <服务id> --kind service --lines 80")]
    Logs {
        id: String,
        /// tunnel（隧道日志）或 service（服务日志）；别的值会直接报错
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
        /// 掉线自动拉起：--watch = 开，--watch false = 关，不传 = 不动
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
        watch: Option<bool>,
        /// 开机自动启动：--autostart = 开，--autostart false = 关，不传 = 不动
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
        autostart: Option<bool>,
    },
    /// 启动
    Start { id: String },
    /// 停止（默认只停应用自己启动的进程；--force 只对配了启动方式的服务生效）
    #[command(after_help = "例：
  cli service stop <服务id>
  cli service stop <服务id> --force   # 端口上不是应用启动的进程时，确认要连它一起停")]
    Stop {
        id: String,
        /// 端口上不是应用启动的进程时，确认要连它一起停（只监控的服务即使加了这个也会被人话拒绝）
        #[arg(long)]
        force: bool,
    },
    /// 重启（先停再启）
    Restart { id: String },
    /// 删除（还有隧道在用会拒绝；要确认）
    #[command(after_help = "例：
  cli service remove <服务id> --yes   # --yes = 不再问 y/N（脚本 / agent 用）")]
    Remove {
        id: String,
        /// 跳过 y/N 确认，直接删（脚本 / agent 用）
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum TunnelCmd {
    /// 新增：cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000
    #[command(after_help = "例：
  cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000
  cli tunnel add ... --access password --access-password 密码
  cli tunnel add ... --access ip --access-ips \"1.2.3.4,5.6.7.8\"
  cli tunnel add ... --access password --access ip --access-password 密码 --access-ips \"1.2.3.4\"")]
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
        /// 访问保护，可叠加：--access password --access ip（也可写成 --access password,ip）
        /// 只认 password / ip / none，拼错会直接报错；不加 = 谁都能进
        #[arg(long, value_delimiter = ',', action = clap::ArgAction::Append)]
        access: Vec<String>,
        #[arg(long = "access-user")]
        access_user: Option<String>,
        /// 访问密码（本机算好 bcrypt 哈希再写进服务器密码文件，密码本身不落盘）
        #[arg(long = "access-password")]
        access_password: Option<String>,
        /// 只允许这些 IP（access 含 ip 时用，例 "1.2.3.4,5.6.7.8"）
        #[arg(long = "access-ips")]
        access_ips: Option<String>,
    },
    /// 改：只写你要改的字段，其余保持不变
    #[command(after_help = "例：
  cli tunnel edit <隧道id> --access password --access-password 新密码
  cli tunnel edit <隧道id> --access ip --access-ips \"1.2.3.4,5.6.7.8\"
  cli tunnel edit <隧道id> --access none   # 取消全部保护")]
    Edit {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "remote-port")]
        remote_port: Option<u16>,
        #[arg(long)]
        url: Option<String>,
        /// 访问保护，可叠加：--access password --access ip；--access none = 取消保护
        /// 不传 = 保持原样
        #[arg(long, value_delimiter = ',', action = clap::ArgAction::Append)]
        access: Vec<String>,
        #[arg(long = "access-user")]
        access_user: Option<String>,
        #[arg(long = "access-password")]
        access_password: Option<String>,
        #[arg(long = "access-ips")]
        access_ips: Option<String>,
    },
    /// 核对云端 nginx 上这条隧道实际开着的访问保护（走 SSH 只读读配置）
    Check { id: String },
    /// 开始发布
    Start { id: String },
    /// 暂停发布（要确认）
    #[command(after_help = "例：
  cli tunnel stop <隧道id> --yes")]
    Stop {
        id: String,
        /// 跳过 y/N 确认，直接暂停（脚本 / agent 用）
        #[arg(long)]
        yes: bool,
    },
    /// 立刻重连
    Restart { id: String },
    /// 删除（要确认）
    #[command(after_help = "例：
  cli tunnel remove <隧道id> --yes")]
    Remove {
        id: String,
        /// 跳过 y/N 确认，直接删（脚本 / agent 用）
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ServerCmd {
    /// 新增：cli server add --name 香港-1 --host api.example.com --user user
    #[command(after_help = "例：
  cli server add --name 香港-1 --host api.example.com --user user
  cli server add --name 香港-1 --host api.example.com --user user --port 2222 --key <密钥文件>
  cli server add --name 宝塔机 --host api.example.com --user user --vhost-dir /www/server/panel/vhost")]
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
        /// 站点目录：面板把网站放在服务器哪个目录（宝塔填 /www/server/panel/vhost；不填 = 按小皮默认找）
        #[arg(long = "vhost-dir")]
        vhost_dir: Option<String>,
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
        /// 密钥文件（不写 = 保持原样）
        #[arg(long)]
        key: Option<String>,
        /// 站点目录：不改就别写（会保留原来的值）；要清空写 --vhost-dir ""
        #[arg(long = "vhost-dir")]
        vhost_dir: Option<String>,
    },
    /// 删除（有隧道在用会拒绝；要确认）
    #[command(after_help = "例：
  cli server remove <服务器id> --yes")]
    Remove {
        id: String,
        /// 跳过 y/N 确认，直接删（脚本 / agent 用）
        #[arg(long)]
        yes: bool,
    },
    /// 用密码登录一次，自动配好免密（密码从标准输入读，不回显、不进命令行历史）
    #[command(after_help = "例：
  echo 服务器密码 | cli server setup-key <服务器id>   # 管道传密码，也不会显示在屏幕上
  cli server setup-key <服务器id>                     # 手输时同样不回显")]
    SetupKey { id: String },
    /// 生成访问密码的哈希（bcrypt，本机算好、给 nginx 用；密码从标准输入读）
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
    // 写法错（参数名拼错、少值）时：clap 的报错照旧，但补一句中文的下一步提示。
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // --help / --version 是正常输出（stdout、退出码 0），别当错误
            if !e.use_stderr() {
                e.exit();
            }
            eprint!("{e}");
            eprintln!("下一步：cli <命令> --help 看这个命令的例子；cli guide 看速查总表。");
            std::process::exit(2);
        }
    };
    let want_json = cli.json;
    match run(&cli) {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
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

fn run(cli: &Cli) -> Result<i32, String> {
    match &cli.cmd {
        Cmd::Status => {
            let snap = snapshot();
            if cli.json {
                print_ok(&snap);
            } else {
                print_status(&snap);
            }
        }
        Cmd::Services => {
            let snap = snapshot();
            if cli.json {
                print_ok(&snap["services"]);
            } else {
                print_services(&snap);
            }
        }
        Cmd::Tunnels => {
            let snap = snapshot();
            if cli.json {
                print_ok(&snap["tunnels"]);
            } else {
                print_tunnels(&snap);
            }
        }
        Cmd::Servers => {
            let snap = snapshot();
            if cli.json {
                print_ok(&snap["servers"]);
            } else {
                print_servers(&snap);
            }
        }
        Cmd::Ports => {
            let v = call("list_local_ports", json!({}))?;
            if cli.json {
                print_ok(&v);
            } else {
                print_ports(&v);
            }
        }
        Cmd::Logs { id, kind, lines } => {
            // --kind 写错不能悄悄按隧道处理；先严格认，再确认 id 存在
            // （免得脚本把「空输出」当成「一切正常」）
            let is_service = match kind.as_str() {
                "tunnel" => false,
                "service" => true,
                other => {
                    return Err(format!(
                        "--kind 只认 tunnel（隧道日志）或 service（服务日志），你写的是「{other}」。\n隧道日志：cli logs {id} --kind tunnel；服务日志：cli logs {id} --kind service"
                    ));
                }
            };
            if is_service {
                find_service(id)?;
            } else {
                find_tunnel(id)?;
            }
            let method = if is_service { "get_service_logs" } else { "get_logs" };
            let v = call(method, json!({"id": id, "lines": lines}))?;
            if cli.json {
                print_ok(&v);
            } else {
                println!("{}", v["text"].as_str().unwrap_or(""));
            }
        }
        Cmd::Nginx { id } => {
            let v = call("get_nginx_snippet", json!({"id": id}))?;
            if cli.json {
                print_ok(&v);
            } else {
                if let Some(s) = v["snippet"].as_str() {
                    println!("这几行加进站点配置里（不要整段替换）：\n{s}");
                }
                if let Some(e) = v["acmeExempt"].as_str() {
                    println!("\n证书自动续期用的一行（放进 location /.well-known/ 里）：\n{e}");
                }
                if let Some(h) = v["htpasswd"].as_str() {
                    println!("\n# 密码文件内容（保存到服务器上，路径见下面的步骤）：\n{h}");
                }
                for step in v["steps"].as_array().cloned().unwrap_or_default() {
                    println!("\n· {}", step.as_str().unwrap_or(""));
                }
            }
        }
        Cmd::Service { sub } => service_cmd(sub, cli.json)?,
        Cmd::Tunnel { sub } => tunnel_cmd(sub, cli.json)?,
        Cmd::Server { sub } => server_cmd(sub, cli.json)?,
        Cmd::App { sub } => return app_cmd(sub, cli.json),
        Cmd::Methods => {
            let v = call("methods", json!({}))?;
            if cli.json {
                print_ok(&v);
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
            let params: Value = serde_json::from_str(params).map_err(|e| {
                format!(
                    "参数不是合法 JSON：{e}\n参数要用单引号包起来，比如：cli call status '{{}}'"
                )
            })?;
            let v = call(method, params)?;
            if cli.json {
                print_ok(&v);
            } else {
                print_value_human(&v);
            }
        }
        Cmd::Guide => {
            if cli.json {
                print_ok(&json!({"text": GUIDE}));
            } else {
                println!("{GUIDE}");
            }
        }
    }
    Ok(0)
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
        ServiceCmd::Stop { id, force } => {
            let v = call(
                "service_stop",
                json!({"id": id, "killForeign": force, "expectedPid": null}),
            )?;
            let kind = v["kind"].as_str().unwrap_or("stopped");
            if kind == "needsConfirm" {
                if *force {
                    return Err(format!(
                        "没能停掉「{id}」：应用仍要求确认（端口占用者可能又变了，先 cli services 看一眼再试）"
                    ));
                }
                // 兜底：只监控的服务（没配启动方式）绝不接管外部进程，
                // 这里不再提示一条必然失败的 --force（core 现在会直接返回人话错误）。
                let svc = find_service(id)?;
                if svc["launchExe"].is_null() {
                    return Err(format!(
                        "这台服务没配启动方式，是你自己开的程序，我不会去动它。\n想接管它，先补全启动方式再停止：cli service edit {id} --exe <程序完整路径>"
                    ));
                }
                let exe = v["exe"].as_str().unwrap_or("一个不是本应用启动的程序");
                let pid = num_of(&v, "pid");
                return Err(format!(
                    "这台服务不是你启动的，正在监听的是 {exe}（PID {pid}）。\n停止会真的把它关掉。确认要停就加 --force：cli service stop {id} --force"
                ));
            }
            let human = if kind == "foreignStopped" {
                format!("✓ 已停止「{id}」（外部启动的进程）")
            } else {
                format!("✓ 已停止「{id}」")
            };
            done(want_json, &v, &human);
        }
        ServiceCmd::Restart { id } => {
            let v = call("service_restart", json!({"id": id}))?;
            if v["kind"].as_str() == Some("needsConfirm") {
                // 兜底：只监控的服务不接管外部进程，不会提示先 --force（那一步必然失败）
                let svc = find_service(id)?;
                if svc["launchExe"].is_null() {
                    return Err(format!(
                        "「{id}」是只监控的服务（没配启动方式），重启不会去动端口上的程序。\n想接管它，先 cli service edit {id} --exe <程序完整路径> 补全启动方式，再试"
                    ));
                }
                let exe = v["exe"].as_str().unwrap_or("一个不是本应用启动的程序");
                let pid = num_of(&v, "pid");
                return Err(format!(
                    "「{id}」现在不是你启动的进程在监听（{exe}，PID {pid}），重启不会去动它。\n要停掉它再重启：先 cli service stop {id} --force，再 cli service start {id}"
                ));
            }
            done(want_json, &v, &format!("✓ 已重启「{id}」"));
        }
        ServiceCmd::Remove { id, yes } => {
            let svc = find_service(id)?;
            let name = str_of(&svc, "name");
            if !confirm_destructive(
                &format!(
                    "要删除本机服务「{name}」（{id}）。删了就得重新登记；已经有隧道在用它的话，应用会拒绝删除。"
                ),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("remove_service", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已删除本机服务「{name}」（{id}）"));
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
            // --access 拼错、或 --access-password / --access-ips 没配套：先报错，
            // 不让它们被 core 柔性丢掉，变成「看着开了保护其实没有」。
            let access = access_from_cli(access)?;
            ensure_access_companions(
                &access,
                access_password.as_deref(),
                access_ips.as_deref(),
                false,
            )?;
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
                &format!(
                    "✓ 已建好隧道「{name}」并开始发布（id={}）\n  {}",
                    id_of(&v),
                    access_echo(&access)
                ),
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
            // 不传 --access = 保持原样（从查到的那条隧道里把数组读回来）；
            // 传 none = 清空；传别的 = 整体替换，可以叠加。
            let access_new = if access.is_empty() {
                old["access"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_string())
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default()
            } else {
                access_from_cli(access)?
            };
            // 传了新密码 / 新白名单，但最终保护设置里没有对应项 → 报错，别静默丢
            ensure_access_companions(
                &access_new,
                access_password.as_deref(),
                access_ips.as_deref(),
                true,
            )?;
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
            done(
                want_json,
                &v,
                &format!(
                    "✓ 已改好隧道「{id}」（会断一下再自动连上）\n  {}",
                    access_echo(&access_new)
                ),
            );
        }
        TunnelCmd::Check { id } => {
            let v = call("check_tunnel_access", json!({"id": id}))?;
            if want_json {
                print_ok(&v);
            } else {
                print_access_check(id, &v);
            }
        }
        TunnelCmd::Start { id } => {
            let v = call("tunnel_start", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已开始发布「{id}」"));
        }
        TunnelCmd::Stop { id, yes } => {
            let t = find_tunnel(id)?;
            let name = str_of(&t, "name");
            if !confirm_destructive(
                &format!("要暂停隧道「{name}」（{id}）。暂停后别人就访问不了这个网址了。"),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("tunnel_stop", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已暂停「{name}」（{id}）"));
        }
        TunnelCmd::Restart { id } => {
            let v = call("tunnel_restart", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已让「{id}」重连"));
        }
        TunnelCmd::Remove { id, yes } => {
            let t = find_tunnel(id)?;
            let name = str_of(&t, "name");
            if !confirm_destructive(
                &format!("要删除隧道「{name}」（{id}）。删了发布就停了，配置也一起没。"),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("remove_tunnel", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已删除隧道「{name}」（{id}）"));
        }
    }
    Ok(())
}

/// 组装 server edit 要提交的 save_server 参数：没写的字段一律从旧值抄。
/// 重点是 vhostDir（站点目录）——CLI 没传时必须原样带上，
/// 因为 core 的 save_server 是整份覆盖，字段缺省会被当成 None 把站点目录清掉。
fn server_edit_params(
    id: &str,
    old: &Value,
    name: Option<&str>,
    host: Option<&str>,
    user: Option<&str>,
    port: Option<u16>,
    key: Option<&str>,
    vhost_dir: Option<&str>,
) -> Value {
    json!({
        "id": id,
        "name": name.map(|s| s.to_string()).unwrap_or_else(|| str_of(old, "name")),
        "host": host.map(|s| s.to_string()).unwrap_or_else(|| str_of(old, "host")),
        "user": user.map(|s| s.to_string()).unwrap_or_else(|| str_of(old, "user")),
        "sshPort": port.unwrap_or_else(|| num_of(old, "sshPort") as u16),
        "keyPath": key.map(|s| s.to_string()).or_else(|| opt_str(old, "keyPath")),
        "vhostDir": vhost_dir.map(|s| s.to_string()).or_else(|| opt_str(old, "vhostDir")),
    })
}

fn server_cmd(sub: &ServerCmd, want_json: bool) -> Result<(), String> {
    match sub {
        ServerCmd::Add {
            name,
            host,
            user,
            port,
            key,
            vhost_dir,
        } => {
            let v = call(
                "save_server",
                json!({"id": null, "name": name, "host": host, "user": user, "sshPort": port, "keyPath": key, "vhostDir": vhost_dir}),
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
            vhost_dir,
        } => {
            let old = find_server(id)?;
            // 没写的字段一律从旧值抄回来（core 的 save_server 是整份覆盖，漏字段会把界面上的设置抹掉）
            let v = call(
                "save_server",
                server_edit_params(
                    id,
                    &old,
                    name.as_deref(),
                    host.as_deref(),
                    user.as_deref(),
                    *port,
                    key.as_deref(),
                    vhost_dir.as_deref(),
                ),
            )?;
            done(want_json, &v, &format!("✓ 已改好服务器「{id}」"));
        }
        ServerCmd::Remove { id, yes } => {
            let srv = find_server(id)?;
            let name = str_of(&srv, "name");
            if !confirm_destructive(
                &format!("要删除服务器「{name}」（{id}）。还有隧道在用它的话，应用会拒绝删除。"),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("delete_server", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已删除服务器「{name}」（{id}）"));
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
                print_ok(&v);
            } else {
                println!("{}", v["hash"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(())
}

fn app_cmd(sub: &AppCmd, want_json: bool) -> Result<i32, String> {
    let dir = default_data_dir();
    match sub {
        AppCmd::Status => {
            let running = ctl::app_running(&dir);
            let v = match &running {
                Some(info) => json!({
                    "running": true,
                    "offline": false,
                    "port": info.port,
                    "pid": info.pid,
                    "appVersion": info.app_version,
                }),
                None => json!({
                    "running": false,
                    "offline": true,
                    "hint": "要叫起来：cli app start",
                }),
            };
            if want_json {
                print_ok(&v);
            } else if running.is_some() {
                println!("✓ 应用在跑（进程号 {}，控制端口 {}）", v["pid"], v["port"]);
            } else {
                println!("✗ 应用没在跑（offline）。要叫起来：cli app start");
            }
            // 没在跑就不算成功：退出码 3（脚本可以直接 `cli app status || cli app start`）
            return Ok(if running.is_some() { 0 } else { 3 });
        }
        AppCmd::Start => {
            if let Some(info) = ctl::app_running(&dir) {
                if want_json {
                    print_ok(&json!({"running": true, "offline": false, "pid": info.pid}));
                } else {
                    println!("✓ 应用本来就在跑（进程号 {}）", info.pid);
                }
                return Ok(0);
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
                        print_ok(&json!({"running": true, "offline": false, "pid": info.pid}));
                    } else {
                        println!("✓ 应用已启动（在托盘里，进程号 {}）", info.pid);
                    }
                    return Ok(0);
                }
            }
            return Err("应用起来了但控制通道没通（看托盘图标，或手动打开应用）".into());
        }
    }
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
                "sshPort": s.ssh_port, "keyPath": s.key_path, "vhostDir": s.vhost_dir,
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

/// 本机算出 bcrypt 访问密码哈希（密码本身不落盘）。
fn hash_password(server_id: &str, password: &str) -> Result<String, String> {
    let v = call(
        "hash_password",
        json!({"serverId": server_id, "password": password}),
    )?;
    v["hash"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "没拿到哈希".into())
}

/// 解析命令行给的 --access（可重复、可逗号 / 加号 / 空格分隔）。
/// 只认 password / ip / none：认不出的值立刻报错，绝不像 core 那样柔性丢掉
/// （core 的柔性是为了读老配置；命令行写错必须当场让用户知道）。
fn access_from_cli(values: &[String]) -> Result<Vec<String>, String> {
    // 一个 --access 都没写：保持老行为（tunnel add = 无保护；tunnel edit 不走这里）
    if values.is_empty() {
        return Ok(Vec::new());
    }
    // 整条 --access 全是空片段（--access= / --access "" / --access ","）等于
    // 「写了等于没写」：不能静默当无保护，必须当场报错。要无保护得明确写 --access none。
    // （只要有一个有效词，逗号 / 加号 / 空格边上的空片段就照常忽略，不整个拒掉。）
    let blank_msg = || {
        "--access 后面是空的。要取消访问保护请明确写 --access none；要加保护就写 password / ip（例：--access password --access ip）。"
            .to_string()
    };
    let mut password = false;
    let mut ip = false;
    let mut none = false;
    let mut any_token = false;
    for raw in values {
        for token in raw.split(|c: char| c == ',' || c == '+' || c == ';' || c.is_whitespace()) {
            match token.trim() {
                // 逗号 / 加号 / 空格多出来的空片段（--access=password, 这类）一律忽略；
                // 只有整条 --access 一个有效词都没有时，才由下面的 any_token 拦住报错。
                "" => {}
                "password" => {
                    password = true;
                    any_token = true;
                }
                "ip" => {
                    ip = true;
                    any_token = true;
                }
                "none" => {
                    none = true;
                    any_token = true;
                }
                other => {
                    return Err(format!(
                        "看不懂 --access 的值「{other}」。只认这三种：password（要密码）、ip（只允许指定 IP）、none（取消保护）。\n例：--access password --access ip"
                    ));
                }
            }
        }
    }
    if !any_token {
        // 全是分隔符 / 空白，等于什么都没写
        return Err(blank_msg());
    }
    if none && (password || ip) {
        return Err(
            "--access none 是「取消全部保护」，不能和 password / ip 一起用。\n要么写 --access none，要么写 --access password --access ip"
                .into(),
        );
    }
    if none {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    if password {
        out.push("password".to_string());
    }
    if ip {
        out.push("ip".to_string());
    }
    Ok(out)
}

/// 组合校验：--access-password / --access-ips 必须和最终生效的保护设置里的
/// password / ip 配套。不配套就报错——否则 core 会安静地把它们丢掉，
/// 用户以为开了保护，实际上什么都没生效。
/// `editing` = 这是在改已有隧道（不传 --access 就沿用原来的保护设置）。
fn ensure_access_companions(
    access: &[String],
    access_password: Option<&str>,
    access_ips: Option<&str>,
    editing: bool,
) -> Result<(), String> {
    let has = |key: &str| access.iter().any(|a| a == key);
    let keep_note = if editing {
        "（改隧道没传 --access，就是在用原来那套保护设置）"
    } else {
        ""
    };
    let target = if editing {
        "edit <隧道id>"
    } else {
        "add --service <服务id> --server <服务器id> --remote-port <云端端口>"
    };
    if access_password.is_some() && !has("password") {
        return Err(format!(
            "你给了 --access-password（访问密码），但这次生效的保护设置里没有 password{keep_note}。密码会被丢掉、保护不会生效。\n正确写法：cli tunnel {target} --access password --access-password <密码>"
        ));
    }
    if access_ips.is_some() && !has("ip") {
        return Err(format!(
            "你给了 --access-ips（IP 白名单），但这次生效的保护设置里没有 ip{keep_note}。白名单会被丢掉、不会生效。\n正确写法：cli tunnel {target} --access ip --access-ips \"1.2.3.4,5.6.7.8\""
        ));
    }
    Ok(())
}

/// 把 access 数组说成人话。
fn access_phrase(access: &[String]) -> String {
    let password = access.iter().any(|a| a == "password");
    let ip = access.iter().any(|a| a == "ip");
    match (password, ip) {
        (true, true) => "密码 + 白名单（只允许指定 IP）".into(),
        (true, false) => "密码".into(),
        (false, true) => "白名单（只允许指定 IP）".into(),
        (false, false) => "无（谁都能进）".into(),
    }
}

/// 成功提示里回显最终生效的保护状态，别让用户猜。
fn access_echo(access: &[String]) -> String {
    if access.is_empty() {
        "访问保护：无（谁都能进）。想加门：--access password --access-password <密码> 或 --access ip --access-ips \"1.2.3.4\""
            .into()
    } else {
        format!("访问保护：{}，已生效", access_phrase(access))
    }
}

/// 危险操作确认：
/// - 有 --yes：直接放行（脚本 / agent 用）。
/// - 交互终端（能敲键盘）：先说清要做什么，再问 y/N。
/// - 非交互（管道 / 脚本 / agent）：拒绝执行，并告诉用户补 --yes。
fn confirm_destructive(what: &str, yes: bool) -> Result<bool, String> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "{what}\n这一步需要你确认，但现在不是在交互终端（管道 / 脚本 / agent），我没法问你 y/N，所以先不动手。\n确定要做就加 --yes 再跑一遍。"
        ));
    }
    // 先让人看清要动的是哪个对象（提示走 stderr，不污染 --json 的 stdout）
    eprintln!("{what}");
    eprint!("确定要这么做吗？[y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("读输入失败：{e}"))?;
    let ans = line.trim().to_ascii_lowercase();
    Ok(matches!(ans.as_str(), "y" | "yes" | "是" | "确定"))
}

/// 用户在确认里选了「不」：什么都没改，退出码 0。
fn cancelled(want_json: bool) -> Result<(), String> {
    done(
        want_json,
        &json!({"cancelled": true}),
        "已取消，什么都没做。",
    );
    Ok(())
}

/// 读密码：交互输入时关掉回显（不显示在屏幕上、不进命令行历史、不进日志）；
/// 管道 / 脚本里依然从标准输入读，所以 `echo 密码 | cli ...` 照旧能用。
fn read_secret(prompt: &str) -> Result<String, String> {
    let s = if std::io::stdin().is_terminal() {
        match rpassword::prompt_password(prompt) {
            Ok(s) => s,
            Err(e) => {
                // 关回显失败也不能让用户以为输的密码被藏起来了
                eprintln!("（没法关回显：{e}，接下来输的内容会显示在屏幕上）");
                read_secret_plain(prompt)?
            }
        }
    } else {
        // 管道 / 脚本：老老实实从标准输入读
        read_secret_plain(prompt)?
    };
    let s = s.trim().to_string();
    if s.is_empty() {
        return Err("密码是空的，什么都没做".into());
    }
    Ok(s)
}

/// 带提示地从标准输入读一行密码（回显管不住，只在非交互或关回显失败时用）。
fn read_secret_plain(prompt: &str) -> Result<String, String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    std::io::stdin()
        .read_line(&mut s)
        .map_err(|e| format!("读密码失败：{e}"))?;
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

/// 成功结果的统一机器格式：{"ok": true, "result": ...}。
fn print_ok(v: &Value) {
    print_json(&json!({"ok": true, "result": v}));
}

fn done(want_json: bool, v: &Value, human: &str) {
    if want_json {
        print_ok(v);
    } else {
        println!("{human}");
    }
}

/// 人话打印任意返回值（cli call 不带 --json 时用）：
/// 字符串直接打；对象一行一个字段；数组一行一项；嵌套的照原样打一行 JSON。
fn print_value_human(v: &Value) {
    match v {
        Value::Null => println!("（没有内容）"),
        Value::Bool(b) => println!("{b}"),
        Value::Number(n) => println!("{n}"),
        Value::String(s) => println!("{s}"),
        Value::Array(items) => {
            if items.is_empty() {
                println!("（空列表）");
            }
            for it in items {
                println!("- {}", one_line(it));
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                println!("（空对象）");
            }
            for (k, val) in map {
                println!("{k}：{}", one_line(val));
            }
        }
    }
}

/// 嵌套值压成一行（是字符串就直接显示，别带引号）。
fn one_line(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
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

fn access_word(value: &Value) -> String {
    let list: Vec<String> = value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    let password = list.iter().any(|s| s == "password");
    let ip = list.iter().any(|s| s == "ip");
    match (password, ip) {
        (true, true) => "要密码 + 只允许指定 IP".into(),
        (true, false) => "要密码".into(),
        (false, true) => "只允许指定 IP".into(),
        (false, false) => "谁都能进".into(),
    }
}

/// 人话输出「云端实际保护」（cli tunnel check 用）。
fn print_access_check(id: &str, v: &Value) {
    let yes = |key: &str| v[key].as_bool().unwrap_or(false);
    let allow_ips: Vec<String> = v["allowIps"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    println!("隧道「{id}」云端实际保护：");
    println!("  密码：{}", if yes("passwordOn") { "开" } else { "关" });
    if yes("ipOn") {
        let ips = if allow_ips.is_empty() {
            "（没写具体 IP）".to_string()
        } else {
            allow_ips.join("、")
        };
        println!("  白名单：开（IP：{ips}）");
    } else {
        println!("  白名单：关");
    }
    println!("  密码文件：{}", if yes("htpasswdExists") { "有" } else { "无" });
    println!("  站点配置：{}", if yes("confFound") { "找到" } else { "没找到" });
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
            access_word(&t["access"])
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

## 危险操作（删除 / 暂停）要确认
- 交互终端（人敲命令）：先打印要动哪个对象，再问你 y/N；回车 = 什么都不做。
- 非交互（管道 / 脚本 / AI agent）：必须显式加 --yes，否则拒绝执行并报错。
      cli tunnel stop <隧道id> --yes
      cli tunnel remove <隧道id> --yes
      cli service remove <服务id> --yes
      cli server remove <服务器id> --yes

## 最常用的命令
    cli status --json                       # 一次看全：应用 + 服务 + 隧道 + 服务器
    cli services --json                     # 本机服务清单（id、端口、状态、启动方式）
    cli tunnels --json                      # 发布隧道清单（服务→服务器、云端端口、状态）
    cli servers --json                      # 云端服务器清单
    cli ports --json                        # 本机正在监听的端口（带进程名和程序路径）
    cli tunnel start <隧道id>               # 开始发布
    cli tunnel stop <隧道id>                # 暂停发布（要确认；非交互加 --yes）
    cli service start <服务id>              # 拉起本机服务（用它登记的启动方式）
    cli service stop <服务id>               # 停止（不是应用启动的进程会先拦一下；配了启动方式才加 --force 真停，只监控的服务会被拒绝）
    cli logs <隧道id> --lines 80            # 看隧道日志（--kind service 看服务日志，别的值报错）
    cli nginx <隧道id>                      # 生成要贴到云端面板的 nginx 配置片段
    cli tunnel check <隧道id>               # 去云端看实际开着的访问保护（只读，不改服务器）

## 配置类
    cli service add --name 我的网站 --port 8188 --exe C:\path\python.exe --args "main.py" --watch
    cli service edit <服务id> --port 8189 --watch true
    cli service edit <服务id> --watch false              # --watch / --autostart 也可以不带值（= 开）
    cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000 --url 我的域名.com
    cli tunnel add ... --access password --access-user 用户名 --access-password 密码
    cli tunnel add ... --access password --access ip --access-ips "1.2.3.4,5.6.7.8"   # 密码 + 白名单两道门同时开
    cli tunnel edit <隧道id> --access password --access-password 新密码   # 换密码
    cli tunnel edit <隧道id> --access none   # 取消访问保护（不传 --access = 保持原样）
    cli server add --name 香港-1 --host api.example.com --user user
    cli server add --name 宝塔机 --host api.example.com --user root --vhost-dir /www/server/panel/vhost   # 站点目录：密码文件/站点配置放哪
    cli server edit <服务器id> --vhost-dir /www/server/panel/vhost   # 不写 = 保留原来的站点目录，不会被清掉
    echo 服务器密码 | cli server setup-key <服务器id>     # 用密码登录一次，自动配好免密
- --access 只认 password / ip / none，拼错直接报错（不会悄悄变成「无保护」）；--access= 这种空值也直接报错，要取消保护写 --access none。
- --access-password 必须配 --access password，--access-ips 必须配 --access ip，配不上会报错。
- 每次改完都会回显最终保护状态，比如「访问保护：密码 + 白名单（只允许指定 IP），已生效」。

## 应用没在跑怎么办
    cli app status        # 看它在不在；没在跑时退出码 3，JSON 里 offline: true
    cli app start         # 叫起来（静默进托盘）
只读命令（status/services/tunnels/servers）在没有应用时也能用——直接读配置文件，
但状态字段会是 unknown；要改东西必须先有应用在跑。

## 想调更底层的东西
    cli methods                       # 列出所有能调的方法 + 参数格式
    cli call <方法> '{"参数":...}'    # 直接调（万能口）

## 输出约定
- 默认中文人话；加 `--json` 输出机器格式（agent 一律加）。读命令、写命令都是同一个信封。
- 成功（status / services / tunnels / servers / ports / logs / nginx / methods / call / guide / 各种改配置）：
  {"ok": true, "result": ...}
- 失败：{"ok": false, "error": "人话原因"}
- 退出码：0 = 成功（确认里选「不」也是 0）；1 = 出错（包括非交互下拒绝执行）；
  2 = 命令写法不对（参数名拼错、少值）；3 = `cli app status` 发现应用没在跑（JSON 里 offline: true）。
- `cli call` 不带 --json 时按人话打印返回值，加了才给上面的 JSON 信封。
- 输密码（cli server setup-key）时屏幕上不回显，管道传也一样安全。
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_tunnel_add(extra: &[&str]) -> Result<Vec<String>, String> {
        let mut args = vec![
            "cli",
            "tunnel",
            "add",
            "--service",
            "svc-1",
            "--server",
            "srv-1",
            "--remote-port",
            "13000",
        ];
        args.extend_from_slice(extra);
        let cli = Cli::try_parse_from(args).unwrap();
        match cli.cmd {
            Cmd::Tunnel {
                sub: TunnelCmd::Add { access, .. },
            } => access_from_cli(&access),
            _ => panic!("应该解析成 tunnel add"),
        }
    }

    #[test]
    fn access_flag_is_repeatable_and_comma_split() {
        // 可重复：--access password --access ip
        assert_eq!(
            parse_tunnel_add(&["--access", "password", "--access", "ip"]).unwrap(),
            vec!["password".to_string(), "ip".to_string()]
        );
        // 逗号分隔：--access password,ip（顺序会归一成 password、ip）
        assert_eq!(
            parse_tunnel_add(&["--access", "ip,password"]).unwrap(),
            vec!["password".to_string(), "ip".to_string()]
        );
        // 不传 = 无保护
        assert!(parse_tunnel_add(&[]).unwrap().is_empty());
    }

    #[test]
    fn access_rejects_unknown_values() {
        // 拼错不能静默变成「无保护」，必须报错并带上写错的值
        let err = parse_tunnel_add(&["--access", "bogus"]).unwrap_err();
        assert!(err.contains("bogus"), "报错要带上写错的值：{err}");
        assert!(parse_tunnel_add(&["--access", "password", "--access", "passwrod"]).is_err());
    }

    #[test]
    fn access_none_clears_and_cannot_mix() {
        assert!(parse_tunnel_add(&["--access", "none"]).unwrap().is_empty());
        assert!(
            parse_tunnel_add(&["--access", "none,ip"]).is_err(),
            "none 不能和 ip 叠一起"
        );
    }

    #[test]
    fn access_blank_value_errors_but_empty_pieces_stay_ignored() {
        // 整条 --access 全是空片段（--access= / --access "" / 只有空白或分隔符）：必须报错
        assert!(parse_tunnel_add(&["--access", ""]).is_err());
        assert!(parse_tunnel_add(&["--access", "   "]).is_err());
        assert!(parse_tunnel_add(&["--access", ","]).is_err());
        // 只要有一个有效词，逗号边上的空片段就照常忽略，不整个拒掉
        assert_eq!(
            parse_tunnel_add(&["--access=password,"]).unwrap(),
            vec!["password".to_string()]
        );
        assert_eq!(
            parse_tunnel_add(&["--access=,password"]).unwrap(),
            vec!["password".to_string()]
        );
        assert_eq!(
            parse_tunnel_add(&["--access=password,,ip"]).unwrap(),
            vec!["password".to_string(), "ip".to_string()]
        );
        assert_eq!(
            parse_tunnel_add(&["--access", "password", "--access", ""]).unwrap(),
            vec!["password".to_string()]
        );
        // "password, ip" 里因分隔符/空格多出来的空片段照旧忽略
        assert_eq!(
            parse_tunnel_add(&["--access", "password, ip"]).unwrap(),
            vec!["password".to_string(), "ip".to_string()]
        );
        // 完全不传 --access 还是老行为：无保护
        assert!(parse_tunnel_add(&[]).unwrap().is_empty());
    }

    #[test]
    fn server_vhost_dir_flag_parses_for_add_and_edit() {
        let cli = Cli::try_parse_from([
            "cli", "server", "add", "--name", "n", "--host", "h", "--user", "u",
            "--vhost-dir", "/www/server/panel/vhost",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Server {
                sub: ServerCmd::Add { vhost_dir, .. },
            } => assert_eq!(vhost_dir.as_deref(), Some("/www/server/panel/vhost")),
            _ => panic!("应该解析成 server add"),
        }
        let cli = Cli::try_parse_from(["cli", "server", "edit", "srv-1", "--vhost-dir", "/xp/panel/vhost"])
            .unwrap();
        match cli.cmd {
            Cmd::Server {
                sub: ServerCmd::Edit { vhost_dir, .. },
            } => assert_eq!(vhost_dir.as_deref(), Some("/xp/panel/vhost")),
            _ => panic!("应该解析成 server edit"),
        }
        // 不写就是 None（表示保持原值），不能变成空串
        let cli = Cli::try_parse_from(["cli", "server", "edit", "srv-1"]).unwrap();
        match cli.cmd {
            Cmd::Server {
                sub: ServerCmd::Edit { vhost_dir, .. },
            } => assert!(vhost_dir.is_none()),
            _ => panic!("应该解析成 server edit"),
        }
    }

    #[test]
    fn server_edit_keeps_vhost_dir_when_flag_omitted() {
        let old = json!({
            "id": "srv-1",
            "name": "香港-1",
            "host": "api.example.com",
            "user": "root",
            "sshPort": 22,
            "keyPath": "C:/k/id",
            "vhostDir": "/www/server/panel/vhost",
        });
        // 没传 --vhost-dir：原样带上旧值，不许清掉
        let p = server_edit_params("srv-1", &old, None, None, None, None, None, None);
        assert_eq!(p["vhostDir"], "/www/server/panel/vhost");
        // 传了：用新值
        let p = server_edit_params(
            "srv-1",
            &old,
            None,
            None,
            None,
            None,
            None,
            Some("/xp/panel/vhost"),
        );
        assert_eq!(p["vhostDir"], "/xp/panel/vhost");
        // 旧值本来就没有：继续保持没有（不是空串）
        let old2 = json!({
            "id": "srv-2",
            "name": "n",
            "host": "h",
            "user": "u",
            "sshPort": 22,
        });
        let p = server_edit_params("srv-2", &old2, None, None, None, None, None, None);
        assert!(p["vhostDir"].is_null());
        // 其它字段的沿用行为没变
        assert_eq!(p["name"], "n");
        assert_eq!(p["sshPort"], 22);
    }

    #[test]
    fn access_companions_must_match() {
        let pw = Some("pw");
        let ips = Some("1.2.3.4");
        // 给了密码但保护里没有 password → 报错
        assert!(ensure_access_companions(&[], pw, None, false).is_err());
        assert!(ensure_access_companions(&["ip".into()], pw, None, false).is_err());
        // 给了白名单但保护里没有 ip → 报错
        assert!(ensure_access_companions(&["password".into()], None, ips, false).is_err());
        // 配套了 → 通过
        assert!(ensure_access_companions(&["password".into()], pw, None, false).is_ok());
        assert!(ensure_access_companions(&["password".into(), "ip".into()], pw, ips, false).is_ok());
        // 什么都没给 → 通过
        assert!(ensure_access_companions(&[], None, None, true).is_ok());
        // 改隧道时带了新密码但生效保护里没有 password → 也要报错
        assert!(ensure_access_companions(&[], pw, None, true).is_err());
    }

    #[test]
    fn access_echo_shows_final_state() {
        assert!(access_echo(&[]).contains("无（谁都能进）"));
        let echo = access_echo(&["password".to_string(), "ip".to_string()]);
        assert!(echo.contains("密码 + 白名单"), "要回显保护内容：{echo}");
        assert!(echo.contains("已生效"), "要说明已生效：{echo}");
    }

    #[test]
    fn service_edit_watch_accepts_flag_or_value() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["cli", "service", "edit", "svc-1"];
            args.extend_from_slice(extra);
            let cli = Cli::try_parse_from(args).unwrap();
            match cli.cmd {
                Cmd::Service {
                    sub: ServiceCmd::Edit {
                        watch, autostart, ..
                    },
                } => (watch, autostart),
                _ => panic!("应该解析成 service edit"),
            }
        };
        assert_eq!(parse(&[]), (None, None));
        assert_eq!(parse(&["--watch"]), (Some(true), None));
        assert_eq!(parse(&["--watch", "false"]), (Some(false), None));
        assert_eq!(parse(&["--autostart"]), (None, Some(true)));
    }

    #[test]
    fn access_phrase_reads_the_array() {
        assert_eq!(access_phrase(&[]), "无（谁都能进）");
        assert_eq!(access_phrase(&["password".to_string()]), "密码");
        assert_eq!(access_phrase(&["ip".to_string()]), "白名单（只允许指定 IP）");
        assert_eq!(
            access_phrase(&["ip".to_string(), "password".to_string()]),
            "密码 + 白名单（只允许指定 IP）"
        );
    }
}
