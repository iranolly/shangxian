//! 配置导出 / 导入（D2 §11）＋ 原有「从启动脚本生成建议配置」的扫描。
//!
//! - 导出：包装格式 `{kind, formatVersion:1, exportedAt, appVersion, configVersion,
//!   redacted, config}`；默认脱敏（`accessHash` 键整个不出现、`keyPath` 换成
//!   `keyPathPresent`、加 `hasPassword`）；`--with-paths` 只影响 `keyPath`，
//!   哈希**永远**不导出。
//! - 导入：接受包装格式，也接受裸 config；`merge` 只覆盖导入 JSON 里出现的字段
//!   （键不出现 = 保留现值；显式 null = 清空）；`replace` 整体替换、先写
//!   `config.json.bak-<epoch ms>`；所有校验 all-or-nothing，任一条不过什么都不写。

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::config::{
    self, access_unknown_tokens, AppConfig, LaunchSpec, LocalService, ServerConfig, Settings,
    Tunnel, CONFIG_VERSION,
};
use crate::state::now_ms;

/// 导出包装的格式版本（冻结）。
pub const EXPORT_FORMAT_VERSION: u32 = 1;
/// 导出包装的 kind 标识（冻结）。
pub const EXPORT_KIND: &str = "shangxian.config-export";
/// ctl `import_config` 的 `doc` 上限（1MB）。
pub const IMPORT_DOC_MAX_BYTES: usize = 1024 * 1024;

// ---------- 导出 ----------

/// 导出应用版本（取编译时的包版本）。
pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 导出成包装格式（`configVersion` 用配置里的版本）。
pub fn export_config(cfg: &AppConfig, redacted: bool) -> Value {
    export_config_with_version(cfg, redacted, cfg.version)
}

/// 导出成包装格式，允许显式指定 `configVersion`
/// （降级拦截时纯读导出 v7 原文用，见 D2 §13）。
pub fn export_config_with_version(cfg: &AppConfig, redacted: bool, config_version: u32) -> Value {
    json!({
        "kind": EXPORT_KIND,
        "formatVersion": EXPORT_FORMAT_VERSION,
        "exportedAt": now_ms(),
        "appVersion": app_version(),
        "configVersion": config_version,
        "redacted": redacted,
        "config": config_to_export_value(cfg, redacted, config_version),
    })
}

/// 导出成文本（`pretty=true` 给人看，false 给机器）。
pub fn export_config_text(cfg: &AppConfig, redacted: bool, pretty: bool) -> String {
    let value = export_config(cfg, redacted);
    if pretty {
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
    } else {
        value.to_string()
    }
}

/// 原子写 JSON（同目录临时文件 + rename）。
pub fn write_json_atomic(path: &Path, value: &Value) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 把配置按导出白名单重建成 JSON（不是「复制后删键」）。
///
/// `redacted=true`：`accessHash` 不出现、`keyPath` 换成 `keyPathPresent`。
/// `redacted=false`（`--with-paths`）：出现真实 `keyPath`，`accessHash` 仍然不出现。
pub fn config_to_export_value(cfg: &AppConfig, redacted: bool, config_version: u32) -> Value {
    let servers: Vec<Value> = cfg
        .servers
        .iter()
        .map(|s| server_to_export(s, redacted))
        .collect();
    let services: Vec<Value> = cfg
        .services
        .iter()
        .map(|s| service_to_export(s))
        .collect();
    let tunnels: Vec<Value> = cfg
        .tunnels
        .iter()
        .map(|t| tunnel_to_export(t, redacted))
        .collect();
    json!({
        "version": config_version,
        "servers": servers,
        "services": services,
        "tunnels": tunnels,
        "notifyEnabled": cfg.notify_enabled,
        "settings": serde_json::to_value(&cfg.settings).unwrap_or(Value::Null),
    })
}

fn server_to_export(server: &ServerConfig, redacted: bool) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), json!(server.id));
    map.insert("name".into(), json!(server.name));
    map.insert("host".into(), json!(server.host));
    map.insert("sshPort".into(), json!(server.ssh_port));
    map.insert("user".into(), json!(server.user));
    map.insert("vhostDir".into(), json!(server.vhost_dir));
    if !redacted {
        map.insert("keyPath".into(), json!(server.key_path));
    }
    map.insert("keyPathPresent".into(), json!(server.key_path.is_some()));
    Value::Object(map)
}

fn service_to_export(service: &LocalService) -> Value {
    let launch = match &service.launch {
        Some(launch) => launch_to_export(launch),
        None => Value::Null,
    };
    json!({
        "id": service.id,
        "name": service.name,
        "localPort": service.local_port,
        "launch": launch,
        "watch": service.watch,
        "autostart": service.autostart,
        "note": service.note,
    })
}

fn launch_to_export(launch: &LaunchSpec) -> Value {
    json!({
        "exe": launch.exe,
        "args": launch.args,
        "workdir": launch.workdir,
    })
}

fn tunnel_to_export(tunnel: &Tunnel, redacted: bool) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), json!(tunnel.id));
    map.insert("name".into(), json!(tunnel.name));
    map.insert("serviceId".into(), json!(tunnel.service_id));
    map.insert("serverId".into(), json!(tunnel.server_id));
    map.insert("remotePort".into(), json!(tunnel.remote_port));
    map.insert("url".into(), json!(tunnel.url));
    if !redacted {
        map.insert("keyPath".into(), json!(tunnel.key_path));
    }
    map.insert("access".into(), json!(tunnel.access));
    map.insert("accessUser".into(), json!(tunnel.access_user));
    map.insert("accessDetail".into(), json!(tunnel.access_detail));
    map.insert("enabled".into(), json!(tunnel.enabled));
    map.insert("preset".into(), json!(tunnel.preset));
    map.insert("keyPathPresent".into(), json!(tunnel.key_path.is_some()));
    map.insert("hasPassword".into(), json!(tunnel.access_hash.is_some()));
    Value::Object(map)
}

// ---------- 导入 ----------

/// 导入模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// 按 id 合并：只覆盖导入 JSON 里出现的字段（默认）。
    Merge,
    /// 整体替换（先备份）。
    Replace,
}

