//! 上线助手命令行（cli.exe）——给 AI agent 和别的程序用的遥控器。
//!
//! 定位：应用（在托盘里跑着）才是真正干活的那个；这个命令行负责查状态、改配置、
//! 起停服务与隧道。默认输出中文人话；加 `--json` 输出机器能直接吃的 JSON。
//!
//! 应用没在跑时：只读的几个命令（status/services/tunnels/servers）会退回到直接读
//! 配置文件，状态字段标成 unknown；要改东西就先 `cli app start`。

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use shangxian_core::config::{self, default_data_dir, AppConfig};
use shangxian_core::ctl;
use shangxian_core::doctor::{self, DoctorMode, DoctorRequest};
use shangxian_core::events::EventLog;

#[derive(Parser)]
#[command(
    name = "cli",
    version,
    about = "上线助手命令行：查询与配置（默认输出中文文本，--json 输出机器可读格式）",
    after_help = "给 AI agent 的速查：cli guide\n全部底层方法：cli methods\n危险操作（删除 / 暂停 / 停止全部 / 重置指纹 / 导入配置）在非交互环境里必须加 --yes；在交互终端会询问 y/N\n应用未运行时：只读命令读取旧配置；需要应用运行的命令（stats / diag / logs / undo / config import / app update-check）退出码 3；cli doctor 离线也能运行，cli app stop-all 在应用未运行时直接返回 0"
)]
struct Cli {
    /// 输出 JSON（供 AI agent / 脚本使用）
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
    /// 体检：应用、文件夹、SSH、本机端口、服务器、隧道、云端、公网、域名
    #[command(after_help = "例：
  cli doctor                # 快速检查（不连接服务器）
  cli doctor --deep         # 深度检查（会连接服务器和域名，耗时较长）
  cli doctor --strict       # 只要有 1 项未通过就退出码 1（供脚本使用；不算命令写错）")]
    Doctor {
        #[arg(long)]
        deep: bool,
        /// 有未通过项就退出码 1（不算命令错误；文本输出会写清「体检有 N 项未通过」）
        #[arg(long)]
        strict: bool,
    },
    /// 生成脱敏诊断包（发给排查人员）
    Diag {
        /// 保存位置；不写 = 保存到数据目录的 diag 文件夹
        #[arg(long)]
        out: Option<String>,
        #[arg(long)]
        deep: bool,
    },
    /// 运行统计：应用运行多久、隧道重连情况、今日重连次数
    Stats,
    /// 读日志：事件（应用中发生的事）/ 应用日志 / 隧道日志 / 本机服务日志
    #[command(after_help = "例：
  cli logs <隧道id> --lines 80
  cli logs <服务id> --kind service --lines 80
  cli logs --source all --format json --json    # 全部来源，带 JSON 信封
  cli logs --source app --format json           # 事件 + 应用日志，一行一条 JSON（供管道使用）
  cli logs <隧道id> --archive 1                 # 查看上一份轮转备份")]
    Logs {
        /// 隧道 / 服务 id；不写 = 查看全部来源（事件 + 应用日志 + 所有隧道 / 服务）
        id: Option<String>,
        /// tunnel（隧道日志）或 service（服务日志）；不写 = tunnel（旧写法兼容）
        #[arg(long, default_value = "tunnel")]
        kind: String,
        /// all | tunnel | service | app；写了它就以它为准（--kind 只作旧写法兼容）
        #[arg(long)]
        source: Option<String>,
        /// lines（默认，文本格式）或 json（一行一条 JSON，供管道使用）
        #[arg(long, default_value = "lines")]
        format: String,
        /// 0 = 仅查看当前文件，1 / 2 = 包含上一份 / 上两份轮转备份
        #[arg(long, default_value_t = 0, allow_negative_numbers = true)]
        archive: i64,
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
    /// 撤销删除（删除后 10 分钟内有效；找不到就退出码 1）
    #[command(after_help = "例：
  cli undo              # 撤销最近一次删除（= cli undo last）
  cli undo list         # 查看可撤销的记录
  cli undo und-3f9a1c2d # 按令牌撤销指定的一条")]
    Undo {
        /// 撤销令牌，或 last（最近一次）/ list（列出能撤销的）；不写 = last
        target: Option<String>,
    },
    /// 生成 nginx 配置片段（人工粘贴到面板）
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
    /// 云端服务器：新增/编辑/删除/配置免密
    Server {
        #[command(subcommand)]
        sub: ServerCmd,
    },
    /// 应用自身：启动、查看状态、停止全部服务、检查更新
    App {
        #[command(subcommand)]
        sub: AppCmd,
    },
    /// 配置备份：导出 / 导入
    Config {
        #[command(subcommand)]
        sub: ConfigCmd,
    },
    /// 列出所有可调用的底层方法（供 agent 查看）
    Methods,
    /// 直接调用底层方法（通用入口）：cli call <方法> '{"参数":...}'
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
        /// 启动程序（不填 = 只监控，不自动启动）
        #[arg(long)]
        exe: Option<String>,
        #[arg(long, default_value = "")]
        args: String,
        #[arg(long)]
        workdir: Option<String>,
        /// 断开后自动启动
        #[arg(long)]
        watch: bool,
        /// 开机自动启动
        #[arg(long)]
        autostart: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// 改：只写需要修改的字段，其余保持不变
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
        /// 断开后自动启动：--watch = 开，--watch false = 关，不传 = 不修改
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
        watch: Option<bool>,
        /// 开机自动启动：--autostart = 开，--autostart false = 关，不传 = 不修改
        #[arg(long, num_args = 0..=1, default_missing_value = "true", value_parser = clap::value_parser!(bool))]
        autostart: Option<bool>,
    },
    /// 启动
    Start { id: String },
    /// 停止（默认只停止由应用启动的进程；--force 只对已配置启动方式的服务生效）
    #[command(after_help = "例：
  cli service stop <服务id>
  cli service stop <服务id> --force   # 端口上不是应用启动的进程时，确认要连同它一起停止")]
    Stop {
        id: String,
        /// 端口上不是应用启动的进程时，确认要连同它一起停止（仅监控的服务即使加了也会被拒绝）
        #[arg(long)]
        force: bool,
    },
    /// 重启（先停再启）
    Restart { id: String },
    /// 删除（还有隧道在用会拒绝；要确认）
    #[command(after_help = "例：
  cli service remove <服务id> --yes   # --yes = 不再询问 y/N（脚本 / agent 使用）")]
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
        /// 云端端口（固定值，不会自动变化）
        #[arg(long = "remote-port")]
        remote_port: u16,
        /// 隧道名字（不填 = 使用「服务名 发布」）
        #[arg(long)]
        name: Option<String>,
        /// 公网网址（可空）
        #[arg(long)]
        url: Option<String>,
        /// 访问保护，可叠加：--access password --access ip（也可写成 --access password,ip）
        /// 仅支持 password / ip / none，拼写错误会直接报错；不加 = 不启用访问保护
        #[arg(long, value_delimiter = ',', action = clap::ArgAction::Append)]
        access: Vec<String>,
        #[arg(long = "access-user")]
        access_user: Option<String>,
        /// 访问密码（本机计算 bcrypt 哈希后写入服务器密码文件，密码本身不写入磁盘）
        #[arg(long = "access-password")]
        access_password: Option<String>,
        /// 只允许这些 IP（access 含 ip 时用，例 "1.2.3.4,5.6.7.8"）
        #[arg(long = "access-ips")]
        access_ips: Option<String>,
        /// 发布预设：web（默认，普通网站）/ ws（WebSocket）/ upload（上传大文件）
        #[arg(long)]
        preset: Option<String>,
    },
    /// 改：只写需要修改的字段，其余保持不变
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
        /// 发布预设：web / ws / upload；不传 = 保持原来的
        #[arg(long)]
        preset: Option<String>,
    },
    /// 核对云端 nginx 上这条隧道实际启用的访问保护（通过 SSH 只读读取配置）
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
    /// 删除（要确认）
    #[command(after_help = "例：
  cli tunnel remove <隧道id> --yes")]
    Remove {
        id: String,
        /// 跳过 y/N 确认，直接删（脚本 / agent 用）
        #[arg(long)]
        yes: bool,
    },
    /// 重置服务器指纹（服务器重装过 / 换过机器时用；要确认）
    #[command(after_help = "例：
  cli tunnel forget-key <隧道id> --yes")]
    ForgetKey {
        /// 这条隧道对应的服务器会被重新信任，然后隧道重新连
        id: String,
        /// 跳过 y/N 确认，直接重置（脚本 / agent 用）
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// 导出设置（默认不含密码哈希、不含密钥路径）
    #[command(after_help = "例：
  cli config export --out 备份.json
  cli config export --json          # 直接获取 JSON（供机器使用）
  cli config export --with-paths    # 同时导出密钥路径（仍然不含密码哈希）")]
    Export {
        /// 保存位置；不写 = 直接打印（自行保存为 .json 即为备份）
        #[arg(long)]
        out: Option<String>,
        /// 同时导出密钥路径（仍然不含密码哈希）
        #[arg(long = "with-paths")]
        with_paths: bool,
    },
    /// 导入设置（会修改设置；交互终端会询问 y/N，非交互需加 --yes；--dry-run 仅检查）
    #[command(after_help = "例：
  cli config import 备份.json --dry-run              # 仅查看将修改的内容，不写盘
  cli config import 备份.json --yes                  # 合并导入（默认）
  cli config import 备份.json --mode replace --yes   # 整份替换（会先自动备份）")]
    Import {
        file: String,
        /// merge（默认，按 id 合并）或 replace（整份替换）
        #[arg(long, default_value = "merge")]
        mode: String,
        /// 仅检查、不写盘；无需确认
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// 跳过 y/N 确认（脚本 / agent 用）
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
        /// 密钥文件（不填 = 使用系统默认密钥）
        #[arg(long)]
        key: Option<String>,
        /// 站点目录：面板将网站放在服务器哪个目录（宝塔填 /www/server/panel/vhost；不填 = 按小皮默认位置查找）
        #[arg(long = "vhost-dir")]
        vhost_dir: Option<String>,
    },
    /// 改：只写需要修改的字段
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
        /// 站点目录：不修改则不写（会保留原值）；如需清空请写 --vhost-dir ""
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
    /// 使用密码登录一次，自动配置免密登录（密码从标准输入读取，不回显、不进入命令行历史）
    #[command(after_help = "例：
  echo 服务器密码 | cli server setup-key <服务器id>   # 通过管道传入密码，同样不会显示在屏幕上
  cli server setup-key <服务器id>                     # 手动输入时同样不回显")]
    SetupKey { id: String },
    /// 生成访问密码哈希（bcrypt，本机计算，供 nginx 使用；密码从标准输入读取）
    HashPassword { id: String },
}

