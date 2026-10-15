//! 应用配置：唯一事实来源（对齐 DESIGN.md §3.3「状态存储」）。
//!
//! - 配置文件能手改、能备份；保存用「临时文件 + 改名」原子替换，防掉电写坏。
//! - 密钥只存路径引用，绝不存密钥内容。
//! - v6 在 v5 上只增不改（`settings` + 隧道 `preset`），旧版本二进制读 v6 文本不会报错。
//! - config 版本高于程序支持时「降级拦截」：只读不写、原文件一字不动、不产生 .bad。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 当前配置版本。
/// 3 → 4：把「服务」拆成「本机服务 + 隧道」。
/// 4 → 5：访问保护从单选（none/password/ip）变成可叠加的数组（password + ip 可以同时要）。
/// 5 → 6：新增 `settings`（刷新间隔 / 静音时段 / 启动检查更新）与隧道 `preset`。
pub const CONFIG_VERSION: u32 = 6;

/// 便携模式标记文件：放在程序（exe）旁边，空文件即可。
pub const PORTABLE_MARKER: &str = "shangxian.portable";
/// 便携模式的数据子目录：`<exe 目录>\data`。
pub const PORTABLE_SUBDIR: &str = "data";

/// 发布预设的默认值，也是非法值的归一目标。
pub const DEFAULT_PRESET: &str = "web";

fn default_ssh_port() -> u16 {
    22
}

fn default_true() -> bool {
    true
}

fn default_refresh_interval_ms() -> u32 {
    800
}

fn default_quiet_start() -> String {
    "22:00".to_string()
}

fn default_quiet_end() -> String {
    "08:00".to_string()
}

fn default_preset() -> String {
    DEFAULT_PRESET.to_string()
}

/// 本机服务探测方式的默认值：TCP。
pub const DEFAULT_PROBE_KIND: &str = "tcp";

fn default_probe_kind() -> String {
    DEFAULT_PROBE_KIND.to_string()
}

/// 探测方式是否程序认识（`tcp` 只看能不能连上，`http` 要一个响应）。
pub fn is_known_probe_kind(value: &str) -> bool {
    matches!(value, "tcp" | "http")
}

/// 归一探测方式：认得的原样返回，认不得的一律换成 `"tcp"`（读配置时用）。
pub fn normalize_probe_kind(value: &str) -> String {
    if is_known_probe_kind(value) {
        value.to_string()
    } else {
        DEFAULT_PROBE_KIND.to_string()
    }
}

// ---------- v6 设置 ----------

/// 全局设置（v6 新增，存 `config.json` 的 `settings`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// 界面刷新间隔（毫秒）。范围 500..=10000。
    #[serde(default = "default_refresh_interval_ms")]
    pub refresh_interval_ms: u32,
    /// 静音时段：这段时间不弹系统通知（事件照记）。
    #[serde(default)]
    pub quiet_hours: QuietHours,
    /// 启动时检查更新。
    #[serde(default = "default_true")]
    pub check_update_on_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_interval_ms: default_refresh_interval_ms(),
            quiet_hours: QuietHours::default(),
            check_update_on_start: true,
        }
    }
}

/// 静音时段开关与起止时间（`HH:MM`，24 小时制，允许跨午夜）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuietHours {
    /// 默认关。
    #[serde(default)]
    pub enabled: bool,
    /// `"HH:MM"`，默认 `"22:00"`。
    #[serde(default = "default_quiet_start")]
    pub start: String,
    /// `"HH:MM"`，默认 `"08:00"`。
    #[serde(default = "default_quiet_end")]
    pub end: String,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start: default_quiet_start(),
            end: default_quiet_end(),
        }
    }
}

impl QuietHours {
    /// 现在（当天第几分钟）在不在静音窗口里。分钟粒度、左闭右开；
    /// `start == end`（或时间解析不了）视为关闭，永远返回 false。
    pub fn is_active_at(&self, minutes_of_day: u32) -> bool {
        if !self.enabled {
            return false;
        }
        let (Some((sh, sm)), Some((eh, em))) = (parse_hhmm(&self.start), parse_hhmm(&self.end))
        else {
            return false;
        };
        let start = sh * 60 + sm;
        let end = eh * 60 + em;
        if start == end {
            return false;
        }
        let now = minutes_of_day % (24 * 60);
        if start < end {
            now >= start && now < end
        } else {
            // 跨午夜：22:00 → 08:00 这种
            now >= start || now < end
        }
    }
}

/// 严格解析 `HH:MM`（两位数字，00:00..=23:59）；别的写法一律 None。
pub fn parse_hhmm(text: &str) -> Option<(u32, u32)> {
    let (h, m) = text.split_once(':')?;
    if h.len() != 2 || m.len() != 2 || !h.bytes().all(|b| b.is_ascii_digit()) || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some((hour, minute))
}

/// 发布预设是否为程序认识的三种之一。
pub fn is_known_preset(preset: &str) -> bool {
    matches!(preset, "web" | "ws" | "upload")
}

/// 归一预设：认得的原样返回，认不得的一律换成 `"web"`（读配置时用）。
pub fn normalize_preset(preset: &str) -> String {
    if is_known_preset(preset) {
        preset.to_string()
    } else {
        DEFAULT_PRESET.to_string()
    }
}

/// 写配置时校验预设（读时归一化，写时拒绝）。
pub fn check_preset_strict(preset: &str) -> Result<(), String> {
    if is_known_preset(preset) {
        Ok(())
    } else {
        Err("发布预设只支持 web / ws / upload 三种".to_string())
    }
}

const SETTINGS_KEYS: [&str; 3] = ["refreshIntervalMs", "quietHours", "checkUpdateOnStart"];
const QUIET_KEYS: [&str; 3] = ["enabled", "start", "end"];

/// 写配置时严格校验整份 settings（`set_settings` 用）。
///
/// 参数必须是三块全给的整份对象；缺字段、越界、时间格式错、开始等于结束、
/// 多余的键，一律拒绝并返回人话（文案见 D2 §3.4，逐字）。
pub fn parse_settings_strict(value: &serde_json::Value) -> Result<Settings, String> {
    let Some(obj) = value.as_object() else {
        return Err("设置项不完整：缺少 refreshIntervalMs、quietHours、checkUpdateOnStart，请整份设置一起提交".to_string());
    };

    let missing: Vec<&str> = SETTINGS_KEYS
        .iter()
        .copied()
        .filter(|k| !obj.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "设置项不完整：缺少 {}，请整份设置一起提交",
            missing.join("、")
        ));
    }
    for key in obj.keys() {
        if !SETTINGS_KEYS.contains(&key.as_str()) {
            return Err(format!("设置中包含无法识别的项：{key}"));
        }
    }

    let refresh = match obj.get("refreshIntervalMs").and_then(|v| v.as_u64()) {
        Some(n) if (500..=10000).contains(&n) => n as u32,
        _ => return Err("界面刷新间隔只能在 500 到 10000 毫秒之间".to_string()),
    };

    let Some(quiet) = obj.get("quietHours").and_then(|v| v.as_object()) else {
        return Err(
            "设置项不完整：缺少 quietHours.enabled、quietHours.start、quietHours.end，请整份设置一起提交"
                .to_string(),
        );
    };
    let quiet_missing: Vec<String> = QUIET_KEYS
        .iter()
        .filter(|k| !quiet.contains_key(**k))
        .map(|k| format!("quietHours.{k}"))
        .collect();
    if !quiet_missing.is_empty() {
        return Err(format!(
            "设置项不完整：缺少 {}，请整份设置一起提交",
            quiet_missing.join("、")
        ));
    }
    for key in quiet.keys() {
        if !QUIET_KEYS.contains(&key.as_str()) {
            return Err(format!("设置中包含无法识别的项：quietHours.{key}"));
        }
    }

    let enabled = quiet
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| "设置项不完整：quietHours.enabled 的值格式不正确，请整份设置一起提交".to_string())?;
    let start = quiet
        .get("start")
        .and_then(|v| v.as_str())
        .filter(|s| parse_hhmm(s).is_some())
        .ok_or_else(|| "免打扰时段须填写 24 小时制时间，例如 22:00".to_string())?
        .to_string();
    let end = quiet
        .get("end")
        .and_then(|v| v.as_str())
        .filter(|s| parse_hhmm(s).is_some())
        .ok_or_else(|| "免打扰时段须填写 24 小时制时间，例如 22:00".to_string())?
        .to_string();
    if start == end {
        return Err("免打扰时段的开始和结束不能相同；如不需要免打扰，请关闭该开关".to_string());
    }

    let check_update_on_start = obj
        .get("checkUpdateOnStart")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            "设置项不完整：checkUpdateOnStart 的值格式不正确，请整份设置一起提交".to_string()
        })?;

    Ok(Settings {
        refresh_interval_ms: refresh,
        quiet_hours: QuietHours {
            enabled,
            start,
            end,
        },
        check_update_on_start,
    })
}