impl ImportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ImportMode::Merge => "merge",
            ImportMode::Replace => "replace",
        }
    }

    /// 解析模式名（ctl/CLI 传进来的字符串）。
    pub fn parse(mode: &str) -> Result<Self, String> {
        match mode {
            "merge" => Ok(ImportMode::Merge),
            "replace" => Ok(ImportMode::Replace),
            other => Err(format!("导入方式只支持 merge（合并）或 replace（整体替换），无法识别 {other}")),
        }
    }
}

/// 每张清单的新增/更新/未变化条数。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCounts {
    pub servers: u32,
    pub services: u32,
    pub tunnels: u32,
}

/// 导入结果摘要（D2 §11.2 返回形状）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub mode: String,
    pub dry_run: bool,
    pub added: ImportCounts,
    pub updated: ImportCounts,
    pub unchanged: ImportCounts,
    pub warnings: Vec<String>,
    pub backup_path: Option<String>,
}

/// 导入的结果：新配置 + 摘要（写盘失败会返回 Err，内存里保持原样）。
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub config: AppConfig,
    pub summary: ImportSummary,
}

/// 只做校验与合并计算，**绝不写盘**（`dry_run` 直接用这个）。
pub fn plan_import(
    current: &AppConfig,
    doc: &str,
    mode: ImportMode,
) -> Result<(AppConfig, ImportSummary), String> {
    let root: Value = serde_json::from_str(doc)
        .map_err(|e| format!("导入失败：该文件不是有效的 JSON（{e}）"))?;
    let (raw_config, version) = unwrap_document(&root)?;

    // 版本检查（D2 §11.2 / §13）
    if version > CONFIG_VERSION {
        return Err(format!(
            "{} 升级后会自动读回，不用重新配。",
            config::config_unsupported_message(version)
        ));
    }
    // 与 CLI 的 check_import_version 同一道门槛：v4 及更旧的文件导入不了，
    // 让用户先用旧版应用打开并保存一次（迁移到新版本）再导出。
    if version < 5 {
        return Err(format!(
            "导入失败：此设置版本过旧（版本 {version}）。请先用旧版「上线助手」打开并保存一次，再用新保存的文件导入。"
        ));
    }
    let Some(raw_obj) = raw_config.as_object() else {
        return Err("导入失败：设置内容不是一份 JSON 对象".to_string());
    };

    let mut warnings: Vec<String> = Vec::new();
    let mut added = ImportCounts::default();
    let mut updated = ImportCounts::default();
    let mut unchanged = ImportCounts::default();

    let mut result = match mode {
        ImportMode::Merge => merge_into(
            current,
            raw_obj,
            &mut warnings,
            &mut added,
            &mut updated,
            &mut unchanged,
        )?,
        ImportMode::Replace => replace_with(
            current,
            raw_obj,
            &mut warnings,
            &mut added,
            &mut updated,
            &mut unchanged,
        )?,
    };

    // 归一化（刷新间隔夹范围、静音时段回默认、preset 回 web）
    config::normalize_config(&mut result);

    validate_config(&result, &mut warnings)?;

    let summary = ImportSummary {
        mode: mode.as_str().to_string(),
        dry_run: true,
        added,
        updated,
        unchanged,
        warnings,
        backup_path: None,
    };
    Ok((result, summary))
}

/// 校验 + 备份 + 写盘；成功返回新配置（调用方换进内存）。
///
/// 调用方必须先确认现在允许写配置（`config_save_allowed`），配置读坏/降级拦截时不要调。
pub fn import_config(
    config_path: &Path,
    current: &AppConfig,
    doc: &str,
    mode: ImportMode,
    dry_run: bool,
) -> Result<ImportOutcome, String> {
    let (new_config, mut summary) = plan_import(current, doc, mode)?;
    if dry_run {
        return Ok(ImportOutcome {
            config: new_config,
            summary,
        });
    }

    // replace / merge 都会改现有对象：先留一份即时备份
    let backup = next_backup_path(config_path, now_ms());
    let backup_result = if config_path.exists() {
        std::fs::copy(config_path, &backup).map(|_| ())
    } else {
        let text = match serde_json::to_string_pretty(current) {
            Ok(text) => text,
            Err(e) => {
                return Err(format!("备份失败：{e}；为了安全，导入未继续"));
            }
        };
        std::fs::write(&backup, text)
    };
    if let Err(e) = backup_result {
        return Err(format!("备份失败：{e}；为了安全，导入未继续"));
    }

    if let Err(e) = new_config.save(config_path) {
        return Err(format!("保存失败：{e}；原有设置未改动"));
    }

    summary.dry_run = false;
    summary.backup_path = Some(backup.to_string_lossy().to_string());
    Ok(ImportOutcome {
        config: new_config,
        summary,
    })
}

/// 备份路径：`config.json.bak-<epoch ms>`；同一毫秒撞了就往后挪。
fn next_backup_path(config_path: &Path, now: i64) -> PathBuf {
    let mut stamp = now;
    loop {
        let candidate = config_path.with_extension(format!("json.bak-{stamp}"));
        if !candidate.exists() {
            return candidate;
        }
        stamp += 1;
    }
}

/// 拆包装：返回 (config 值, configVersion)。
fn unwrap_document(root: &Value) -> Result<(Value, u32), String> {
    let Some(obj) = root.as_object() else {
        return Err("导入失败：文件内容不是一份 JSON 对象".to_string());
    };
    let is_wrapper =
        obj.contains_key("kind") || obj.contains_key("formatVersion") || obj.contains_key("config");

    if is_wrapper {
        if let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) {
            if kind != EXPORT_KIND {
                return Err(format!(
                    "导入失败：这份文件不是「上线助手」的配置备份（kind={kind}）"
                ));
            }
        }
        match obj.get("formatVersion").and_then(|v| v.as_u64()) {
            Some(n) if n == EXPORT_FORMAT_VERSION as u64 => {}
            Some(other) => {
                return Err(format!(
                    "导入失败：这份备份是更新版本的格式（formatVersion={other}），请先把「上线助手」升级到最新版"
                ))
            }
            None => return Err("导入失败：备份文件中没有 formatVersion，无法识别".to_string()),
        }
        let config = obj
            .get("config")
            .cloned()
            .ok_or_else(|| "导入失败：备份文件里没有设置内容（config）".to_string())?;
        let version = obj
            .get("configVersion")
            .and_then(|v| v.as_u64())
            .or_else(|| config.get("version").and_then(|v| v.as_u64()))
            .ok_or_else(|| "导入失败：备份文件里没有配置版本号（configVersion）".to_string())?;
        Ok((config, version as u32))
    } else {
        let version = obj
            .get("version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                "导入失败：文件中没有版本号（version），无法判断这是否是一份设置".to_string()
            })?;
        Ok((root.clone(), version as u32))
    }
}