#[derive(Subcommand)]
enum AppCmd {
    /// 启动应用（在托盘里静默运行）
    Start,
    /// 查看应用是否运行
    Status,
    /// 停止全部发布隧道和应用启动的本机服务（升级 / 卸载前使用；需要确认）
    #[command(after_help = "例：
  cli app stop-all --yes   # 安装器升级/卸载前调用：先停止服务和隧道，再让安装程序操作 app.exe
  应用未运行时：什么都不会做，退出码 0（安装器脚本无需处理失败）")]
    StopAll {
        /// 跳过 y/N 确认，直接停止（脚本 / agent / 安装器使用）
        #[arg(long)]
        yes: bool,
    },
    /// 检查是否有新版本（仅检查，不下载、不安装）
    UpdateCheck,
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
            eprintln!("下一步：cli <命令> --help 查看该命令的示例；cli guide 查看速查总表。");
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
                    serde_json::to_string_pretty(&json!({"ok": false, "error": shown_error(&e)})).unwrap()
                );
            } else {
                eprintln!("✗ {}", shown_error(&e));
            }
            std::process::exit(exit_code_for_error(&e));
        }
    }
}

/// 需要应用、但应用没在跑的错：带这个内部前缀，退出码映射成 3，打印前剥掉。
const OFFLINE_ERR: &str = "\u{1}离线\u{1}";

fn offline_error(msg: &str) -> String {
    format!("{OFFLINE_ERR}{msg}")
}

fn is_offline_error(msg: &str) -> bool {
    msg.starts_with(OFFLINE_ERR) || msg.starts_with(ctl::APP_NOT_RUNNING)
}

/// 打印前剥掉内部前缀（用户看不到任何标记）。
fn shown_error(msg: &str) -> &str {
    msg.strip_prefix(OFFLINE_ERR).unwrap_or(msg)
}

/// 退出码：命令本身的问题 → 1；需要应用而应用没在跑 → 3（0/2 由别处给）。
fn exit_code_for_error(msg: &str) -> i32 {
    if is_offline_error(msg) {
        3
    } else {
        1
    }
}

