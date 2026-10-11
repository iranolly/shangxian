//! 守护：按配置管理全部隧道和本机服务（启停、重试、状态汇总、导入与配置编辑）。
//!
//! 纪律（对齐 DESIGN.md §1.3 设计红线）：
//! - 每条隧道一条独立进程、独立日志；每台本机服务独立进程、独立日志；
//! - 没有全局清理路径：暂停一条隧道只停它自己；
//! - 端口写死，冲突不自动漂移；
//! - 删本机服务前先看有没有隧道在用它（有就拦住报错，不自动连带删）。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use crate::cleanup::{CleanupSpec, SshExec};
use crate::config::{self, AppConfig, LaunchSpec, LocalService, ServerConfig, Tunnel};
use crate::import::ImportSuggestion;
use crate::localservice::{LocalRunner, LocalSpec};
use crate::protection::AccessCheck;
use crate::ssh::{self, TunnelTarget};
use crate::supervisor::{service_log_path, Event, Phase, Tunnel as TunnelProc, TunnelSpec};

// ---------- 给界面看的视图 ----------

/// 一台本机服务（界面用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    pub id: String,
    pub name: String,
    pub local_port: u16,
    /// stopped | starting | running | not_listening | failed
    pub status: String,
    pub watch: bool,
    pub autostart: bool,
    pub launch_exe: Option<String>,
    pub launch_args: Option<String>,
    pub launch_workdir: Option<String>,
    pub note: Option<String>,
    /// 已经发布到几条隧道
    pub tunnel_count: usize,
    /// 这些隧道的名字（界面上展开看）
    pub tunnel_names: Vec<String>,
    /// 出问题的原因（人话）
    pub reason: Option<String>,
    pub log_path: String,
}

/// 一条发布隧道（界面用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelView {
    pub id: String,
    pub name: String,
    pub service_id: String,
    pub service_name: String,
    pub local_port: u16,
    pub server_id: String,
    pub remote_port: u16,
    pub url: Option<String>,
    /// running | connecting | disconnected | error | paused
    pub status: String,
    pub enabled: bool,
    /// 出错原因（人话），给「出错」状态的修复带用
    pub reason: Option<String>,
    /// 访问保护：["password"]、["ip"] 或两者叠加；空 = 谁都能进
    pub access: Vec<String>,
    pub access_user: Option<String>,
    /// 是否已经设过访问密码（密码本身不落盘）
    pub has_password: bool,
    pub access_detail: Option<String>,
    /// 本机服务在不在跑（界面要标出来）
    pub service_ok: bool,
    pub service_status: String,
    /// 单独指定的密钥（可空 = 继承服务器）
    pub key_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerView {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    pub ssh_port: u16,
    pub key_path: Option<String>,
}

// ---------- 界面提交的输入 ----------

/// 新增/编辑本机服务。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInput {
    pub id: Option<String>,
    pub name: String,
    pub local_port: u16,
    pub launch_exe: Option<String>,
    pub launch_args: Option<String>,
    pub launch_workdir: Option<String>,
    pub watch: bool,
    pub autostart: bool,
    pub note: Option<String>,
}

/// 新增隧道（向导提交）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTunnelInput {
    pub name: String,
    pub service_id: String,
    pub server_id: String,
    pub remote_port: u16,
    pub url: Option<String>,
    /// 访问保护：可以同时要密码 + 只允许指定 IP；缺失 = 谁都能进
    #[serde(default, deserialize_with = "config::deserialize_access")]
    pub access: Vec<String>,
    pub access_user: Option<String>,
    pub access_hash: Option<String>,
    pub access_detail: Option<String>,
}

/// 编辑已有隧道。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTunnelInput {
    pub id: String,
    pub name: String,
    pub remote_port: u16,
    pub url: Option<String>,
    /// 访问保护：缺失或 null = 保持原样；给了就替换（空数组或 ["none"] = 取消保护）
    #[serde(default, deserialize_with = "config::deserialize_access_opt")]
    pub access: Option<Vec<String>>,
    pub access_user: Option<String>,
    /// 新密码的哈希；None = 保持原密码
    pub access_hash: Option<String>,
    pub access_detail: Option<String>,
}

/// 新增/编辑服务器。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInput {
    pub id: Option<String>,
    pub name: String,
    pub host: String,
    pub ssh_port: u16,
    pub user: String,
    pub key_path: Option<String>,
}

pub struct Guardian {
    config: AppConfig,
    config_path: PathBuf,
    log_dir: PathBuf,
    /// 运行中的隧道（按隧道 id）
    tunnels: HashMap<String, TunnelProc>,
    /// 隧道最近一次出错原因
    reasons: HashMap<String, String>,
    /// 本机服务的运行时（按服务 id）
    local: HashMap<String, LocalRunner>,
    /// 开机自启只做一次
    autostart_done: bool,
}

