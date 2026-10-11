//! 生成云端 nginx 配置片段（面板格式，人工应用；对齐 DESIGN.md §3.3「nginx 下发」）。
//!
//! MVP 阶段只生成 + 展示 + 一键复制：不改服务器、不自动 reload，
//! 由用户粘进小皮面板的站点配置里（和现有工作流一致）。

use crate::config::{ServerConfig, Tunnel};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NginxSnippet {
    /// 可直接粘贴进面板站点配置的 server 片段
    pub snippet: String,
    /// 密码文件内容（user:hash）；没有密码保护时为 None
    pub htpasswd: Option<String>,
    /// 使用说明（人话）
    pub steps: Vec<String>,
}

pub fn generate(svc: &Tunnel, local_port: u16, _server: &ServerConfig) -> NginxSnippet {
    let domain = svc
        .url
        .clone()
        .unwrap_or_else(|| format!("你的域名（服务「{}」还没填网址）", svc.name));

    let mut steps: Vec<String> = vec![
        "把下面的片段放进这台服务器上该域名的站点配置里（小皮面板 → 对应站点 → 配置修改）。".into(),
        "保存后重载 nginx，然后从外网打开一次网址确认。".into(),
    ];
    let mut htpasswd = None;

    let access = match svc.access.as_deref() {
        Some("password") => {
            let user = svc
                .access_user
                .clone()
                .unwrap_or_else(|| "shangxian".into());
            let file = format!(
                "/xp/panel/vhost/{}.htpasswd",
                svc.url.clone().unwrap_or_else(|| "站点名".into())
            );
            if let Some(hash) = &svc.access_hash {
                htpasswd = Some(format!("{user}:{hash}"));
                steps.push(format!(
                    "把下面这行密码保存到服务器上的 {file}（一行就够；没有这个文件就新建一个）。"
                ));
            } else {
                steps.push("这个服务还没有密码哈希：回应用里重新设置一次访问密码即可生成。".into());
            }
            format!(
                "        auth_basic \"需要密码\";\n        auth_basic_user_file {file};\n"
            )
        }
        Some("ip") => {
            let mut block = String::new();
            if let Some(list) = &svc.access_detail {
                for ip in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                    block.push_str(&format!("        allow {ip};\n"));
                }
            }
            block.push_str("        deny all;\n");
            steps.push("只有白名单里的 IP 能访问；换网络或换手机时记得回应用里加 IP。".into());
            block
        }
        _ => {
            steps.push("⚠ 这个服务目前没有访问保护，建议在应用里给它设一个访问密码。".into());
            "        # ⚠ 没有访问保护：任何人知道网址就能访问\n".into()
        }
    };

    let mut s = String::new();
    s.push_str(&format!("# 由「上线助手」生成 · 服务：{}\n", svc.name));
    s.push_str(&format!(
        "# 目标：本机 {} → 云端回环 {}\n",
        local_port, svc.remote_port
    ));
    s.push_str("server {\n");
    s.push_str("    listen 443 ssl;\n");
    s.push_str(&format!("    server_name {};\n", domain));
    s.push_str("    # 证书沿用面板里这个域名的配置（面板保存后会自动带上）\n\n");
    s.push_str("    location / {\n");
    s.push_str(&format!(
        "        proxy_pass http://127.0.0.1:{};\n",
        svc.remote_port
    ));
    s.push_str("        proxy_http_version 1.1;\n");
    s.push_str("        proxy_set_header Host $host;\n");
    s.push_str("        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n");
    s.push_str("        proxy_set_header X-Forwarded-Proto $scheme;\n");
    s.push_str("        proxy_set_header Upgrade $http_upgrade;\n");
    s.push_str("        proxy_set_header Connection \"upgrade\";\n");
    s.push_str("        client_max_body_size 100m;\n");
    s.push_str(&access);
    s.push_str("    }\n");
    s.push_str("}\n");

    NginxSnippet {
        snippet: s,
        htpasswd,
        steps,
    }
}