/// merge：按 id 匹配，只覆盖导入 JSON 里出现的字段。
fn merge_into(
    current: &AppConfig,
    raw: &Map<String, Value>,
    warnings: &mut Vec<String>,
    added: &mut ImportCounts,
    updated: &mut ImportCounts,
    unchanged: &mut ImportCounts,
) -> Result<AppConfig, String> {
    let mut result = current.clone();
    result.version = CONFIG_VERSION;

    // 服务器
    let imported = imported_array(raw, "servers", "服务器")?;
    check_duplicate_ids(&imported, "服务器")?;
    let mut values = values_of(&result.servers);
    apply_merge(
        &mut values,
        &imported,
        &["keyPathPresent"],
        "服务器",
        added,
        updated,
        unchanged,
        Slot::Servers,
    )?;
    result.servers = deserialize_list::<ServerConfig>(values, "服务器")?;

    // 本机服务
    let imported = imported_array(raw, "services", "本机服务")?;
    check_duplicate_ids(&imported, "本机服务")?;
    let mut values = values_of(&result.services);
    apply_merge(
        &mut values,
        &imported,
        &[],
        "本机服务",
        added,
        updated,
        unchanged,
        Slot::Services,
    )?;
    result.services = deserialize_list::<LocalService>(values, "本机服务")?;

    // 隧道
    let imported = imported_array(raw, "tunnels", "隧道")?;
    check_duplicate_ids(&imported, "隧道")?;
    let imported: Vec<Value> = imported
        .iter()
        .map(|item| normalize_tunnel_value(item, warnings))
        .collect();
    let mut values = values_of(&result.tunnels);
    apply_merge(
        &mut values,
        &imported,
        &["hasPassword", "keyPathPresent"],
        "隧道",
        added,
        updated,
        unchanged,
        Slot::Tunnels,
    )?;
    result.tunnels = deserialize_list::<Tunnel>(values, "隧道")?;

    // settings / notifyEnabled：出现在文件里才覆盖（settings 整份替换）
    if let Some(settings) = raw.get("settings") {
        result.settings = serde_json::from_value(settings.clone())
            .map_err(|e| format!("导入失败：设置项无法读取（{e}）"))?;
    }
    if let Some(notify) = raw.get("notifyEnabled") {
        result.notify_enabled = notify
            .as_bool()
            .ok_or_else(|| "导入失败：notifyEnabled 只能填 true 或 false".to_string())?;
    }
    Ok(result)
}

/// replace：servers/services/tunnels/settings/notifyEnabled 整体替换；
/// 文件里没有的 settings 字段用默认值（不是现值）。
fn replace_with(
    current: &AppConfig,
    raw: &Map<String, Value>,
    warnings: &mut Vec<String>,
    added: &mut ImportCounts,
    updated: &mut ImportCounts,
    unchanged: &mut ImportCounts,
) -> Result<AppConfig, String> {
    let servers: Vec<ServerConfig> = match raw.get("servers") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("导入失败：服务器列表无法读取（{e}）"))?,
        None => Vec::new(),
    };
    let services: Vec<LocalService> = match raw.get("services") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("导入失败：本机服务列表无法读取（{e}）"))?,
        None => Vec::new(),
    };
    let tunnels: Vec<Tunnel> = match raw.get("tunnels") {
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for item in items {
                let item = normalize_tunnel_value(item, warnings);
                out.push(serde_json::from_value::<Tunnel>(item.clone()).map_err(|e| {
                    format!("导入失败：隧道列表中有条目无法读取（{e}）")
                })?);
            }
            out
        }
        Some(_) => return Err("导入失败：tunnels 必须是一个列表".to_string()),
        None => Vec::new(),
    };
    let settings: Settings = match raw.get("settings") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("导入失败：设置项无法读取（{e}）"))?,
        None => Settings::default(),
    };
    let notify_enabled = match raw.get("notifyEnabled") {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "导入失败：notifyEnabled 只能填 true 或 false".to_string())?,
        None => true,
    };

    // 计一计「比现在多了什么/变了什么」
    count_replace(&current.servers, &servers, added, updated, unchanged, Slot::Servers);
    count_replace(&current.services, &services, added, updated, unchanged, Slot::Services);
    count_replace(&current.tunnels, &tunnels, added, updated, unchanged, Slot::Tunnels);

    Ok(AppConfig {
        version: CONFIG_VERSION,
        servers,
        services,
        tunnels,
        notify_enabled,
        settings,
    })
}

#[derive(Clone, Copy)]
enum Slot {
    Servers,
    Services,
    Tunnels,
}

#[derive(PartialEq)]
enum MergeKind {
    Added,
    Updated,
    Unchanged,
}

fn count(
    kind: &MergeKind,
    added: &mut ImportCounts,
    updated: &mut ImportCounts,
    unchanged: &mut ImportCounts,
    slot: Slot,
) {
    let target = match kind {
        MergeKind::Added => added,
        MergeKind::Updated => updated,
        MergeKind::Unchanged => unchanged,
    };
    match slot {
        Slot::Servers => target.servers += 1,
        Slot::Services => target.services += 1,
        Slot::Tunnels => target.tunnels += 1,
    }
}

fn values_of<T: Serialize>(items: &[T]) -> Vec<Value> {
    items
        .iter()
        .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
        .collect()
}

fn imported_array(
    raw: &Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<Vec<Value>, String> {
    match raw.get(key) {
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(_) => Err(format!("导入失败：{label}列表（{key}）不是一个列表")),
        None => Ok(Vec::new()),
    }
}

fn check_duplicate_ids(items: &[Value], label: &str) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            return Err(format!("导入失败：{label}列表中有条目未填写 id"));
        };
        if !seen.insert(id.to_string()) {
            return Err(format!("导入失败：{label}列表中有两条记录使用了同一个 id：{id}"));
        }
    }
    Ok(())
}