// ---------- 配置结构 ----------

/// 一台云端服务器。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    /// 主机名或 IP（例：api.example.com）
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    /// 登录用户名
    pub user: String,
    /// 私钥文件路径；None = 用 ssh 默认密钥
    #[serde(default)]
    pub key_path: Option<String>,
    /// 这台服务器上面板放站点的目录（可空 = 按环境变量/小皮默认找）。
    /// 小皮面板默认 /xp/panel/vhost；宝塔等面板填 /www/server/panel/vhost。
    /// nginx 配置在它下面的 nginx/ 里，htpasswd 密码文件直接在它下面。
    /// 解析顺序：服务器这个字段 → 环境变量 SHANGXIAN_HTPASSWD_DIR → 小皮默认
    /// （解析函数是 nginx 模块里的 htpasswd_dir_for）。
    #[serde(default)]
    pub vhost_dir: Option<String>,
}

/// 怎么启动一台本机服务（整个 launch 留空 = 只监控不启动）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSpec {
    /// 程序或脚本路径（.bat/.cmd 走 cmd，.ps1 走 powershell，.sh 走 bash）
    pub exe: String,
    /// 启动参数（一行文本，支持双引号包住带空格的参数）
    #[serde(default)]
    pub args: String,
    /// 工作目录（可空 = 用程序自己的目录）
    #[serde(default)]
    pub workdir: Option<String>,
}

/// 一台本机服务：这台电脑上的一个东西（跟发布到哪台服务器无关）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalService {
    pub id: String,
    pub name: String,
    /// 本机端口
    pub local_port: u16,
    /// 探测方式：`"tcp"`（默认，只看能不能连上）| `"http"`（要一个 HTTP 响应）。
    /// 非法值读时归一成 `"tcp"` 并记 warn 事件（D1 §14.6）。
    #[serde(default = "default_probe_kind")]
    pub probe_kind: String,
    /// 启动方式；None = 只监控不启动
    #[serde(default)]
    pub launch: Option<LaunchSpec>,
    /// 看护：进程掉了自动拉起
    #[serde(default)]
    pub watch: bool,
    /// 开机自启：登录后自动启动
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub note: Option<String>,
}

impl LocalService {
    /// 配置里的探测方式 → `probe` 模块的枚举（认不得按 TCP；读配置时已经归一过）。
    pub fn probe_kind_enum(&self) -> crate::probe::ProbeKind {
        if self.probe_kind == "http" {
            crate::probe::ProbeKind::Http
        } else {
            crate::probe::ProbeKind::Tcp
        }
    }
}

/// 一条发布隧道：把某台本机服务接到某台云服务器上。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tunnel {
    pub id: String,
    pub name: String,
    /// 指向 LocalService
    pub service_id: String,
    /// 指向 ServerConfig
    pub server_id: String,
    /// 云端端口（写死，永不自动漂移）
    pub remote_port: u16,
    /// 公网网址（可空）
    #[serde(default)]
    pub url: Option<String>,
    /// 覆盖服务器的密钥；None = 继承服务器
    #[serde(default)]
    pub key_path: Option<String>,
    /// 访问保护：["password"]、["ip"] 或两者叠加；空 = 谁都能进。
    /// 老配置里写字符串（none / password / ip / "password,ip"）也能读进来，见
    /// [`deserialize_access`]。
    #[serde(default, deserialize_with = "deserialize_access")]
    pub access: Vec<String>,
    /// 访问密码的用户名（配 nginx 时用）
    #[serde(default)]
    pub access_user: Option<String>,
    /// 访问密码的哈希（优先 bcrypt `$2y$12$…`；服务器 nginx 验不了时是兼容格式 `$apr1$…`；
    /// 明文密码只显示一次，不落盘）
    #[serde(default)]
    pub access_hash: Option<String>,
    /// 访问保护细节（如 IP 白名单，逗号分隔）
    #[serde(default)]
    pub access_detail: Option<String>,
    /// 是否正在发布（false = 已暂停）
    #[serde(default)]
    pub enabled: bool,
    /// nginx 片段预设：web（默认）| ws | upload。
    /// 非法值一律归一成 "web" 并记一条 warn 事件。
    #[serde(default = "default_preset")]
    pub preset: String,
}

impl Tunnel {
    /// 这条隧道要不要密码保护。
    pub fn wants_password(&self) -> bool {
        self.access.iter().any(|a| a == "password")
    }

    /// 这条隧道要不要只允许指定 IP。
    pub fn wants_ip(&self) -> bool {
        self.access.iter().any(|a| a == "ip")
    }
}

// ---------- 访问保护的「柔性」读写 ----------
//
// 老配置、老客户端可能把访问保护写成字符串（none / password / ip / "password,ip"），
// 新结构统一用数组。这两个函数只认 password 和 ip，认不出的值直接忽略，
// 结果固定按 ["password", "ip"] 的顺序去重，保证同一个含义只有一种写法。

/// 把一段文本拆成访问保护开关（`"password,ip"` / `"password+ip"` / `"password ip"` 都认）。
pub fn access_from_text(text: &str) -> Vec<String> {
    let mut password = false;
    let mut ip = false;
    scan_access_text(text, &mut password, &mut ip);
    access_from_flags(password, ip)
}

fn scan_access_text(text: &str, password: &mut bool, ip: &mut bool) {
    for token in text.split(|c: char| c == ',' || c == '+' || c == ';' || c.is_whitespace()) {
        match token.trim() {
            "password" => *password = true,
            "ip" => *ip = true,
            _ => {}
        }
    }
}

fn access_from_flags(password: bool, ip: bool) -> Vec<String> {
    let mut out = Vec::new();
    if password {
        out.push("password".to_string());
    }
    if ip {
        out.push("ip".to_string());
    }
    out
}

fn access_from_value(value: serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(s) => access_from_text(&s),
        serde_json::Value::Array(items) => {
            let mut password = false;
            let mut ip = false;
            for item in items {
                if let Some(s) = item.as_str() {
                    scan_access_text(s, &mut password, &mut ip);
                }
            }
            access_from_flags(password, ip)
        }
        _ => Vec::new(),
    }
}

/// `access` 字段里有没有程序不认识的写法（导入时给 warning 用）。
/// `None`/缺失不算；`"none"`、空串是认识的（= 无保护）。
pub fn access_unknown_tokens(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut check = |token: &str| {
        let t = token.trim();
        if !t.is_empty() && t != "none" && t != "password" && t != "ip" && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    };
    match value {
        serde_json::Value::String(s) => {
            for token in s.split(|c: char| c == ',' || c == '+' || c == ';' || c.is_whitespace()) {
                check(token);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    for token in
                        s.split(|c: char| c == ',' || c == '+' || c == ';' || c.is_whitespace())
                    {
                        check(token);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// `Tunnel::access` 用的柔性反序列化：null / 缺失 / "none" / "" → 空数组。
pub fn deserialize_access<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)
        .map_err(serde::de::Error::custom)?;
    Ok(value.map(access_from_value).unwrap_or_default())
}

/// 编辑隧道输入用的柔性反序列化：缺失或 null = 保持原样（None）；
/// 给了就替换（空数组或 ["none"] 归一成空数组）。
pub fn deserialize_access_opt<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)
        .map_err(serde::de::Error::custom)?;
    Ok(value.map(access_from_value))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub version: u32,
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
    /// 本机服务（这台电脑上的东西）
    #[serde(default)]
    pub services: Vec<LocalService>,
    /// 发布隧道（服务 → 服务器）
    #[serde(default)]
    pub tunnels: Vec<Tunnel>,
    /// 掉线时是否弹系统通知（默认开，留在根字段不迁进 settings）
    #[serde(default = "default_true")]
    pub notify_enabled: bool,
    /// 全局设置（v6 新增）
    #[serde(default)]
    pub settings: Settings,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            servers: Vec::new(),
            services: Vec::new(),
            tunnels: Vec::new(),
            notify_enabled: true,
            settings: Settings::default(),
        }
    }
}

