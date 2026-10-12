//! 体检（doctor）：D2 §14 的 9 步固定顺序，quick / deep 两种模式。
//!
//! 设计约束（以 D0 / D2 / D3 为准）：
//! - 顶层 9 步顺序固定：`app → dirs → ssh → ports → servers → tunnels → cloud → web → dns`；
//! - 每步 `pass | warn | fail | skip`（D0 第 1 条），带 `detail / advice / children / durationMs / fix`；
//! - quick 只跑本机步骤（cloud / web / dns 标 skip）；deep 才会连服务器和公网；
//! - deep 单步 ≤20 秒、整次 ≤60 秒；超时按 `fail`，advice 用 D2 指定文案；
//! - 任何一步内部 panic 都 catch 成 `fail`，报告永远能出来；
//! - 跑完把整份报告写到 `<dataDir>/logs/doctor-last.json`，并发一条 `doctorRan` 事件。
//!
//! 调用方（G1 的 guardian、CLI、Tauri）只构造 [`DoctorRequest`] 然后调 [`run`]：
//! 本模块**不**反向依赖 guardian，也不持有任何锁；联网动作都在 `run` 内完成，
//! 由调用方放在锁外执行（D1 §6.4）。
//!
//! `fix.label` 是给前端按标签映射动作的按钮文案（D2 §14.1）；没有对应动作时前端
//! 只展示 `fix.text` 或不展示，都是允许的。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::cleanup::SshExec;
use crate::config::{AppConfig, ServerConfig};
use crate::events::{EventLine, EventLog, EventSubject, KIND_DOCTOR, LEVEL_INFO};
use crate::reason::ReasonCode;
use crate::ssh;
use crate::state::{now_ms, truncate_chars};

/// 报告 schema 版本，恒为 1（D2 §14.1）。
pub const SCHEMA_VERSION: u32 = 1;
/// deep 单步上限：20 秒（D2 §14.2）。
pub const STEP_LIMIT: Duration = Duration::from_secs(20);
/// 整次体检上限：60 秒（D2 §14.2）。quick 也一样有这条总闸，但本机步骤根本跑不满。
pub const TOTAL_BUDGET: Duration = Duration::from_secs(60);
/// cloud 步最多测的服务器台数。
pub const MAX_CLOUD_SERVERS: usize = 5;
/// web 步最多测的隧道条数。
pub const MAX_WEB_TUNNELS: usize = 10;
/// dns 步最多查的域名个数。
pub const MAX_DNS_DOMAINS: usize = 10;
/// 单台服务器 SSH 探活的最长等待。
const SSH_TIMEOUT: Duration = Duration::from_secs(10);
/// 单条隧道公网探测（服务器上 2 次 curl，各 8 秒）的最长等待。
const WEB_PROBE_TIMEOUT: Duration = Duration::from_secs(16);
/// 没有 curl 时，本机 TCP 直连 host:remotePort 的等待（D2 §14.2 的 3 秒）。
const TCP_FALLBACK_TIMEOUT: Duration = Duration::from_secs(3);
/// 单个域名解析的最长等待（std 解析没有超时，靠线程 + recv_timeout 兜）。
const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// detail / advice 的字符上限（D2 §14.1：detail ≤200）。
const DETAIL_MAX_CHARS: usize = 200;
const ADVICE_MAX_CHARS: usize = 200;
/// 超时统一文案（D2 §14.2）。
pub const TIMEOUT_ADVICE: &str = "网络慢或服务器没响应；稍后再试。";

// ---------- 模式与状态 ----------

/// 体检模式。JSON 里是 `"quick"` / `"deep"`（D2 §14.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DoctorMode {
    /// 只查本机（默认）。
    #[default]
    Quick,
    /// 连服务器 + 查公网，最多 60 秒。
    Deep,
}

impl DoctorMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DoctorMode::Quick => "quick",
            DoctorMode::Deep => "deep",
        }
    }

    pub fn is_deep(self) -> bool {
        self == DoctorMode::Deep
    }

    /// 解析调用方传来的模式字符串：空 = quick；不认得的写法报人话错误。
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "quick" => Ok(DoctorMode::Quick),
            "deep" => Ok(DoctorMode::Deep),
            other => Err(format!("体检模式只支持 quick 或 deep，收到的是「{other}」")),
        }
    }
}

impl std::fmt::Display for DoctorMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单步状态。JSON 里是 `"pass" | "warn" | "fail" | "skip"`（D0 第 1 条）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl DoctorStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DoctorStatus::Pass => "pass",
            DoctorStatus::Warn => "warn",
            DoctorStatus::Fail => "fail",
            DoctorStatus::Skip => "skip",
        }
    }
}

impl std::fmt::Display for DoctorStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------- 对外数据契约（D2 §14.1） ----------

/// 每个状态的计数（D0 第 1 条：在 D2 三值上新增 `warn`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSummary {
    pub pass: u32,
    pub warn: u32,
    pub fail: u32,
    pub skip: u32,
}

impl DoctorSummary {
    pub fn from_steps(steps: &[DoctorStep]) -> Self {
        let mut summary = DoctorSummary::default();
        for step in steps {
            match step.status {
                DoctorStatus::Pass => summary.pass += 1,
                DoctorStatus::Warn => summary.warn += 1,
                DoctorStatus::Fail => summary.fail += 1,
                DoctorStatus::Skip => summary.skip += 1,
            }
        }
        summary
    }

    /// 有没有没过项（CLI `--strict` 用）。
    pub fn has_fail(&self) -> bool {
        self.fail > 0
    }
}

/// 结构化的「怎么办」：`label` 是按钮文案，`text` 是 2~4 步大白话操作说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorFix {
    pub label: String,
    pub text: String,
}

/// 一行体检结果。`children` 是可选子行（cloud / web / dns 用），不改变顶层 9 步顺序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorStep {
    pub id: String,
    pub title: String,
    pub status: DoctorStatus,
    pub detail: String,
    /// status=pass 时为 ""（空字符串，不是 null）。
    pub advice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<DoctorStep>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<DoctorFix>,
}

/// 一份完整报告（写 `<dataDir>/logs/doctor-last.json` 的内容就是它）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub schema_version: u32,
    pub checked_at: i64,
    /// 离线运行且读不到版本时为 null。
    pub app_version: Option<String>,
    pub app_running: bool,
    pub mode: DoctorMode,
    pub steps: Vec<DoctorStep>,
    pub summary: DoctorSummary,
}

impl DoctorReport {
    /// 按 id 找一步（顶层）。
    pub fn step(&self, id: &str) -> Option<&DoctorStep> {
        self.steps.iter().find(|s| s.id == id)
    }
}

// ---------- 请求（调用方构造） ----------

/// 一台本机服务的实时状态（来自快照；离线时为空）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorService {
    pub id: String,
    pub name: String,
    pub local_port: u16,
    /// `"up" | "down" | "unknown"`（D2 §6.3）。
    pub health: String,
}

/// 一条发布隧道的实时状态（来自快照；离线时可为空，doctor 会回退读 config）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorTunnel {
    pub id: String,
    pub name: String,
    pub service_id: String,
    pub server_id: String,
    pub local_port: u16,
    pub remote_port: u16,
    /// 用户填的公网网址（可空）。
    pub url: Option<String>,
    pub enabled: bool,
    /// `"running" | "connecting" | "disconnected" | "error" | "paused" | "unknown"`。
    pub status: String,
    /// 当前失败原因（人话，可空）。
    pub reason: Option<String>,
    /// 这条隧道单独指定的密钥（可空 = 继承服务器）。
    pub key_path: Option<String>,
}

/// 体检请求。**设计目标：调用方直接填字段就能调 [`run`]，不需要碰 guardian 内部。**
///
/// - G1（guardian）：`app_running / app_pid / app_version / data_dir / config_trusted /
///   config(=guardian.config) / services / tunnels` 从快照与配置克隆；
///   `events = 已注入的 Arc<EventLog>`；`servers` 可留空（会自动回退 `config.servers`）。
/// - CLI 离线：`app_running=false`、`config` 读文件、其余留空；deep 时 cloud / dns 照跑。
#[derive(Debug, Clone)]
pub struct DoctorRequest {
    pub mode: DoctorMode,
    /// 应用进程在跑吗。false = 离线体检（ports / tunnels / web 会 skip）。
    pub app_running: bool,
    /// 应用进程号，只用来写 detail（可空）。
    pub app_pid: Option<u32>,
    /// 应用版本；离线读不到时为 None（报告里也是 null）。
    pub app_version: Option<String>,
    /// 数据目录（doctor-last.json 写它下面的 logs/）。
    pub data_dir: PathBuf,
    /// 配置是否可信（configTrusted）；false = 读坏 / 降级拦截，写操作会被拒。
    pub config_trusted: bool,
    /// 解析后的完整配置；读不出来 = None（第一次运行没有配置文件也允许）。
    pub config: Option<AppConfig>,
    /// 读配置失败的人话原因（config=None 且确实读坏时填）。
    pub config_error: Option<String>,
    /// 本机服务实时状态（应用没在跑就留空）。
    pub services: Vec<DoctorService>,
    /// 隧道实时状态（应用没在跑就留空；doctor 会回退用 config.tunnels 查引用/域名）。
    pub tunnels: Vec<DoctorTunnel>,
    /// 云端服务器。留空时用 `config.servers`；调用方有环境识别结果时，
    /// 可把识别到的站点目录填进对应 server 的 `vhost_dir`。
    pub servers: Vec<ServerConfig>,
    /// 服务器 id → 已识别到的站点目录（D0 第 12 条解析链的第二层，来自 state 缓存）。
    /// 空 map = 没有识别结果。
    pub detected_vhost_dirs: BTreeMap<String, String>,
    /// 覆盖 ssh.exe 路径；None = 系统默认。
    pub ssh_exe: Option<PathBuf>,
    /// 用来写 `doctorRan` 事件；None = 不写事件（单测 / 离线可留空）。
    pub events: Option<Arc<EventLog>>,
}