fn run(cli: &Cli) -> Result<i32, String> {
    match &cli.cmd {
        Cmd::Status => {
            let snap = snapshot()?;
            if cli.json {
                print_ok(&snap);
            } else {
                print_status(&snap);
            }
        }
        Cmd::Services => {
            let snap = snapshot()?;
            if cli.json {
                print_ok(&snap["services"]);
            } else {
                print_services(&snap);
            }
        }
        Cmd::Tunnels => {
            let snap = snapshot()?;
            if cli.json {
                print_ok(&snap["tunnels"]);
            } else {
                print_tunnels(&snap);
            }
        }
        Cmd::Servers => {
            let snap = snapshot()?;
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
        Cmd::Doctor { deep, strict } => return doctor_cmd(*deep, *strict, cli.json),
        Cmd::Diag { out, deep } => diag_cmd(out.as_deref(), *deep, cli.json)?,
        Cmd::Stats => stats_cmd(cli.json)?,
        Cmd::Logs {
            id,
            kind,
            source,
            format,
            archive,
            lines,
        } => logs_cmd(
            id.as_deref(),
            kind,
            source.as_deref(),
            format,
            *archive,
            *lines,
            cli.json,
        )?,
        Cmd::Undo { target } => undo_cmd(target.as_deref(), cli.json)?,
        Cmd::Nginx { id } => {
            let v = call("get_nginx_snippet", json!({"id": id}))?;
            if cli.json {
                print_ok(&v);
            } else {
                if let Some(s) = v["snippet"].as_str() {
                    println!("将以下内容添加到站点配置中（不要整段替换）：\n{s}");
                }
                if let Some(e) = v["acmeExempt"].as_str() {
                    println!("\n证书自动续期用的一行（放入 location /.well-known/ 中）：\n{e}");
                }
                if let Some(h) = v["htpasswd"].as_str() {
                    println!("\n# 密码文件内容（保存到服务器上，路径见以下步骤）：\n{h}");
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
        Cmd::Config { sub } => config_cmd(sub, cli.json)?,
        Cmd::Methods => {
            let v = call("methods", json!({}))?;
            if cli.json {
                print_ok(&v);
            } else {
                println!("可调用的方法（cli call <方法> '<参数JSON>'）：\n");
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
                    "参数不是合法 JSON：{e}\n参数需用单引号包裹，例如：cli call status '{{}}'"
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
            done(want_json, &v, &format!("✓ 已修改本机服务「{id}」"));
        }
        ServiceCmd::Start { id } => {
            let v = call("service_start", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已启动「{id}」（稍后可用 cli services 查看结果）"));
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
                        "未能停止「{id}」：应用仍要求确认（端口占用者可能已变化，先 cli services 查看后再试）"
                    ));
                }
                // 兜底：只监控的服务（没配启动方式）绝不接管外部进程，
                // 这里不再提示一条必然失败的 --force（core 现在会直接返回人话错误）。
                let svc = find_service(id)?;
                if svc["launchExe"].is_null() {
                    return Err(format!(
                        "这台服务未配置启动方式，不是本应用启动的程序，不会停止它。\n如需接管，请先补全启动方式再停止：cli service edit {id} --exe <程序完整路径>"
                    ));
                }
                let exe = v["exe"].as_str().unwrap_or("一个不是本应用启动的程序");
                let pid = num_of(&v, "pid");
                return Err(format!(
                    "这台服务不是本应用启动的，正在监听的是 {exe}（PID {pid}）。\n停止会实际结束该进程。确认要停止请加 --force：cli service stop {id} --force"
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
                        "「{id}」是只监控的服务（未配置启动方式），重启不会改动端口上的程序。\n如需接管，请先用 cli service edit {id} --exe <程序完整路径> 补全启动方式，然后重试"
                    ));
                }
                let exe = v["exe"].as_str().unwrap_or("一个不是本应用启动的程序");
                let pid = num_of(&v, "pid");
                return Err(format!(
                    "「{id}」当前监听的进程不是本应用启动的（{exe}，PID {pid}），重启不会结束该进程。\n如需停止后重启：先 cli service stop {id} --force，再 cli service start {id}"
                ));
            }
            done(want_json, &v, &format!("✓ 已重启「{id}」"));
        }
        ServiceCmd::Remove { id, yes } => {
            let svc = find_service(id)?;
            let name = str_of(&svc, "name");
            if !confirm_destructive(
                &format!(
                    "将删除本机服务「{name}」（{id}）。删除后需要重新登记；已有隧道在使用时，应用会拒绝删除。"
                ),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("remove_service", json!({"id": id}))?;
            done(
                want_json,
                &v,
                &format!("✓ 已删除本机服务「{name}」（{id}）\n（10 分钟内可以撤销：cli undo last）"),
            );
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
            preset,
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
            if let Some(p) = preset {
                config::check_preset_strict(p)?;
            }
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
                    "preset": preset,
                }),
            )?;
            done(
                want_json,
                &v,
                &format!(
                    "✓ 已创建隧道「{name}」并开始发布（id={}）\n  {}",
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
            preset,
        } => {
            let old = find_tunnel(id)?;
            if let Some(p) = preset {
                config::check_preset_strict(p)?;
            }
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
                    "preset": preset.clone().or_else(|| opt_str(&old, "preset")),
                }),
            )?;
            done(
                want_json,
                &v,
                &format!(
                    "✓ 已修改隧道「{id}」（会短暂断开后自动重连）\n  {}",
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
                &format!("将暂停隧道「{name}」（{id}）。暂停后该网址将无法访问。"),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("tunnel_stop", json!({"id": id}))?;
            done(want_json, &v, &format!("✓ 已暂停「{name}」（{id}）"));
        }
        TunnelCmd::Remove { id, yes } => {
            let t = find_tunnel(id)?;
            let name = str_of(&t, "name");
            if !confirm_destructive(
                &format!("将删除隧道「{name}」（{id}）。删除后发布停止，配置同时删除。"),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("remove_tunnel", json!({"id": id}))?;
            done(
                want_json,
                &v,
                &format!("✓ 已删除隧道「{name}」（{id}）\n（10 分钟内可以撤销：cli undo last）"),
            );
        }
        TunnelCmd::ForgetKey { id, yes } => {
            let t = find_tunnel(id)?;
            let name = str_of(&t, "name");
            let server = str_of(&t, "serverName");
            if !confirm_destructive(
                &format!(
                    "将重新信任服务器「{server}」的新指纹（隧道「{name}」，{id}）。\n仅在确认服务器确实重装过或更换过机器时操作；操作后这条隧道会重新连接。"
                ),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("forget_host_key", json!({"id": id}))?;
            let msg = v["message"]
                .as_str()
                .unwrap_or("已清除旧指纹，下次连接会重新记录。");
            done(want_json, &v, &format!("✓ {msg}"));
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
                    "✓ 已添加服务器「{name}」（id={}）\n  未配置密钥：cli server setup-key {}",
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
            done(want_json, &v, &format!("✓ 已修改服务器「{id}」"));
        }
        ServerCmd::Remove { id, yes } => {
            let srv = find_server(id)?;
            let name = str_of(&srv, "name");
            if !confirm_destructive(
                &format!("将删除服务器「{name}」（{id}）。仍有隧道使用时会拒绝删除。"),
                *yes,
            )? {
                return cancelled(want_json);
            }
            let v = call("delete_server", json!({"id": id}))?;
            done(
                want_json,
                &v,
                &format!("✓ 已删除服务器「{name}」（{id}）\n（10 分钟内可以撤销：cli undo last）"),
            );
        }
        ServerCmd::SetupKey { id } => {
            let pw = read_secret("请输入这台服务器的登录密码（不会保存、不会写入日志）：")?;
            let v = call(
                "setup_passwordless",
                json!({"serverId": id, "password": pw}),
            )?;
            done(
                want_json,
                &v,
                v["message"].as_str().unwrap_or("✓ 已配置完成"),
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
                    "hint": "启动方式：cli app start",
                }),
            };
            if want_json {
                print_ok(&v);
            } else if running.is_some() {
                println!("✓ 应用运行中（进程号 {}，控制端口 {}）", v["pid"], v["port"]);
            } else {
                println!("✗ 应用未运行（offline）。启动方式：cli app start");
            }
            // 没在跑就不算成功：退出码 3（脚本可以直接 `cli app status || cli app start`）
            return Ok(if running.is_some() { 0 } else { 3 });
        }
        AppCmd::Start => {
            if let Some(info) = ctl::app_running(&dir) {
                if want_json {
                    print_ok(&json!({"running": true, "offline": false, "pid": info.pid}));
                } else {
                    println!("✓ 应用已在运行（进程号 {}）", info.pid);
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
            return Err("应用已启动但控制通道未连通（请查看托盘图标，或手动打开应用）".into());
        }
        AppCmd::UpdateCheck => {
            let v = match call("check_update", json!({})) {
                Ok(v) => v,
                Err(e) if is_offline_error(&e) => {
                    return Err(offline_error("应用未运行；请先执行 cli app start 再检查更新"));
                }
                Err(e) => return Err(e),
            };
            if want_json {
                print_ok(&v);
            } else {
                print_update(&v);
            }
        }
        AppCmd::StopAll { yes } => return stop_all_cmd(*yes, want_json),
    }
    Ok(0)
}

/// `cli app stop-all`：安装 / 卸载前把隧道和本机服务都停掉（D5 §5.3）。
///
/// - 应用没在跑：没东西可停，打印一句人话、退出码 0（安装器会忽略失败，不能卡它）。
/// - 非交互必须 `--yes`（跟别的破坏性命令一个规矩）；交互会问 y/N。
/// - 总耗时硬上限 15 秒：超时就先返回 0 让安装器继续，应用那头会自己把剩下的停完。
fn stop_all_cmd(yes: bool, want_json: bool) -> Result<i32, String> {
    let dir = default_data_dir();
    // 连 ctl.json 都没有：不用问也不用连
    if ctl::read_ctl_info(&dir).is_none() {
        stop_all_nothing(want_json);
        return Ok(0);
    }
    if !confirm_destructive(
        "将停止全部发布隧道和应用启动的本机服务（安装 / 卸载前会执行此操作）。",
        yes,
    )? {
        // 确认里选「不」：什么都没做，退出码 0
        return Ok(0);
    }

    // 15 秒硬上限：把请求丢给一条线程，主线程最多等 15 秒。
    // 超时就直接走人（进程退出会断开连接）；应用收到请求后会自己把该停的停完。
    let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
    std::thread::spawn(move || {
        let _ = tx.send(call("stop_all", json!({})));
    });
    let result = match rx.recv_timeout(std::time::Duration::from_secs(15)) {
        Ok(result) => result,
        Err(_) => {
            if want_json {
                print_ok(&json!({
                    "running": true,
                    "timedOut": true,
                    "message": "等待 15 秒仍未停止完成；请先让安装/卸载继续，应用会停止剩余内容。",
                }));
            } else {
                println!("等待 15 秒仍未停止完成；安装/卸载可继续，应用会停止剩余内容。");
            }
            return Ok(0);
        }
    };
    let v = match result {
        Ok(v) => v,
        // 这 15 秒里应用刚好退出：也算没东西可停
        Err(e) if is_offline_error(&e) => {
            stop_all_nothing(want_json);
            return Ok(0);
        }
        Err(e) => return Err(e),
    };
    if want_json {
        print_ok(&v);
    } else {
        println!("{}", stop_all_summary_text(&v));
    }
    Ok(0)
}

/// stop_all 结果 → 人话汇总。
fn stop_all_summary_text(v: &Value) -> String {
    let tunnels = num_of(v, "tunnelsStopped");
    let tunnels_failed = num_of(v, "tunnelsFailed");
    let services = num_of(v, "servicesStopped");
    let services_failed = num_of(v, "servicesFailed");
    let mut text = format!("✓ 已停止 {tunnels} 条发布隧道、{services} 个本机服务。");
    if tunnels_failed + services_failed > 0 {
        text.push_str(&format!(
            "\n还有 {tunnels_failed} 条隧道、{services_failed} 个服务未能停止（可能是脚本启动或已被其它程序占用）；如需操作 app.exe，请先手动处理。"
        ));
    }
    text
}

/// stop-all 的「应用没在跑」统一输出（退出码由调用方给 0）。
fn stop_all_nothing(want_json: bool) {
    if want_json {
        print_ok(&json!({
            "running": false,
            "stopped": false,
            "message": "应用未运行，没有需要停止的内容",
        }));
    } else {
        println!("应用未运行，没有需要停止的内容。");
    }
}

/// 更新检查的人话输出（有新版额外一行下载地址）。
fn print_update(v: &Value) {
    match str_of(v, "status").as_str() {
        "available" => {
            let latest = opt_str(v, "latest").unwrap_or_else(|| "?".into());
            println!("发现新版本 {latest}，下载：{}", opt_str(v, "url").unwrap_or_default());
        }
        "upToDate" => println!("✓ 已是最新版本（{}）", opt_str(v, "latest").unwrap_or_default()),
        "error" => println!(
            "本次未能连接更新服务器：{}",
            opt_str(v, "error").unwrap_or_else(|| "稍后再试".into())
        ),
        _ => println!("尚未执行更新检查；请重试：cli app update-check"),
    }
}

// ---------- 体检 / 诊断包 / 统计 / 日志 / 撤销 / 配置备份 ----------

/// `cli doctor`：应用在跑就让应用体检；没在跑就本地跑能跑的那几步（D2 §14.4）。
/// `--strict`：有没过 → 退出码 1（这不算命令错误，是「检查没过」）。
fn doctor_cmd(deep: bool, strict: bool, want_json: bool) -> Result<i32, String> {
    let mode = if deep { "deep" } else { "quick" };
    let report = match call("doctor", json!({"mode": mode})) {
        Ok(v) => v,
        Err(e) if is_offline_error(&e) => offline_doctor(deep),
        Err(e) => return Err(e),
    };
    if want_json {
        print_ok(&report);
    } else {
        print_doctor(&report);
    }
    let fails = report["summary"]["fail"].as_u64().unwrap_or(0);
    if strict && fails > 0 {
        Ok(1)
    } else {
        Ok(0)
    }
}

fn doctor_mark(status: &str) -> &'static str {
    match status {
        "pass" => "✓",
        "warn" => "!",
        "fail" => "✗",
        "skip" => "·",
        _ => "?",
    }
}

/// 离线体检：直接复用 core::doctor::run，跟应用里跑的是同一套 9 步。
/// deep 时 cloud / dns 会真连服务器、查域名；跑完 core 自己写 doctor-last.json、发 doctorRan。
fn offline_doctor(deep: bool) -> Value {
    offline_doctor_in(&default_data_dir(), deep)
}

/// offline_doctor 的可测版本：数据目录从参数来，单测能指到临时目录。
fn offline_doctor_in(dir: &Path, deep: bool) -> Value {
    let config_path = dir.join("config.json");
    let (config, config_error) = match read_config_for_doctor(&config_path) {
        Ok(cfg) => (Some(cfg), None),
        Err(e) => (None, Some(e)),
    };
    let mode = if deep {
        DoctorMode::Deep
    } else {
        DoctorMode::Quick
    };
    let req = DoctorRequest {
        mode,
        app_running: false,
        data_dir: dir.to_path_buf(),
        config_trusted: config_error.is_none(),
        config,
        config_error,
        events: Some(Arc::new(EventLog::new(dir))),
        ..DoctorRequest::default()
    };
    let report = doctor::run(&req);
    serde_json::to_value(&report)
        .unwrap_or_else(|e| json!({"error": format!("体检报告生成失败：{e}")}))
}

/// 体检用只读读配置：不迁移、不写盘（doctor 不自动改任何东西）。
fn read_config_for_doctor(path: &Path) -> Result<AppConfig, String> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    if let Some(v) = config::config_version_too_new(path) {
        return Err(config::config_unsupported_message(v));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("设置文件无法读取：{e}"))?;
    serde_json::from_str::<AppConfig>(&text).map_err(|e| format!("设置文件无法读取：{e}"))
}

fn print_doctor(r: &Value) {
    let mode_word = if str_of(r, "mode") == "deep" {
        "深度检查"
    } else {
        "快速检查"
    };
    println!("「上线助手」体检（{mode_word}）");
    for s in r["steps"].as_array().cloned().unwrap_or_default() {
        let mark = doctor_mark(str_of(&s, "status").as_str());
        println!(
            "{mark} {} {}",
            pad(&str_of(&s, "title"), 12),
            str_of(&s, "detail")
        );
        let advice = str_of(&s, "advice");
        if !advice.is_empty() {
            println!("    处理建议：{advice}");
        }
    }
    let sm = &r["summary"];
    let (pass, warn, fail, skip) = (
        num_of(sm, "pass"),
        num_of(sm, "warn"),
        num_of(sm, "fail"),
        num_of(sm, "skip"),
    );
    let skip_note = if skip > 0 {
        format!("，{skip} 项跳过")
    } else {
        String::new()
    };
    if fail > 0 {
        println!("✗ 体检有 {fail} 项未通过{skip_note}");
    } else if warn > 0 {
        println!("! 体检基本正常：{pass} 项通过，{warn} 项需注意{skip_note}");
    } else {
        println!("✓ 体检全部通过：{pass} 项正常{skip_note}");
    }
}