/// 在现值列表上合并导入项：新 id 追加到末尾；已有 id 只覆盖导入 JSON 里出现的键。
/// `ignored` 里的键是展示信息（`hasPassword`/`keyPathPresent`），不参与合并。
#[allow(clippy::too_many_arguments)]
fn apply_merge(
    current_values: &mut Vec<Value>,
    imported: &[Value],
    ignored: &[&str],
    label: &str,
    added: &mut ImportCounts,
    updated: &mut ImportCounts,
    unchanged: &mut ImportCounts,
    slot: Slot,
) -> Result<(), String> {
    for item in imported {
        let Some(item_obj) = item.as_object() else {
            return Err(format!("导入失败：{label}列表中有条目不是对象"));
        };
        let Some(id) = item_obj.get("id").and_then(|v| v.as_str()) else {
            return Err(format!("导入失败：{label}列表中有条目未填写 id"));
        };
        let index = current_values
            .iter()
            .position(|value| value.get("id").and_then(|v| v.as_str()) == Some(id));
        let Some(index) = index else {
            current_values.push(item.clone());
            count(&MergeKind::Added, added, updated, unchanged, slot);
            continue;
        };
        let Some(current_obj) = current_values[index].as_object() else {
            current_values[index] = item.clone();
            count(&MergeKind::Updated, added, updated, unchanged, slot);
            continue;
        };
        let mut merged = current_obj.clone();
        for (key, value) in item_obj {
            if ignored.contains(&key.as_str()) {
                continue;
            }
            merged.insert(key.clone(), value.clone());
        }
        let merged = Value::Object(merged);
        let kind = if merged == current_values[index] {
            MergeKind::Unchanged
        } else {
            MergeKind::Updated
        };
        count(&kind, added, updated, unchanged, slot);
        current_values[index] = merged;
    }
    Ok(())
}

fn deserialize_list<T: serde::de::DeserializeOwned>(
    values: Vec<Value>,
    label: &str,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for value in values {
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("(未填写 id)")
            .to_string();
        let item = serde_json::from_value::<T>(value)
            .map_err(|e| format!("导入失败：{label} {id} 无法读取（{e}）"))?;
        out.push(item);
    }
    Ok(out)
}

/// 导入的隧道值预处理：`hasPassword` 是展示信息（忽略，不参与合并判断），
/// `preset` 非法 → 归一 `"web"` + 记 warning；`access` 里的未知写法 → 记 warning
/// （归一由柔性反序列化完成）。
fn normalize_tunnel_value(item: &Value, warnings: &mut Vec<String>) -> Value {
    let mut item = item.clone();
    let Some(obj) = item.as_object_mut() else {
        return item;
    };
    obj.remove("hasPassword");

    if let Some(preset) = obj.get("preset").and_then(|v| v.as_str()) {
        if !config::is_known_preset(preset) {
            obj.insert("preset".into(), json!(config::DEFAULT_PRESET));
            warnings.push(
                "有隧道的历史预设字段（preset）无法识别，已按网页（web）处理；该字段新版本不再由界面/CLI 设置，老隧道的值会尽量保留"
                    .to_string(),
            );
        }
    }

    if let Some(access) = obj.get("access") {
        let unknown = access_unknown_tokens(access);
        if !unknown.is_empty() {
            warnings.push(format!(
                "有隧道的访问保护写法无法识别（{}），已忽略这几项",
                unknown.join("、")
            ));
        }
    }
    item
}

fn count_replace<T: Serialize>(
    current: &[T],
    result: &[T],
    added: &mut ImportCounts,
    updated: &mut ImportCounts,
    unchanged: &mut ImportCounts,
    slot: Slot,
) {
    let current_values = values_of(current);
    for item in result {
        let value = serde_json::to_value(item).unwrap_or(Value::Null);
        let id = value.get("id").and_then(|v| v.as_str()).unwrap_or("");
        match current_values
            .iter()
            .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(id))
        {
            None => count(&MergeKind::Added, added, updated, unchanged, slot),
            Some(existing) if *existing == value => {
                count(&MergeKind::Unchanged, added, updated, unchanged, slot)
            }
            Some(_) => count(&MergeKind::Updated, added, updated, unchanged, slot),
        }
    }
}

/// 结果配置的完整性校验（all-or-nothing）：重复 id、引用的服务/服务器、
/// 端口重复或为 0；顺手收集「密码没带哈希」这类 warning。
fn validate_config(cfg: &AppConfig, warnings: &mut Vec<String>) -> Result<(), String> {
    // 1) 同一列表内 id 重复
    unique_ids(&cfg.servers, "服务器")?;
    unique_ids(&cfg.services, "本机服务")?;
    unique_ids(&cfg.tunnels, "隧道")?;

    // 2) 隧道引用必须存在
    for tunnel in &cfg.tunnels {
        if cfg.service(&tunnel.service_id).is_none() {
            return Err(format!(
                "导入失败：隧道「{}」指向的本机服务（{}）不在导入内容中；请补上服务后重新导入",
                tunnel.name, tunnel.service_id
            ));
        }
        if cfg.servers.iter().all(|s| s.id != tunnel.server_id) {
            return Err(format!(
                "导入失败：隧道「{}」指向的云端服务器（{}）不在导入内容中；请补上服务器后重新导入",
                tunnel.name, tunnel.server_id
            ));
        }
    }

    // 3) 同一服务器上云端端口不能重复
    let mut used: std::collections::BTreeMap<(String, u16), String> = std::collections::BTreeMap::new();
    for tunnel in &cfg.tunnels {
        let key = (tunnel.server_id.clone(), tunnel.remote_port);
        if let Some(other) = used.insert(key, tunnel.name.clone()) {
            return Err(format!(
                "导入失败：云端端口 {} 被两条隧道同时使用（{} 和 {}），请先修改其中一个再导入",
                tunnel.remote_port, other, tunnel.name
            ));
        }
    }

    // 4) 本机端口：不能是 0，不能重复
    let mut local_ports: std::collections::BTreeMap<u16, String> = std::collections::BTreeMap::new();
    for service in &cfg.services {
        if service.local_port == 0 {
            return Err(format!(
                "导入失败：本机服务「{}」的本机端口为 0，必须填写 1 到 65535",
                service.name
            ));
        }
        if let Some(other) = local_ports.insert(service.local_port, service.name.clone()) {
            return Err(format!(
                "导入失败：本机端口 {} 被两台服务同时使用（{} 和 {}），请先修改其中一个再导入",
                service.local_port, other, service.name
            ));
        }
    }

    // 5) 设了访问密码但没有哈希（merge 时可能保留现值，所以看结果配置）
    let no_hash = cfg
        .tunnels
        .iter()
        .filter(|t| t.wants_password() && t.access_hash.is_none())
        .count();
    if no_hash > 0 {
        warnings.push(format!(
            "有 {no_hash} 条隧道已设置访问密码，但未包含密码；导入后请重新设置访问密码"
        ));
    }
    Ok(())
}