impl Default for DoctorRequest {
    fn default() -> Self {
        Self {
            mode: DoctorMode::Quick,
            app_running: false,
            app_pid: None,
            app_version: None,
            data_dir: PathBuf::new(),
            config_trusted: false,
            config: None,
            config_error: None,
            services: Vec::new(),
            tunnels: Vec::new(),
            servers: Vec::new(),
            detected_vhost_dirs: BTreeMap::new(),
            ssh_exe: None,
            events: None,
        }
    }
}

impl DoctorRequest {
    /// 最常用的构造：给模式和数据目录，其余用默认值再逐项填。
    pub fn new(mode: DoctorMode, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            data_dir: data_dir.into(),
            ..Default::default()
        }
    }
}

// ---------- 主体 ----------

/// 跑一次体检。只读、不占锁、不自动改任何东西（D1 §6.1）。
///
/// deep 模式下单步 ≤20 秒、整次 ≤60 秒；任何一步内部出错都会变成 `fail` 行，
/// 函数本身不会 panic、不会返回错误。跑完会尽力写 `doctor-last.json` 并发事件。
pub fn run(req: &DoctorRequest) -> DoctorReport {
    let checked_at = now_ms();
    let deadline = Instant::now() + TOTAL_BUDGET;

    let steps = vec![
        timed("app", "程序状态", || step_app(req)),
        timed("dirs", "文件夹", || step_dirs(req)),
        timed("ssh", "SSH 程序", || step_ssh(req)),
        timed("ports", "本机端口", || step_ports(req)),
        timed("servers", "云端服务器", || step_servers(req)),
        timed("tunnels", "发布隧道", || step_tunnels(req)),
        timed("cloud", "云端配置", || step_cloud(req, deadline)),
        timed("web", "公网访问", || step_web(req, deadline)),
        timed("dns", "域名解析", || step_dns(req, deadline)),
    ];
    let summary = DoctorSummary::from_steps(&steps);

    let report = DoctorReport {
        schema_version: SCHEMA_VERSION,
        checked_at,
        app_version: req.app_version.clone(),
        app_running: req.app_running,
        mode: req.mode,
        steps,
        summary,
    };

    // 落盘和事件都是「尽力而为」：写不下来也不能让体检结果丢了。
    let _ = persist_report(req, &report);
    emit_doctor_ran(req, &report);
    report
}

// ---------- 步骤 1：app ----------

fn step_app(req: &DoctorRequest) -> StepResult {
    if !req.app_running {
        return StepResult::fail(
            "应用没在跑；这是离线体检，实时状态看不到。",
            "先启动「上线助手」，再在应用里跑一次体检。",
        );
    }
    if !req.config_trusted {
        let why = req
            .config_error
            .clone()
            .unwrap_or_else(|| "配置文件读坏了（已留底成 config.json.bad），现在新设置不能保存。".into());
        return StepResult::fail(why, "先修好设置文件，或恢复 config.json.bad，再重开应用。")
            .with_fix(open_data_dir_fix());
    }
    if req.config.is_none() {
        let why = req
            .config_error
            .clone()
            .unwrap_or_else(|| "设置文件读不出来。".into());
        return StepResult::fail(why, "检查设置文件；修好或恢复备份后再重开应用。")
            .with_fix(open_data_dir_fix());
    }

    let pid = req
        .app_pid
        .map(|p| format!("（进程号 {p}）"))
        .unwrap_or_default();
    let ver = req
        .app_version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!("，版本 {v}"))
        .unwrap_or_default();
    StepResult::pass(format!("应用在跑{pid}{ver}，设置文件能正常读。"))
}

// ---------- 步骤 2：dirs ----------

fn step_dirs(req: &DoctorRequest) -> StepResult {
    if req.data_dir.as_os_str().is_empty() {
        return StepResult::fail(
            "没拿到数据目录，没法检查文件夹。",
            "先启动应用；命令行体检时用参数指定数据目录。",
        );
    }
    if !req.data_dir.is_dir() {
        return StepResult::fail(
            format!("数据目录不在：{}。", req.data_dir.display()),
            "先去设置里确认数据目录；重新启动应用一般会自动把它建好。",
        );
    }
    if !dir_writable(&req.data_dir) {
        return StepResult::fail(
            format!("数据目录在，但写不进去：{}。", req.data_dir.display()),
            "检查这个文件夹的权限（也可能是杀毒软件或只读盘）；不行就把数据目录换到别的位置。",
        );
    }

    let bad = req.data_dir.join("config.json.bad");
    if bad.exists() {
        return StepResult::fail(
            "上次配置读坏了，原文件留底在 config.json.bad；现在新设置保存不了。",
            "把 config.json.bad 修好改回 config.json，或删掉它重新配置，然后重开应用。",
        )
        .with_fix(open_data_dir_fix());
    }
    if let Some(err) = &req.config_error {
        return StepResult::fail(
            err.clone(),
            "修好设置文件，或恢复备份，再重开应用。",
        )
        .with_fix(open_data_dir_fix());
    }

    if req.config.is_none() {
        return StepResult::pass("数据目录能读能写；还没有设置文件（第一次运行正常）。");
    }
    StepResult::pass("配置和数据目录都在，能读能写。")
}

/// 数据目录可写探测：建一个探针文件再删掉（只读体检不改用户数据）。
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".doctor-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(mut file) => {
            let _ = file.write_all(b"ok");
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ---------- 步骤 3：ssh ----------

fn step_ssh(req: &DoctorRequest) -> StepResult {
    if !cfg!(windows) {
        return StepResult::skip("当前系统不是 Windows；这一步跳过。");
    }
    let exe = req
        .ssh_exe
        .clone()
        .unwrap_or_else(ssh::default_ssh_exe);
    if exe.is_file() {
        StepResult::pass(format!("找到系统自带 ssh：{}", exe.display()))
    } else {
        StepResult::fail(
            format!("没找到 ssh 程序：{} 不存在。", exe.display()),
            "去「设置 → 应用 → 可选功能」里装上「OpenSSH 客户端」，装完重开应用。",
        )
        .with_fix(DoctorFix {
            label: "怎么装 OpenSSH".into(),
            text: "1. 打开 Windows「设置」；\n2. 进「应用 → 可选功能」；\n3. 添加「OpenSSH 客户端」；\n4. 装好重开「上线助手」再体检。".into(),
        })
    }
}

// ---------- 步骤 4：ports ----------

fn step_ports(req: &DoctorRequest) -> StepResult {
    if !req.app_running {
        return StepResult::skip("应用没在跑，看不到实时状态。");
    }
    if req.services.is_empty() {
        return StepResult::skip("还没有登记本机服务。");
    }
    let n = req.services.len();
    let up = req.services.iter().filter(|s| s.health == "up").count();
    let down: Vec<&DoctorService> = req.services.iter().filter(|s| s.health == "down").collect();
    let unknown = n - up - down.len();

    if down.is_empty() {
        let mut detail = format!("{n} 台服务里 {up} 台端口正常");
        if unknown > 0 {
            detail.push_str(&format!("，{unknown} 台还没探到"));
        }
        StepResult::pass(format!("{detail}。"))
    } else {
        let names = down
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("、");
        StepResult::fail(
            format!("{n} 台服务里 {} 台端口不通：{names}。", down.len()),
            "先把这些本机服务启动起来；启动后还不通，就看服务自己的日志。",
        )
    }
}

// ---------- 步骤 5：servers ----------

fn step_servers(req: &DoctorRequest) -> StepResult {
    let servers = effective_servers(req);
    if servers.is_empty() {
        return StepResult::skip("还没有配置云端服务器。");
    }
    let tunnels = effective_tunnels(req);
    let enabled: Vec<&DoctorTunnel> = tunnels.iter().filter(|t| t.enabled).collect();
    if enabled.is_empty() {
        return StepResult::skip("没有开启的隧道，没有服务器引用要查。");
    }

    let mut bad: Vec<String> = Vec::new();
    for tun in &enabled {
        match find_server(&servers, &tun.server_id) {
            None => bad.push(format!("{}（云端服务器不在了）", tun.name)),
            Some(server) => {
                if server.host.trim().is_empty() {
                    bad.push(format!("{}（服务器地址是空的）", tun.name));
                    continue;
                }
                if server.ssh_port == 0 {
                    bad.push(format!("{}（SSH 端口不对）", tun.name));
                    continue;
                }
                let key = tun
                    .key_path
                    .as_deref()
                    .or(server.key_path.as_deref())
                    .map(str::trim)
                    .filter(|k| !k.is_empty());
                if let Some(key) = key {
                    if !Path::new(key).is_file() {
                        bad.push(format!("{}（密钥文件不在了：{key}）", tun.name));
                    }
                }
            }
        }
    }

    if bad.is_empty() {
        StepResult::pass(format!(
            "{n} 条开启的隧道，引用的服务器信息都正常。",
            n = enabled.len()
        ))
    } else {
        StepResult::fail(
            format!(
                "{n} 条开启的隧道里 {m} 条有问题：{list}。",
                n = enabled.len(),
                m = bad.len(),
                list = bad.join("；")
            ),
            "去「云端服务器」页补齐信息；密钥文件不在的话，重新用密码配一次免密。",
        )
        .with_fix(DoctorFix {
            label: "去云端服务器".into(),
            text: "1. 打开「云端服务器」页；\n2. 核对地址、端口和密钥；\n3. 密钥没了就重新配一次免密；\n4. 回来再体检。".into(),
        })
    }
}