/// `cli diag`：让应用打个诊断包（要应用在跑；离线退出码 3）。
fn diag_cmd(out: Option<&str>, deep: bool, want_json: bool) -> Result<(), String> {
    let mut params = json!({"deep": deep});
    if let Some(o) = out {
        params["out"] = json!(abs_out_path(o));
    }
    let v = call("build_diag", params)?;
    if want_json {
        print_ok(&v);
    } else {
        println!(
            "✓ 诊断包已生成：{}（{}，已脱敏）",
            str_of(&v, "path"),
            fmt_bytes(num_of(&v, "bytes"))
        );
        println!("  请自行将此文件发给排查人员；包内不含密码、密钥和通行证。");
    }
    Ok(())
}

fn stats_cmd(want_json: bool) -> Result<(), String> {
    let v = call("stats", json!({}))?;
    if want_json {
        print_ok(&v);
    } else {
        print_stats(&v);
    }
    Ok(())
}

fn print_stats(v: &Value) {
    let app = &v["app"];
    println!(
        "「上线助手」{}（进程 {}）已运行 {}",
        opt_str(app, "appVersion").unwrap_or_else(|| "?".into()),
        opt_str(app, "pid").unwrap_or_else(|| "?".into()),
        fmt_duration(num_of(app, "uptimeSecs"))
    );
    let svc = &v["services"];
    println!(
        "本机服务：{} 台，{} 台运行中，{} 台未启动，{} 台状态未知",
        num_of(svc, "total"),
        num_of(svc, "up"),
        num_of(svc, "down"),
        num_of(svc, "unknown")
    );
    let t = &v["tunnels"];
    let mut line = format!(
        "发布隧道：{} 条（{} 条已启用），{} 条发布中",
        num_of(t, "total"),
        num_of(t, "enabled"),
        num_of(t, "running")
    );
    if num_of(t, "retrying") > 0 {
        line.push_str(&format!("，{} 条重试中", num_of(t, "retrying")));
    }
    line.push_str(&format!("，今日重连 {} 次", num_of(t, "reconnectsToday")));
    println!("{line}");
    for item in t["items"].as_array().cloned().unwrap_or_default() {
        if let Some(secs) = item["retryInSecs"].as_u64() {
            println!(
                "  ·「{}」第 {} 次连接失败，{}后重试",
                str_of(&item, "name"),
                num_of(&item, "attempt").max(1),
                fmt_duration_short(secs)
            );
        }
    }
    let u = &v["update"];
    match str_of(u, "status").as_str() {
        "available" => println!(
            "更新：有新版本 {}，下载：{}",
            opt_str(u, "latest").unwrap_or_default(),
            opt_str(u, "url").unwrap_or_default()
        ),
        "upToDate" => println!("更新：已是最新版本"),
        "error" => println!(
            "更新：上次检查未能连接更新服务器（{}）",
            opt_str(u, "error").unwrap_or_default()
        ),
        _ => println!("更新：尚未检查（可执行 cli app update-check）"),
    }
}

/// 把 --source / --kind / id 归一成真正要调 logs_query 的 source 与 id。
fn resolve_logs_source(
    source: Option<&str>,
    id: Option<&str>,
    kind: &str,
) -> Result<(String, Option<String>), String> {
    match source {
        Some("all") => Ok(("all".into(), None)),
        Some("app") => Ok(("app".into(), None)),
        Some("tunnel") => match id {
            Some(i) => Ok(("tunnel".into(), Some(i.to_string()))),
            None => Err(
                "--source tunnel 需要指定 id：cli logs <隧道id> --source tunnel".into(),
            ),
        },
        Some("service") => match id {
            Some(i) => Ok(("service".into(), Some(i.to_string()))),
            None => Err(
                "--source service 需要指定 id：cli logs <服务id> --source service".into(),
            ),
        },
        Some(other) => Err(format!(
            "--source 仅支持 all / tunnel / service / app，当前值为「{other}」。\n例：cli logs --source all --format json"
        )),
        None => {
            if kind != "tunnel" && kind != "service" {
                return Err(format!(
                    "--kind 仅支持 tunnel（隧道日志）或 service（服务日志），当前值为「{kind}」。\n查看隧道日志：cli logs <隧道id>；查看服务日志：cli logs <服务id> --kind service"
                ));
            }
            match id {
                Some(i) => Ok((kind.to_string(), Some(i.to_string()))),
                None => Ok(("all".into(), None)),
            }
        }
    }
}

/// --archive 越界 → 退出码 1，文案固定。
fn check_archive(archive: i64) -> Result<u8, String> {
    match archive {
        0..=2 => Ok(archive as u8),
        _ => Err("--archive 只能是 0、1、2；0 = 仅查看当前文件".into()),
    }
}

fn logs_cmd(
    id: Option<&str>,
    kind: &str,
    source: Option<&str>,
    format: &str,
    archive: i64,
    lines: usize,
    want_json: bool,
) -> Result<(), String> {
    let archive = check_archive(archive)?;
    if format != "lines" && format != "json" {
        return Err(format!(
            "--format 仅支持 lines（文本格式）或 json（一行一条 JSON），当前值为「{format}」"
        ));
    }
    let (source, id) = resolve_logs_source(source, id, kind)?;
    // id 存在性先查（报错信息更人话；离线时用配置对照）
    if let Some(id) = &id {
        match source.as_str() {
            "tunnel" => {
                find_tunnel(id)?;
            }
            "service" => {
                find_service(id)?;
            }
            _ => {}
        }
    }
    let v = call(
        "logs_query",
        json!({"source": source, "id": id, "lines": lines, "archive": archive, "format": format}),
    )?;
    if format == "json" {
        if want_json {
            print_ok(&v);
        } else {
            // JSONL：一 entry 一行（给管道用；没有信封）
            for e in v["entries"].as_array().cloned().unwrap_or_default() {
                println!("{}", serde_json::to_string(&e).unwrap_or_default());
            }
        }
    } else if want_json {
        // 旧形状别破：result 里先给 text
        print_ok(&json!({"text": v["text"].as_str().unwrap_or("")}));
    } else {
        println!("{}", v["text"].as_str().unwrap_or(""));
        if bool_of(&v, "truncated") {
            eprintln!("（日志过多，仅显示最近 {lines} 条；查看上一份备份请加 --archive 1）");
        }
    }
    Ok(())
}

fn expired_undo_msg() -> String {
    "此撤销已过期（删除已超过 10 分钟）；如需恢复只能重新创建".to_string()
}

fn undo_type_word(t: &str) -> &'static str {
    match t {
        "tunnel" => "隧道",
        "service" => "本机服务",
        "server" => "服务器",
        _ => "对象",
    }
}

fn undo_cmd(target: Option<&str>, want_json: bool) -> Result<(), String> {
    if target == Some("list") {
        return undo_list(want_json);
    }
    // 撤销要改配置：应用没在跑就不动手（退出码 3）。
    let listing = match call("list_deleted", json!({})) {
        Ok(v) => v,
        Err(e) if is_offline_error(&e) => {
            return Err(offline_error(
                "撤销需要修改设置，请先启动应用：执行 cli app start 后再执行 cli undo",
            ))
        }
        Err(e) => return Err(e),
    };
    let token = match target {
        None | Some("last") => latest_undo_token(&listing).ok_or_else(expired_undo_msg)?,
        Some(t) => {
            let found = listing["items"]
                .as_array()
                .map(|a| a.iter().any(|i| i["token"] == t))
                .unwrap_or(false);
            if !found {
                return Err(expired_undo_msg());
            }
            t.to_string()
        }
    };
    let v = call("restore_deleted", json!({"token": token}))?;
    if want_json {
        print_ok(&v);
        return Ok(());
    }
    let restored = &v["restored"];
    let typ = str_of(restored, "type");
    let id = str_of(restored, "id");
    let tail = match typ.as_str() {
        "tunnel" => format!("（隧道保持暂停状态；如需继续发布请点击「开始发布」或执行 cli tunnel start {id}）"),
        "service" => "（不会自动启动；如需使用请点击「启动」）".to_string(),
        _ => String::new(),
    };
    println!("✓ 已恢复「{}」{tail}", str_of(restored, "name"));
    Ok(())
}

fn undo_list(want_json: bool) -> Result<(), String> {
    let listing = match call("list_deleted", json!({})) {
        Ok(v) => v,
        // 离线也能看：state.json 是只读件（真撤销还是要等应用跑起来）
        Err(e) if is_offline_error(&e) => offline_undo_list(),
        Err(e) => return Err(e),
    };
    if want_json {
        print_ok(&listing);
        return Ok(());
    }
    let items = listing["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("没有可撤销的删除记录（删除后 10 分钟内可撤销，最多保留 20 条）。");
        return Ok(());
    }
    println!("可撤销的删除记录（删除后 10 分钟内有效，最多保留 20 条）：");
    let now = shangxian_core::state::now_ms();
    for it in &items {
        println!(
            "  {}  {}「{}」  剩余可撤销时间 {}",
            str_of(it, "token"),
            undo_type_word(&str_of(it, "type")),
            str_of(it, "name"),
            fmt_remaining(i64_of(it, "expiresAt"), now)
        );
    }
    Ok(())
}