/// v3 及以前的老结构（服务 = 本机端口 + 云端端口 + 服务器，捆在一起）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyConfig {
    #[serde(default)]
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    servers: Vec<ServerConfig>,
    #[serde(default)]
    services: Vec<LegacyService>,
    #[serde(default = "default_true")]
    notify_enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyService {
    id: String,
    name: String,
    #[serde(default)]
    enabled: bool,
    server_id: String,
    local_port: u16,
    remote_port: u16,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    key_path: Option<String>,
    #[serde(default)]
    access: Option<String>,
    #[serde(default)]
    access_user: Option<String>,
    #[serde(default)]
    access_hash: Option<String>,
    #[serde(default)]
    access_detail: Option<String>,
}

impl LegacyConfig {
    /// 老配置拆成「本机服务 + 隧道」：同一个本机端口只建一台服务，
    /// 隧道沿用老的服务 id（界面上的标识保持稳定）。
    fn migrate(self) -> AppConfig {
        let mut services: Vec<LocalService> = Vec::new();
        let mut tunnels: Vec<Tunnel> = Vec::new();
        for svc in self.services {
            let service_id = match services.iter().find(|s| s.local_port == svc.local_port) {
                Some(existing) => existing.id.clone(),
                None => {
                    let id = new_id("svc");
                    services.push(LocalService {
                        id: id.clone(),
                        name: svc.name.clone(),
                        local_port: svc.local_port,
                        probe_kind: default_probe_kind(),
                        launch: None,
                        watch: false,
                        autostart: false,
                        note: None,
                    });
                    id
                }
            };
            tunnels.push(Tunnel {
                id: svc.id,
                name: svc.name,
                service_id,
                server_id: svc.server_id,
                remote_port: svc.remote_port,
                url: svc.url,
                key_path: svc.key_path,
                access: access_from_text(svc.access.as_deref().unwrap_or("")),
                access_user: svc.access_user,
                access_hash: svc.access_hash,
                access_detail: svc.access_detail,
                enabled: svc.enabled,
                preset: default_preset(),
            });
        }
        AppConfig {
            version: CONFIG_VERSION,
            servers: self.servers,
            services,
            tunnels,
            notify_enabled: self.notify_enabled,
            settings: Settings::default(),
        }
    }
}

// ---------- 读时归一化 / 写时拒绝 ----------

/// 把内存里的配置按 v6 规则归一化（读配置、导入后都调用）：
/// - 刷新间隔夹到 500..=10000；
/// - 静音时段时间格式不对 → 默认值；
/// - 隧道 preset 不认识的 → `"web"`。
///
/// 返回被改过的字段名（人话，用于 `configNormalized` 事件），没改就是空。
/// 归一化不写盘：下次用户操作保存时自然落盘。
pub fn normalize_config(cfg: &mut AppConfig) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();

    if cfg.settings.refresh_interval_ms < 500 {
        cfg.settings.refresh_interval_ms = 500;
        notes.push("settings.refreshIntervalMs".to_string());
    } else if cfg.settings.refresh_interval_ms > 10000 {
        cfg.settings.refresh_interval_ms = 10000;
        notes.push("settings.refreshIntervalMs".to_string());
    }

    if parse_hhmm(&cfg.settings.quiet_hours.start).is_none() {
        cfg.settings.quiet_hours.start = default_quiet_start();
        notes.push("settings.quietHours.start".to_string());
    }
    if parse_hhmm(&cfg.settings.quiet_hours.end).is_none() {
        cfg.settings.quiet_hours.end = default_quiet_end();
        notes.push("settings.quietHours.end".to_string());
    }

    let mut bad_preset = false;
    for tunnel in &mut cfg.tunnels {
        if !is_known_preset(&tunnel.preset) {
            tunnel.preset = default_preset();
            bad_preset = true;
        }
    }
    if bad_preset {
        notes.push("tunnels[].preset".to_string());
    }

    let mut bad_probe = false;
    for service in &mut cfg.services {
        if !is_known_probe_kind(&service.probe_kind) {
            service.probe_kind = default_probe_kind();
            bad_probe = true;
        }
    }
    if bad_probe {
        notes.push("services[].probeKind".to_string());
    }

    notes.sort();
    notes.dedup();
    notes
}

// ---------- 读配置 ----------

/// 这次读配置发生了什么（给事件用）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadOutcome {
    /// 从哪个版本迁移上来的；None = 没有迁移。
    pub migrated_from: Option<u32>,
    /// 归一化过的字段（已去重）。
    pub normalized: Vec<String>,
}

impl AppConfig {
    /// 读配置（保持原签名，事件信息看 [`AppConfig::load_detailed`]）。
    /// 老版本会自动迁移：
    /// - v5：备份成 `config.json.v5.bak`，加 version=6、settings 默认块、隧道 preset="web"；
    /// - v4：结构和 v5 一样，只是访问保护还写成字符串，柔性反序列化直接兼容；
    ///   先备份成 `config.json.v4.bak`，再写回 v6；
    /// - v3 及以前：先备份成 `config.json.v3.bak`，走老结构迁移再写回 v6。
    ///
    /// 读坏时绝不按空配置糊弄过去：把原文件复制留底成 `config.json.bad`，再把原文件
    /// 挪走，然后返回清楚的错误。这样调用方不会拿空配置把用户的配置冲掉。
    ///
    /// 版本高于程序支持（>6）：降级拦截，返回 InvalidData 错误，**原文件一字不动、
    /// 不产生 .bad、不迁移**（见 D2 §13）。
    pub fn load(path: &Path) -> io::Result<Self> {
        Self::load_detailed(path).map(|(cfg, _)| cfg)
    }

    /// 和 [`AppConfig::load`] 一样，另外返回「迁移/归一化」信息，给事件用。
    pub fn load_detailed(path: &Path) -> io::Result<(Self, LoadOutcome)> {
        let text = fs::read_to_string(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                e
            } else {
                // 连读都读不出来（权限、编码坏了）：也把原文件留底，别让它被空配置盖掉
                let _ = quarantine_bad_config(path);
                io::Error::new(
                    e.kind(),
                    format!(
                        "配置文件无法读取：{e}；原文件已备份为 config.json.bad，修复后再打开应用"
                    ),
                )
            }
        })?;

        let mut outcome = LoadOutcome::default();
        let mut cfg = match read_version(&text) {
            VersionRead::NotNumber => return Err(corrupt_version_error(path)),
            VersionRead::Value(version) if version > CONFIG_VERSION => {
                return Err(config_unsupported_error(version));
            }
            VersionRead::Value(version) => {
                if version == CONFIG_VERSION {
                    let cfg = parse_current(&text, path, "配置文件格式不正确")?;
                    cfg
                } else if version == 5 {
                    let mut cfg = parse_current(&text, path, "配置文件格式不正确")?;
                    backup_once(path, "json.v5.bak", &text);
                    cfg.version = CONFIG_VERSION;
                    // 写回是尽力而为：磁盘满/没权限也不能让调用方退回空配置。
                    let _ = cfg.save(path);
                    outcome.migrated_from = Some(5);
                    cfg
                } else if version == 4 {
                    let mut cfg = parse_current(&text, path, "配置文件格式不正确")?;
                    backup_once(path, "json.v4.bak", &text);
                    cfg.version = CONFIG_VERSION;
                    let _ = cfg.save(path);
                    outcome.migrated_from = Some(4);
                    cfg
                } else {
                    let old: LegacyConfig = serde_json::from_str(&text).map_err(|e| {
                        corrupt_config_error(path, "旧配置文件格式不正确，无法迁移", &e)
                    })?;
                    let migrated = old.migrate();
                    backup_once(path, "json.v3.bak", &text);
                    // 同上：迁移后的写回失败不报错，宁可内存里用迁移结果。
                    let _ = migrated.save(path);
                    outcome.migrated_from = Some(version);
                    migrated
                }
            }
            // 不是 JSON 或没有 version：按老结构走，解析失败会留底报错
            VersionRead::NotObject => {
                let old: LegacyConfig = serde_json::from_str(&text).map_err(|e| {
                    corrupt_config_error(path, "旧配置文件格式不正确，无法迁移", &e)
                })?;
                let migrated = old.migrate();
                backup_once(path, "json.v3.bak", &text);
                let _ = migrated.save(path);
                outcome.migrated_from = Some(1);
                migrated
            }
        };

        outcome.normalized = normalize_config(&mut cfg);
        Ok((cfg, outcome))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?; // 原子替换
        Ok(())
    }

    /// 找一台本机服务。
    pub fn service(&self, id: &str) -> Option<&LocalService> {
        self.services.iter().find(|s| s.id == id)
    }

    /// 找一条隧道。
    pub fn tunnel(&self, id: &str) -> Option<&Tunnel> {
        self.tunnels.iter().find(|t| t.id == id)
    }

    /// 一台服务器上已经占用的云端端口。
    pub fn used_remote_ports(&self, server_id: &str, except_tunnel: Option<&str>) -> Vec<u16> {
        self.tunnels
            .iter()
            .filter(|t| t.server_id == server_id && except_tunnel != Some(t.id.as_str()))
            .map(|t| t.remote_port)
            .collect()
    }

    /// 本机服务被几条隧道用着。
    pub fn tunnels_of_service(&self, service_id: &str) -> Vec<&Tunnel> {
        self.tunnels
            .iter()
            .filter(|t| t.service_id == service_id)
            .collect()
    }
}