fn unique_ids<T>(items: &[T], label: &str) -> Result<(), String>
where
    T: Serialize,
{
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        let value = serde_json::to_value(item).unwrap_or(Value::Null);
        let Some(id) = value.get("id").and_then(|v| v.as_str()) else {
            return Err(format!("导入失败：{label}列表中有条目未填写 id"));
        };
        if !seen.insert(id.to_string()) {
            return Err(format!("导入失败：{label}列表中有两条记录使用了同一个 id：{id}"));
        }
    }
    Ok(())
}

// ---------- 原有：从启动脚本生成建议配置 ----------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSuggestion {
    pub name: String,
    /// 来源脚本路径（给用户看"从哪儿读出来的"）
    pub source: String,
    pub server_host: String,
    pub server_user: String,
    pub ssh_port: u16,
    pub local_port: u16,
    pub remote_port: u16,
    pub url: Option<String>,
}

/// 已知的候选脚本（找不到就跳过）。
/// 路径从环境变量拼出来，不写死某个人的电脑；想加自己的，
/// 设 SHANGXIAN_SCAN_EXTRA="名字=完整路径;名字=完整路径"。
fn candidates() -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("USERPROFILE") {
        if !home.is_empty() {
            out.push((
                "pi-web".to_string(),
                format!(r"{home}\.pi\pi-web\start-pi-web.ps1"),
            ));
        }
    }
    if let Ok(extra) = std::env::var("SHANGXIAN_SCAN_EXTRA") {
        for item in extra.split(';').filter(|s| !s.trim().is_empty()) {
            if let Some((name, path)) = item.split_once('=') {
                out.push((name.trim().to_string(), path.trim().to_string()));
            }
        }
    }
    out
}

pub fn scan() -> Vec<ImportSuggestion> {
    let mut out = Vec::new();
    for (name, path) in candidates() {
        if let Some(s) = scan_one(&name, Path::new(&path)) {
            out.push(s);
        }
    }
    out
}

fn scan_one(name: &str, path: &Path) -> Option<ImportSuggestion> {
    let text = std::fs::read_to_string(path).ok()?;

    // 端口/目标：兼容 PowerShell 默认值写法与 shell 的 ${VAR:-默认} 写法
    let remote_port = find_port(&text, &["PI_WEB_SSH_REMOTE_PORT", "REMOTE_PORT"])?;
    let local_port = find_port(&text, &["$env:PORT", "PORT", "LOCAL_PORT"])?;
    let target = find_target(&text, &["PI_WEB_SSH_TARGET", "SSH_TARGET"])?;
    let (user, host) = target.split_once('@')?;

    Some(ImportSuggestion {
        name: name.to_string(),
        source: path.display().to_string(),
        server_host: host.to_string(),
        server_user: user.to_string(),
        ssh_port: 22,
        local_port,
        remote_port,
        url: None,
    })
}

/// 在文本里找 `KEY` 后面的第一个合法端口默认值。
fn find_port(text: &str, keys: &[&str]) -> Option<u16> {
    for key in keys {
        for value in values_after(text, key) {
            if let Ok(port) = value.parse::<u16>() {
                if port > 0 {
                    return Some(port);
                }
            }
        }
    }
    None
}

fn find_target(text: &str, keys: &[&str]) -> Option<String> {
    for key in keys {
        for value in values_after(text, key) {
            // 形如 user@host，且主机部分要有域名点或 IP 点，排除注释里的示例值
            let Some((user, host)) = value.split_once('@') else {
                continue;
            };
            if user.is_empty() || host.is_empty() || !host.contains('.') {
                continue;
            }
            if value.chars().any(|c| c.is_whitespace() || c == '"' || c == '}') {
                continue;
            }
            return Some(value);
        }
    }
    None
}

/// 收集 KEY 后面所有出现位置能解析出的值（先收集、后过滤，避开注释里的干扰）。
/// 支持两种写法：
/// - PowerShell：`KEY)) { "值" }`
/// - shell：`KEY:-值}` 或 `KEY:值}`
fn values_after(text: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(idx) = text[from..].find(key) {
        let start = from + idx + key.len();
        let rest = &text[start..];
        // 只看同一行：避免把后面无关行里的冒号/花括号误当成默认值
        let rest = &rest[..rest.find('\n').unwrap_or(rest.len())];

        // PowerShell 写法：{ "值" }（花括号和引号之间可能有空格）
        if let Some(open) = rest.find('{') {
            let after = rest[open + 1..].trim_start();
            if let Some(tail) = after.strip_prefix('"') {
                if let Some(end) = tail.find('"') {
                    out.push(tail[..end].trim().to_string());
                }
            }
        }
        // shell 写法：:-值} 或 :值}
        if let Some(colon) = rest.find(':') {
            let tail = &rest[colon + 1..];
            let tail = tail.strip_prefix('-').unwrap_or(tail);
            if let Some(end) = tail.find('}') {
                let v = tail[..end].trim();
                if !v.is_empty() {
                    out.push(v.to_string());
                }
            }
        }

        from = start;
    }
    out
}