// ---------- 步骤 6：tunnels ----------

fn step_tunnels(req: &DoctorRequest) -> StepResult {
    if !req.app_running {
        return StepResult::skip("应用没在跑，看不到实时状态。");
    }
    let servers = effective_servers(req);
    let tunnels = effective_tunnels(req);
    let enabled: Vec<&DoctorTunnel> = tunnels.iter().filter(|t| t.enabled).collect();
    if enabled.is_empty() {
        return StepResult::pass("现在没有开启的发布隧道；建好并开启后这里会显示发布情况。");
    }

    let running = enabled.iter().filter(|t| t.status == "running").count();
    let no_url = enabled
        .iter()
        .filter(|t| t.url.as_deref().map(|u| u.trim().is_empty()).unwrap_or(true))
        .count();

    let mut bad: Vec<String> = Vec::new();
    for tun in &enabled {
        let mut why: Vec<String> = Vec::new();
        if !has_service(req, &tun.service_id) {
            why.push("本机服务不在了".into());
        }
        if find_server(&servers, &tun.server_id).is_none() {
            why.push("云端服务器不在了".into());
        }
        if tun.status == "error" {
            let reason = tun
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .unwrap_or("发布出错了");
            why.push(reason.to_string());
        }
        if !why.is_empty() {
            bad.push(format!("{}（{}）", tun.name, why.join("；")));
        }
    }

    let mut detail = format!(
        "{} 条开启：{running} 条正在发布，{no_url} 条还没填网址",
        enabled.len()
    );
    if bad.is_empty() {
        if no_url > 0 {
            detail.push_str("（没填网址的隧道公网检查会跳过）");
        }
        StepResult::pass(format!("{detail}。"))
    } else {
        StepResult::fail(
            format!(
                "{detail}；{} 条有问题：{}。",
                bad.len(),
                bad.join("；")
            ),
            "先看这几条隧道的日志，点「重连」；引用缺了就回「本机服务」或「云端服务器」补齐。",
        )
        .with_fix(DoctorFix {
            label: "去看日志".into(),
            text: "1. 打开对应隧道的卡片；\n2. 点「看日志」，先看最后几行；\n3. 修好后点「重连」；\n4. 回来再体检。".into(),
        })
    }
}

// ---------- 步骤 7：cloud ----------

fn step_cloud(req: &DoctorRequest, deadline: Instant) -> StepResult {
    if !req.mode.is_deep() {
        return StepResult::skip("快速检查不连服务器；想看云端就切「深度检查」再跑。");
    }
    let servers = effective_servers(req);
    if servers.is_empty() {
        return StepResult::skip("还没有配置云端服务器，没得测。");
    }
    if budget_exhausted(deadline) {
        return budget_fail("云端配置");
    }

    // 优先测「被开启隧道引用」的服务器；没有开启隧道时退化成测全部服务器。
    let tunnels = effective_tunnels(req);
    let referenced: BTreeSet<String> = tunnels
        .iter()
        .filter(|t| t.enabled)
        .map(|t| t.server_id.clone())
        .collect();
    let mut targets: Vec<ServerConfig> = if referenced.is_empty() {
        servers
    } else {
        servers
            .into_iter()
            .filter(|s| referenced.contains(&s.id))
            .collect()
    };
    if targets.is_empty() {
        return StepResult::skip("开启的隧道没有引用到现存服务器；先去「云端服务器」页补回来。");
    }
    let truncated = targets.len() > MAX_CLOUD_SERVERS;
    targets.truncate(MAX_CLOUD_SERVERS);

    let step_end = step_deadline(deadline);
    let children: Vec<DoctorStep> = targets
        .iter()
        .map(|server| {
            child_step(
                format!("cloud.{}", server.id),
                format!("云端：{}", server.name),
                || check_cloud_server(req, server, step_end, deadline),
            )
        })
        .collect();

    let status = aggregate(&children);
    let total = children.len();
    let fail_n = children.iter().filter(|c| c.status == DoctorStatus::Fail).count();
    let warn_n = children.iter().filter(|c| c.status == DoctorStatus::Warn).count();

    let mut detail = match status {
        DoctorStatus::Pass => format!("{total} 台服务器都连得上，站点目录也在。"),
        DoctorStatus::Fail => {
            let names = children
                .iter()
                .filter(|c| c.status == DoctorStatus::Fail)
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>()
                .join("、");
            format!("{total} 台服务器里 {fail_n} 台没通过：{names}。")
        }
        DoctorStatus::Warn => format!("{total} 台服务器能连上，但有 {warn_n} 台要留意。"),
        DoctorStatus::Skip => format!("{total} 台服务器这次都跳过了。"),
    };
    if truncated {
        detail.push_str("（最多测 5 台，其余这次没测）");
    }

    let mut result = StepResult::from_parts(status, detail, first_actionable_advice(&children));
    result.children = children;
    if status == DoctorStatus::Fail {
        result.fix = Some(DoctorFix {
            label: "去云端服务器".into(),
            text: "1. 打开「云端服务器」页；\n2. 核对地址、SSH 端口和密钥；\n3. 确认面板站点目录填对（宝塔一般是 /www/server/panel/vhost）；\n4. 回来再体检。".into(),
        });
    }
    result
}

/// 单台服务器的云端探活：`echo shangxian-ok` + `test -d <vhostDir>/nginx`（D2 §14.2 / 本任务书）。
fn check_cloud_server(
    req: &DoctorRequest,
    server: &ServerConfig,
    step_end: Instant,
    global_deadline: Instant,
) -> StepResult {
    if server.host.trim().is_empty() {
        return StepResult::fail(
            format!("{}：服务器地址是空的。", server.name),
            "去「云端服务器」页把地址补上，再体检。",
        );
    }
    let timeout = step_timeout(step_end, SSH_TIMEOUT);
    if timeout.is_zero() {
        return deadline_fail("云端配置", step_end, global_deadline);
    }

    let dir = resolve_vhost_dir(req, server);
    let nginx_dir = format!("{}/nginx", dir.trim_end_matches(|c| c == '/' || c == '\\'));
    let script = format!(
        "echo shangxian-ok; if [ -d {q} ]; then echo dir-ok; else echo dir-missing; fi",
        q = crate::cleanup::shell_quote(&nginx_dir)
    );

    match crate::cleanup::run(&ssh_exec(req, server), &script, timeout) {
        Ok(out) => {
            let reached = out.contains("shangxian-ok")
                || out.contains("dir-ok")
                || out.contains("dir-missing");
            if !reached {
                StepResult::fail(
                    format!("{}：连上了，但没看懂服务器返回的内容。", server.name),
                    "稍后再试；一直这样就把诊断包发给懂行的人。",
                )
            } else if out.contains("dir-ok") {
                StepResult::pass(format!("{}：连接正常，站点目录也在。", server.name))
            } else {
                StepResult::fail(
                    format!("{}：连上了，但站点目录不在：{nginx_dir}。", server.name),
                    "去「云端服务器」页把「站点目录」改成面板的实际目录（宝塔一般是 /www/server/panel/vhost），再重新检查。",
                )
            }
        }
        Err(err) => ssh_failure_step(&err),
    }
}

// ---------- 步骤 8：web ----------

fn step_web(req: &DoctorRequest, deadline: Instant) -> StepResult {
    if !req.mode.is_deep() {
        return StepResult::skip("快速检查不连服务器；想看公网就切「深度检查」再跑。");
    }
    if !req.app_running {
        return StepResult::skip("应用没在跑，看不到隧道状态；先启动应用再跑深度检查。");
    }
    let tunnels = effective_tunnels(req);
    let mut targets: Vec<&DoctorTunnel> = tunnels
        .iter()
        .filter(|t| t.enabled)
        .filter(|t| t.url.as_deref().map(|u| !u.trim().is_empty()).unwrap_or(false))
        .collect();
    if targets.is_empty() {
        return StepResult::skip("没有开着、又填了公网网址的隧道。");
    }
    let truncated = targets.len() > MAX_WEB_TUNNELS;
    targets.truncate(MAX_WEB_TUNNELS);

    if budget_exhausted(deadline) {
        return budget_fail("公网访问");
    }

    let step_end = step_deadline(deadline);
    let children: Vec<DoctorStep> = targets
        .iter()
        .map(|tun| {
            child_step(format!("web.{}", tun.id), format!("公网：{}", tun.name), || {
                check_web_tunnel(req, tun, step_end, deadline)
            })
        })
        .collect();

    let status = aggregate(&children);
    let total = children.len();
    let fail_n = children.iter().filter(|c| c.status == DoctorStatus::Fail).count();
    let warn_n = children
        .iter()
        .filter(|c| c.status == DoctorStatus::Warn)
        .count();
    let pass_n = children
        .iter()
        .filter(|c| c.status == DoctorStatus::Pass)
        .count();

    let mut detail = match status {
        DoctorStatus::Fail => {
            let names = children
                .iter()
                .filter(|c| c.status == DoctorStatus::Fail)
                .map(|c| c.title.trim_start_matches("公网："))
                .collect::<Vec<_>>()
                .join("、");
            format!("{total} 条隧道里 {fail_n} 条打不开：{names}。")
        }
        DoctorStatus::Warn => format!("{total} 条隧道能打开，但有 {warn_n} 条要留意。"),
        DoctorStatus::Pass => {
            let skip_n = children
                .iter()
                .filter(|c| c.status == DoctorStatus::Skip)
                .count();
            if skip_n == 0 {
                format!("{total} 条隧道都能打开。")
            } else {
                format!("{total} 条隧道里 {pass_n} 条正常，{skip_n} 条没在发布（跳过）。")
            }
        }
        DoctorStatus::Skip => "这次没有正在发布的隧道，等隧道跑起来再测。".into(),
    };
    if truncated {
        detail.push_str("（最多测 10 条，其余这次没测）");
    }

    let mut result = StepResult::from_parts(status, detail, first_actionable_advice(&children));
    result.children = children;
    if status == DoctorStatus::Fail {
        result.fix = Some(diag_fix());
    }
    result
}