/// 一次「无条件备份」：文件不存在才写（不覆盖更早的旧版备份）。
fn backup_once(path: &Path, ext: &str, text: &str) {
    let backup = path.with_extension(ext);
    if !backup.exists() {
        let _ = fs::write(&backup, text);
    }
}

/// 解析当前结构；失败就留底报错。
fn parse_current(text: &str, path: &Path, what: &str) -> io::Result<AppConfig> {
    serde_json::from_str(text).map_err(|e| corrupt_config_error(path, what, &e))
}

enum VersionRead {
    /// 读到了数字版本号。
    Value(u32),
    /// version 字段存在但不是数字（按「格式不对」留底）。
    NotNumber,
    /// 不是 JSON 对象，或者没有 version（按老结构走）。
    NotObject,
}

/// 从配置文本里读 version。
fn read_version(text: &str) -> VersionRead {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => match map.get("version") {
            Some(value) => match value.as_u64() {
                Some(n) if n <= u32::MAX as u64 => VersionRead::Value(n as u32),
                _ => VersionRead::NotNumber,
            },
            None => VersionRead::NotObject,
        },
        _ => VersionRead::NotObject,
    }
}

/// 把读坏的原文件复制留底成 `config.json.bad`，再删掉原文件（免得被空配置覆盖）。
/// 已经有旧的 `.bad` 时直接覆盖成最新的这份。
fn quarantine_bad_config(path: &Path) -> io::Result<PathBuf> {
    let bad = path.with_extension("json.bad");
    fs::copy(path, &bad)?;
    fs::remove_file(path)?;
    Ok(bad)
}

/// 配置读坏时的统一处理：留底 + 一句人话错误（绝不返回空配置）。
fn corrupt_config_error(path: &Path, what: &str, err: &serde_json::Error) -> io::Error {
    let tail = quarantine_tail(path);
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{what}：{err}；{tail}；修复后再打开应用"),
    )
}

fn corrupt_version_error(path: &Path) -> io::Error {
    let tail = quarantine_tail(path);
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("配置文件格式不正确：version 字段不是数字；{tail}；修复后再打开应用"),
    )
}

fn quarantine_tail(path: &Path) -> String {
    match quarantine_bad_config(path) {
        Ok(bad) => format!("原文件已备份为 {}", bad.display()),
        Err(e) => format!("原文件备份失败（{e}），请先手动备份再修改"),
    }
}

// ---------- 降级拦截（config 版本高于程序支持） ----------

/// 降级拦截的固定文案第一句（D2 §13.2，逐字）。
pub fn config_unsupported_message(found: u32) -> String {
    format!(
        "这份设置是新版「上线助手」存的（版本 {found}），当前版本只认到 {CONFIG_VERSION}。请先升级到最新版再打开；设置文件未被修改。"
    )
}

/// 界面红条用的文案：第一句 + 第二句（同一句里空格连接）。
pub fn config_unsupported_bar_message(found: u32) -> String {
    format!(
        "{} 升级后会自动读回，不用重新配。",
        config_unsupported_message(found)
    )
}

/// 降级拦截的统一错误（load / 导入都用它）。
pub fn config_unsupported_error(found: u32) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, config_unsupported_message(found))
}

/// 这个配置文件是不是「版本比程序新」（只读检查，不写、不改名）。
pub fn config_version_too_new(path: &Path) -> Option<u32> {
    let text = fs::read_to_string(path).ok()?;
    match read_version(&text) {
        VersionRead::Value(n) if n > CONFIG_VERSION => Some(n),
        _ => None,
    }
}

/// 配置文件现在能不能读成一份配置（只读检查，不迁移、不写盘）。
fn config_parses(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    match read_version(&text) {
        // 版本比程序新：降级拦截，禁止任何自动保存
        VersionRead::Value(n) if n > CONFIG_VERSION => false,
        VersionRead::NotNumber => false,
        VersionRead::Value(n) if n >= 4 => serde_json::from_str::<AppConfig>(&text).is_ok(),
        _ => serde_json::from_str::<LegacyConfig>(&text).is_ok(),
    }
}

/// guardian 自动保存前先问这个：现在到底能不能安全地把配置写回去。
///
/// - 配置文件存在：必须现在就能读成配置（读坏/版本过高时 save 会报错，而不是把空配置写出去）；
/// - 配置文件不存在、也没有 `config.json.bad`：第一次用，允许建立配置；
/// - 配置文件不存在、但有 `config.json.bad`：刚读坏留过底，禁止自动保存。
pub fn config_save_allowed(path: &Path) -> bool {
    if path.exists() {
        return config_parses(path);
    }
    !path.with_extension("json.bad").exists()
}

// ---------- 数据目录（含便携模式，D1 §12） ----------

/// 数据目录从哪儿来的（给设置页显示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DataDirKind {
    /// 环境变量 `SHANGXIAN_DATA_DIR` 指定
    Env,
    /// exe 旁边有 `shangxian.portable`，用 `<exe 目录>\data`
    Portable,
    /// 老的 `%APPDATA%\Shangxian`（已存在就继续用）
    LegacyRoaming,
    /// `%LOCALAPPDATA%\Shangxian`
    Local,
    /// `%APPDATA%\Shangxian`
    Roaming,
}

impl DataDirKind {
    /// 给界面看的来源名（大白话）。
    pub fn label(self) -> &'static str {
        match self {
            DataDirKind::Env => "环境变量指定",
            DataDirKind::Portable => "便携模式",
            DataDirKind::LegacyRoaming => "漫游应用数据",
            DataDirKind::Local => "本机应用数据",
            DataDirKind::Roaming => "漫游应用数据",
        }
    }
}

/// 纯解析数据目录（可单测）：
/// `exe_dir` = 程序目录；`env` = `SHANGXIAN_DATA_DIR`。
///
/// 顺序：环境变量（必须绝对路径）→ 便携 marker → 老的 `%APPDATA%\Shangxian`
/// → `%LOCALAPPDATA%\Shangxian` → `%APPDATA%\Shangxian`。
pub fn resolve_data_dir(
    exe_dir: &Path,
    env: Option<&str>,
    legacy_roaming_exists: bool,
    local_appdata: Option<&Path>,
    roaming_appdata: Option<&Path>,
) -> io::Result<(PathBuf, DataDirKind)> {
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        let dir = PathBuf::from(raw);
        if !dir.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "SHANGXIAN_DATA_DIR 必须是完整路径（当前填写的是 {raw}）；相对路径可能指向其他人可访问的目录"
                ),
            ));
        }
        return Ok((dir, DataDirKind::Env));
    }

    if exe_dir.join(PORTABLE_MARKER).exists() {
        return Ok((exe_dir.join(PORTABLE_SUBDIR), DataDirKind::Portable));
    }

    if legacy_roaming_exists {
        if let Some(roaming) = roaming_appdata {
            return Ok((roaming.join("Shangxian"), DataDirKind::LegacyRoaming));
        }
    }
    if let Some(local) = local_appdata {
        return Ok((local.join("Shangxian"), DataDirKind::Local));
    }
    if let Some(roaming) = roaming_appdata {
        return Ok((roaming.join("Shangxian"), DataDirKind::Roaming));
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "环境变量 LOCALAPPDATA 和 APPDATA 均未设置，无法确定配置和密钥的存放位置；不能退回到相对目录（可能被其他人访问）",
    ))
}