/// 离线读 state.json 的墓碑（纯读，不碰文件）。
fn offline_undo_list() -> Value {
    let dir = default_data_dir();
    let text = std::fs::read_to_string(dir.join("state.json")).unwrap_or_default();
    let state: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let now = shangxian_core::state::now_ms();
    let items: Vec<Value> = state["trash"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|t| i64_of(t, "expiresAt") > now)
        .map(|t| {
            json!({
                "token": str_of(t, "token"),
                "type": str_of(t, "kind"),
                "id": t["item"]["id"].as_str().unwrap_or(""),
                "name": t["item"]["name"].as_str().unwrap_or(""),
                "deletedAt": i64_of(t, "deletedAt"),
                "expiresAt": i64_of(t, "expiresAt"),
            })
        })
        .collect();
    json!({"items": items})
}

fn latest_undo_token(listing: &Value) -> Option<String> {
    listing["items"]
        .as_array()?
        .iter()
        .filter(|i| i["token"].as_str().is_some())
        .max_by_key(|i| i64_of(i, "deletedAt"))
        .and_then(|i| i["token"].as_str().map(str::to_string))
}

fn config_cmd(sub: &ConfigCmd, want_json: bool) -> Result<(), String> {
    match sub {
        ConfigCmd::Export { out, with_paths } => export_cmd(out.as_deref(), *with_paths, want_json),
        ConfigCmd::Import {
            file,
            mode,
            dry_run,
            yes,
        } => import_cmd(file, mode, *dry_run, *yes, want_json),
    }
}

fn export_cmd(out: Option<&str>, with_paths: bool, want_json: bool) -> Result<(), String> {
    let redacted = !with_paths;
    let mut params = json!({"redacted": redacted});
    if let Some(o) = out {
        params["out"] = json!(abs_out_path(o));
    }
    let v = match call("export_config", params) {
        Ok(v) => v,
        // 导出是纯读：应用没在跑也能导（离线读 config.json）
        Err(e) if is_offline_error(&e) => offline_export(redacted, out)?,
        Err(e) => return Err(e),
    };
    if want_json {
        print_ok(&v);
    } else {
        print_export(&v, with_paths);
    }
    Ok(())
}

/// 离线导出：读 config.json，按导出白名单重建。
/// 版本高于程序支持时不拒绝（D2 §13 的自救路径）：原文按已知键脱敏后照导。
fn offline_export(redacted: bool, out: Option<&str>) -> Result<Value, String> {
    offline_export_from(&default_data_dir(), redacted, out)
}

/// 离线导出（可测版）：`dir` 是数据目录。
/// 第一次运行还没有 config.json 时导出一份默认设置（空备份），
/// 不再报含糊的「设置文件读不出来」。
fn offline_export_from(dir: &Path, redacted: bool, out: Option<&str>) -> Result<Value, String> {
    let path = dir.join("config.json");
    let (config_value, config_version) = if !path.exists() {
        let cfg = AppConfig::default();
        (
            shangxian_core::import::config_to_export_value(&cfg, redacted, cfg.version),
            cfg.version,
        )
    } else {
        match AppConfig::load(&path) {
            Ok(cfg) => (
                shangxian_core::import::config_to_export_value(&cfg, redacted, cfg.version),
                cfg.version,
            ),
            Err(_) => {
                let found = config::config_version_too_new(&path)
                    .ok_or_else(|| format!("读取设置失败：{} 无法读取", path.display()))?;
                let text =
                    std::fs::read_to_string(&path).map_err(|e| format!("读取设置失败：{e}"))?;
                let raw: Value = serde_json::from_str(&text)
                    .map_err(|e| format!("设置文件不是一份合法 JSON：{e}"))?;
                (scrub_raw_config(&raw, redacted), found)
            }
        }
    };
    let doc = json!({
        "kind": "shangxian.config-export",
        "formatVersion": 1,
        "exportedAt": shangxian_core::state::now_ms(),
        "appVersion": shangxian_core::import::app_version(),
        "configVersion": config_version,
        "redacted": redacted,
        "config": config_value.clone(),
    });
    let mut result = json!({"redacted": redacted, "config": config_value});
    if let Some(o) = out.map(str::trim).filter(|o| !o.is_empty()) {
        let target = PathBuf::from(abs_out_path(o));
        shangxian_core::import::write_json_atomic(&target, &doc)
            .map_err(|e| format!("导出失败：{e}"))?;
        result["path"] = json!(target.display().to_string());
    }
    Ok(result)
}

/// 把用户给的相对路径按命令行的当前目录补成绝对路径：
/// 应用（ctl）的工作目录跟这里不一定一样，直接传相对路径会写到别处去。
fn abs_out_path(out: &str) -> String {
    let p = Path::new(out.trim());
    if p.is_absolute() {
        return p.display().to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p).display().to_string(),
        Err(_) => p.display().to_string(),
    }
}

/// 按已知键脱敏一份「版本比程序新」的配置原文（accessHash 永不出，keyPath 看开关）。
fn scrub_raw_config(v: &Value, redacted: bool) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                match k.as_str() {
                    "accessHash" => {
                        out.insert("hasPassword".into(), json!(!val.is_null()));
                    }
                    "keyPath" if redacted => {
                        out.insert("keyPathPresent".into(), json!(!val.is_null()));
                    }
                    _ => {
                        out.insert(k.clone(), scrub_raw_config(val, redacted));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|i| scrub_raw_config(i, redacted)).collect())
        }
        other => other.clone(),
    }
}

fn print_export(v: &Value, with_paths: bool) {
    if let Some(path) = opt_str(v, "path") {
        let extra = if with_paths {
            "含密钥路径，不含密码哈希"
        } else {
            "已脱敏：不含密码哈希、不含密钥路径"
        };
        println!("✓ 设置已导出到 {path}（{extra}）");
        println!("  导回方式：cli config import {path}");
        return;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&v["config"]).unwrap_or_default()
    );
    if std::io::stdout().is_terminal() {
        println!();
        println!("将以上全部内容保存为 .json 文件即为一份备份；导回方式：cli config import <文件>");
        if !with_paths {
            println!("（默认已脱敏：不含密码哈希、不含密钥路径）");
        }
    }
}

/// 导入前要不要人确认：--dry-run / --yes 都不用；非交互又没有 --yes → 拒绝（退出码 1）。
fn import_needs_confirm(dry_run: bool, yes: bool, interactive: bool) -> Result<bool, String> {
    if dry_run || yes {
        return Ok(false);
    }
    if !interactive {
        return Err(
            "导入会修改设置；当前不是交互终端（脚本 / agent），必须加 --yes 才能执行。\n如需先查看将修改的内容，请加 --dry-run。"
                .into(),
        );
    }
    Ok(true)
}

fn import_cmd(file: &str, mode: &str, dry_run: bool, yes: bool, want_json: bool) -> Result<(), String> {
    let mode = shangxian_core::import::ImportMode::parse(mode)?;
    let path = PathBuf::from(abs_out_path(file));
    if !path.is_file() {
        return Err(format!("找不到这个文件：{}", path.display()));
    }
    let meta = std::fs::metadata(&path).map_err(|e| format!("无法读取此文件：{e}"))?;
    if meta.len() > shangxian_core::import::IMPORT_DOC_MAX_BYTES as u64 {
        return Err("导入文件过大（最多 1MB）；请使用 cli config export 导出的文件".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("无法读取此文件：{e}"))?;
    let doc: Value = serde_json::from_str(&text).map_err(|_| {
        "此文件不是 JSON 配置；只有用 cli config export 导出的文件才能导入。".to_string()
    })?;
    check_import_version(&doc)?;
    // 先干跑拿摘要（也顺便把「会改什么」给用户看）
    let planned = call(
        "import_config",
        json!({"path": path.display().to_string(), "mode": mode.as_str(), "dryRun": true}),
    )?;
    if dry_run {
        if want_json {
            print_ok(&planned);
        } else {
            print_import_summary(&planned, true);
            println!("（--dry-run：仅检查，未做任何修改）");
        }
        return Ok(());
    }
    if !want_json {
        print_import_summary(&planned, true);
    }
    if import_needs_confirm(false, yes, std::io::stdin().is_terminal())? {
        if !confirm_destructive("确定按以上内容导入？导入前会自动备份一份。", false)? {
            return cancelled(want_json);
        }
    }
    let v = call(
        "import_config",
        json!({"path": path.display().to_string(), "mode": mode.as_str(), "dryRun": false}),
    )?;
    if want_json {
        print_ok(&v);
    } else {
        print_import_summary(&v, false);
        if let Some(b) = opt_str(&v, "backupPath") {
            println!("✓ 设置已导入（原设置自动备份在 {b}）");
        } else {
            println!("✓ 设置已导入");
        }
    }
    Ok(())
}

/// 从导入文件里读配置版本：带包装（kind / formatVersion / config）的读 configVersion，
/// 没有就读 config.version；裸配置读顶层 version。读不到返回 None（交给应用报人话）。
fn import_doc_version(v: &Value) -> Option<u64> {
    let wrapped =
        v.get("kind").is_some() || v.get("formatVersion").is_some() || v.get("config").is_some();
    if wrapped {
        v.get("configVersion").and_then(Value::as_u64).or_else(|| {
            v.get("config")
                .and_then(|c| c.get("version"))
                .and_then(Value::as_u64)
        })
    } else {
        v.get("version").and_then(Value::as_u64)
    }
}

/// 导入版本门槛（D2 §11.2）：只接受 5 / 6。
/// 4 及更旧的在 CLI 就拦下给大白话；读配置文件的老迁移路径在 config.rs，不受影响。
fn check_import_version(v: &Value) -> Result<(), String> {
    let Some(version) = import_doc_version(v) else {
        return Ok(());
    };
    if version < 5 {
        return Err(format!(
            "导入失败：此设置版本过旧（版本 {version}）。请先用旧版「上线助手」打开并保存一次，再用新保存的文件导入。"
        ));
    }
    Ok(())
}

fn print_import_summary(v: &Value, dry_run: bool) {
    let mode_word = if str_of(v, "mode") == "replace" {
        "整份替换"
    } else {
        "按 id 合并"
    };
    println!(
        "{}（{mode_word}）：",
        if dry_run { "将修改的内容" } else { "导入结果" }
    );
    for (key, label) in [("added", "新增"), ("updated", "更新"), ("unchanged", "无变化")] {
        println!("  {label}：{}", import_counts_text(&v[key]));
    }
    for w in v["warnings"].as_array().cloned().unwrap_or_default() {
        println!("  ⚠ {}", w.as_str().unwrap_or(""));
    }
}

fn import_counts_text(c: &Value) -> String {
    let mut parts = Vec::new();
    let servers = num_of(c, "servers");
    if servers > 0 {
        parts.push(format!("服务器 {servers} 台"));
    }
    let services = num_of(c, "services");
    if services > 0 {
        parts.push(format!("本机服务 {services} 台"));
    }
    let tunnels = num_of(c, "tunnels");
    if tunnels > 0 {
        parts.push(format!("发布隧道 {tunnels} 条"));
    }
    if parts.is_empty() {
        "没有".to_string()
    } else {
        parts.join("、")
    }
}

/// 秒数 → 人话时长（统计用）。
fn fmt_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{} 小时 {} 分钟", secs / 3600, (secs / 60) % 60)
    } else if secs >= 60 {
        format!("{} 分钟", secs / 60)
    } else {
        format!("{secs} 秒")
    }
}