impl Guardian {
    pub fn new(config: AppConfig, config_path: PathBuf, log_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&log_dir);
        Self {
            config,
            config_path,
            log_dir,
            tunnels: HashMap::new(),
            reasons: HashMap::new(),
            local: HashMap::new(),
            autostart_done: false,
        }
    }

    // ---------- 看护 ----------

    /// 周期调用：先让隧道表和配置对齐，再推进每条隧道和每台本机服务。
    pub fn poll(&mut self) -> Vec<(String, Event)> {
        if !self.autostart_done {
            self.autostart_done = true;
            self.start_autostart_services();
        }
        self.sync_tunnels();
        self.sync_local();

        let ids: Vec<String> = self.tunnels.keys().cloned().collect();
        let mut events = Vec::new();
        for id in ids {
            if let Some(tunnel) = self.tunnels.get_mut(&id) {
                for event in tunnel.poll() {
                    if let Event::SpawnFailed { error, .. } = &event {
                        self.reasons.insert(id.clone(), error.clone());
                    }
                    events.push((id.clone(), event));
                }
            }
        }
        for runner in self.local.values_mut() {
            runner.poll();
        }
        events
    }

    /// 开机自启：把勾了自启的服务拉起来（应用启动时做一次）。
    fn start_autostart_services(&mut self) {
        let ids: Vec<String> = self
            .config
            .services
            .iter()
            .filter(|s| s.autostart && s.launch.is_some())
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            self.sync_local(); // 保证 runner 存在
            if let Some(runner) = self.local.get_mut(&id) {
                runner.start_for_autostart();
                runner.poll();
            }
        }
    }

    /// 让运行中的隧道与配置保持一致：停掉被暂停/删除的，补上新增的。
    fn sync_tunnels(&mut self) {
        let enabled: HashSet<String> = self
            .config
            .tunnels
            .iter()
            .filter(|t| t.enabled && self.config.service(&t.service_id).is_some())
            .filter(|t| self.config.servers.iter().any(|s| s.id == t.server_id))
            .map(|t| t.id.clone())
            .collect();

        let to_remove: Vec<String> = self
            .tunnels
            .keys()
            .filter(|id| !enabled.contains(*id))
            .cloned()
            .collect();
        for id in to_remove {
            if let Some(mut tunnel) = self.tunnels.remove(&id) {
                tunnel.stop();
            }
        }

        let tunnels: Vec<Tunnel> = self
            .config
            .tunnels
            .iter()
            .filter(|t| enabled.contains(&t.id))
            .cloned()
            .collect();
        for tun in tunnels {
            if self.tunnels.contains_key(&tun.id) {
                continue;
            }
            let Some(service) = self.config.service(&tun.service_id).cloned() else {
                continue;
            };
            match self.build_tunnel(&tun, &service) {
                Ok(tunnel) => {
                    self.tunnels.insert(tun.id.clone(), tunnel);
                }
                Err(err) => {
                    self.reasons.insert(tun.id.clone(), err);
                }
            }
        }
    }

    /// 让本机服务的运行时表和配置保持一致。
    fn sync_local(&mut self) {
        // 删掉的
        let ids: HashSet<String> = self.config.services.iter().map(|s| s.id.clone()).collect();
        self.local.retain(|id, _| ids.contains(id));

        // 新增 / 改配置的
        for svc in &self.config.services {
            let spec = LocalSpec {
                name: svc.name.clone(),
                local_port: svc.local_port,
                launch: svc.launch.clone(),
                watch: svc.watch,
            };
            match self.local.get_mut(&svc.id) {
                Some(runner) => {
                    let same = {
                        let old = runner.spec();
                        old.name == spec.name
                            && old.local_port == spec.local_port
                            && old.watch == spec.watch
                            && launch_eq(&old.launch, &spec.launch)
                    };
                    if !same {
                        runner.set_spec(spec);
                    }
                }
                None => {
                    self.local
                        .insert(svc.id.clone(), LocalRunner::new(spec, &self.log_dir));
                }
            }
        }
    }

    fn build_tunnel(&self, tun: &Tunnel, service: &LocalService) -> Result<TunnelProc, String> {
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == tun.server_id)
            .ok_or_else(|| format!("找不到云端服务器（{}）", tun.server_id))?;

        let identity = tun
            .key_path
            .clone()
            .or_else(|| server.key_path.clone())
            .map(PathBuf::from);

        let target = TunnelTarget {
            ssh_target: format!("{}@{}", server.user, server.host),
            ssh_port: server.ssh_port,
            remote_port: tun.remote_port,
            local_port: service.local_port,
            identity_file: identity.clone(),
        };
        let cleanup = CleanupSpec {
            exec: SshExec {
                ssh_exe: ssh::default_ssh_exe(),
                target: format!("{}@{}", server.user, server.host),
                ssh_port: server.ssh_port,
                identity_file: identity,
            },
            remote_port: tun.remote_port,
        };
        let spec = TunnelSpec::new(
            tun.name.clone(),
            ssh::default_ssh_exe(),
            ssh::tunnel_args(&target),
        )
        .with_cleanup(cleanup);
        Ok(TunnelProc::new(spec, &self.log_dir))
    }

    // ---------- 快照 ----------

    /// 本机服务列表（界面「本机服务」页用）。
    pub fn snapshot(&self) -> Vec<ServiceView> {
        self.config
            .services
            .iter()
            .map(|svc| {
                let runner = self.local.get(&svc.id);
                let tunnels = self.config.tunnels_of_service(&svc.id);
                ServiceView {
                    id: svc.id.clone(),
                    name: svc.name.clone(),
                    local_port: svc.local_port,
                    status: runner
                        .map(|r| r.status_word().to_string())
                        .unwrap_or_else(|| "stopped".into()),
                    watch: svc.watch,
                    autostart: svc.autostart,
                    launch_exe: svc.launch.as_ref().map(|l| l.exe.clone()),
                    launch_args: svc.launch.as_ref().map(|l| l.args.clone()),
                    launch_workdir: svc.launch.as_ref().and_then(|l| l.workdir.clone()),
                    note: svc.note.clone(),
                    tunnel_count: tunnels.len(),
                    tunnel_names: tunnels.iter().map(|t| t.name.clone()).collect(),
                    reason: runner.and_then(|r| r.reason().map(str::to_string)),
                    log_path: runner
                        .map(|r| r.log_path().display().to_string())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// 隧道列表（界面「发布隧道」页用）。
    pub fn tunnel_snapshot(&self) -> Vec<TunnelView> {
        self.config
            .tunnels
            .iter()
            .map(|tun| {
                let service = self.config.service(&tun.service_id);
                let runner = self.local.get(&tun.service_id);
                TunnelView {
                    id: tun.id.clone(),
                    name: tun.name.clone(),
                    service_id: tun.service_id.clone(),
                    service_name: service.map(|s| s.name.clone()).unwrap_or_default(),
                    local_port: service.map(|s| s.local_port).unwrap_or(0),
                    server_id: tun.server_id.clone(),
                    remote_port: tun.remote_port,
                    url: tun.url.clone(),
                    status: self.status_of(tun).to_string(),
                    enabled: tun.enabled,
                    reason: self.reasons.get(&tun.id).cloned(),
                    access: tun.access.clone(),
                    access_user: tun.access_user.clone(),
                    has_password: tun
                        .access_hash
                        .as_deref()
                        .map(|h| !h.is_empty())
                        .unwrap_or(false),
                    access_detail: tun.access_detail.clone(),
                    service_ok: runner.map(|r| r.listening()).unwrap_or(false),
                    service_status: runner
                        .map(|r| r.status_word().to_string())
                        .unwrap_or_else(|| "stopped".into()),
                    key_path: tun.key_path.clone(),
                }
            })
            .collect()
    }

    /// 算一条隧道的状态词（快照和掉线通知共用）。
    fn status_of(&self, tun: &Tunnel) -> &'static str {
        if !tun.enabled {
            "paused"
        } else if self.config.service(&tun.service_id).is_none() {
            "error"
        } else if let Some(tunnel) = self.tunnels.get(&tun.id) {
            match tunnel.phase() {
                Phase::Running { .. } => {
                    if tunnel.connected() {
                        "running"
                    } else {
                        "connecting"
                    }
                }
                Phase::Backoff { .. } => {
                    if tunnel.spawn_failed() {
                        "error"
                    } else if tunnel.ever_connected() {
                        "disconnected"
                    } else {
                        "connecting"
                    }
                }
                Phase::Idle => "connecting",
                Phase::Stopped => "paused",
            }
        } else if self.config.servers.iter().any(|s| s.id == tun.server_id) {
            "connecting"
        } else {
            "error"
        }
    }

    /// 掉线通知用：只关心「开着但没在跑」的隧道，(id, 显示名, 是否正常)。
    pub fn health_snapshot(&self) -> Vec<(String, String, bool)> {
        self.config
            .tunnels
            .iter()
            .filter(|t| t.enabled)
            .map(|t| (t.id.clone(), t.name.clone(), self.status_of(t) == "running"))
            .collect()
    }

    /// 掉线通知开关。
    pub fn notify_enabled(&self) -> bool {
        self.config.notify_enabled
    }

    pub fn set_notify_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.config.notify_enabled = enabled;
        self.save()
    }

    pub fn servers(&self) -> Vec<ServerView> {
        self.config
            .servers
            .iter()
            .map(|s| ServerView {
                id: s.id.clone(),
                name: s.name.clone(),
                host: s.host.clone(),
                user: s.user.clone(),
                ssh_port: s.ssh_port,
                key_path: s.key_path.clone(),
            })
            .collect()
    }

    // ---------- 本机服务：增删改 + 启停 ----------

    /// 新增或编辑一台本机服务。
    pub fn save_service(&mut self, input: ServiceInput) -> Result<String, String> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err("请填写服务名".into());
        }
        if input.local_port == 0 {
            return Err("请填写本机端口".into());
        }
        if self
            .config
            .services
            .iter()
            .any(|s| s.local_port == input.local_port && Some(s.id.as_str()) != input.id.as_deref())
        {
            return Err(format!(
                "本机端口 {} 已经被另一台服务用了",
                input.local_port
            ));
        }

        let launch = build_launch(&input);
        let id = match input.id.clone() {
            Some(id) => {
                let svc = self
                    .config
                    .services
                    .iter_mut()
                    .find(|s| s.id == id)
                    .ok_or_else(|| format!("找不到服务：{id}"))?;
                svc.name = name;
                svc.local_port = input.local_port;
                svc.launch = launch;
                svc.watch = input.watch;
                svc.autostart = input.autostart;
                svc.note = clean_optional(input.note);
                id
            }
            None => {
                let id = config::new_id("svc");
                self.config.services.push(LocalService {
                    id: id.clone(),
                    name,
                    local_port: input.local_port,
                    launch,
                    watch: input.watch,
                    autostart: input.autostart,
                    note: clean_optional(input.note),
                });
                id
            }
        };
        self.save()?;
        self.sync_local();
        Ok(id)
    }

    /// 删除一台本机服务：还有隧道在用它就拦住（不自动连带删）。
    pub fn remove_service(&mut self, id: &str) -> Result<(), String> {
        let used = self.config.tunnels_of_service(id);
        if !used.is_empty() {
            let names: Vec<String> = used.iter().map(|t| format!("「{}」", t.name)).collect();
            return Err(format!(
                "还有 {} 条隧道在用它（{}），先把这些隧道删掉",
                used.len(),
                names.join("、")
            ));
        }
        if let Some(mut runner) = self.local.remove(id) {
            runner.stop_now();
        }
        self.config.services.retain(|s| s.id != id);
        self.save()
    }

    /// 启动一台本机服务。
    pub fn start_service(&mut self, id: &str) -> Result<(), String> {
        let svc = self
            .config
            .service(id)
            .ok_or_else(|| format!("找不到服务：{id}"))?;
        if svc.launch.is_none() {
            return Err("这台服务没有配置启动方式（只能看状态，不能帮你启动）".into());
        }
        self.sync_local();
        if let Some(runner) = self.local.get_mut(id) {
            runner.start_now();
            runner.poll();
        }
        Ok(())
    }

    /// 停止一台本机服务（只停我们自己启动的那个进程）。
    pub fn stop_service(&mut self, id: &str) -> Result<(), String> {
        let svc = self
            .config
            .service(id)
            .ok_or_else(|| format!("找不到服务：{id}"))?;
        if svc.launch.is_none() {
            return Err("这台服务没配启动方式，是你自己开的程序，我不会去动它".into());
        }
        if let Some(runner) = self.local.get_mut(id) {
            runner.stop_now();
        }
        Ok(())
    }

    /// 看护开关。
    pub fn set_service_watch(&mut self, id: &str, watch: bool) -> Result<(), String> {
        {
            let svc = self
                .config
                .services
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("找不到服务：{id}"))?;
            svc.watch = watch;
        }
        self.save()?;
        self.sync_local();
        Ok(())
    }

    /// 开机自启开关。
    pub fn set_service_autostart(&mut self, id: &str, autostart: bool) -> Result<(), String> {
        {
            let svc = self
                .config
                .services
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("找不到服务：{id}"))?;
            svc.autostart = autostart;
        }
        self.save()
    }

    /// 本机服务当前进程号（没有在跑就是 None）。
    pub fn service_pid(&self, id: &str) -> Option<u32> {
        self.local.get(id).and_then(|r| r.pid())
    }

    /// 本机服务自己的输出（stdout/stderr + 看护记录）。
    pub fn service_log_tail(&self, id: &str, lines: usize) -> String {
        let Some(runner) = self.local.get(id) else {
            return String::new();
        };
        read_tail(runner.log_path(), lines)
    }

    // ---------- 隧道：增删改 + 启停 ----------

    /// 新增一条隧道。
    pub fn add_tunnel(&mut self, input: NewTunnelInput) -> Result<String, String> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err("请填写隧道名字".into());
        }
        let service = self
            .config
            .service(&input.service_id)
            .ok_or("请先选择一台本机服务")?;
        let local_port = service.local_port;
        if !self.config.servers.iter().any(|s| s.id == input.server_id) {
            return Err("请先选择云端服务器".into());
        }
        if self
            .config
            .used_remote_ports(&input.server_id, None)
            .contains(&input.remote_port)
        {
            return Err(format!(
                "云端端口 {} 已经被这个服务器上的其他隧道用了",
                input.remote_port
            ));
        }
        let _ = local_port;

        // 勾了保护就必须有对应的内容：密码要有哈希、白名单要有 IP
        if input.access.iter().any(|a| a == "password")
            && clean_optional(input.access_hash.clone()).is_none()
        {
            return Err("这条隧道选了「要密码才能进」，请先设置访问密码".into());
        }
        if input.access.iter().any(|a| a == "ip")
            && clean_optional(input.access_detail.clone()).is_none()
        {
            return Err("这条隧道选了「只允许指定 IP」，请先把允许访问的 IP 填上".into());
        }

        let id = config::new_id("tun");
        self.config.tunnels.push(Tunnel {
            id: id.clone(),
            name,
            service_id: input.service_id,
            server_id: input.server_id,
            remote_port: input.remote_port,
            url: clean_optional(input.url),
            key_path: None,
            access: input.access,
            access_user: clean_optional(input.access_user),
            access_hash: clean_optional(input.access_hash),
            access_detail: clean_optional(input.access_detail),
            enabled: true,
        });
        self.save()?;
        Ok(id)
    }

    /// 编辑一条隧道。改云端端口会停掉旧隧道，由看护线程按新端口重新拉起。
    pub fn update_tunnel(&mut self, input: UpdateTunnelInput) -> Result<(), String> {
        let name = input.name.trim().to_string();
        if name.is_empty() {
            return Err("请填写隧道名字".into());
        }

        let (server_id, port_changed, old_access, old_hash, old_detail) = {
            let tun = self
                .config
                .tunnel(&input.id)
                .ok_or_else(|| format!("找不到隧道：{}", input.id))?;
            (
                tun.server_id.clone(),
                tun.remote_port != input.remote_port,
                tun.access.clone(),
                tun.access_hash.clone(),
                tun.access_detail.clone(),
            )
        };

        if self
            .config
            .used_remote_ports(&server_id, Some(&input.id))
            .contains(&input.remote_port)
        {
            return Err(format!(
                "云端端口 {} 已经被这个服务器上的其他隧道用了",
                input.remote_port
            ));
        }

        // 没传 access 就保持原样；传了就整体替换。
        let access = input.access.clone().unwrap_or(old_access);
        let new_hash = clean_optional(input.access_hash.clone());
        let wants_ip = access.iter().any(|a| a == "ip");
        // IP 校验/写回都用「合并后的 detail」：新值优先，没传就用旧的（CLI 不传时保留原值）。
        let final_detail = if wants_ip {
            clean_optional(input.access_detail.clone()).or(old_detail)
        } else {
            // 不要 IP 保护了：把旧的 IP 列表清掉（等于界面传 null）。
            None
        };
        if access.iter().any(|a| a == "password")
            && new_hash.is_none()
            && clean_optional(old_hash).is_none()
        {
            return Err("这条隧道选了「要密码才能进」，请先设置访问密码".into());
        }
        if wants_ip && final_detail.is_none() {
            return Err("这条隧道选了「只允许指定 IP」，请先把允许访问的 IP 填上".into());
        }

        {
            let tun = self
                .config
                .tunnels
                .iter_mut()
                .find(|t| t.id == input.id)
                .ok_or_else(|| format!("找不到隧道：{}", input.id))?;
            tun.name = name;
            tun.remote_port = input.remote_port;
            tun.url = clean_optional(input.url);
            tun.access = access;
            tun.access_user = clean_optional(input.access_user);
            if let Some(hash) = new_hash {
                tun.access_hash = Some(hash);
            }
            tun.access_detail = final_detail;
        }

        if port_changed {
            if let Some(mut tunnel) = self.tunnels.remove(&input.id) {
                tunnel.stop();
            }
        }

        self.save()
    }

    /// 开始发布 / 暂停发布。
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        {
            let tun = self
                .config
                .tunnels
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| format!("找不到隧道：{id}"))?;
            tun.enabled = enabled;
        }
        self.save()
    }

    /// 立刻重连：停掉当前隧道并重新开始（配置里顺带恢复发布）。
    pub fn restart(&mut self, id: &str) -> Result<(), String> {
        {
            let tun = self
                .config
                .tunnels
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| format!("找不到隧道：{id}"))?;
            tun.enabled = true;
        }
        if let Some(mut tunnel) = self.tunnels.remove(id) {
            tunnel.stop();
        }
        self.save()
    }

    /// 停止并删除一条隧道（配置一起删）。
    pub fn remove_tunnel(&mut self, id: &str) -> Result<(), String> {
        if let Some(mut tunnel) = self.tunnels.remove(id) {
            tunnel.stop();
        }
        self.config.tunnels.retain(|t| t.id != id);
        self.save()
    }

    // ---------- 导入 ----------

    /// 导入接管：把扫描到的建议写成「已暂停」的服务 + 隧道（用户确认后手动开始发布）。
    pub fn import_services(&mut self, items: Vec<ImportSuggestion>) -> Result<usize, String> {
        let mut added = 0usize;
        for item in items {
            let server_id = match self
                .config
                .servers
                .iter()
                .find(|s| s.host == item.server_host && s.user == item.server_user)
            {
                Some(s) => s.id.clone(),
                None => {
                    let id = config::new_id("srv");
                    self.config.servers.push(ServerConfig {
                        id: id.clone(),
                        name: format!("{}（导入）", item.server_host),
                        host: item.server_host.clone(),
                        ssh_port: item.ssh_port,
                        user: item.server_user.clone(),
                        key_path: None,
                    });
                    id
                }
            };
            if self
                .config
                .tunnels
                .iter()
                .any(|t| t.server_id == server_id && t.remote_port == item.remote_port)
            {
                continue; // 已有同端口的隧道，跳过
            }

            // 同一个本机端口只建一台本机服务
            let service_id = match self
                .config
                .services
                .iter()
                .find(|s| s.local_port == item.local_port)
            {
                Some(s) => s.id.clone(),
                None => {
                    let id = config::new_id("svc");
                    self.config.services.push(LocalService {
                        id: id.clone(),
                        name: item.name.clone(),
                        local_port: item.local_port,
                        launch: None,
                        watch: false,
                        autostart: false,
                        note: Some("从现有脚本导入".into()),
                    });
                    id
                }
            };

            self.config.tunnels.push(Tunnel {
                id: config::new_id("tun"),
                name: item.name.clone(),
                service_id,
                server_id,
                remote_port: item.remote_port,
                url: item.url.clone(),
                key_path: None,
                access: Vec::new(),
                access_user: None,
                access_hash: None,
                access_detail: None,
                enabled: false,
            });
            added += 1;
        }
        self.save()?;
        Ok(added)
    }

    // ---------- 服务器管理 ----------

    pub fn save_server(&mut self, input: ServerInput) -> Result<String, String> {
        if input.host.trim().is_empty() || input.user.trim().is_empty() {
            return Err("服务器地址和登录账号都不能为空".into());
        }
        let id = input.id.clone().unwrap_or_else(|| config::new_id("srv"));
        if let Some(server) = self.config.servers.iter_mut().find(|s| s.id == id) {
            server.name = input.name;
            server.host = input.host;
            server.ssh_port = input.ssh_port;
            server.user = input.user;
            server.key_path = clean_optional(input.key_path);
        } else {
            self.config.servers.push(ServerConfig {
                id: id.clone(),
                name: input.name,
                host: input.host,
                ssh_port: input.ssh_port,
                user: input.user,
                key_path: clean_optional(input.key_path),
            });
        }
        self.save()?;
        Ok(id)
    }

    /// 配免密要用：拿到服务器的地址/端口/账号。
    pub fn server_basics(&self, id: &str) -> Result<(String, u16, String), String> {
        let s = self
            .config
            .servers
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("找不到服务器：{id}"))?;
        Ok((s.host.clone(), s.ssh_port, s.user.clone()))
    }

    /// 配好免密后，把密钥路径记到这台服务器上（隧道就会用它）。
    pub fn set_server_key(&mut self, id: &str, key_path: &str) -> Result<(), String> {
        {
            let s = self
                .config
                .servers
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("找不到服务器：{id}"))?;
            s.key_path = Some(key_path.to_string());
        }
        self.save()
    }

    pub fn delete_server(&mut self, id: &str) -> Result<(), String> {
        if self.config.tunnels.iter().any(|t| t.server_id == id) {
            return Err("还有隧道挂在这台服务器上，先处理它们".into());
        }
        self.config.servers.retain(|s| s.id != id);
        self.save()
    }

    // ---------- 日志 / 密码 / nginx ----------

    /// 读一条隧道的日志尾部（界面「看日志」用）。
    pub fn log_tail(&self, id: &str, lines: usize) -> String {
        let Some(tun) = self.config.tunnel(id) else {
            return String::new();
        };
        read_tail(&service_log_path(&self.log_dir, &tun.name), lines)
    }

    /// 生成这条隧道的 nginx 配置片段（人工应用到面板）。
    pub fn nginx_snippet(&self, id: &str) -> Result<crate::nginx::NginxSnippet, String> {
        let tun = self
            .config
            .tunnel(id)
            .ok_or_else(|| format!("找不到隧道：{id}"))?;
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == tun.server_id)
            .ok_or_else(|| "找不到这条隧道挂的云端服务器".to_string())?;
        let local_port = self
            .config
            .service(&tun.service_id)
            .map(|s| s.local_port)
            .unwrap_or(0);
        Ok(crate::nginx::generate(tun, local_port, server))
    }

    /// 在服务器上生成 apr1 密码哈希（新建/编辑隧道用）。
    pub fn hash_password(&self, server_id: &str, password: &str) -> Result<String, String> {
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == server_id)
            .ok_or("找不到云端服务器")?;
        let exec = SshExec {
            ssh_exe: ssh::default_ssh_exe(),
            target: format!("{}@{}", server.user, server.host),
            ssh_port: server.ssh_port,
            identity_file: server.key_path.clone().map(PathBuf::from),
        };
        crate::cleanup::hash_password(&exec, password)
    }

    /// 只读地把核对需要的连接信息（目标、端口、密钥、域名）克隆出来，不联网。
    ///
    /// 真正跑 SSH 的是自由函数 [`run_access_check`]，调用方（Tauri 命令、控制通道）
    /// 可以在短锁里拿到 request、放锁后再跑，别占着看护轮询的全局锁最长 20 秒。
    pub fn access_check_request(&self, id: &str) -> Result<AccessCheckRequest, String> {
        let tun = self
            .config
            .tunnel(id)
            .ok_or_else(|| format!("找不到隧道：{id}"))?;
        let raw_url = tun
            .url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                format!(
                    "隧道「{}」还没填公网网址，没法核对云端保护；先回应用里把网址填上",
                    tun.name
                )
            })?;
        let domain = normalize_domain(raw_url);
        if domain.is_empty() {
            return Err(format!(
                "隧道「{}」还没填公网网址，没法核对云端保护；先回应用里把网址填上",
                tun.name
            ));
        }
        let server = self
            .config
            .servers
            .iter()
            .find(|s| s.id == tun.server_id)
            .ok_or_else(|| "找不到这条隧道挂的云端服务器".to_string())?;
        Ok(AccessCheckRequest {
            target: format!("{}@{}", server.user, server.host),
            ssh_port: server.ssh_port,
            identity_file: server.key_path.clone().map(PathBuf::from),
            domain,
        })
    }

    /// 核对这条隧道的网站上实际开着的访问保护：通过 SSH 只读地看云端 nginx 配置，
    /// 不改服务器上的任何东西。薄包装：先取连接信息，再跑核对。
    pub fn check_tunnel_access(&self, id: &str) -> Result<AccessCheck, String> {
        let req = self.access_check_request(id)?;
        run_access_check(&req)
    }

    fn save(&self) -> Result<(), String> {
        self.config
            .save(&self.config_path)
            .map_err(|e| format!("保存配置失败：{e}"))
    }
}

