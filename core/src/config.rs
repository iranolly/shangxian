//! 应用配置：唯一事实来源（对齐 DESIGN.md §3.3「状态存储」）。
//!
//! - 配置文件能手改、能备份；保存用「临时文件 + 改名」原子替换，防掉电写坏。
//! - 密钥只存路径引用，绝不存密钥内容。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 当前配置版本。
/// 3 → 4：把「服务」拆成「本机服务 + 隧道」。
/// 4 → 5：访问保护从单选（none/password/ip）变成可叠加的数组（password + ip 可以同时要）。
pub const CONFIG_VERSION: u32 = 5;

fn default_ssh_port() -> u16 {
    22
}

fn default_true() -> bool {
    true
}

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
    /// 访问密码的 apr1 哈希（明文密码只显示一次，不落盘）
    #[serde(default)]
    pub access_hash: Option<String>,
    /// 访问保护细节（如 IP 白名单，逗号分隔）
    #[serde(default)]
    pub access_detail: Option<String>,
    /// 是否正在发布（false = 已暂停）
    #[serde(default)]
    pub enabled: bool,
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
    /// 掉线时是否弹系统通知（默认开）
    #[serde(default = "default_true")]
    pub notify_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            servers: Vec::new(),
            services: Vec::new(),
            tunnels: Vec::new(),
            notify_enabled: true,
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
            });
        }
        AppConfig {
            version: CONFIG_VERSION,
            servers: self.servers,
            services,
            tunnels,
            notify_enabled: self.notify_enabled,
        }
    }
}

impl AppConfig {
    /// 读配置。老版本会自动迁移：
    /// - v4：结构和 v5 一样，只是访问保护还写成字符串，柔性反序列化直接兼容；
    ///   先备份成 `config.json.v4.bak`，再写回 v5；
    /// - v3 及以前：先备份成 `config.json.v3.bak`，走老结构迁移再写回 v5。
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let version = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("version").and_then(|x| x.as_u64()))
            .unwrap_or(1) as u32;

        if version >= CONFIG_VERSION {
            return serde_json::from_str(&text).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("配置文件格式不对：{e}"))
            });
        }

        if version == 4 {
            let mut cfg: AppConfig = serde_json::from_str(&text).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("配置文件格式不对：{e}"))
            })?;
            let backup = path.with_extension("json.v4.bak");
            if !backup.exists() {
                let _ = fs::write(&backup, &text);
            }
            cfg.version = CONFIG_VERSION;
            // 写回是尽力而为：磁盘满/没权限也不能让调用方退回空配置，
            // 内存里先拿迁移后的结果用（下次保存还会再试）。
            let _ = cfg.save(path);
            return Ok(cfg);
        }

        let old: LegacyConfig = serde_json::from_str(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("老配置文件格式不对，没法迁移：{e}"),
            )
        })?;
        let migrated = old.migrate();
        let backup = path.with_extension("json.v3.bak");
        if !backup.exists() {
            let _ = fs::write(&backup, &text);
        }
        // 同上：迁移后的写回失败不报错，宁可内存里用迁移结果，也不要让调用方退回空配置。
        let _ = migrated.save(path);
        Ok(migrated)
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

/// 数据目录：%APPDATA%\Shangxian（取不到就用当前目录 ./data）。
pub fn default_data_dir() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir).join("Shangxian"),
        _ => PathBuf::from("./data"),
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
        { "id": "svc-a", "name": "pi-web", "enabled": true, "serverId": "srv-api", "localPort": 30141, "remotePort": 13041, "url": "piweb.example.com", "access": "password", "accessUser": "pi", "accessHash": "$apr1$x$y" },
        { "id": "svc-b", "name": "viewer", "enabled": false, "serverId": "srv-api", "localPort": 8787, "remotePort": 18787 },
        { "id": "svc-c", "name": "comfyui", "enabled": false, "serverId": "srv-api", "localPort": 30141, "remotePort": 13042 }
      ],
      "notifyEnabled": true
    }
    "#;

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

        // 迁移后不能出现悬空隧道
        for t in &cfg.tunnels {
            assert!(cfg.service(&t.service_id).is_some(), "隧道引用了不存在的服务");
        }
    }

    #[test]
    fn v4_config_round_trips() {
        let cfg = AppConfig::default();
        let text = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back.version, CONFIG_VERSION);
        assert!(back.services.is_empty() && back.tunnels.is_empty());
    }

    const V4_JSON: &str = r#"
    {
      "version": 4,
      "servers": [],
      "services": [],
      "tunnels": [
        {
          "id": "tun-1", "name": "相册", "serviceId": "svc-1", "serverId": "srv-1",
          "remotePort": 18080, "url": "album.example.com",
          "access": "password,ip", "accessUser": "u", "accessHash": "$apr1$x$y",
          "accessDetail": "1.2.3.4", "enabled": true
        }
      ],
      "notifyEnabled": true
    }
    "#;

    #[test]
    fn v4_config_with_string_access_loads_and_upgrades() {
        let dir = std::env::temp_dir().join("sx-config-v4-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, V4_JSON).unwrap();

        let cfg = AppConfig::load(&path).unwrap();
        assert_eq!(cfg.version, CONFIG_VERSION);
        assert_eq!(cfg.tunnels.len(), 1);
        // 老写法 "password,ip" 读进来变成规范化数组
        assert_eq!(cfg.tunnels[0].access, vec!["password", "ip"]);
        assert!(cfg.tunnels[0].wants_password() && cfg.tunnels[0].wants_ip());
        // 老文件先备份，再写回 v5（access 落盘为数组）
        assert!(dir.join("config.json.v4.bak").exists(), "应该留下 v4 备份");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["version"], 5);
        assert_eq!(value["tunnels"][0]["access"], serde_json::json!(["password", "ip"]));
    }

    #[test]
    fn migration_survives_write_back_failure() {
        // 写回失败（临时文件路径被占成目录）时，load 不能返回 Err、更不能退化成空配置。
        let dir = std::env::temp_dir().join("sx-config-migrate-fail-v4");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
        let dir3 = std::env::temp_dir().join("sx-config-migrate-fail-v3");
        let _ = std::fs::remove_dir_all(&dir3);
        std::fs::create_dir_all(&dir3).unwrap();
        let path3 = dir3.join("config.json");
        std::fs::write(&path3, V3_JSON).unwrap();
        std::fs::create_dir_all(path3.with_extension("json.tmp")).unwrap();

        let cfg3 = AppConfig::load(&path3).expect("v3 迁移的写回失败也不能返回 Err");
        assert_eq!(cfg3.version, CONFIG_VERSION);
        assert_eq!(cfg3.tunnels.len(), 3, "v3 迁移结果不能因为写回失败丢掉");
        assert!(dir3.join("config.json.v3.bak").exists());
    }

    #[test]
    fn access_missing_means_no_protection() {
        let text = r#"{"id":"t","name":"x","serviceId":"s","serverId":"srv","remotePort":1}"#;
        let tun: Tunnel = serde_json::from_str(text).unwrap();
        assert!(tun.access.is_empty());
        assert!(!tun.wants_password() && !tun.wants_ip());

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
    }
}