/// 单条隧道的公网探测：服务器端 curl（D3 §14），服务器没有 curl 时退回本机 TCP 直连。
fn check_web_tunnel(
    req: &DoctorRequest,
    tun: &DoctorTunnel,
    step_end: Instant,
    global_deadline: Instant,
) -> StepResult {
    if tun.status != "running" {
        return StepResult::skip("这条隧道没在发布，先点「开始」再测。");
    }
    let servers = effective_servers(req);
    let Some(server) = find_server(&servers, &tun.server_id) else {
        return StepResult::fail(
            "这条隧道挂的云端服务器不在了。",
            "去「云端服务器」页重新选一台，或删掉这条隧道。",
        );
    };
    let raw = tun.url.as_deref().map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return StepResult::skip("这条隧道没填公网网址，跳过。");
    }
    let domain = crate::nginx::normalize_domain(raw);
    if domain.is_empty() {
        return StepResult::fail(
            format!("网址「{raw}」看不出域名。"),
            "回隧道设置把网址填成 example.com 这种域名（或带 https:// 的完整网址）。",
        );
    }
    if server.host.trim().is_empty() {
        return StepResult::fail(
            "服务器地址是空的，没法探测。",
            "去「云端服务器」页把地址补上。",
        );
    }

    let timeout = step_timeout(step_end, WEB_PROBE_TIMEOUT);
    if timeout.is_zero() {
        return deadline_fail("公网访问", step_end, global_deadline);
    }

    match crate::cleanup::run(
        &ssh_exec(req, server),
        &web_probe_script(&domain),
        timeout,
    ) {
        Ok(out) => {
            let probe = parse_probe_output(&out);
            if probe.curl_missing {
                let host = server.host.trim();
                let fallback = step_timeout(step_end, TCP_FALLBACK_TIMEOUT);
                if fallback.is_zero() {
                    return deadline_fail("公网访问", step_end, global_deadline);
                }
                return match tcp_probe(host, tun.remote_port, fallback) {
                    Ok(()) => StepResult::pass(format!(
                        "服务器上没装 curl；用本机直连 {host}:{} 测了一下：通。",
                        tun.remote_port
                    )),
                    Err(err) => StepResult::fail(
                        format!(
                            "服务器上没装 curl；本机连 {host}:{} 也不通（{err}）。",
                            tun.remote_port
                        ),
                        "检查域名解析和防火墙；服务器上装上 curl 后体检会更准。",
                    ),
                };
            }
            match judge_probe(&probe, &domain) {
                Some((status, detail, advice)) => StepResult::from_parts(status, detail, advice),
                None => StepResult::fail(
                    "没拿到公网探测结果。",
                    TIMEOUT_ADVICE,
                ),
            }
        }
        Err(err) => ssh_failure_step(&err),
    }
}

/// 服务器端公网探测脚本（D3 §14：只 GET、不跟随跳转、绝不 `-k`）。
fn web_probe_script(domain: &str) -> String {
    let d = crate::cleanup::shell_quote(domain);
    format!(
        "d={d}\n\
if ! command -v curl >/dev/null 2>&1; then\n\
  echo 'PROBE_SKIP=curl_missing'\n\
else\n\
  out=$(curl -sS -o /dev/null -w '%{{http_code}}|%{{time_total}}|%{{ssl_verify_result}}' --max-time 8 -H 'User-Agent: shangxian-doctor/1.0' \"https://$d/\" 2>/dev/null); rc=$?\n\
  echo \"PROBE_HTTPS=$out\"\n\
  echo \"PROBE_HTTPS_RC=$rc\"\n\
  out2=$(curl -sS -o /dev/null -w '%{{http_code}}|%{{time_total}}' --max-time 8 -H 'User-Agent: shangxian-doctor/1.0' \"http://$d/\" 2>/dev/null); rc2=$?\n\
  echo \"PROBE_HTTP=$out2\"\n\
  echo \"PROBE_HTTP_RC=$rc2\"\n\
fi"
    )
}

/// curl 探测输出（服务器端脚本）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeOutput {
    curl_missing: bool,
    https_code: Option<u16>,
    /// curl 的 `%{ssl_verify_result}`；0 = 证书校验通过。
    https_ssl: i32,
    /// curl https 退出码；-1 = 脚本没给这一行。
    https_rc: i32,
    http_code: Option<u16>,
    /// curl http 退出码；-1 = 脚本没给这一行。
    http_rc: i32,
}

impl Default for ProbeOutput {
    fn default() -> Self {
        Self {
            curl_missing: false,
            https_code: None,
            https_ssl: 0,
            https_rc: -1,
            http_code: None,
            http_rc: -1,
        }
    }
}

/// 解析服务器端 curl 输出（纯函数，单测覆盖）。
fn parse_probe_output(text: &str) -> ProbeOutput {
    let mut out = ProbeOutput::default();
    let mut https_pair: Option<(Option<u16>, i32)> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(v) = line.strip_prefix("PROBE_SKIP=") {
            if v.trim() == "curl_missing" {
                out.curl_missing = true;
            }
        } else if let Some(v) = line.strip_prefix("PROBE_HTTPS=") {
            let mut parts = v.split('|');
            let code = parts.next().and_then(parse_http_code);
            // 剩下：time, ssl → nth(0)=time，nth(1)=ssl
            let ssl = parts
                .nth(1)
                .and_then(|s| s.trim().parse::<i32>().ok())
                .unwrap_or(0);
            https_pair = Some((code, ssl));
        } else if let Some(v) = line.strip_prefix("PROBE_HTTPS_RC=") {
            out.https_rc = v.trim().parse().unwrap_or(-1);
        } else if let Some(v) = line.strip_prefix("PROBE_HTTP=") {
            out.http_code = v.split('|').next().and_then(parse_http_code);
        } else if let Some(v) = line.strip_prefix("PROBE_HTTP_RC=") {
            out.http_rc = v.trim().parse().unwrap_or(-1);
        }
    }
    if let Some((code, ssl)) = https_pair {
        out.https_code = code;
        out.https_ssl = ssl;
    }
    out
}

fn parse_http_code(raw: &str) -> Option<u16> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<u16>().ok().filter(|code| *code != 0)
}

/// 按 D3 §14 的表判断一次公网探测结果；`curl_missing` 时返回 None（调用方走本机 TCP 兜底）。
fn judge_probe(probe: &ProbeOutput, domain: &str) -> Option<(DoctorStatus, String, String)> {
    if probe.curl_missing {
        return None;
    }

    // https 优先。
    let https: (DoctorStatus, String, String) = if probe.https_rc == 0 {
        if probe.https_ssl != 0 {
            (
                DoctorStatus::Fail,
                format!("https://{domain}/ 的证书有问题（校验结果 {}）。", probe.https_ssl),
                "去面板点「续签证书」，或检查证书绑的域名对不对。".into(),
            )
        } else {
            match probe.https_code {
                Some(code) if matches!(code, 200 | 204 | 301 | 302 | 307 | 308) => (
                    DoctorStatus::Pass,
                    format!("https://{domain}/ 返回 {code}，能打开。"),
                    String::new(),
                ),
                Some(code) if matches!(code, 401 | 403) => (
                    DoctorStatus::Pass,
                    format!("https://{domain}/ 返回 {code}（要密码或不在白名单），说明保护正常、线路也通。"),
                    String::new(),
                ),
                Some(404) => (
                    DoctorStatus::Warn,
                    format!(
                        "https://{domain}/ 返回 404：线路通，但网站本身可能没配对。"
                    ),
                    "去面板核对「反向代理」是不是指向 http://127.0.0.1:<本隧道的云端端口>，再看本机服务在不在跑。".into(),
                ),
                Some(code) if (500..=599).contains(&code) => (
                    DoctorStatus::Warn,
                    format!("https://{domain}/ 返回 {code}：线路通，但网站本身出错了。"),
                    "去面板看站点配置；本机服务的日志里通常有线索。".into(),
                ),
                Some(code) => (
                    DoctorStatus::Warn,
                    format!("https://{domain}/ 返回了 {code}，说不上好还是坏。"),
                    "先用浏览器打开看看；打不开就查面板里的站点配置。".into(),
                ),
                None => (
                    DoctorStatus::Warn,
                    format!("https://{domain}/ 没拿到 HTTP 状态码。"),
                    "过一会儿再测；一直这样就把诊断包发给懂行的人。".into(),
                ),
            }
        }
    } else if probe.https_rc == 60 {
        (
            DoctorStatus::Fail,
            format!("https://{domain}/ 的证书有问题（curl 退出码 60）。"),
            "去面板点「续签」，或检查证书绑的域名。".into(),
        )
    } else if probe.https_rc == 6 {
        (
            DoctorStatus::Fail,
            format!("{domain} 域名解析不了。"),
            "检查域名解析（DNS）是不是指向这台服务器。".into(),
        )
    } else if probe.https_rc == 7 || probe.https_rc == 28 {
        (
            DoctorStatus::Fail,
            format!("https://{domain}/ 连不上或超时（curl 退出码 {}）。", probe.https_rc),
            "可能是域名没解析、防火墙没开 443/80，或者服务器在 NAT/CDN 后面、自己访问不了自己的公网地址。可以先换手机流量再点一次体检。".into(),
        )
    } else if probe.https_rc < 0 {
        (
            DoctorStatus::Fail,
            format!("https://{domain}/ 没拿到探测结果。"),
            "检查服务器上能不能正常跑命令；稍后再试。".into(),
        )
    } else {
        (
            DoctorStatus::Fail,
            format!("https://{domain}/ 探测失败（curl 退出码 {}）。", probe.https_rc),
            TIMEOUT_ADVICE.into(),
        )
    };

    // https 没失败时，再看 http 有没有强制跳转。
    if https.0 != DoctorStatus::Fail
        && probe.http_rc == 0
        && probe.http_code == Some(200)
    {
        return Some((
            DoctorStatus::Warn,
            format!("http://{domain}/ 能直接打开，没跳 https。"),
            "去面板打开「强制 HTTPS」，不然内容（和密码）是明文传的。".into(),
        ));
    }

    Some(https)
}