/// 秒数 → 短时长（倒计时用）。
fn fmt_duration_short(secs: u64) -> String {
    if secs >= 60 {
        format!("{} 分 {} 秒", secs / 60, secs % 60)
    } else {
        format!("{secs} 秒")
    }
}

fn fmt_remaining(expires_at: i64, now: i64) -> String {
    let secs = ((expires_at - now).max(0) / 1000) as u64;
    fmt_duration_short(secs)
}

fn fmt_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} 字节")
    }
}

fn i64_of(v: &Value, key: &str) -> i64 {
    v[key].as_i64().unwrap_or(0)
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
/// 读配置失败不再静默按空配置：报错退出 1（D2 §13；读坏/降级拦截的人话在错误里）。
fn snapshot() -> Result<Value, String> {
    if let Ok(v) = call("status", json!({})) {
        return Ok(v);
    }
    let dir = default_data_dir();
    let cfg = load_config_offline(&dir)?;
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
                "preset": t.preset,
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
    Ok(json!({
        "app": {"pid": null, "appVersion": null, "dataDir": dir.display().to_string(), "running": false},
        "services": services,
        "tunnels": tunnels,
        "servers": servers,
    }))
}

/// 离线读配置（只读命令的兵库）：读不出来就报错，不再默默当空配置。
fn load_config_offline(dir: &Path) -> Result<AppConfig, String> {
    let path = dir.join("config.json");
    if let Some(v) = config::config_version_too_new(&path) {
        return Err(config::config_unsupported_message(v));
    }
    if !path.exists() {
        let bad = dir.join("config.json.bad");
        if bad.exists() {
            return Err(format!(
                "设置文件无法读取（损坏文件的备份位于 {}）；请先修复或恢复它，再重试",
                bad.display()
            ));
        }
        // 第一次运行：还没有设置文件，空配置是正常状态
        return Ok(AppConfig::default());
    }
    AppConfig::load(&path).map_err(|e| format!("读取设置失败：{e}"))
}

/// 从实时/离线快照里找一台本机服务。
fn find_service(id: &str) -> Result<Value, String> {
    let snap = snapshot()?;
    snap["services"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == id))
        .cloned()
        .ok_or_else(|| format!("未找到本机服务「{id}」（可用 cli services 查看 id）"))
}

fn find_tunnel(id: &str) -> Result<Value, String> {
    let snap = snapshot()?;
    snap["tunnels"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["id"] == id))
        .cloned()
        .ok_or_else(|| format!("未找到隧道「{id}」（可用 cli tunnels 查看 id）"))
}

fn find_server(id: &str) -> Result<Value, String> {
    let snap = snapshot()?;
    snap["servers"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == id))
        .cloned()
        .ok_or_else(|| format!("未找到服务器「{id}」（可用 cli servers 查看 id）"))
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
        .ok_or_else(|| "未获取到哈希".into())
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
        "--access 后为空。如需取消访问保护请明确写 --access none；如需启用保护请写 password / ip（例：--access password --access ip）。"
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
                        "无法识别 --access 的值「{other}」。仅支持三种：password（需要密码）、ip（只允许指定 IP）、none（取消保护）。\n例：--access password --access ip"
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
            "--access none 表示「取消全部保护」，不能与 password / ip 同时使用。\n请写 --access none，或写 --access password --access ip"
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
        "（修改隧道时未传 --access，即沿用原有保护设置）"
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
            "已提供 --access-password（访问密码），但本次生效的保护设置中没有 password{keep_note}。密码会被丢弃，保护不会生效。\n正确写法：cli tunnel {target} --access password --access-password <密码>"
        ));
    }
    if access_ips.is_some() && !has("ip") {
        return Err(format!(
            "已提供 --access-ips（IP 白名单），但本次生效的保护设置中没有 ip{keep_note}。白名单会被丢弃，不会生效。\n正确写法：cli tunnel {target} --access ip --access-ips \"1.2.3.4,5.6.7.8\""
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
        (false, false) => "无（任何人可访问）".into(),
    }
}

/// 成功提示里回显最终生效的保护状态，别让用户猜。
fn access_echo(access: &[String]) -> String {
    if access.is_empty() {
        "访问保护：无（任何人可访问）。如需启用保护：--access password --access-password <密码> 或 --access ip --access-ips \"1.2.3.4\""
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
            "{what}\n此步骤需要确认，但当前不是交互终端（管道 / 脚本 / agent），无法询问 y/N，因此未执行任何操作。\n确定要执行请加 --yes 后重试。"
        ));
    }
    // 先让人看清要动的是哪个对象（提示走 stderr，不污染 --json 的 stdout）
    eprintln!("{what}");
    eprint!("确认执行？[y/N] ");
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
        "已取消，未做任何修改。",
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
                eprintln!("（无法关闭回显：{e}，后续输入的内容会显示在屏幕上）");
                read_secret_plain(prompt)?
            }
        }
    } else {
        // 管道 / 脚本：老老实实从标准输入读
        read_secret_plain(prompt)?
    };
    let s = s.trim().to_string();
    if s.is_empty() {
        return Err("密码为空，未执行任何操作".into());
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
    Err("未找到『上线助手』主程序。\n  处理办法：手动点击图标打开应用，命令行仍然可以连接（可用 cli app status 确认）。".into())
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
        "running" => "运行中",
        "starting" => "启动中",
        "not_listening" => "端口未监听",
        "failed" => "启动失败",
        "stopped" => "未启动",
        _ => "状态未知（应用未运行）",
    }
}

/// 隧道状态词（跟界面完全一致）。
fn tunnel_word(status: &str) -> &'static str {
    match status {
        "running" => "发布中",
        "connecting" => "连接中",
        "disconnected" => "重连中",
        "error" => "发布失败",
        "paused" => "已暂停",
        _ => "状态未知（应用未运行）",
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
        (true, true) => "已加密 + 只允许指定 IP".into(),
        (true, false) => "已加密".into(),
        (false, true) => "只允许指定 IP".into(),
        (false, false) => "任何人可访问".into(),
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
    println!("  密码保护：{}", if yes("passwordOn") { "开" } else { "关" });
    if yes("ipOn") {
        let ips = if allow_ips.is_empty() {
            "（未填写具体 IP）".to_string()
        } else {
            allow_ips.join("、")
        };
        println!("  白名单：开（IP：{ips}）");
    } else {
        println!("  白名单：关");
    }
    println!("  密码文件：{}", if yes("htpasswdExists") { "有" } else { "无" });
    println!("  站点配置：{}", if yes("confFound") { "找到" } else { "未找到" });
}