fn build_launch(input: &ServiceInput) -> Option<LaunchSpec> {
    let exe = input
        .launch_exe
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    Some(LaunchSpec {
        exe,
        args: input.launch_args.clone().unwrap_or_default(),
        workdir: clean_optional(input.launch_workdir.clone()),
    })
}

fn launch_eq(a: &Option<LaunchSpec>, b: &Option<LaunchSpec>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            x.exe == y.exe && x.args == y.args && x.workdir == y.workdir
        }
        _ => false,
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 核对云端访问保护需要的最小连接信息（从配置里只读克隆出来，不联网）。
#[derive(Debug, Clone)]
pub struct AccessCheckRequest {
    /// `user@host`
    pub target: String,
    pub ssh_port: u16,
    pub identity_file: Option<PathBuf>,
    /// 已规范化过的域名（去掉了协议前缀和路径）
    pub domain: String,
}

/// 跑一次云端访问保护核对（只读 SSH，最长 20 秒后超时）。
///
/// 不碰 Guardian，也不持有任何锁，所以调用方可以放锁后再调（见 P2-2）。
pub fn run_access_check(req: &AccessCheckRequest) -> Result<AccessCheck, String> {
    let exec = SshExec {
        ssh_exe: ssh::default_ssh_exe(),
        target: req.target.clone(),
        ssh_port: req.ssh_port,
        identity_file: req.identity_file.clone(),
    };
    let out = crate::cleanup::run(
        &exec,
        &access_check_script(&req.domain),
        Duration::from_secs(20),
    )?;
    Ok(crate::protection::parse(&out))
}