// ---------- 步骤 9：dns ----------

fn step_dns(req: &DoctorRequest, deadline: Instant) -> StepResult {
    if !req.mode.is_deep() {
        return StepResult::skip("快速检查不查域名；想看域名就切「深度检查」再跑。");
    }
    let tunnels = effective_tunnels(req);
    let domains = tunnel_domains(&tunnels, MAX_DNS_DOMAINS);
    if domains.is_empty() {
        return StepResult::skip("没有填公网网址的隧道，没域名可查。");
    }
    if budget_exhausted(deadline) {
        return budget_fail("域名解析");
    }

    let step_end = step_deadline(deadline);
    let children: Vec<DoctorStep> = domains
        .iter()
        .map(|domain| {
            child_step(
                format!("dns.{domain}"),
                domain.clone(),
                || dns_lookup(domain, step_end, deadline),
            )
        })
        .collect();

    let status = aggregate(&children);
    let total = children.len();
    let fail_n = children
        .iter()
        .filter(|c| c.status == DoctorStatus::Fail)
        .count();
    let detail = match status {
        DoctorStatus::Fail => {
            let names = children
                .iter()
                .filter(|c| c.status == DoctorStatus::Fail)
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>()
                .join("、");
            format!("{total} 个域名里 {fail_n} 个解析不了：{names}。")
        }
        _ => format!("{total} 个域名都能解析。"),
    };

    let mut result = StepResult::from_parts(status, detail, first_actionable_advice(&children));
    result.children = children;
    if status == DoctorStatus::Fail {
        result.fix = Some(diag_fix());
    }
    result
}

fn dns_lookup(domain: &str, step_end: Instant, global_deadline: Instant) -> StepResult {
    let timeout = step_timeout(step_end, DNS_LOOKUP_TIMEOUT);
    if timeout.is_zero() {
        return deadline_fail("域名解析", step_end, global_deadline);
    }
    match resolve_with_timeout(domain, 443, timeout) {
        Ok(ips) => StepResult::pass(format!(
            "{domain} 能解析到 {} 个地址（例：{}）。",
            ips.len(),
            ips[0]
        )),
        Err(err) => StepResult::fail(
            format!("{domain} 解析不了：{err}。"),
            "检查域名解析是不是指向这台服务器；也可以换个本机 DNS 再试。",
        ),
    }
}

/// 从开启的隧道里取「不同域名」列表（保序、大小写去重、最多 max 个）。
fn tunnel_domains(tunnels: &[DoctorTunnel], max: usize) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for tun in tunnels.iter().filter(|t| t.enabled) {
        let Some(url) = tun.url.as_deref() else { continue };
        let domain = crate::nginx::normalize_domain(url);
        if domain.is_empty() {
            continue;
        }
        if seen.insert(domain.to_ascii_lowercase()) {
            out.push(domain);
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

// ---------- 网络小工具（可在单测里用本地地址替换） ----------

/// 本机 TCP 直连 `host:port`，成功才算通（web 步没有 curl 时的兜底）。
fn tcp_probe(host: &str, port: u16, timeout: Duration) -> Result<(), String> {
    if timeout.is_zero() {
        return Err("时间不够".into());
    }
    let half = timeout / 2;
    let ips = resolve_with_timeout(host, port, half)?;
    let mut last = String::new();
    for ip in ips {
        let addr = SocketAddr::new(ip, port);
        match TcpStream::connect_timeout(&addr, half) {
            Ok(_) => return Ok(()),
            Err(err) => last = err.to_string(),
        }
    }
    Err(if last.is_empty() {
        "连不上".into()
    } else {
        last
    })
}

/// 带超时的域名解析：std 的 `ToSocketAddrs` 没有超时参数，放到独立线程里等。
fn resolve_with_timeout(host: &str, port: u16, timeout: Duration) -> Result<Vec<IpAddr>, String> {
    if timeout.is_zero() {
        return Err("时间不够".into());
    }
    let (tx, rx) = mpsc::channel();
    let owned = host.to_string();
    std::thread::spawn(move || {
        let result = (owned.as_str(), port)
            .to_socket_addrs()
            .map(|iter| iter.map(|addr| addr.ip()).collect::<Vec<_>>())
            .map_err(|err| err.to_string());
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(ips)) if !ips.is_empty() => Ok(ips),
        Ok(Ok(_)) => Err("没有解析到 IP".into()),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(format!("解析超时（{} 秒）", timeout.as_secs())),
    }
}

// ---------- 结果拼装 ----------

/// 内部步骤结果（未截断、未计时）。
struct StepResult {
    status: DoctorStatus,
    detail: String,
    advice: String,
    children: Vec<DoctorStep>,
    fix: Option<DoctorFix>,
}

impl StepResult {
    fn from_parts(status: DoctorStatus, detail: impl Into<String>, advice: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
            advice: advice.into(),
            children: Vec::new(),
            fix: None,
        }
    }

    fn pass(detail: impl Into<String>) -> Self {
        Self::from_parts(DoctorStatus::Pass, detail, "")
    }

    #[allow(dead_code)]
    fn warn(detail: impl Into<String>, advice: impl Into<String>) -> Self {
        Self::from_parts(DoctorStatus::Warn, detail, advice)
    }

    fn fail(detail: impl Into<String>, advice: impl Into<String>) -> Self {
        Self::from_parts(DoctorStatus::Fail, detail, advice)
    }

    fn skip(detail: impl Into<String>) -> Self {
        Self::from_parts(DoctorStatus::Skip, detail, "")
    }

    fn with_fix(mut self, fix: DoctorFix) -> Self {
        self.fix = Some(fix);
        self
    }
}

/// 跑一步：计时 + catch panic，任何意外都变成 fail 人话（报告永远出得来）。
fn timed(id: &str, title: &str, f: impl FnOnce() -> StepResult) -> DoctorStep {
    let start = Instant::now();
    let result = catch_result(f);
    make_step(id.to_string(), title.to_string(), result, elapsed_ms(start))
}

/// 跑一个子步骤（cloud / web / dns 用）。
fn child_step(id: String, title: String, f: impl FnOnce() -> StepResult) -> DoctorStep {
    let start = Instant::now();
    let result = catch_result(f);
    make_step(id, title, result, elapsed_ms(start))
}

fn catch_result(f: impl FnOnce() -> StepResult) -> StepResult {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let message = panic_text(payload);
            StepResult::fail(
                format!("检查时出了个意外错误：{message}"),
                "先重开应用再体检一次；还不行就把诊断包发给懂行的人。",
            )
        }
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "未知错误".to_string()
    }
}

