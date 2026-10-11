//! 生成云端 nginx 的「新增配置」片段（面板格式，人工应用；对齐 DESIGN.md §3.3「nginx 下发」）。
//!
//! 只生成两样东西：
//! 1. 要加到用户现有站点配置里的那几行（复制按钮复制的就是它）；
//! 2. 一段简短示意（灰色＝原配置的样子，高亮＝新增内容该放哪里，省略处用 ... 占位）。
//! 不生成整段站点配置：每个人的配置都不一样，整段替换有覆盖别人原有内容的风险。
//! MVP 阶段只生成 + 展示 + 一键复制：不改服务器、不自动 reload。

use crate::config::{ServerConfig, Tunnel};

/// 示意配置里的一行。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetLine {
    pub text: String,
    /// true = 这行是新增内容（界面高亮）；false = 原配置的样子（界面灰显）
    pub is_new: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NginxSnippet {
    /// 要新增的几行（复制按钮复制的就是它）；没有访问保护时为 None
    pub snippet: Option<String>,
    /// 证书自动续期要加的一行（放进 location /.well-known/ 里）；没有密码保护时为 None
    pub acme_exempt: Option<String>,
    /// 简短示意：灰 = 原配置的样子，高亮 = 新增内容的位置
    pub preview: Vec<SnippetLine>,
    /// 密码文件内容（user:hash）；没有密码保护时为 None
    pub htpasswd: Option<String>,
    /// 使用说明（人话）
    pub steps: Vec<String>,
}

fn gray(text: impl Into<String>) -> SnippetLine {
    SnippetLine {
        text: text.into(),
        is_new: false,
    }
}

fn add(text: impl Into<String>) -> SnippetLine {
    SnippetLine {
        text: text.into(),
        is_new: true,
    }
}

pub fn generate(svc: &Tunnel, _local_port: u16, _server: &ServerConfig) -> NginxSnippet {
    let has_url = svc.url.is_some();
    let domain = svc.url.clone().unwrap_or_else(|| "你的域名".into());
    let want_password = svc.wants_password();
    let want_ip = svc.wants_ip();

    let mut preview: Vec<SnippetLine> = vec![
        gray("server {"),
        gray("    ..."),
        gray(format!("    server_name {domain};")),
    ];

    // 两种保护可以同时要：先出密码块，再出 IP 白名单块（nginx 默认 satisfy all，两块会叠加生效）。
    let mut blocks: Vec<String> = Vec::new();
    let mut acme_exempt: Option<String> = None;
    let mut htpasswd = None;
    let file = format!("/xp/panel/vhost/{domain}.htpasswd");

    if want_password {
        let user = svc
            .access_user
            .clone()
            .unwrap_or_else(|| "shangxian".into());
        blocks.push(format!(
            "    # 访问密码（由「上线助手」新增）\n    auth_basic \"需要密码\";\n    auth_basic_user_file {file};"
        ));
        acme_exempt = Some("auth_basic off;".into());
        if let Some(hash) = &svc.access_hash {
            htpasswd = Some(format!("{user}:{hash}"));
        }
        preview.push(gray(""));
        preview.push(add("    # ① 新增：访问密码（放在 server_name 下面）"));
        preview.push(add("    auth_basic \"需要密码\";"));
        preview.push(add(format!("    auth_basic_user_file {file};")));
    }

    if want_ip {
        let mut block = String::new();
        if want_password {
            preview.push(gray(""));
            preview.push(add("    # ② 新增：只允许白名单里的 IP（放在 server_name 下面）"));
        } else {
            preview.push(add("    # 新增：只允许白名单里的 IP（放在 server_name 下面）"));
        }
        if let Some(list) = &svc.access_detail {
            for ip in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                block.push_str(&format!("    allow {ip};\n"));
                preview.push(add(format!("    allow {ip};")));
            }
        }
        block.push_str("    deny all;");
        preview.push(add("    deny all;"));
        blocks.push(block);
    }

    if !want_password && !want_ip {
        preview.push(add(
            "    # ⚠ 没有访问保护：任何人知道网址都能访问（建议回应用里设一个访问密码）",
        ));
    }

    // 证书续期目录只给密码保护开口子；IP 白名单时那里本来就有面板自带的 allow all;，不动它。
    if want_password {
        preview.push(gray("    ..."));
        preview.push(gray("    # SSL证书验证目录"));
        preview.push(gray("    location /.well-known/ {"));
        preview.push(add(if want_ip {
            "        # ③ 新增：证书续期用（放进这个目录里）"
        } else {
            "        # ② 新增：证书续期用（放进这个目录里）"
        }));
        preview.push(add("        auth_basic off;"));
        preview.push(gray("        allow all;"));
        preview.push(gray("    }"));
    }

    let snippet = if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n"))
    };

    preview.push(gray("    ..."));
    preview.push(gray("}"));

    // ---------- 使用说明（人话） ----------
    let mut steps: Vec<String> = vec![
        "先在面板里把这个域名的站点建好、证书开好（面板里已经能打开就跳过）。".into(),
        format!(
            "转发在面板的「反向代理」里配：目标填 http://127.0.0.1:{}（配过就跳过）。",
            svc.remote_port
        ),
        "把新增的几行加进面板的「站点 → 配置修改」里，位置照上面的示意图（不要整段替换）。".into(),
        "保存后重载 nginx，然后从外网打开一次网址确认。".into(),
    ];
    if !has_url {
        steps.insert(
            0,
            "⚠ 这条隧道还没填公网网址：先回应用里把网址填上，再回来重新生成（现在示意里的域名是占位符）。".into(),
        );
    }

    match (want_password, want_ip) {
        (true, true) => {
            steps.push(
                "示意图里最后那处高亮（auth_basic off;）别漏：放进证书验证目录，不然证书到期自动续不了。"
                    .into(),
            );
        }
        (true, false) => {
            steps.push("示意图里第二处高亮（auth_basic off;）别漏：放进证书验证目录，不然证书到期自动续不了。".into());
        }
        _ => {}
    }
    if want_password {
        if htpasswd.is_some() {
            steps.push(format!(
                "把下面这行密码保存到服务器上的 {file}（一行就够；没有这个文件就新建一个）。"
            ));
        } else {
            steps.push("这个服务还没有密码哈希：回应用里重新设置一次访问密码，再回来生成。".into());
        }
    }
    if want_password && want_ip {
        steps.push(
            "两道门都开着：只有白名单里的 IP 能打开，打开后还要输密码。换网络或换手机时记得回应用里加 IP。"
                .into(),
        );
    } else if want_ip {
        steps.push("只有白名单里的 IP 能访问；换网络或换手机时记得回应用里加 IP。".into());
    } else if !want_password {
        steps.push("⚠ 这个服务目前没有访问保护，建议在应用里给它设一个访问密码。".into());
    }

    NginxSnippet {
        snippet,
        acme_exempt,
        preview,
        htpasswd,
        steps,
    }
}