/// 网址 → 域名：去空白、去掉 http:// / https:// 前缀、砍掉第一个 `/` 或空白
/// 后面的路径。例如 `https://karryn.iranol1.xyz/x` → `karryn.iranol1.xyz`。
fn normalize_domain(raw: &str) -> String {
    let mut domain = raw.trim();
    for prefix in ["https://", "http://"] {
        if let Some(rest) = domain.strip_prefix(prefix) {
            domain = rest;
            break;
        }
        // 大写写法（HTTP://...）也认；前缀是纯 ASCII，按下标切不会切坏字符
        if domain.len() >= prefix.len()
            && domain.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            domain = &domain[prefix.len()..];
            break;
        }
    }
    let end = domain
        .find(|c: char| c == '/' || c.is_whitespace())
        .unwrap_or(domain.len());
    domain[..end].trim().to_string()
}

/// 核对访问保护的远端脚本：只读地把站点配置夹在正文标记之间打印出来，
/// 标记之外再报告配置文件 / 密码文件在不在。
/// 域名用单引号包好，避免被 shell 当成命令解释。
fn access_check_script(domain: &str) -> String {
    format!(
        "d={}; f=\"/xp/panel/vhost/nginx/$d.conf\"; if [ -f \"$f\" ]; then echo '===SHANGXIAN-CONF-BEGIN==='; cat \"$f\"; echo; echo '===SHANGXIAN-CONF-END==='; echo CONF_FOUND; else echo CONF_MISSING; fi; if [ -f \"/xp/panel/vhost/$d.htpasswd\" ]; then echo HTPASSWD_YES; else echo HTPASSWD_NO; fi",
        crate::cleanup::shell_quote(domain)
    )
}