/// 按当前进程的环境解析数据目录（路径 + 来源）。
fn resolve_current_data_dir() -> io::Result<(PathBuf, DataDirKind)> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let env = std::env::var("SHANGXIAN_DATA_DIR").ok();
    let roaming = env_dir("APPDATA");
    let local = env_dir("LOCALAPPDATA");
    let legacy_exists = roaming
        .as_ref()
        .map(|dir| dir.join("Shangxian").exists())
        .unwrap_or(false);
    resolve_data_dir(
        &exe_dir,
        env.as_deref(),
        legacy_exists,
        local.as_deref(),
        roaming.as_deref(),
    )
}

/// 数据目录，按 [`resolve_data_dir`] 的顺序解析。
///
/// 便携模式（exe 旁边有 `shangxian.portable`）时会先把 `<exe>\data` 建出来：
/// 建不出来说明程序放在不能写的目录里，直接报人话错误（不静默回退到别的地方）。
///
/// 两个环境变量都没有时直接报错（panic），绝不回退到相对路径 `./data`：
/// 那里可能放密钥和控制令牌，相对目录（尤其是网络盘/共享盘）容易被别人看到。
pub fn default_data_dir() -> PathBuf {
    try_default_data_dir().unwrap_or_else(|e| panic!("{}", attach_env_hint(e)))
}

/// [`default_data_dir`] 的可失败版本：调用方想自己处理错误时用它。
pub fn try_default_data_dir() -> io::Result<PathBuf> {
    let (dir, kind) = resolve_current_data_dir()?;
    match kind {
        DataDirKind::Portable => {
            if let Err(e) = fs::create_dir_all(&dir) {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "便携模式：程序位于不可写的目录（{}）。请将程序移动到可写的文件夹，或删除 {PORTABLE_MARKER} 使用普通模式。",
                        dir.display()
                    ),
                ));
            }
        }
        DataDirKind::Env => {
            fs::create_dir_all(&dir).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("环境变量指定的数据目录无法创建：{}（{e}）", dir.display()),
                )
            })?;
        }
        _ => {}
    }
    Ok(dir)
}

/// 给界面/CLI 看：路径 + 来源（和 [`default_data_dir`] 同样的解析与报错）。
pub fn data_dir_info() -> (PathBuf, DataDirKind) {
    match resolve_current_data_dir() {
        Ok((dir, kind)) => {
            if kind == DataDirKind::Portable {
                if let Err(e) = fs::create_dir_all(&dir) {
                    panic!(
                        "便携模式：程序位于不可写的目录（{}）。请将程序移动到可写的文件夹，或删除 {PORTABLE_MARKER} 使用普通模式。（{e}）",
                        dir.display()
                    );
                }
            }
            (dir, kind)
        }
        Err(e) => panic!("{}", attach_env_hint(e)),
    }
}

/// 环境变量缺失类错误补一句修环境的提示；便携/指定目录类错误原样给人话。
fn attach_env_hint(err: io::Error) -> String {
    let msg = err.to_string();
    if msg.contains("LOCALAPPDATA") || msg.contains("APPDATA") {
        format!("{msg}；请修复系统环境变量后再打开应用")
    } else {
        msg
    }
}

/// 读一个环境变量当目录：没设、空串、全是空白都不算。
fn env_dir(name: &str) -> Option<PathBuf> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// 某台服务器专属密钥的存放位置（配免密时生成）。
pub fn key_path_for(data_dir: &Path, server_id: &str) -> PathBuf {
    data_dir
        .join("keys")
        .join(format!("{server_id}_ed25519"))
}