fn make_step(id: String, title: String, result: StepResult, duration_ms: u64) -> DoctorStep {
    let mut detail = result.detail.trim().to_string();
    if detail.is_empty() {
        detail = "（没有更多信息）".to_string();
    }
    DoctorStep {
        id,
        title,
        status: result.status,
        detail: truncate_chars(&detail, DETAIL_MAX_CHARS),
        advice: truncate_chars(result.advice.trim(), ADVICE_MAX_CHARS),
        children: if result.children.is_empty() {
            None
        } else {
            Some(result.children)
        },
        duration_ms: Some(duration_ms),
        fix: result.fix,
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// 有 fail → fail；否则有 warn → warn；全 skip → skip；其余 → pass。
fn aggregate(children: &[DoctorStep]) -> DoctorStatus {
    if children.iter().any(|c| c.status == DoctorStatus::Fail) {
        DoctorStatus::Fail
    } else if children.iter().any(|c| c.status == DoctorStatus::Warn) {
        DoctorStatus::Warn
    } else if !children.is_empty() && children.iter().all(|c| c.status == DoctorStatus::Skip) {
        DoctorStatus::Skip
    } else {
        DoctorStatus::Pass
    }
}

/// 取第一个「有建议」的失败/警告子行的 advice（父步的 advice）。
fn first_actionable_advice(children: &[DoctorStep]) -> String {
    children
        .iter()
        .filter(|c| c.status == DoctorStatus::Fail)
        .chain(children.iter().filter(|c| c.status == DoctorStatus::Warn))
        .map(|c| c.advice.clone())
        .find(|advice| !advice.is_empty())
        .unwrap_or_default()
}

// ---------- 超时预算 ----------

/// 这一步这次最多能等多久：单步上限和总预算的余量取小。
fn step_timeout(deadline: Instant, cap: Duration) -> Duration {
    cap.min(deadline.saturating_duration_since(Instant::now()))
}

/// 单步的截止时刻：`min(进入这一步的时刻 + 20 秒, 整次体检的截止时刻)`。
fn step_deadline(global_deadline: Instant) -> Instant {
    (Instant::now() + STEP_LIMIT).min(global_deadline)
}

fn budget_exhausted(deadline: Instant) -> bool {
    Instant::now() >= deadline
}

fn budget_fail(what: &str) -> StepResult {
    StepResult::fail(
        format!("体检总时间用完了（最多 60 秒），「{what}」没跑完。"),
        TIMEOUT_ADVICE,
    )
}

/// 等不到再跑时的人话：分清单步 20 秒和整次 60 秒两种预算。
fn deadline_fail(what: &str, step_end: Instant, global_deadline: Instant) -> StepResult {
    if step_end >= global_deadline {
        budget_fail(what)
    } else {
        StepResult::fail(
            format!("「{what}」等太久了（单步最多 20 秒），这次没跑完。"),
            TIMEOUT_ADVICE,
        )
    }
}

// ---------- 错误翻译 ----------

/// SSH 失败 → 人话 detail + advice。超时统一用 D2 文案。
fn ssh_failure(err: &str) -> (DoctorStatus, String, String) {
    let lower = err.to_ascii_lowercase();
    if lower.contains("命令超时") || lower.contains("timed out") {
        return (
            DoctorStatus::Fail,
            "连服务器超时（网络慢或服务器没响应）。".into(),
            TIMEOUT_ADVICE.into(),
        );
    }
    if let Some(code) = ReasonCode::from_ssh_text(err) {
        return (
            DoctorStatus::Fail,
            code.text().to_string(),
            code.advice()
                .unwrap_or("检查服务器信息后重试。")
                .to_string(),
        );
    }
    (
        DoctorStatus::Fail,
        format!("连服务器失败：{}", one_line(err)),
        "检查网络、服务器地址、SSH 端口和密钥，再试一次。".into(),
    )
}

fn ssh_failure_step(err: &str) -> StepResult {
    let (status, detail, advice) = ssh_failure(err);
    StepResult::from_parts(status, detail, advice)
}

fn one_line(text: &str) -> String {
    let joined = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&joined, 120)
}

// ---------- 复用配置 / 服务器 ----------

fn effective_servers(req: &DoctorRequest) -> Vec<ServerConfig> {
    if !req.servers.is_empty() {
        return req.servers.clone();
    }
    req.config
        .as_ref()
        .map(|config| config.servers.clone())
        .unwrap_or_default()
}

/// 隧道列表：优先用快照的实时状态；没有快照时回退读 config（离线 deep 的 cloud/dns 用）。
fn effective_tunnels(req: &DoctorRequest) -> Vec<DoctorTunnel> {
    if !req.tunnels.is_empty() {
        return req.tunnels.clone();
    }
    let Some(config) = &req.config else {
        return Vec::new();
    };
    config
        .tunnels
        .iter()
        .map(|tun| {
            let local_port = config
                .services
                .iter()
                .find(|s| s.id == tun.service_id)
                .map(|s| s.local_port)
                .unwrap_or(0);
            DoctorTunnel {
                id: tun.id.clone(),
                name: tun.name.clone(),
                service_id: tun.service_id.clone(),
                server_id: tun.server_id.clone(),
                local_port,
                remote_port: tun.remote_port,
                url: tun.url.clone(),
                enabled: tun.enabled,
                status: if tun.enabled {
                    "unknown".to_string()
                } else {
                    "paused".to_string()
                },
                reason: None,
                key_path: tun.key_path.clone(),
            }
        })
        .collect()
}

fn find_server<'a>(servers: &'a [ServerConfig], id: &str) -> Option<&'a ServerConfig> {
    servers.iter().find(|s| s.id == id)
}

fn has_service(req: &DoctorRequest, id: &str) -> bool {
    if req.services.iter().any(|s| s.id == id) {
        return true;
    }
    req.config
        .as_ref()
        .map(|config| config.services.iter().any(|s| s.id == id))
        .unwrap_or(false)
}

/// 站点目录解析（D0 第 12 条）：server.vhostDir → 识别结果 → 环境变量/默认。
fn resolve_vhost_dir(req: &DoctorRequest, server: &ServerConfig) -> String {
    fn clean(raw: &str) -> Option<String> {
        let text = raw.trim().trim_end_matches(|c| c == '/' || c == '\\');
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    }
    if let Some(dir) = server.vhost_dir.as_deref().and_then(clean) {
        return dir;
    }
    if let Some(dir) = req
        .detected_vhost_dirs
        .get(&server.id)
        .and_then(|dir| clean(dir))
    {
        return dir;
    }
    crate::nginx::htpasswd_dir_for(server)
}

fn ssh_exec(req: &DoctorRequest, server: &ServerConfig) -> SshExec {
    SshExec {
        ssh_exe: req.ssh_exe.clone().unwrap_or_else(ssh::default_ssh_exe),
        target: format!("{}@{}", server.user.trim(), server.host.trim()),
        ssh_port: server.ssh_port,
        identity_file: server
            .key_path
            .clone()
            .filter(|key| !key.trim().is_empty())
            .map(PathBuf::from),
    }
}

// ---------- 落盘与事件 ----------

fn persist_report(req: &DoctorRequest, report: &DoctorReport) -> Result<(), String> {
    if req.data_dir.as_os_str().is_empty() {
        return Err("数据目录是空的".into());
    }
    let logs = req.data_dir.join("logs");
    std::fs::create_dir_all(&logs).map_err(|err| format!("建日志目录失败：{err}"))?;
    let path = logs.join("doctor-last.json");
    let text = serde_json::to_string_pretty(report)
        .map_err(|err| format!("体检报告序列化失败：{err}"))?;
    let text = format!("{text}\n");

    let tmp = logs.join(format!("doctor-last.json.tmp{}", std::process::id()));
    if std::fs::write(&tmp, text.as_bytes()).is_ok() {
        if std::fs::rename(&tmp, &path).is_ok() {
            return Ok(());
        }
        let _ = std::fs::remove_file(&tmp);
    }
    std::fs::write(&path, text.as_bytes()).map_err(|err| format!("写体检报告失败：{err}"))
}

fn emit_doctor_ran(req: &DoctorRequest, report: &DoctorReport) {
    let Some(events) = &req.events else {
        return;
    };
    let summary = &report.summary;
    let pass = summary.pass.to_string();
    let fail = summary.fail.to_string();
    let skip = summary.skip.to_string();
    let message = ReasonCode::DoctorRan.render(&[
        ("pass", pass.as_str()),
        ("fail", fail.as_str()),
        ("skip", skip.as_str()),
    ]);
    let data = serde_json::json!({
        "mode": report.mode.as_str(),
        "pass": summary.pass,
        "warn": summary.warn,
        "fail": summary.fail,
        "skip": summary.skip,
    });
    let line = EventLine::new(
        KIND_DOCTOR,
        LEVEL_INFO,
        Some(ReasonCode::DoctorRan),
        Some(EventSubject::app()),
        message,
    )
    .with_data(data);
    events.append(line);
}

// ---------- 文案固定件 ----------

fn open_data_dir_fix() -> DoctorFix {
    DoctorFix {
        label: "打开数据目录".into(),
        text: "1. 打开应用的数据目录；\n2. 先把 config.json.bad 备份出来；\n3. 修好 config.json，或删掉重配；\n4. 重开应用再体检。".into(),
    }
}

fn diag_fix() -> DoctorFix {
    DoctorFix {
        label: "导出诊断包".into(),
        text: "1. 在体检弹层点「导出诊断包」；\n2. 把它发给帮你排查的人；\n3. 里面不含密码和密钥，放心发。".into(),
    }
}