fn read_tail(path: &std::path::Path, lines: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return format!("（还没有日志：{}）", path.display());
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn check_tunnel_access_errors_are_plain_chinese() {
        let dir = std::env::temp_dir().join("sx-access-check-test");
        let _ = std::fs::remove_dir_all(&dir);

        // 找不到隧道
        let g = Guardian::new(AppConfig::default(), dir.join("config.json"), dir.join("logs"));
        let err = g.check_tunnel_access("tun-nope").unwrap_err();
        assert!(err.contains("找不到隧道"), "err={err}");

        // 隧道存在但没填网址 → 提醒先填网址（不去连服务器）
        let cfg = AppConfig {
            version: config::CONFIG_VERSION,
            servers: Vec::new(),
            services: Vec::new(),
            tunnels: vec![Tunnel {
                id: "tun-1".into(),
                name: "相册".into(),
                service_id: "svc-1".into(),
                server_id: "srv-1".into(),
                remote_port: 18080,
                url: None,
                key_path: None,
                access: Vec::new(),
                access_user: None,
                access_hash: None,
                access_detail: None,
                enabled: true,
            }],
            notify_enabled: true,
        };
        let g = Guardian::new(cfg, dir.join("config.json"), dir.join("logs"));
        let err = g.check_tunnel_access("tun-1").unwrap_err();
        assert!(err.contains("网址"), "err={err}");
    }

    #[test]
    fn normalize_domain_strips_scheme_path_and_spaces() {
        assert_eq!(normalize_domain("https://karryn.iranol1.xyz/x"), "karryn.iranol1.xyz");
        assert_eq!(normalize_domain("http://album.example.com"), "album.example.com");
        assert_eq!(normalize_domain(" album.example.com/y/z?q=1 "), "album.example.com");
        assert_eq!(normalize_domain("album.example.com 8080"), "album.example.com");
        assert_eq!(normalize_domain("HTTPS://Album.example.com/x"), "Album.example.com");
        assert_eq!(normalize_domain("/only-path"), "");
        assert_eq!(normalize_domain("karryn.iranol1.xyz"), "karryn.iranol1.xyz");
    }

    #[test]
    fn access_check_request_clones_connection_info() {
        let dir = std::env::temp_dir().join("sx-access-request-test");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = AppConfig {
            version: config::CONFIG_VERSION,
            servers: vec![ServerConfig {
                id: "srv-1".into(),
                name: "香港-1".into(),
                host: "karryn.iranol1.xyz".into(),
                ssh_port: 2222,
                user: "root".into(),
                key_path: Some("C:/k/id_ed25519".into()),
            }],
            services: Vec::new(),
            tunnels: vec![Tunnel {
                id: "tun-1".into(),
                name: "相册".into(),
                service_id: "svc-1".into(),
                server_id: "srv-1".into(),
                remote_port: 18080,
                url: Some("  https://karryn.iranol1.xyz/album?q=1  ".into()),
                key_path: None,
                access: Vec::new(),
                access_user: None,
                access_hash: None,
                access_detail: None,
                enabled: true,
            }],
            notify_enabled: true,
        };
        let g = Guardian::new(cfg, dir.join("config.json"), dir.join("logs"));
        let req = g.access_check_request("tun-1").unwrap();
        assert_eq!(req.domain, "karryn.iranol1.xyz");
        assert_eq!(req.target, "root@karryn.iranol1.xyz");
        assert_eq!(req.ssh_port, 2222);
        assert_eq!(
            req.identity_file.as_deref(),
            Some(std::path::Path::new("C:/k/id_ed25519"))
        );
    }

    #[test]
    #[ignore = "只读真机对照：需要能免密 SSH 到 api.iranol1.xyz；手动跑 --ignored"]
    fn real_machine_karryn_access_check() {
        // 真机对照（只读，不改服务器）：karryn.iranol1.xyz 的 conf 是
        // server 级 auth_basic "需要密码" + .well-known 里 auth_basic off/allow all，
        // 没有任何整站 allow/deny → passwordOn=true、ipOn=false、allowIps=[]。
        let req = AccessCheckRequest {
            target: "cgt@api.iranol1.xyz".into(),
            ssh_port: 22,
            identity_file: None,
            domain: "karryn.iranol1.xyz".into(),
        };
        let check = run_access_check(&req).expect("真机 SSH 只读核对失败");
        assert!(check.conf_found, "站点 conf 应该存在");
        assert!(check.htpasswd_exists, "htpasswd 应该存在");
        assert!(check.password_on, "server 级 auth_basic 应该算开密码");
        assert!(!check.ip_on, ".well-known 里的 allow all 不能算整站白名单");
        assert!(check.allow_ips.is_empty(), "没有整站白名单", );
    }

    #[test]
    fn access_check_script_wraps_conf_in_markers() {
        let script = access_check_script("album.example.com");
        assert!(script.starts_with("d='album.example.com';"));
        assert!(script.contains("/xp/panel/vhost/nginx/$d.conf"));
        assert!(script.contains("/xp/panel/vhost/$d.htpasswd"));
        assert!(
            script.contains("===SHANGXIAN-CONF-BEGIN===")
                && script.contains("===SHANGXIAN-CONF-END==="),
            "script={script}"
        );
        assert!(script.contains("CONF_FOUND") && script.contains("CONF_MISSING"));
        // 顺序：BEGIN → cat 正文 → END → 结果标记
        let begin = script.find("===SHANGXIAN-CONF-BEGIN===").unwrap();
        let cat = script.find("cat \"$f\"").unwrap();
        let end = script.find("===SHANGXIAN-CONF-END===").unwrap();
        let found = script.find("CONF_FOUND").unwrap();
        assert!(begin < cat && cat < end && end < found, "script={script}");
        // 域名里的单引号被转义，不会跑出 shell 命令
        let tricky = access_check_script("a'b");
        assert!(tricky.starts_with(r#"d='a'\''b';"#), "script={tricky}");
    }
}