/// 生成一个新 id（前缀 + 随机数）。
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{:06x}", rand::random::<u32>() & 0x00ff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V3_JSON: &str = r#"
    {
      "version": 1,
      "servers": [
        { "id": "srv-api", "name": "香港-1", "host": "api.example.com", "sshPort": 22, "user": "user", "keyPath": "C:/k/id_ed25519" }
      ],
      "services": [
        { "id": "svc-a", "name": "pi-web", "enabled": true, "serverId": "srv-api", "localPort": 30141, "remotePort": 13041, "url": "piweb.example.com", "access": "password", "accessUser": "pi", "accessHash": "$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0" },
        { "id": "svc-b", "name": "viewer", "enabled": false, "serverId": "srv-api", "localPort": 8787, "remotePort": 18787 },
        { "id": "svc-c", "name": "comfyui", "enabled": false, "serverId": "srv-api", "localPort": 30141, "remotePort": 13042 }
      ],
      "notifyEnabled": true
    }
    "#;

    const V5_JSON: &str = r#"
    {
      "version": 5,
      "servers": [
        { "id": "srv-1a2b3c", "name": "香港-1", "host": "api.example.com", "sshPort": 2222, "user": "root", "keyPath": "C:/k/srv_ed25519", "vhostDir": "/xp/panel/vhost" }
      ],
      "services": [
        { "id": "svc-4d5e6f", "name": "相册", "localPort": 8188, "launch": { "exe": "C:/Python311/python.exe", "args": "app.py", "workdir": "D:/album" }, "watch": true, "autostart": true, "note": "手的相册" }
      ],
      "tunnels": [
        { "id": "tun-7a8b9c", "name": "相册发布", "serviceId": "svc-4d5e6f", "serverId": "srv-1a2b3c", "remotePort": 18080, "url": "album.example.com", "keyPath": null, "access": ["password"], "accessUser": "u", "accessHash": "$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0", "accessDetail": null, "enabled": true }
      ],
      "notifyEnabled": false
    }
    "#;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sx-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---------- v3 ----------

    #[test]
    fn v3_config_migrates_without_losing_data() {
        let old: LegacyConfig = serde_json::from_str(V3_JSON).unwrap();
        let cfg = old.migrate();

        // 三条老服务 → 三条隧道（id 不变，界面标识稳定）
        assert_eq!(cfg.tunnels.len(), 3);
        assert_eq!(cfg.tunnels[0].id, "svc-a");
        assert_eq!(cfg.tunnels[0].remote_port, 13041);
        assert!(cfg.tunnels[0].enabled);
        assert_eq!(cfg.tunnels[0].access, vec!["password"]);
        assert!(cfg.tunnels[0].wants_password());
        assert!(!cfg.tunnels[0].wants_ip());
        assert_eq!(cfg.tunnels[0].access_user.as_deref(), Some("pi"));
        assert_eq!(cfg.tunnels[0].preset, "web");
        assert!(!cfg.tunnels[1].enabled);

        // 相同本机端口的两条隧道合成一台本机服务
        assert_eq!(cfg.services.len(), 2);
        let pi = cfg.services.iter().find(|s| s.local_port == 30141).unwrap();
        assert_eq!(cfg.tunnels_of_service(&pi.id).len(), 2);
        let viewer = cfg.services.iter().find(|s| s.local_port == 8787).unwrap();
        assert_eq!(cfg.tunnels_of_service(&viewer.id).len(), 1);

        // 服务器原样保留
        assert_eq!(cfg.servers.len(), 1);
        assert_eq!(cfg.servers[0].host, "api.example.com");
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert_eq!(cfg.settings, Settings::default());

        // 迁移后不能出现悬空隧道
        for t in &cfg.tunnels {
            assert!(cfg.service(&t.service_id).is_some(), "隧道引用了不存在的服务");
        }
    }

    // ---------- v5 → v6 ----------

    #[test]
    fn v5_to_v6_is_lossless_and_backs_up() {
        let dir = temp_dir("v5-lossless");
        let path = dir.join("config.json");
        std::fs::write(&path, V5_JSON).unwrap();

        let (cfg, outcome) = AppConfig::load_detailed(&path).unwrap();
        assert_eq!(outcome.migrated_from, Some(5));
        assert!(outcome.normalized.is_empty());
        assert_eq!(cfg.version, 6);

        // 备份是迁移前的原文，一份就够（再读不覆盖）
        let backup = dir.join("config.json.v5.bak");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), V5_JSON);
        std::fs::write(&path, V5_JSON).unwrap();
        AppConfig::load(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), V5_JSON);

        // 除了 version/settings/preset/probeKind，逐字段和 v5 原文相等
        let mut before: serde_json::Value = serde_json::from_str(V5_JSON).unwrap();
        before.as_object_mut().unwrap().remove("version");
        let mut after = serde_json::to_value(&cfg).unwrap();
        after.as_object_mut().unwrap().remove("version");
        after.as_object_mut().unwrap().remove("settings");
        for tunnel in after["tunnels"].as_array_mut().unwrap() {
            tunnel.as_object_mut().unwrap().remove("preset");
        }
        for service in after["services"].as_array_mut().unwrap() {
            service.as_object_mut().unwrap().remove("probeKind");
        }
        assert_eq!(before, after, "v5→v6 迁移必须无损");

        // 新增字段按规范填默认值
        assert_eq!(cfg.settings, Settings::default());
        assert_eq!(cfg.tunnels[0].preset, "web");
        assert_eq!(cfg.services[0].probe_kind, "tcp", "老配置没有 probeKind 时默认 tcp");
        assert!(!cfg.notify_enabled, "根字段 notifyEnabled 不动");
        // 落盘后的文件是 v6
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk["version"], 6);
        assert_eq!(on_disk["settings"]["refreshIntervalMs"], 800);
    }

    // ---------- 服务探测方式 probeKind ----------

    #[test]
    fn probe_kind_defaults_to_tcp_and_round_trips() {
        let dir = temp_dir("probe-kind");
        let path = dir.join("config.json");
        // 老文本（没有 probeKind）：读出来默认 tcp
        std::fs::write(&path, V5_JSON).unwrap();
        let cfg = AppConfig::load(&path).unwrap();
        assert_eq!(cfg.services[0].probe_kind, "tcp");
        assert_eq!(cfg.services[0].probe_kind_enum(), crate::probe::ProbeKind::Tcp);

        // 写回后带上 probeKind；http 能原样读回
        let mut cfg2 = cfg.clone();
        cfg2.services[0].probe_kind = "http".into();
        cfg2.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"probeKind\": \"http\""), "text={text}");
        let cfg3 = AppConfig::load(&path).unwrap();
        assert_eq!(cfg3.services[0].probe_kind, "http");
        assert_eq!(cfg3.services[0].probe_kind_enum(), crate::probe::ProbeKind::Http);
    }

    #[test]
    fn unknown_probe_kind_is_normalized_with_note() {
        let mut cfg = AppConfig::default();
        cfg.services.push(LocalService {
            id: "svc-1".into(),
            name: "怪服务".into(),
            local_port: 12345,
            probe_kind: "udp".into(),
            launch: None,
            watch: false,
            autostart: false,
            note: None,
        });
        let notes = normalize_config(&mut cfg);
        assert_eq!(cfg.services[0].probe_kind, "tcp");
        assert!(notes.contains(&"services[].probeKind".to_string()), "notes={notes:?}");
    }

    // ---------- 严格超集（v5 形状读 v6 文本） ----------

    /// 只含 v5 字段的「旧程序形状」结构体：没有 settings、没有 preset。
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct V5Shape {
        version: u32,
        #[serde(default)]
        servers: Vec<V5ShapeServer>,
        #[serde(default)]
        services: Vec<V5ShapeService>,
        #[serde(default)]
        tunnels: Vec<V5ShapeTunnel>,
        #[serde(default = "default_true")]
        notify_enabled: bool,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct V5ShapeServer {
        id: String,
        name: String,
        host: String,
        ssh_port: u16,
        user: String,
        #[serde(default)]
        key_path: Option<String>,
        #[serde(default)]
        vhost_dir: Option<String>,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct V5ShapeService {
        id: String,
        name: String,
        local_port: u16,
        #[serde(default)]
        launch: Option<V5ShapeLaunch>,
        #[serde(default)]
        watch: bool,
        #[serde(default)]
        autostart: bool,
        #[serde(default)]
        note: Option<String>,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct V5ShapeLaunch {
        exe: String,
        #[serde(default)]
        args: String,
        #[serde(default)]
        workdir: Option<String>,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    #[allow(dead_code)]
    struct V5ShapeTunnel {
        id: String,
        name: String,
        service_id: String,
        server_id: String,
        remote_port: u16,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        key_path: Option<String>,
        #[serde(default, deserialize_with = "deserialize_access")]
        access: Vec<String>,
        #[serde(default)]
        access_user: Option<String>,
        #[serde(default)]
        access_hash: Option<String>,
        #[serde(default)]
        access_detail: Option<String>,
        #[serde(default)]
        enabled: bool,
    }

    #[test]
    fn v6_text_still_parses_as_v5_shape() {
        // 先造一份 v6 配置（迁移 V5_JSON 得到）
        let dir = temp_dir("superset");
        let path = dir.join("config.json");
        std::fs::write(&path, V5_JSON).unwrap();
        let cfg = AppConfig::load(&path).unwrap();
        let v6_text = serde_json::to_string(&cfg).unwrap();

        // 旧程序（只认 v5 字段）读 v6 文本：必须成功，且旧字段值一致
        let old: V5Shape = serde_json::from_str(&v6_text).expect("v6 必须是 v5 的严格超集");
        assert_eq!(old.version, 6);
        assert_eq!(old.servers[0].id, "srv-1a2b3c");
        assert_eq!(old.servers[0].ssh_port, 2222);
        assert_eq!(
            old.servers[0].key_path.as_deref(),
            Some("C:/k/srv_ed25519")
        );
        assert_eq!(old.services[0].name, "相册");
        assert_eq!(old.services[0].local_port, 8188);
        assert_eq!(old.services[0].launch.as_ref().unwrap().args, "app.py");
        assert_eq!(old.tunnels[0].id, "tun-7a8b9c");
        assert_eq!(old.tunnels[0].remote_port, 18080);
        assert_eq!(old.tunnels[0].access, vec!["password"]);
        assert_eq!(old.tunnels[0].access_user.as_deref(), Some("u"));
        assert!(old.tunnels[0].access_hash.is_some());
        assert!(old.tunnels[0].enabled);
        assert!(!old.notify_enabled);
    }

    // ---------- v4 ----------

    const V4_JSON: &str = r#"
    {
      "version": 4,
      "servers": [],
      "services": [],
      "tunnels": [
        {
          "id": "tun-1", "name": "相册", "serviceId": "svc-1", "serverId": "srv-1",
          "remotePort": 18080, "url": "album.example.com",
          "access": "password,ip", "accessUser": "u", "accessHash": "$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0",
          "accessDetail": "1.2.3.4", "enabled": true
        }
      ],
      "notifyEnabled": true
    }
    "#;

    #[test]
    fn v4_config_with_string_access_loads_and_upgrades() {
        let dir = temp_dir("v4");
        let path = dir.join("config.json");
        std::fs::write(&path, V4_JSON).unwrap();

        let cfg = AppConfig::load(&path).unwrap();
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert_eq!(cfg.tunnels.len(), 1);
        // 老写法 "password,ip" 读进来变成规范化数组
        assert_eq!(cfg.tunnels[0].access, vec!["password", "ip"]);
        assert!(cfg.tunnels[0].wants_password() && cfg.tunnels[0].wants_ip());
        // 老文件先备份，再写回 v6（access 落盘为数组）
        assert!(dir.join("config.json.v4.bak").exists(), "应该留下 v4 备份");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["version"], 6);
        assert_eq!(value["tunnels"][0]["preset"], "web");
        assert_eq!(value["tunnels"][0]["access"], serde_json::json!(["password", "ip"]));
    }

    #[test]
    fn migration_survives_write_back_failure() {
        // 写回失败（临时文件路径被占成目录）时，load 不能返回 Err、更不能退化成空配置。
        let dir = temp_dir("migrate-fail-v4");
        let path = dir.join("config.json");
        std::fs::write(&path, V4_JSON).unwrap();
        std::fs::create_dir_all(path.with_extension("json.tmp")).unwrap();

        let cfg = AppConfig::load(&path).expect("迁移后的写回失败也不能返回 Err");
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert_eq!(cfg.tunnels.len(), 1, "内存里必须拿着迁移后的配置，不能是空的");
        assert_eq!(cfg.tunnels[0].access, vec!["password", "ip"]);
        // 备份先写：写回失败也留下了 v4 备份
        assert!(dir.join("config.json.v4.bak").exists());
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["version"], 4, "写回失败时老文件保持原样");

        // v3 及以前分支同样容错
        let dir3 = temp_dir("migrate-fail-v3");
        let path3 = dir3.join("config.json");
        std::fs::write(&path3, V3_JSON).unwrap();
        std::fs::create_dir_all(path3.with_extension("json.tmp")).unwrap();

        let cfg3 = AppConfig::load(&path3).expect("v3 迁移的写回失败也不能返回 Err");
        assert_eq!(cfg3.version, CONFIG_VERSION);
        assert_eq!(cfg3.tunnels.len(), 3, "v3 迁移结果不能因为写回失败丢掉");
        assert!(dir3.join("config.json.v3.bak").exists());
    }

    // ---------- 降级拦截 ----------

    #[test]
    fn version_7_is_read_only_and_leaves_file_alone() {
        let dir = temp_dir("downgrade");
        let path = dir.join("config.json");
        let text = r#"{"version":7,"servers":[],"services":[],"tunnels":[],"notifyEnabled":true,"settings":{"refreshIntervalMs":800,"quietHours":{"enabled":false,"start":"22:00","end":"08:00"},"checkUpdateOnStart":true}}"#;
        std::fs::write(&path, text).unwrap();

        let err = AppConfig::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), config_unsupported_message(7));
        assert!(err.to_string().contains("版本 7"), "err={err}");
        assert!(err.to_string().contains("只认到 6"), "err={err}");
        // 原文件一字不动、不产生 .bad
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(!dir.join("config.json.bad").exists());
        // 只读检查：不允许保存
        assert!(!config_save_allowed(&path));
        assert_eq!(config_version_too_new(&path), Some(7));
        // 红条文案 = 第一句 + 空格 + 第二句
        assert!(config_unsupported_bar_message(7).ends_with(" 升级后会自动读回，不用重新配。"));
    }

    #[test]
    fn non_numeric_version_is_quarantined() {
        let dir = temp_dir("version-string");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"version":"六","notifyEnabled":true}"#).unwrap();
        let err = AppConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("version 字段不是数字"), "err={err}");
        assert!(dir.join("config.json.bad").exists());
        assert!(!path.exists(), "读坏后原文件必须挪走");
        assert!(!config_save_allowed(&path));
    }

    // ---------- 归一化 ----------

    #[test]
    fn v6_bad_values_are_normalized_on_read() {
        let dir = temp_dir("normalize");
        let path = dir.join("config.json");
        let text = r#"{
          "version": 6,
          "servers": [], "services": [],
          "tunnels": [
            { "id": "t1", "name": "一", "serviceId": "s", "serverId": "srv", "remotePort": 1, "preset": "wat" },
            { "id": "t2", "name": "二", "serviceId": "s", "serverId": "srv", "remotePort": 2, "preset": "upload" }
          ],
          "notifyEnabled": true,
          "settings": { "refreshIntervalMs": 100, "quietHours": { "enabled": true, "start": "25:00", "end": "08:00" }, "checkUpdateOnStart": true }
        }"#;
        std::fs::write(&path, text).unwrap();

        let (cfg, outcome) = AppConfig::load_detailed(&path).unwrap();
        assert_eq!(cfg.settings.refresh_interval_ms, 500, "小于 500 夹到 500");
        assert_eq!(cfg.settings.quiet_hours.start, "22:00", "时间格式错回默认");
        assert_eq!(cfg.tunnels[0].preset, "web", "非法 preset 归一成 web");
        assert_eq!(cfg.tunnels[1].preset, "upload", "合法 preset 不动");
        assert_eq!(outcome.migrated_from, None);
        assert!(outcome.normalized.contains(&"settings.refreshIntervalMs".to_string()));
        assert!(outcome.normalized.contains(&"settings.quietHours.start".to_string()));
        assert!(outcome.normalized.contains(&"tunnels[].preset".to_string()));
        // 归一化不写盘
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        let dir2 = temp_dir("normalize-hi");
        let path2 = dir2.join("config.json");
        std::fs::write(
            &path2,
            r#"{"version":6,"settings":{"refreshIntervalMs":99999,"quietHours":{"enabled":false,"start":"22:00","end":"08:00"},"checkUpdateOnStart":true}}"#,
        )
        .unwrap();
        let cfg2 = AppConfig::load(&path2).unwrap();
        assert_eq!(cfg2.settings.refresh_interval_ms, 10000, "大于 10000 夹到 10000");
        assert!(cfg2.notify_enabled, "缺 notifyEnabled 默认开");
    }

    #[test]
    fn quiet_hours_window_covers_midnight() {
        let q = QuietHours {
            enabled: true,
            start: "22:00".into(),
            end: "08:00".into(),
        };
        assert!(q.is_active_at(23 * 60));
        assert!(q.is_active_at(0));
        assert!(q.is_active_at(7 * 60 + 59));
        assert!(!q.is_active_at(8 * 60), "结束点不算在里面");
        assert!(!q.is_active_at(12 * 60));
        // start == end 视为关闭；开关关了永远 false
        let off = QuietHours {
            enabled: true,
            start: "08:00".into(),
            end: "08:00".into(),
        };
        assert!(!off.is_active_at(8 * 60));
        let mut disabled = q.clone();
        disabled.enabled = false;
        assert!(!disabled.is_active_at(23 * 60));
    }

    // ---------- 写时校验文案 ----------

    #[test]
    fn settings_write_validation_messages_are_exact() {
        let ok = serde_json::json!({
            "refreshIntervalMs": 800,
            "quietHours": { "enabled": false, "start": "22:00", "end": "08:00" },
            "checkUpdateOnStart": true
        });
        let parsed = parse_settings_strict(&ok).unwrap();
        assert_eq!(parsed, Settings::default());

        let err = |v: serde_json::Value| parse_settings_strict(&v).unwrap_err();

        assert_eq!(
            err(serde_json::json!({"quietHours": {"enabled": false, "start": "22:00", "end": "08:00"}, "checkUpdateOnStart": true})),
            "设置项不完整：缺少 refreshIntervalMs，请整份设置一起提交"
        );
        assert_eq!(
            err(serde_json::json!({"refreshIntervalMs": 499, "quietHours": {"enabled": false, "start": "22:00", "end": "08:00"}, "checkUpdateOnStart": true})),
            "界面刷新间隔只能在 500 到 10000 毫秒之间"
        );
        assert_eq!(
            err(serde_json::json!({"refreshIntervalMs": 10001, "quietHours": {"enabled": false, "start": "22:00", "end": "08:00"}, "checkUpdateOnStart": true})),
            "界面刷新间隔只能在 500 到 10000 毫秒之间"
        );
        assert_eq!(
            err(serde_json::json!({"refreshIntervalMs": 800, "quietHours": {"enabled": false, "start": "9:00", "end": "08:00"}, "checkUpdateOnStart": true})),
            "免打扰时段须填写 24 小时制时间，例如 22:00"
        );
        assert_eq!(
            err(serde_json::json!({"refreshIntervalMs": 800, "quietHours": {"enabled": true, "start": "08:00", "end": "08:00"}, "checkUpdateOnStart": true})),
            "免打扰时段的开始和结束不能相同；如不需要免打扰，请关闭该开关"
        );
        assert_eq!(
            err(serde_json::json!({"refreshIntervalMs": 800, "quietHours": {"enabled": false, "start": "22:00", "end": "08:00"}, "checkUpdateOnStart": true, "bogus": 1})),
            "设置中包含无法识别的项：bogus"
        );
        assert_eq!(
            err(serde_json::json!({"refreshIntervalMs": 800, "quietHours": {"enabled": false, "start": "22:00", "end": "08:00", "x": 1}, "checkUpdateOnStart": true})),
            "设置中包含无法识别的项：quietHours.x"
        );

        assert_eq!(
            check_preset_strict("wat").unwrap_err(),
            "发布预设只支持 web / ws / upload 三种"
        );
        assert!(check_preset_strict("ws").is_ok());
    }

    // ---------- 访问保护柔性读写 ----------

    #[test]
    fn access_missing_means_no_protection() {
        let text = r#"{"id":"t","name":"x","serviceId":"s","serverId":"srv","remotePort":1}"#;
        let tun: Tunnel = serde_json::from_str(text).unwrap();
        assert!(tun.access.is_empty());
        assert!(!tun.wants_password() && !tun.wants_ip());
        assert_eq!(tun.preset, "web", "preset 缺省是 web");

        // 老客户端发字符串：none / "" / null 都是无保护
        for raw in [r#""none""#, r#""""#, "null"] {
            let text = format!(
                r#"{{"id":"t","name":"x","serviceId":"s","serverId":"srv","remotePort":1,"access":{raw}}}"#
            );
            let tun: Tunnel = serde_json::from_str(&text).unwrap();
            assert!(tun.access.is_empty(), "access={raw} 应该是无保护");
        }
    }

    #[test]
    fn access_strings_and_arrays_are_normalized() {
        /// 模拟配置/输入结构上的字段。
        #[derive(serde::Deserialize)]
        struct Probe {
            #[serde(default, deserialize_with = "deserialize_access")]
            access: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct OptProbe {
            #[serde(default, deserialize_with = "deserialize_access_opt")]
            access: Option<Vec<String>>,
        }

        let cases: &[(&str, &[&str])] = &[
            (r#""password""#, &["password"]),
            (r#""ip""#, &["ip"]),
            (r#""password,ip""#, &["password", "ip"]),
            (r#""ip,password""#, &["password", "ip"]),
            (r#""password+ip""#, &["password", "ip"]),
            (r#""password ip""#, &["password", "ip"]),
            (r#""ip ip password""#, &["password", "ip"]),
            (r#"["ip", "password"]"#, &["password", "ip"]),
            (r#"["password", "ip", "bogus"]"#, &["password", "ip"]),
            (r#"["none"]"#, &[]),
        ];
        for (raw, want) in cases {
            let text = format!(r#"{{"access":{raw}}}"#);
            let probe: Probe = serde_json::from_str(&text).unwrap();
            let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
            assert_eq!(probe.access, want, "access={raw}");
        }

        // 编辑输入：缺失 / null = 保持原样；给了就替换（none / [] = 清空）
        let opt = |text: &str| serde_json::from_str::<OptProbe>(text).unwrap().access;
        assert_eq!(opt("{}"), None);
        assert_eq!(opt(r#"{"access":null}"#), None);
        assert_eq!(opt(r#"{"access":"password"}"#), Some(vec!["password".into()]));
        assert_eq!(opt(r#"{"access":[]}"#), Some(vec![]));
        assert_eq!(opt(r#"{"access":"none"}"#), Some(vec![]));
        assert_eq!(opt(r#"{"access":["none"]}"#), Some(vec![]));

        // 不认识的值能被挑出来（导入 warning 用）
        let unknown = access_unknown_tokens(&serde_json::json!(["password", "bogus", "none"]));
        assert_eq!(unknown, vec!["bogus"]);
        assert!(access_unknown_tokens(&serde_json::json!("password,ip")).is_empty());
        assert!(access_unknown_tokens(&serde_json::json!(null)).is_empty());
    }

    // ---------- 读坏留底 ----------

    #[test]
    fn bad_config_is_quarantined_and_load_fails() {
        let dir = temp_dir("bad");
        let path = dir.join("config.json");
        std::fs::write(&path, "这不是 json {{{ 读坏了").unwrap();

        let err = AppConfig::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("config.json.bad"), "err={err}");
        // 原文件被挪走留底，内容还在
        assert!(!path.exists(), "读坏后原文件必须挪走，不能被空配置覆盖");
        let bad = dir.join("config.json.bad");
        assert_eq!(
            std::fs::read_to_string(&bad).unwrap(),
            "这不是 json {{{ 读坏了"
        );
        // 留底之后禁止自动保存（guardian 的保护）
        assert!(!config_save_allowed(&path), "读坏留底后不能允许自动保存");
    }

    #[test]
    fn truncated_json_is_quarantined_and_fails() {
        // 半截 JSON（掉电写坏那种）也要留底、也要报错
        let dir = temp_dir("truncated");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"version": 5, "servers": [{"#).unwrap();

        let err = AppConfig::load(&path).unwrap_err();
        assert!(err.to_string().contains("格式不正确"), "err={err}");
        assert!(dir.join("config.json.bad").exists());
        assert!(!path.exists());
    }

    #[test]
    fn save_is_allowed_for_fresh_and_good_configs() {
        let dir = temp_dir("save-guard");
        let path = dir.join("config.json");

        // 第一次用：没文件也没 .bad，允许建立配置
        assert!(config_save_allowed(&path));

        // 能读的配置：允许
        AppConfig::default().save(&path).unwrap();
        assert!(config_save_allowed(&path));

        // 有 .bad 又没配置文件：说明刚读坏留过底，禁止
        std::fs::remove_file(&path).unwrap();
        std::fs::write(dir.join("config.json.bad"), "坏的").unwrap();
        assert!(!config_save_allowed(&path));
    }

    // ---------- 便携模式 / 数据目录 ----------

    #[test]
    fn env_absolute_wins() {
        let exe = temp_dir("dir-env-exe");
        let target = temp_dir("dir-env-target");
        let (dir, kind) = resolve_data_dir(
            &exe,
            Some(&target.to_string_lossy()),
            false,
            Some(Path::new("C:/Local")),
            Some(Path::new("C:/Roaming")),
        )
        .unwrap();
        assert_eq!(kind, DataDirKind::Env);
        assert_eq!(dir, target);
    }

    #[test]
    fn env_relative_rejected() {
        let exe = temp_dir("dir-rel-exe");
        let err = resolve_data_dir(&exe, Some("relative/data"), false, None, None).unwrap_err();
        assert!(err.to_string().contains("必须是完整路径"), "err={err}");
    }

    #[test]
    fn portable_marker_prefers_exe_dir() {
        let exe = temp_dir("dir-portable");
        std::fs::write(exe.join(PORTABLE_MARKER), "").unwrap();
        let (dir, kind) = resolve_data_dir(
            &exe,
            None,
            true,
            Some(Path::new("C:/Local")),
            Some(Path::new("C:/Roaming")),
        )
        .unwrap();
        assert_eq!(kind, DataDirKind::Portable);
        assert_eq!(dir, exe.join(PORTABLE_SUBDIR));
        assert_eq!(DataDirKind::Portable.label(), "便携模式");
    }

    #[test]
    fn legacy_roaming_kept() {
        let exe = temp_dir("dir-legacy");
        let (dir, kind) = resolve_data_dir(
            &exe,
            None,
            true,
            Some(Path::new("C:/Local")),
            Some(Path::new("C:/Roaming")),
        )
        .unwrap();
        assert_eq!(kind, DataDirKind::LegacyRoaming);
        assert_eq!(dir, Path::new("C:/Roaming").join("Shangxian"));
    }

    #[test]
    fn local_before_roaming() {
        let exe = temp_dir("dir-local");
        let (dir, kind) = resolve_data_dir(
            &exe,
            None,
            false,
            Some(Path::new("C:/Local")),
            Some(Path::new("C:/Roaming")),
        )
        .unwrap();
        assert_eq!(kind, DataDirKind::Local);
        assert_eq!(dir, Path::new("C:/Local").join("Shangxian"));

        let (dir2, kind2) = resolve_data_dir(
            &exe,
            None,
            false,
            None,
            Some(Path::new("C:/Roaming")),
        )
        .unwrap();
        assert_eq!(kind2, DataDirKind::Roaming);
        assert_eq!(dir2, Path::new("C:/Roaming").join("Shangxian"));
    }

    #[test]
    fn no_env_at_all_is_an_error() {
        let exe = temp_dir("dir-none");
        let err = resolve_data_dir(&exe, None, false, None, None).unwrap_err();
        assert!(err.to_string().contains("LOCALAPPDATA"), "err={err}");
    }
}