// ---------- 单测 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shangxian-doctor-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup_dir(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn base_request(dir: &Path) -> DoctorRequest {
        let mut req = DoctorRequest::new(DoctorMode::Quick, dir.to_path_buf());
        req.app_running = true;
        req.app_pid = Some(4108);
        req.app_version = Some("0.6.0".to_string());
        req.config_trusted = true;
        req.config = Some(AppConfig::default());
        req
    }

    fn step<'a>(report: &'a DoctorReport, id: &str) -> &'a DoctorStep {
        report.step(id).unwrap_or_else(|| panic!("缺步骤 {id}"))
    }

    fn tun(id: &str, url: Option<&str>, enabled: bool, status: &str) -> DoctorTunnel {
        DoctorTunnel {
            id: id.to_string(),
            name: format!("隧道{id}"),
            service_id: "svc-1".to_string(),
            server_id: "srv-1".to_string(),
            local_port: 8080,
            remote_port: 18080,
            url: url.map(str::to_string),
            enabled,
            status: status.to_string(),
            reason: None,
            key_path: None,
        }
    }

    fn server(id: &str, key_path: Option<&str>) -> ServerConfig {
        ServerConfig {
            id: id.to_string(),
            name: format!("服务器{id}"),
            host: "example.com".to_string(),
            ssh_port: 22,
            user: "root".to_string(),
            key_path: key_path.map(str::to_string),
            vhost_dir: None,
        }
    }

    // ---- 步骤顺序与总体形状 ----

    #[test]
    fn steps_keep_fixed_order_in_quick_mode() {
        let dir = test_dir("order");
        let req = base_request(&dir);
        let report = run(&req);
        let ids: Vec<&str> = report.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "app", "dirs", "ssh", "ports", "servers", "tunnels", "cloud", "web", "dns"
            ]
        );
        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(report.mode, DoctorMode::Quick);
        assert!(report.app_running);
        assert_eq!(report.app_version.as_deref(), Some("0.6.0"));
        cleanup_dir(&dir);
    }

    #[test]
    fn quick_mode_skips_network_steps() {
        let dir = test_dir("quickskip");
        let req = base_request(&dir);
        let report = run(&req);
        assert_eq!(step(&report, "cloud").status, DoctorStatus::Skip);
        assert_eq!(step(&report, "web").status, DoctorStatus::Skip);
        assert_eq!(step(&report, "dns").status, DoctorStatus::Skip);
        assert_eq!(step(&report, "app").status, DoctorStatus::Pass);
        assert_eq!(step(&report, "dirs").status, DoctorStatus::Pass);
        cleanup_dir(&dir);
    }

    #[test]
    fn offline_quick_skips_live_steps() {
        let dir = test_dir("offline");
        let mut req = DoctorRequest::new(DoctorMode::Quick, dir.clone());
        req.config = Some(AppConfig::default());
        req.config_trusted = true;
        let report = run(&req);
        assert_eq!(step(&report, "app").status, DoctorStatus::Fail);
        assert_eq!(step(&report, "dirs").status, DoctorStatus::Pass);
        assert_eq!(step(&report, "ports").status, DoctorStatus::Skip);
        assert_eq!(step(&report, "tunnels").status, DoctorStatus::Skip);
        assert_eq!(step(&report, "web").status, DoctorStatus::Skip);
        cleanup_dir(&dir);
    }

    #[test]
    fn summary_counts_four_statuses() {
        let mk = |id: &str, status: DoctorStatus| DoctorStep {
            id: id.to_string(),
            title: id.to_string(),
            status,
            detail: "x".to_string(),
            advice: String::new(),
            children: None,
            duration_ms: Some(1),
            fix: None,
        };
        let steps = vec![
            mk("a", DoctorStatus::Pass),
            mk("b", DoctorStatus::Pass),
            mk("c", DoctorStatus::Warn),
            mk("d", DoctorStatus::Fail),
            mk("e", DoctorStatus::Skip),
        ];
        let summary = DoctorSummary::from_steps(&steps);
        assert_eq!(
            summary,
            DoctorSummary {
                pass: 2,
                warn: 1,
                fail: 1,
                skip: 1
            }
        );
        assert!(summary.has_fail());
    }

    #[test]
    fn report_serializes_camel_case() {
        let dir = test_dir("json");
        let req = base_request(&dir);
        let report = run(&req);
        let value = serde_json::to_value(&report).unwrap();
        assert!(value.get("schemaVersion").is_some());
        assert!(value.get("checkedAt").is_some());
        assert_eq!(value["mode"], "quick");
        assert_eq!(value["steps"][0]["id"], "app");
        assert_eq!(value["steps"][0]["status"], "pass");
        assert!(value["steps"][0].get("durationMs").is_some());
        assert!(value["steps"][0].get("children").is_none());
        assert!(value.get("summary").is_some());
        // 报告能原样读回来。
        let round_trip: DoctorReport = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip.steps.len(), 9);
        cleanup_dir(&dir);
    }

    #[test]
    fn mode_parse_accepts_empty_and_rejects_unknown() {
        assert_eq!(DoctorMode::parse("").unwrap(), DoctorMode::Quick);
        assert_eq!(DoctorMode::parse(" Quick ").unwrap(), DoctorMode::Quick);
        assert_eq!(DoctorMode::parse("deep").unwrap(), DoctorMode::Deep);
        assert!(DoctorMode::parse("slow").is_err());
    }

    // ---- 超时与出错归因 ----

    #[test]
    fn timeout_failure_gets_d2_advice() {
        let (status, _, advice) = ssh_failure("命令超时（10 秒），已经把它结束。");
        assert_eq!(status, DoctorStatus::Fail);
        assert_eq!(advice, TIMEOUT_ADVICE);

        let result = budget_fail("云端配置");
        assert_eq!(result.status, DoctorStatus::Fail);
        assert_eq!(result.advice, TIMEOUT_ADVICE);
        assert!(result.detail.contains("60"));
    }

    #[test]
    fn ssh_failure_maps_known_reasons() {
        let (status, detail, _) =
            ssh_failure("ssh 退出码 Some(255)：Permission denied (publickey).");
        assert_eq!(status, DoctorStatus::Fail);
        assert!(detail.contains("登录被服务器拒绝"));

        let (status, detail, _) = ssh_failure("ssh 退出码 Some(255)：Connection timed out");
        assert_eq!(status, DoctorStatus::Fail);
        assert!(detail.contains("超时"));

        let (status, detail, _) = ssh_failure("完全认不出的故障 xyz-unknown");
        assert_eq!(status, DoctorStatus::Fail);
        assert!(detail.contains("连服务器失败"));
    }

    #[test]
    fn step_timeout_never_exceeds_step_limit_or_deadline() {
        let now = Instant::now();
        let near_five = step_timeout(now + Duration::from_secs(5), STEP_LIMIT);
        assert!(near_five <= Duration::from_secs(5));
        assert!(near_five > Duration::from_secs(4));
        assert_eq!(step_timeout(now + Duration::from_secs(60), STEP_LIMIT), STEP_LIMIT);
        let past = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
        assert!(step_timeout(past, STEP_LIMIT).is_zero());
        assert!(budget_exhausted(past));
    }

    #[test]
    fn step_budget_is_capped_at_twenty_seconds() {
        let global = Instant::now() + TOTAL_BUDGET;
        let step_end = step_deadline(global);
        assert!(step_end < global);
        assert!(step_end.saturating_duration_since(Instant::now()) <= STEP_LIMIT);

        let step_fail = deadline_fail("公网访问", step_end, global);
        assert_eq!(step_fail.status, DoctorStatus::Fail);
        assert_eq!(step_fail.advice, TIMEOUT_ADVICE);
        assert!(step_fail.detail.contains("20"));

        let global_fail = deadline_fail("公网访问", global, global);
        assert!(global_fail.detail.contains("60"));
    }

    // ---- 公网探测（纯解析 + 判定） ----

    #[test]
    fn probe_parse_reads_all_lines() {
        let out = parse_probe_output(
            "PROBE_HTTPS=200|0.123|0\nPROBE_HTTPS_RC=0\nPROBE_HTTP=301|0.045\nPROBE_HTTP_RC=0\n",
        );
        assert!(!out.curl_missing);
        assert_eq!(out.https_code, Some(200));
        assert_eq!(out.https_ssl, 0);
        assert_eq!(out.https_rc, 0);
        assert_eq!(out.http_code, Some(301));
        assert_eq!(out.http_rc, 0);
    }

    #[test]
    fn probe_parse_treats_000_as_no_code() {
        let out = parse_probe_output("PROBE_HTTPS=000|0.000|0\nPROBE_HTTPS_RC=7\n");
        assert_eq!(out.https_code, None);
        assert_eq!(out.https_rc, 7);
    }

    #[test]
    fn probe_parse_detects_missing_curl() {
        let out = parse_probe_output("PROBE_SKIP=curl_missing\n");
        assert!(out.curl_missing);
        assert!(judge_probe(&out, "a.example.com").is_none());
    }

    #[test]
    fn probe_404_is_warn_and_http_200_is_warn() {
        let out = parse_probe_output(
            "PROBE_HTTPS=404|0.2|0\nPROBE_HTTPS_RC=0\nPROBE_HTTP=200|0.1\nPROBE_HTTP_RC=0\n",
        );
        let (status, detail, advice) = judge_probe(&out, "a.example.com").unwrap();
        assert_eq!(status, DoctorStatus::Warn);
        assert!(detail.contains("http://"));
        assert!(advice.contains("强制 HTTPS"));
    }

    #[test]
    fn probe_401_is_pass() {
        let out = parse_probe_output(
            "PROBE_HTTPS=401|0.2|0\nPROBE_HTTPS_RC=0\nPROBE_HTTP=301|0.1\nPROBE_HTTP_RC=0\n",
        );
        let (status, detail, advice) = judge_probe(&out, "a.example.com").unwrap();
        assert_eq!(status, DoctorStatus::Pass);
        assert!(detail.contains("401"));
        assert_eq!(advice, "");
    }

    #[test]
    fn probe_bad_cert_is_fail() {
        let out = parse_probe_output(
            "PROBE_HTTPS=200|0.2|1\nPROBE_HTTPS_RC=60\nPROBE_HTTP=000|0.0\nPROBE_HTTP_RC=7\n",
        );
        let (status, detail, advice) = judge_probe(&out, "a.example.com").unwrap();
        assert_eq!(status, DoctorStatus::Fail);
        assert!(detail.contains("证书"));
        assert!(advice.contains("续签"));
    }

    #[test]
    fn probe_dns_failure_is_fail_with_dns_advice() {
        let out = parse_probe_output(
            "PROBE_HTTPS=000|0.0|0\nPROBE_HTTPS_RC=6\nPROBE_HTTP=000|0.0\nPROBE_HTTP_RC=6\n",
        );
        let (status, detail, advice) = judge_probe(&out, "a.example.com").unwrap();
        assert_eq!(status, DoctorStatus::Fail);
        assert!(detail.contains("域名解析"));
        assert!(advice.contains("DNS"));
    }

    #[test]
    fn probe_missing_result_is_fail() {
        let out = parse_probe_output("");
        let (status, detail, _) = judge_probe(&out, "a.example.com").unwrap();
        assert_eq!(status, DoctorStatus::Fail);
        assert!(detail.contains("没拿到探测结果"));
    }

    // ---- 域名提取 ----

    #[test]
    fn tunnel_domains_dedupes_case_and_skips_disabled() {
        let tunnels = vec![
            tun("t1", Some("https://A.example.com/x"), true, "running"),
            tun("t2", Some("a.EXAMPLE.com"), true, "running"),
            tun("t3", Some("https://b.example.com"), false, "paused"),
            tun("t4", None, true, "running"),
            tun("t5", Some("   "), true, "running"),
        ];
        assert_eq!(
            tunnel_domains(&tunnels, 10),
            vec!["A.example.com".to_string()]
        );
    }

    #[test]
    fn tunnel_domains_respects_max() {
        let tunnels: Vec<DoctorTunnel> = (0..15)
            .map(|i| {
                let url = format!("d{i}.example.com");
                tun(&format!("t{i}"), Some(&url), true, "running")
            })
            .collect();
        assert_eq!(tunnel_domains(&tunnels, 10).len(), 10);
    }

    // ---- 各步的失败/建议映射 ----

    #[test]
    fn dirs_step_flags_bad_config() {
        let dir = test_dir("dirsbad");
        let req = base_request(&dir);
        std::fs::write(dir.join("config.json.bad"), b"{}").unwrap();
        let result = step_dirs(&req);
        assert_eq!(result.status, DoctorStatus::Fail);
        assert!(result.detail.contains("config.json.bad"));
        assert!(result.fix.is_some());
        cleanup_dir(&dir);
    }

    #[test]
    fn dirs_step_passes_without_config_file() {
        let dir = test_dir("dirsfirst");
        let mut req = DoctorRequest::new(DoctorMode::Quick, dir.clone());
        req.config = None;
        let result = step_dirs(&req);
        assert_eq!(result.status, DoctorStatus::Pass);
        cleanup_dir(&dir);
    }

    #[test]
    fn ports_step_down_service_fails_with_advice() {
        let dir = test_dir("ports");
        let mut req = base_request(&dir);
        req.services = vec![
            DoctorService {
                id: "svc-1".into(),
                name: "相册".into(),
                local_port: 8080,
                health: "up".into(),
            },
            DoctorService {
                id: "svc-2".into(),
                name: "看图".into(),
                local_port: 8081,
                health: "down".into(),
            },
        ];
        let result = step_ports(&req);
        assert_eq!(result.status, DoctorStatus::Fail);
        assert!(result.detail.contains("看图"));
        assert!(result.advice.contains("启动"));
        cleanup_dir(&dir);
    }

    #[test]
    fn ports_step_unknown_does_not_fail() {
        let dir = test_dir("portsunknown");
        let mut req = base_request(&dir);
        req.services = vec![DoctorService {
            id: "svc-1".into(),
            name: "相册".into(),
            local_port: 8080,
            health: "unknown".into(),
        }];
        let result = step_ports(&req);
        assert_eq!(result.status, DoctorStatus::Pass);
        assert!(result.detail.contains("还没探到"));
        cleanup_dir(&dir);
    }

    #[test]
    fn servers_step_reports_missing_key_file() {
        let dir = test_dir("servers");
        let mut req = base_request(&dir);
        req.servers = vec![server("srv-1", Some("C:\\not-exist\\key"))];
        req.tunnels = vec![tun("t1", Some("a.example.com"), true, "running")];
        let result = step_servers(&req);
        assert_eq!(result.status, DoctorStatus::Fail);
        assert!(result.detail.contains("密钥文件不在了"));
        assert!(result.fix.is_some());
        cleanup_dir(&dir);
    }

    #[test]
    fn servers_step_skips_without_enabled_tunnels() {
        let dir = test_dir("serversskip");
        let mut req = base_request(&dir);
        req.servers = vec![server("srv-1", None)];
        req.tunnels = vec![tun("t1", None, false, "paused")];
        assert_eq!(step_servers(&req).status, DoctorStatus::Skip);
        cleanup_dir(&dir);
    }

    #[test]
    fn tunnels_step_flags_error_status() {
        let dir = test_dir("tunnels");
        let mut req = base_request(&dir);
        req.services = vec![DoctorService {
            id: "svc-1".into(),
            name: "相册".into(),
            local_port: 8080,
            health: "up".into(),
        }];
        req.servers = vec![server("srv-1", None)];
        let mut t = tun("t1", Some("a.example.com"), true, "error");
        t.reason = Some("登录被服务器拒绝了".into());
        req.tunnels = vec![t];
        let result = step_tunnels(&req);
        assert_eq!(result.status, DoctorStatus::Fail);
        assert!(result.detail.contains("登录被服务器拒绝了"));
        assert!(result.fix.is_some());
        cleanup_dir(&dir);
    }

    #[test]
    fn tunnels_step_allows_missing_url() {
        let dir = test_dir("tunnelnourl");
        let mut req = base_request(&dir);
        req.services = vec![DoctorService {
            id: "svc-1".into(),
            name: "相册".into(),
            local_port: 8080,
            health: "up".into(),
        }];
        req.servers = vec![server("srv-1", None)];
        req.tunnels = vec![tun("t1", None, true, "running")];
        let result = step_tunnels(&req);
        assert_eq!(result.status, DoctorStatus::Pass);
        assert!(result.detail.contains("没填网址"));
        cleanup_dir(&dir);
    }

    #[test]
    fn web_child_skips_tunnel_that_is_not_running() {
        let dir = test_dir("webpause");
        let mut req = base_request(&dir);
        req.mode = DoctorMode::Deep;
        req.servers = vec![server("srv-1", None)];
        let result = check_web_tunnel(
            &req,
            &tun("t1", Some("a.example.com"), true, "paused"),
            Instant::now() + STEP_LIMIT,
            Instant::now() + TOTAL_BUDGET,
        );
        assert_eq!(result.status, DoctorStatus::Skip);
        cleanup_dir(&dir);
    }

    #[test]
    fn web_step_skips_when_offline() {
        let dir = test_dir("weboffline");
        let mut req = DoctorRequest::new(DoctorMode::Deep, dir.clone());
        req.app_running = false;
        req.config = Some(AppConfig::default());
        let result = step_web(&req, Instant::now() + TOTAL_BUDGET);
        assert_eq!(result.status, DoctorStatus::Skip);
        assert!(result.detail.contains("应用没在跑"));
        cleanup_dir(&dir);
    }

    #[test]
    fn cloud_step_skips_without_servers() {
        let dir = test_dir("cloudempty");
        let mut req = base_request(&dir);
        req.mode = DoctorMode::Deep;
        req.config = Some(AppConfig::default());
        let result = step_cloud(&req, Instant::now() + TOTAL_BUDGET);
        assert_eq!(result.status, DoctorStatus::Skip);
        cleanup_dir(&dir);
    }

    #[test]
    fn vhost_dir_resolution_prefers_server_then_detected() {
        let dir = test_dir("vhost");
        let mut req = base_request(&dir);

        let mut srv = server("srv-1", None);
        srv.vhost_dir = Some("/www/server/panel/vhost/".into());
        assert_eq!(resolve_vhost_dir(&req, &srv), "/www/server/panel/vhost");

        let mut srv = server("srv-1", None);
        srv.vhost_dir = None;
        req.detected_vhost_dirs
            .insert("srv-1".into(), "/xp/panel/vhost/".into());
        assert_eq!(resolve_vhost_dir(&req, &srv), "/xp/panel/vhost");
        cleanup_dir(&dir);
    }

    #[test]
    fn effective_tunnels_fall_back_to_config_offline() {
        let dir = test_dir("fallback");
        let mut req = DoctorRequest::new(DoctorMode::Deep, dir.clone());
        let mut config = AppConfig::default();
        config.services.push(crate::config::LocalService {
            id: "svc-1".into(),
            name: "相册".into(),
            local_port: 8080,
            probe_kind: crate::config::DEFAULT_PROBE_KIND.to_string(),
            launch: None,
            watch: false,
            autostart: false,
            note: None,
        });
        config.tunnels.push(crate::config::Tunnel {
            id: "tun-1".into(),
            name: "发布".into(),
            service_id: "svc-1".into(),
            server_id: "srv-1".into(),
            remote_port: 18080,
            url: Some("a.example.com".into()),
            key_path: None,
            access: Vec::new(),
            access_user: None,
            access_hash: None,
            access_detail: None,
            enabled: true,
            preset: "web".into(),
        });
        req.config = Some(config);

        let tunnels = effective_tunnels(&req);
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].id, "tun-1");
        assert_eq!(tunnels[0].local_port, 8080);
        assert!(tunnels[0].enabled);
        assert_eq!(tunnels[0].status, "unknown");
        cleanup_dir(&dir);
    }

    // ---- 本机 TCP 探测 ----

    #[test]
    fn tcp_probe_succeeds_on_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_probe("127.0.0.1", port, Duration::from_secs(2)).is_ok());
    }

    #[test]
    fn tcp_probe_fails_on_closed_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(tcp_probe("127.0.0.1", port, Duration::from_millis(500)).is_err());
    }

    // ---- 落盘与事件 ----

    #[test]
    fn run_writes_report_file_and_doctor_ran_event() {
        let dir = test_dir("persist");
        let mut req = base_request(&dir);
        req.events = Some(Arc::new(EventLog::new(&dir)));
        let report = run(&req);

        let path = dir.join("logs").join("doctor-last.json");
        assert!(path.is_file(), "报告文件应该写出来");
        let text = std::fs::read_to_string(&path).unwrap();
        let saved: DoctorReport = serde_json::from_str(&text).unwrap();
        assert_eq!(saved.schema_version, SCHEMA_VERSION);
        assert_eq!(saved.steps.len(), 9);
        assert_eq!(saved.summary, report.summary);

        let events = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert!(events.contains("doctorRan"), "events.jsonl 里应该有 doctorRan：{events}");
        cleanup_dir(&dir);
    }

    #[test]
    fn step_results_are_capped_and_never_empty() {
        let result = StepResult::fail("", "");
        let step = make_step("x".into(), "标题".into(), result, 5);
        assert_eq!(step.detail, "（没有更多信息）");
        assert_eq!(step.advice, "");
        assert_eq!(step.duration_ms, Some(5));

        let long = "很长的文本".repeat(100);
        let result = StepResult::fail(long.clone(), long);
        let step = make_step("x".into(), "标题".into(), result, 1);
        assert!(step.detail.chars().count() <= DETAIL_MAX_CHARS);
        assert!(step.advice.chars().count() <= ADVICE_MAX_CHARS);
    }
}