fn print_status(snap: &Value) {
    let app = &snap["app"];
    if app["running"].as_bool() == Some(false) || app["pid"].is_null() {
        println!("应用：未运行（以下为配置文件中的内容，状态无法判断；可用 cli app start 启动）\n");
    } else {
        println!(
            "应用：运行中（进程号 {}，配置文件夹 {}）\n",
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
                "仅监控，不自动启动".to_string()
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

## 三条铁律（务必遵守）
1. **严禁操作 pi-web**：本机 pi-web 服务及其隧道是用户正在使用的通道，暂停/重启会直接中断会话。
2. 隧道暂停后外部将无法访问；不确定是否要停止时，请先询问。
3. 删除本机服务/服务器前，请先查看它是否被隧道引用（存在引用时应用会拒绝并给出说明）。

## 危险操作（删除 / 暂停 / 重置指纹 / 导入配置）需要确认
- 交互终端（人工输入命令）：先打印将要操作的对象，再询问 y/N；回车 = 不执行任何操作。
- 非交互（管道 / 脚本 / AI agent）：必须显式加 --yes，否则拒绝执行并报错。
      cli tunnel stop <隧道id> --yes
      cli tunnel remove <隧道id> --yes
      cli service remove <服务id> --yes
      cli server remove <服务器id> --yes
      cli tunnel forget-key <隧道id> --yes   # 仅在服务器重装过、指纹变化时使用
      cli app stop-all --yes                  # 升级 / 卸载前停止全部隧道和服务（安装器会调用它）
      cli config import 备份.json --yes       # 导入配置；如需先查看将修改的内容，请加 --dry-run

## 最常用的命令
    cli status --json                       # 一次查看全部：应用 + 服务 + 隧道 + 服务器
    cli services --json                     # 本机服务清单（id、端口、状态、启动方式）
    cli tunnels --json                      # 发布隧道清单（服务→服务器、云端端口、状态）
    cli servers --json                      # 云端服务器清单
    cli ports --json                        # 本机正在监听的端口（带进程名和程序路径）
    cli tunnel start <隧道id>               # 开始发布
    cli tunnel stop <隧道id>                # 暂停发布（需要确认；非交互加 --yes）
    cli service start <服务id>              # 启动本机服务（使用已登记的启动方式）
    cli service stop <服务id>               # 停止（非应用启动的进程会先拦截；已配置启动方式时加 --force 可实际停止，仅监控的服务会被拒绝）
    cli logs <隧道id> --lines 80            # 查看隧道日志（--kind service 查看服务日志，其它值会报错）
    cli logs --source all --format json --json  # 事件 + 应用日志 + 所有隧道/服务日志（合并读取）
    cli logs <隧道id> --archive 1           # 查看上一份轮转备份（0/1/2）
    cli nginx <隧道id>                      # 生成需要粘贴到云端面板的 nginx 配置片段
    cli tunnel check <隧道id>               # 到云端查看实际启用的访问保护（只读，不修改服务器）

## 体检 / 排查
    cli doctor                              # 体检：应用、文件夹、SSH、端口、服务器、隧道、云端、公网、域名
    cli doctor --deep                       # 深度检查（会连接服务器和域名，耗时较长）
    cli doctor --strict                     # 有未通过项就直接退出码 1（这不算命令错误，是「检查未通过」）
    cli stats                               # 运行时长、隧道重连情况、今日重连次数、更新状态
    cli diag --out 诊断包.zip               # 生成脱敏诊断包（发给排查人员）
    cli app stop-all --yes                  # 升级 / 卸载前停止全部隧道和服务（安装器会调用它）
    cli app update-check                    # 检查是否有新版本（仅检查，不下载、不安装）

## 撤销删除
    cli undo                                # 撤销最近一次删除（= cli undo last）
    cli undo list                           # 查看可撤销的记录（删除后 10 分钟内、最多 20 条）
    cli undo und-3f9a1c2d                   # 按令牌撤销指定的一条
- 隧道撤销后一律是暂停状态，如需继续发布请执行 cli tunnel start <id>。
- 删除命令成功后，文本输出会提示「（10 分钟内可以撤销：cli undo last）」。

## 配置类
    cli service add --name 我的网站 --port 8188 --exe C:\path\python.exe --args "main.py" --watch
    cli service edit <服务id> --port 8189 --watch true
    cli service edit <服务id> --watch false              # --watch / --autostart 也可以不带值（= 开启）
    cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000 --url 我的域名.com
    cli tunnel add ... --preset ws          # 发布预设：web（默认）/ ws（WebSocket）/ upload（上传大文件）
    cli tunnel add ... --access password --access-user 用户名 --access-password 密码
    cli tunnel add ... --access password --access ip --access-ips "1.2.3.4,5.6.7.8"   # 密码 + 白名单两种保护同时启用
    cli tunnel edit <隧道id> --access password --access-password 新密码   # 更换密码
    cli tunnel edit <隧道id> --access none   # 取消访问保护（不传 --access = 保持原样）
    cli server add --name 香港-1 --host api.example.com --user user
    cli server add --name 宝塔机 --host api.example.com --user root --vhost-dir /www/server/panel/vhost   # 站点目录：密码文件与站点配置的存放位置
    cli server edit <服务器id> --vhost-dir /www/server/panel/vhost   # 不写 = 保留原有站点目录，不会被清除
    echo 服务器密码 | cli server setup-key <服务器id>     # 使用密码登录一次，自动配置免密登录
- --access 仅支持 password / ip / none，拼写错误会直接报错（不会静默变成「无保护」）；--access= 这种空值也会直接报错，如需取消保护请写 --access none。
- --access-password 必须配 --access password，--access-ips 必须配 --access ip，不匹配会报错。
- 每次修改后都会回显最终保护状态，例如「访问保护：密码 + 白名单（只允许指定 IP），已生效」。

## 配置备份（导出 / 导入）
    cli config export --out 备份.json     # 导出（默认脱敏：不含密码哈希、不含密钥路径）
    cli config export --with-paths        # 同时导出密钥路径（仍然不含密码哈希）
    cli config import 备份.json --dry-run # 先查看将修改的内容（不写盘、无需确认）
    cli config import 备份.json --yes     # 合并导入（按 id 合并，未写的字段保持原值）
    cli config import 备份.json --mode replace --yes   # 整份替换（导入前自动备份）
- 导入会修改设置：交互终端会询问 y/N；管道 / 脚本 / agent 必须加 --yes，否则退出码 1。
- 使用导出的脱敏文件导回时，密码哈希和密钥路径会保留（不会被清空）。

## 应用未运行时怎么办
    cli app status        # 查看是否在运行；未运行时退出码 3，JSON 中 offline: true
    cli app start         # 启动应用（静默进入托盘）
    cli app stop-all --yes # 停止全部发布隧道和应用启动的本机服务（升级 / 卸载前使用）
- `cli app stop-all` 是特例：应用未运行时直接输出「没有需要停止的内容」并退出码 0（安装器不会卡住）。
- 离线也能用的：status / services / tunnels / servers（直接读取配置文件，状态字段为 unknown）、
  cli doctor（本地可执行的那几步）、cli undo list（读取运行记录）、cli config export（读取配置文件）。
- 需要应用运行的（离线时退出码 3）：stats / diag / logs / undo（实际撤销）/ config import / app update-check。
- 修改设置、启停任何内容，都必须先执行 cli app start。

## 需要调用更底层接口时
    cli methods                       # 列出所有可调用的方法 + 参数格式
    cli call <方法> '{"参数":...}'    # 直接调用（通用入口）

## 输出约定
- 默认输出中文文本；加 `--json` 输出机器格式（agent 一律加）。读命令、写命令都是同一个信封。
- 成功（status / services / tunnels / servers / ports / logs / nginx / stats / doctor / diag / methods / call / guide / undo / config / 各种改配置）：
  {"ok": true, "result": ...}
- 失败：{"ok": false, "error": "原因说明"}
- 退出码：0 = 成功（确认里选「不」也是 0）；1 = 出错（包括非交互下拒绝执行、undo 令牌过期、doctor --strict 有未通过项）；
  2 = 命令写法不对（参数名拼错、少值）；3 = 这个命令需要应用运行，但应用未运行（JSON 里 offline: true）。
- `cli logs --format json` 不带 --json 时输出 JSONL（一行一条，无信封，供管道使用）；
  带 --json 时才给上面的信封（`--format lines --json` 保持旧形状，result 里有 text）。
- `cli call` 不带 --json 时按文本方式打印返回值，加了才给上面的 JSON 信封。
- 输入密码（cli server setup-key）时屏幕上不回显，通过管道传入同样安全。
- `cli doctor --strict` 退出码 1 不是命令执行错误，而是「检查未通过」；文本输出会写「✗ 体检有 N 项未通过」。
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
        assert!(access_echo(&[]).contains("无（任何人可访问）"));
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
        assert_eq!(access_phrase(&[]), "无（任何人可访问）");
        assert_eq!(access_phrase(&["password".to_string()]), "密码");
        assert_eq!(access_phrase(&["ip".to_string()]), "白名单（只允许指定 IP）");
        assert_eq!(
            access_phrase(&["ip".to_string(), "password".to_string()]),
            "密码 + 白名单（只允许指定 IP）"
        );
    }

    #[test]
    fn logs_source_defaults_and_overrides() {
        // 有 id 不写 --source：按 --kind（默认 tunnel）
        assert_eq!(
            resolve_logs_source(None, Some("tun-1"), "tunnel").unwrap(),
            ("tunnel".to_string(), Some("tun-1".to_string()))
        );
        assert_eq!(
            resolve_logs_source(None, Some("svc-1"), "service").unwrap(),
            ("service".to_string(), Some("svc-1".to_string()))
        );
        // 没 id 不写 --source：all
        assert_eq!(
            resolve_logs_source(None, None, "tunnel").unwrap(),
            ("all".to_string(), None)
        );
        // --source 优先于 --kind（--kind 只作旧写法兼容）
        assert_eq!(
            resolve_logs_source(Some("service"), Some("svc-1"), "tunnel").unwrap(),
            ("service".to_string(), Some("svc-1".to_string()))
        );
        assert_eq!(
            resolve_logs_source(Some("all"), Some("tun-1"), "service").unwrap(),
            ("all".to_string(), None)
        );
        // --source tunnel/service 必须带 id
        assert!(resolve_logs_source(Some("tunnel"), None, "tunnel").is_err());
        assert!(resolve_logs_source(Some("service"), None, "service").is_err());
        // 拼错的值直接报错（不静默当隧道）
        let err = resolve_logs_source(None, Some("tun-1"), "bogus").unwrap_err();
        assert!(err.contains("bogus"));
        assert!(resolve_logs_source(Some("bogus"), None, "tunnel").is_err());
    }

    #[test]
    fn logs_archive_range_is_checked() {
        assert_eq!(check_archive(0).unwrap(), 0);
        assert_eq!(check_archive(2).unwrap(), 2);
        for bad in [-1i64, 3, 9] {
            let err = check_archive(bad).unwrap_err();
            assert_eq!(err, "--archive 只能是 0、1、2；0 = 仅查看当前文件");
        }
    }

    #[test]
    fn offline_errors_map_to_exit_code_three() {
        // 应用没在跑（ctl 的原话）→ 3
        assert_eq!(exit_code_for_error(ctl::APP_NOT_RUNNING), 3);
        // 自定义的离线错也用内部前缀标出来 → 3
        assert_eq!(exit_code_for_error(&offline_error("应用未运行；请先执行 cli app start")), 3);
        assert_eq!(shown_error(&offline_error("文本")), "文本");
        // 普通错 → 1
        assert_eq!(exit_code_for_error("参数不对"), 1);
        // 前缀不能误伤以「应用未在运行」开头的普通错误？ctl 原话就是完整提示，照旧映射成 3
        assert_eq!(
            exit_code_for_error(&format!("{}（找不到 ctl.json）", ctl::APP_NOT_RUNNING)),
            3
        );
    }

    #[test]
    fn import_confirm_rules() {
        // --dry-run / --yes 不用确认
        assert!(!import_needs_confirm(true, false, false).unwrap());
        assert!(!import_needs_confirm(false, true, false).unwrap());
        // 交互终端：问 y/N
        assert!(import_needs_confirm(false, false, true).unwrap());
        // 非交互又没有 --yes：拒绝（交给 main 映射退出码 1）
        let err = import_needs_confirm(false, false, false).unwrap_err();
        assert!(err.contains("--yes"), "要教用户加 --yes：{err}");
    }

    #[test]
    fn out_paths_are_made_absolute_for_the_app() {
        // 绝对路径原样保留（应用的工作目录跟 cli 不一定一样）
        let abs = abs_out_path(if cfg!(windows) { "C:/tmp/x.json" } else { "/tmp/x.json" });
        assert!(Path::new(&abs).is_absolute());
        // 相对路径按命令行当前目录补成绝对路径
        let rel = abs_out_path("rel.json");
        assert!(Path::new(&rel).is_absolute(), "{rel}");
        assert!(rel.ends_with("rel.json"), "{rel}");
    }

    #[test]
    fn stop_all_summary_mentions_failures() {
        let text = stop_all_summary_text(&json!({
            "tunnelsStopped": 2,
            "tunnelsFailed": 1,
            "servicesStopped": 3,
            "servicesFailed": 1,
        }));
        assert!(text.contains("2 条发布隧道"), "{text}");
        assert!(text.contains("3 个本机服务"), "{text}");
        assert!(text.contains("未能停止"), "{text}");
        let ok = stop_all_summary_text(&json!({"tunnelsStopped": 0, "servicesStopped": 0}));
        assert!(!ok.contains("未能停止"), "{ok}");
    }

    #[test]
    fn new_commands_parse() {
        let cli = Cli::try_parse_from(["cli", "doctor", "--deep", "--strict"]).unwrap();
        match cli.cmd {
            Cmd::Doctor { deep, strict } => assert!(deep && strict),
            _ => panic!("应该解析成 doctor"),
        }
        let cli = Cli::try_parse_from(["cli", "diag", "--out", "d.zip", "--deep"]).unwrap();
        match cli.cmd {
            Cmd::Diag { out, deep } => {
                assert_eq!(out.as_deref(), Some("d.zip"));
                assert!(deep);
            }
            _ => panic!("应该解析成 diag"),
        }
        let cli = Cli::try_parse_from(["cli", "stats", "--json"]).unwrap();
        assert!(cli.json);
        // undo：不写 / last / list / 令牌
        let cli = Cli::try_parse_from(["cli", "undo"]).unwrap();
        match cli.cmd {
            Cmd::Undo { target } => assert!(target.is_none()),
            _ => panic!("应该解析成 undo"),
        }
        let cli = Cli::try_parse_from(["cli", "undo", "list"]).unwrap();
        match cli.cmd {
            Cmd::Undo { target } => assert_eq!(target.as_deref(), Some("list")),
            _ => panic!("应该解析成 undo list"),
        }
        // config export / import
        let cli =
            Cli::try_parse_from(["cli", "config", "export", "--with-paths", "--out", "b.json"])
                .unwrap();
        match cli.cmd {
            Cmd::Config {
                sub: ConfigCmd::Export { out, with_paths },
            } => {
                assert!(with_paths);
                assert_eq!(out.as_deref(), Some("b.json"));
            }
            _ => panic!("应该解析成 config export"),
        }
        let cli = Cli::try_parse_from([
            "cli", "config", "import", "b.json", "--mode", "replace", "--dry-run", "--yes",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Config {
                sub: ConfigCmd::Import {
                    file,
                    mode,
                    dry_run,
                    yes,
                },
            } => {
                assert_eq!(file, "b.json");
                assert_eq!(mode, "replace");
                assert!(dry_run && yes);
            }
            _ => panic!("应该解析成 config import"),
        }
        // app stop-all
        let cli = Cli::try_parse_from(["cli", "app", "stop-all", "--yes"]).unwrap();
        match cli.cmd {
            Cmd::App {
                sub: AppCmd::StopAll { yes },
            } => assert!(yes),
            _ => panic!("应该解析成 app stop-all"),
        }
        let cli = Cli::try_parse_from(["cli", "app", "stop-all"]).unwrap();
        match cli.cmd {
            Cmd::App {
                sub: AppCmd::StopAll { yes },
            } => assert!(!yes),
            _ => panic!("应该解析成 app stop-all"),
        }
        // app update-check
        let cli = Cli::try_parse_from(["cli", "app", "update-check"]).unwrap();
        match cli.cmd {
            Cmd::App {
                sub: AppCmd::UpdateCheck,
            } => {}
            _ => panic!("应该解析成 app update-check"),
        }
        // logs 全参数 + 越界 archive 也能解析（越界在 run 里报错退出 1）
        let cli = Cli::try_parse_from([
            "cli", "logs", "tun-1", "--source", "tunnel", "--format", "json", "--archive", "3",
            "--lines", "50",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Logs {
                archive, lines, ..
            } => {
                assert_eq!(archive, 3);
                assert_eq!(lines, 50);
            }
            _ => panic!("应该解析成 logs"),
        }
        // tunnel add / edit 的 --preset
        let cli = Cli::try_parse_from([
            "cli", "tunnel", "add", "--service", "svc-1", "--server", "srv-1", "--remote-port",
            "13000", "--preset", "ws",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Tunnel {
                sub: TunnelCmd::Add { preset, .. },
            } => assert_eq!(preset.as_deref(), Some("ws")),
            _ => panic!("应该解析成 tunnel add"),
        }
        let cli = Cli::try_parse_from(["cli", "tunnel", "edit", "tun-1"]).unwrap();
        match cli.cmd {
            Cmd::Tunnel {
                sub: TunnelCmd::Edit { preset, .. },
            } => assert!(preset.is_none(), "不传 = 保持原来的预设"),
            _ => panic!("应该解析成 tunnel edit"),
        }
        // 重置指纹
        let cli =
            Cli::try_parse_from(["cli", "tunnel", "forget-key", "tun-1", "--yes"]).unwrap();
        match cli.cmd {
            Cmd::Tunnel {
                sub: TunnelCmd::ForgetKey { id, yes },
            } => {
                assert_eq!(id, "tun-1");
                assert!(yes);
            }
            _ => panic!("应该解析成 tunnel forget-key"),
        }
    }

    /// 给需要落盘的单测建一个干净临时目录。
    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shangxian-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn offline_doctor_reuses_core_and_persists() {
        let dir = test_dir("doctor-offline");
        let quick = offline_doctor_in(&dir, false);
        assert_eq!(quick["mode"], "quick");
        assert_eq!(quick["appRunning"], false);
        assert_eq!(quick["schemaVersion"], json!(1));
        assert_eq!(quick["steps"].as_array().unwrap().len(), 9);
        // 第一次运行还没有 config.json：dirs 应该 pass，不是吓人的 fail
        assert_eq!(quick["steps"][1]["status"], "pass");
        // core 负责落盘和发事件
        assert!(dir.join("logs").join("doctor-last.json").is_file());
        let events = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
        assert!(events.contains("doctorRan"), "events={events}");

        // deep 也是同一套 9 步（cloud / dns 会真跑）
        let deep = offline_doctor_in(&dir, true);
        assert_eq!(deep["mode"], "deep");
        assert_eq!(deep["steps"].as_array().unwrap().len(), 9);
        assert_eq!(deep["steps"][6]["id"], "cloud");
        assert_eq!(deep["steps"][8]["id"], "dns");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_export_without_config_gives_default_backup() {
        let dir = test_dir("export-first-run");
        let v = offline_export_from(&dir, true, None).unwrap();
        assert_eq!(v["redacted"], true);
        assert_eq!(v["config"]["version"], json!(6));
        assert!(v["config"]["servers"].as_array().unwrap().is_empty());

        // 指定 --out 时写出完整备份文件（第一次运行也能备份）
        let out = dir.join("backup.json");
        let v2 = offline_export_from(&dir, true, Some(out.to_str().unwrap())).unwrap();
        assert_eq!(v2["path"], out.display().to_string());
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(doc["kind"], "shangxian.config-export");
        assert_eq!(doc["formatVersion"], json!(1));
        assert_eq!(doc["configVersion"], json!(6));
        assert_eq!(doc["config"]["version"], json!(6));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_version_gate_only_accepts_5_and_6() {
        assert!(check_import_version(&json!({"version": 6})).is_ok());
        assert!(check_import_version(&json!({"version": 5})).is_ok());
        for old in [3, 4] {
            let err = check_import_version(&json!({"version": old})).unwrap_err();
            assert!(err.contains("过旧"), "v{old} 要给出版本过旧的提示：{err}");
            assert!(err.contains(&old.to_string()), "报错要写版本号：{err}");
        }
        // 带包装的备份：configVersion 优先，其次 config.version
        assert!(check_import_version(&json!({
            "kind": "shangxian.config-export",
            "formatVersion": 1,
            "configVersion": 5,
            "config": {"version": 5}
        }))
        .is_ok());
        assert!(check_import_version(&json!({
            "kind": "shangxian.config-export",
            "formatVersion": 1,
            "config": {"version": 4}
        }))
        .is_err());
        // 比程序新的版本不在这里拦：交给应用报「升级后会自动读回」
        assert!(check_import_version(&json!({"version": 7})).is_ok());
        // 没有版本号：交给应用报它那句人话
        assert!(check_import_version(&json!({})).is_ok());
    }

    #[test]
    fn import_doc_version_reads_both_shapes() {
        assert_eq!(import_doc_version(&json!({"version": 5})), Some(5));
        assert_eq!(
            import_doc_version(&json!({"configVersion": 6, "config": {"version": 4}})),
            Some(6)
        );
        assert_eq!(
            import_doc_version(&json!({"config": {"version": 5}})),
            Some(5)
        );
        assert_eq!(
            import_doc_version(&json!({"kind": "x", "version": 5})),
            None
        );
        assert_eq!(import_doc_version(&json!({})), None);
    }
}