// ---------- 单测 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sx-import-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_config() -> AppConfig {
        serde_json::from_value(json!({
            "version": 6,
            "servers": [
                { "id": "srv-1", "name": "香港-1", "host": "a.example.com", "sshPort": 22,
                  "user": "root", "keyPath": "C:/k/srv-1_ed25519", "vhostDir": "/xp/panel/vhost" }
            ],
            "services": [
                { "id": "svc-1", "name": "相册", "localPort": 8188,
                  "launch": { "exe": "C:/py/python.exe", "args": "app.py", "workdir": "D:/album" },
                  "watch": true, "autostart": true, "note": null }
            ],
            "tunnels": [
                { "id": "tun-1", "name": "相册发布", "serviceId": "svc-1", "serverId": "srv-1",
                  "remotePort": 18080, "url": "album.example.com", "keyPath": "C:/k/tun_ed25519",
                  "access": ["password"], "accessUser": "u",
                  "accessHash": "$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0",
                  "accessDetail": null, "enabled": true, "preset": "ws" }
            ],
            "notifyEnabled": true,
            "settings": { "refreshIntervalMs": 800,
                          "quietHours": { "enabled": false, "start": "22:00", "end": "08:00" },
                          "checkUpdateOnStart": true }
        }))
        .unwrap()
    }

    fn no_hash_json() -> Value {
        json!({
            "kind": EXPORT_KIND,
            "formatVersion": 1,
            "exportedAt": 1_760_000_000_000i64,
            "appVersion": "0.6.0",
            "configVersion": 6,
            "redacted": true,
            "config": {
                "version": 6,
                "servers": [],
                "services": [],
                "tunnels": [],
                "notifyEnabled": true,
                "settings": { "refreshIntervalMs": 800,
                              "quietHours": { "enabled": false, "start": "22:00", "end": "08:00" },
                              "checkUpdateOnStart": true }
            }
        })
    }

    // ---------- 导出 ----------

    #[test]
    fn export_redacts_secrets_by_default() {
        let cfg = sample_config();
        let doc = export_config(&cfg, true);
        assert_eq!(doc["kind"], EXPORT_KIND);
        assert_eq!(doc["formatVersion"], 1);
        assert_eq!(doc["redacted"], true);
        assert_eq!(doc["configVersion"], 6);
        assert!(doc["exportedAt"].as_i64().unwrap() > 0);

        let text = doc.to_string();
        assert!(!text.contains("accessHash"), "哈希键不能出现（连 null 都不行）");
        assert!(!text.contains("keyPath\""), "keyPath 键不能出现");
        assert!(!text.contains("$2y$"), "哈希内容不能出现");
        assert_eq!(doc["config"]["servers"][0]["keyPathPresent"], true);
        assert_eq!(doc["config"]["tunnels"][0]["keyPathPresent"], true);
        assert_eq!(doc["config"]["tunnels"][0]["hasPassword"], true);
        assert_eq!(doc["config"]["tunnels"][0]["preset"], "ws");
        assert_eq!(doc["config"]["settings"]["refreshIntervalMs"], 800);
        assert_eq!(doc["config"]["notifyEnabled"], true);
    }

    #[test]
    fn export_with_paths_keeps_key_path_but_never_hash() {
        let cfg = sample_config();
        let doc = export_config(&cfg, false);
        assert_eq!(doc["redacted"], false);
        assert_eq!(doc["config"]["servers"][0]["keyPath"], "C:/k/srv-1_ed25519");
        assert_eq!(doc["config"]["tunnels"][0]["keyPath"], "C:/k/tun_ed25519");
        assert_eq!(doc["config"]["servers"][0]["keyPathPresent"], true);
        assert!(!doc.to_string().contains("accessHash"));
        assert!(!doc.to_string().contains("$2y$"));
    }

    // ---------- 导入：merge ----------

    #[test]
    fn merge_keeps_hash_and_untouched_fields() {
        let current = sample_config();
        // 脱敏导出 → 改名 + 关掉 → 再导入：哈希、密钥、没提到的字段都要留着
        let mut doc = export_config(&current, true);
        doc["config"]["tunnels"][0]["name"] = json!("相册发布（改名）");
        doc["config"]["tunnels"][0]["enabled"] = json!(false);
        doc["config"]["tunnels"][0]["url"] = json!("new.example.com");
        let doc = doc.to_string();

        let (cfg, summary) =
            plan_import(&current, &doc, ImportMode::Merge).unwrap();
        assert_eq!(summary.mode, "merge");
        assert!(summary.dry_run);
        assert_eq!(summary.updated.tunnels, 1);
        assert_eq!(summary.added.tunnels, 0);
        assert_eq!(summary.unchanged.tunnels, 0);
        assert_eq!(summary.unchanged.servers, 1);
        assert_eq!(summary.unchanged.services, 1);

        let tunnel = &cfg.tunnels[0];
        assert_eq!(tunnel.name, "相册发布（改名）");
        assert_eq!(tunnel.url.as_deref(), Some("new.example.com"));
        assert!(!tunnel.enabled);
        assert!(tunnel.access_hash.is_some(), "脱敏导入必须保留原哈希");
        assert_eq!(tunnel.key_path.as_deref(), Some("C:/k/tun_ed25519"));
        assert_eq!(tunnel.preset, "ws", "没提 preset 就保持现值");
        // 服务器密钥也保留
        assert_eq!(cfg.servers[0].key_path.as_deref(), Some("C:/k/srv-1_ed25519"));
        assert!(summary.warnings.is_empty(), "warnings={:?}", summary.warnings);
    }

    #[test]
    fn merge_explicit_null_clears_hash_and_key_path() {
        let current = sample_config();
        let doc = json!({
            "version": 6,
            "tunnels": [
                { "id": "tun-1", "accessHash": null, "keyPath": null }
            ]
        })
        .to_string();
        let (cfg, _summary) = plan_import(&current, &doc, ImportMode::Merge).unwrap();
        assert!(cfg.tunnels[0].access_hash.is_none(), "显式 null = 清空");
        assert!(cfg.tunnels[0].key_path.is_none());
    }

    #[test]
    fn merge_adds_new_ids_and_counts() {
        let current = sample_config();
        let doc = json!({
            "version": 6,
            "servers": [],
            "services": [
                { "id": "svc-2", "name": "看图", "localPort": 8787, "watch": false, "autostart": false }
            ],
            "tunnels": [
                { "id": "tun-2", "name": "看图发布", "serviceId": "svc-2", "serverId": "srv-1",
                  "remotePort": 18081, "access": [], "enabled": true, "preset": "web" }
            ]
        })
        .to_string();
        let (cfg, summary) = plan_import(&current, &doc, ImportMode::Merge).unwrap();
        assert_eq!(summary.added.services, 1);
        assert_eq!(summary.added.tunnels, 1);
        assert_eq!(summary.unchanged.servers, 0);
        assert_eq!(cfg.services.len(), 2);
        assert_eq!(cfg.tunnels.len(), 2);
        assert_eq!(cfg.tunnels[1].name, "看图发布");
    }

    #[test]
    fn merge_updates_settings_only_when_present() {
        let current = sample_config();
        let keep = plan_import(&current, r#"{"version":6}"#, ImportMode::Merge)
            .unwrap()
            .0;
        assert_eq!(keep.settings, current.settings);
        assert_eq!(keep.notify_enabled, current.notify_enabled);

        let change = plan_import(
            &current,
            r#"{"version":6,"settings":{"refreshIntervalMs":1000,"quietHours":{"enabled":true,"start":"23:00","end":"07:00"},"checkUpdateOnStart":false},"notifyEnabled":false}"#,
            ImportMode::Merge,
        )
        .unwrap()
        .0;
        assert_eq!(change.settings.refresh_interval_ms, 1000);
        assert!(change.settings.quiet_hours.enabled);
        assert!(!change.notify_enabled);
    }

    // ---------- 导入：replace ----------

    #[test]
    fn replace_backs_up_and_replaces_everything() {
        let dir = temp_dir("replace");
        let path = dir.join("config.json");
        let current = sample_config();
        current.save(&path).unwrap();

        let mut doc = export_config(&current, true);
        doc["config"]["tunnels"][0]["name"] = json!("换了一整套");
        doc["config"]["tunnels"][0]["accessHash"] = json!(null); // replace 里显式给 null 就是没有
        let doc = doc.to_string();

        let outcome = import_config(&path, &current, &doc, ImportMode::Replace, false).unwrap();
        assert!(!outcome.summary.dry_run);
        assert_eq!(outcome.summary.mode, "replace");
        assert_eq!(outcome.summary.updated.tunnels, 1);
        let backup = outcome.summary.backup_path.unwrap();
        assert!(Path::new(&backup).exists(), "replace 前必须备份");
        assert!(backup.contains("config.json.bak-"));

        // 磁盘上就是新配置
        let on_disk = AppConfig::load(&path).unwrap();
        assert_eq!(on_disk.tunnels[0].name, "换了一整套");

        // replace 时文件里没有的 settings 字段用默认值
        let (cfg, _summary) = plan_import(
            &current,
            r#"{"version":6,"servers":[],"services":[],"tunnels":[]}"#,
            ImportMode::Replace,
        )
        .unwrap();
        assert!(cfg.servers.is_empty() && cfg.tunnels.is_empty());
        assert_eq!(cfg.settings, Settings::default());
        assert!(cfg.notify_enabled, "没写 notifyEnabled → 默认开");
    }

    #[test]
    fn dry_run_never_writes() {
        let dir = temp_dir("dry");
        let path = dir.join("config.json");
        let current = sample_config();
        current.save(&path).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let doc = json!({"version": 6, "tunnels": [{"id": "tun-1", "name": "改了"}]}).to_string();
        let outcome = import_config(&path, &current, &doc, ImportMode::Merge, true).unwrap();
        assert!(outcome.summary.dry_run);
        assert!(outcome.summary.backup_path.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "dry-run 绝不写盘");
        // 目录里也不该多出备份
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries.len(), 1, "dry-run 不能留备份：{entries:?}");
    }

    // ---------- 校验：all-or-nothing ----------

    #[test]
    fn dangling_reference_rejects_and_writes_nothing() {
        let dir = temp_dir("dangling");
        let path = dir.join("config.json");
        let current = sample_config();
        current.save(&path).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let doc = json!({
            "version": 6,
            "services": [{ "id": "svc-9", "name": "孤儿", "localPort": 9000 }],
            "tunnels": [{ "id": "tun-9", "name": "孤儿隧道", "serviceId": "svc-不存在", "serverId": "srv-1", "remotePort": 19000, "access": [], "preset": "web" }]
        })
        .to_string();

        let err = import_config(&path, &current, &doc, ImportMode::Merge, false).unwrap_err();
        assert!(err.contains("本机服务"), "err={err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "校验不过不能写盘");
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(entries.len(), 1, "校验不过不能留备份：{entries:?}");
    }

    #[test]
    fn duplicate_ids_and_ports_are_rejected() {
        let current = sample_config();

        // 导入列表内 id 重复
        let doc = json!({
            "version": 6,
            "servers": [{ "id": "srv-1", "name": "A", "host": "a.com", "user": "u" },
                        { "id": "srv-1", "name": "B", "host": "b.com", "user": "u" }]
        })
        .to_string();
        assert!(plan_import(&current, &doc, ImportMode::Merge)
            .unwrap_err()
            .contains("同一个 id"));

        // 同一服务器上云端端口重复
        let doc = json!({
            "version": 6,
            "tunnels": [
                { "id": "tun-1", "name": "A", "serviceId": "svc-1", "serverId": "srv-1", "remotePort": 18080, "access": [], "preset": "web" },
                { "id": "tun-2", "name": "B", "serviceId": "svc-1", "serverId": "srv-1", "remotePort": 18080, "access": [], "preset": "web" }
            ]
        })
        .to_string();
        let err = plan_import(&current, &doc, ImportMode::Merge).unwrap_err();
        assert!(err.contains("云端端口 18080"), "err={err}");

        // 本机端口 0
        let doc = json!({
            "version": 6,
            "services": [{ "id": "svc-0", "name": "零端口", "localPort": 0 }]
        })
        .to_string();
        assert!(plan_import(&current, &doc, ImportMode::Merge)
            .unwrap_err()
            .contains("本机端口为 0"));

        // 本机端口重复
        let doc = json!({
            "version": 6,
            "services": [{ "id": "svc-2", "name": "撞端口", "localPort": 8188 }]
        })
        .to_string();
        assert!(plan_import(&current, &doc, ImportMode::Merge)
            .unwrap_err()
            .contains("本机端口 8188"));
    }

    // ---------- 版本检查 ----------

    #[test]
    fn future_version_rejected_with_fixed_copy() {
        let doc = no_hash_json().to_string().replace("\"configVersion\":6", "\"configVersion\":7");
        let err = plan_import(&sample_config(), &doc, ImportMode::Merge).unwrap_err();
        assert!(err.contains("版本 7"), "err={err}");
        assert!(err.contains("请先升级到最新版再打开"), "err={err}");
        assert!(err.contains("升级后会自动读回，不用重新配"), "err={err}");

        let ancient = json!({
            "kind": EXPORT_KIND, "formatVersion": 1, "configVersion": 3,
            "config": { "version": 3, "servers": [], "services": [], "tunnels": [] }
        })
        .to_string();
        let err = plan_import(&sample_config(), &ancient, ImportMode::Merge).unwrap_err();
        assert!(err.contains("版本过旧"), "err={err}");
        assert!(err.contains("版本 3"), "err={err}");
        assert!(
            err.contains("先用旧版「上线助手」打开并保存一次"),
            "err={err}"
        );

        let bad_format = json!({
            "kind": EXPORT_KIND, "formatVersion": 2, "configVersion": 6,
            "config": { "version": 6 }
        })
        .to_string();
        assert!(plan_import(&sample_config(), &bad_format, ImportMode::Merge)
            .unwrap_err()
            .contains("formatVersion"));

        assert!(ImportMode::parse("merge").is_ok());
        assert!(ImportMode::parse("replace").is_ok());
        assert!(ImportMode::parse("覆盖").is_err());
    }

    // ---------- 警告 ----------

    #[test]
    fn import_version_gate_only_accepts_5_and_6() {
        // v5 / v6 放行：v5 缺失的 preset 走迁移默认值
        assert!(plan_import(
            &sample_config(),
            r#"{"version":5,"servers":[],"services":[],"tunnels":[]}"#,
            ImportMode::Merge
        )
        .is_ok());
        assert!(plan_import(&sample_config(), r#"{"version":6}"#, ImportMode::Merge).is_ok());

        // v4 及更旧拦下：文案与 CLI 一字不差
        for old in [3u32, 4] {
            let doc = format!(r#"{{"version":{old}}}"#);
            let err = plan_import(&sample_config(), &doc, ImportMode::Merge).unwrap_err();
            assert_eq!(
                err,
                format!(
                    "导入失败：此设置版本过旧（版本 {old}）。请先用旧版「上线助手」打开并保存一次，再用新保存的文件导入。"
                ),
                "v{old} 的文案要和 CLI 一致"
            );
        }

        // 带包装的备份按 configVersion 拦（config.version 只作兜底）
        let wrapped_old = json!({
            "kind": EXPORT_KIND, "formatVersion": 1, "configVersion": 4,
            "config": { "version": 4, "servers": [], "services": [], "tunnels": [] }
        })
        .to_string();
        assert!(plan_import(&sample_config(), &wrapped_old, ImportMode::Merge)
            .unwrap_err()
            .contains("版本 4"));
    }

    #[test]
    fn warnings_for_missing_password_and_bad_preset() {
        let current = sample_config();
        // 新隧道设了密码但没有哈希；preset 非法；access 有不认识的值
        let doc = json!({
            "version": 6,
            "tunnels": [
                { "id": "tun-2", "name": "新站", "serviceId": "svc-1", "serverId": "srv-1",
                  "remotePort": 18082, "access": ["password", "bogus"], "preset": "wat" }
            ]
        })
        .to_string();
        let (cfg, summary) = plan_import(&current, &doc, ImportMode::Merge).unwrap();
        assert_eq!(cfg.tunnels[1].preset, "web", "非法 preset 归一成 web");
        assert_eq!(cfg.tunnels[1].access, vec!["password"], "未知 access 被忽略");
        let joined = summary.warnings.join("；");
        assert!(joined.contains("历史预设字段"), "warnings={joined}");
        assert!(joined.contains("访问保护写法"), "warnings={joined}");
        assert!(joined.contains("访问密码"), "warnings={joined}");
    }

    #[test]
    fn bare_config_without_wrapper_is_accepted() {
        let current = sample_config();
        let doc = json!({
            "version": 6,
            "tunnels": [{ "id": "tun-1", "name": "裸配置改名" }]
        })
        .to_string();
        let (cfg, summary) = plan_import(&current, &doc, ImportMode::Merge).unwrap();
        assert_eq!(cfg.tunnels[0].name, "裸配置改名");
        assert_eq!(summary.updated.tunnels, 1);
        // 裸 v5 也能走迁移（replace 时文件里没 preset → 默认 web）
        let doc5 = json!({
            "version": 5,
            "servers": [{ "id": "srv-1", "name": "香港", "host": "a.example.com", "sshPort": 22, "user": "root" }],
            "services": [{ "id": "svc-1", "name": "相册", "localPort": 8188 }],
            "tunnels": [{ "id": "tun-1", "name": "v5 裸配置", "serviceId": "svc-1", "serverId": "srv-1", "remotePort": 18080, "access": [], "enabled": true }]
        })
        .to_string();
        let (cfg5, _s) = plan_import(&current, &doc5, ImportMode::Replace).unwrap();
        assert_eq!(cfg5.version, CONFIG_VERSION);
        assert_eq!(cfg5.tunnels[0].preset, "web", "v5 缺 preset 走默认");
        assert_eq!(cfg5.tunnels[0].name, "v5 裸配置");

        // merge 时文件里没写的字段保持现值（现值 preset=ws，不能被默默改掉）
        let (cfg_merge, _s) = plan_import(&current, &doc5, ImportMode::Merge).unwrap();
        assert_eq!(cfg_merge.tunnels[0].preset, "ws");
    }

    #[test]
    fn summary_shape_is_camel_case() {
        let (_, summary) = plan_import(&sample_config(), r#"{"version":6}"#, ImportMode::Merge).unwrap();
        let value = serde_json::to_value(&summary).unwrap();
        for key in ["mode", "dryRun", "added", "updated", "unchanged", "warnings", "backupPath"] {
            assert!(value.get(key).is_some(), "摘要缺字段 {key}: {value}");
        }
        assert!(value["added"].get("servers").is_some());
    }
}
