//! 导入接管：扫描现有启动脚本，生成建议配置（用户确认后才导入）。

use std::path::Path;

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
