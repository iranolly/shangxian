//! 生成云端 nginx 的「新增配置」片段（面板格式，人工应用；对齐 DESIGN.md §3.3「nginx 下发」）。
//!
//! 只生成两样东西：
//! 1. 要加到用户现有站点配置里的那几行（复制按钮复制的就是它）；
//! 2. 一段简短示意（灰色＝原配置的样子，高亮＝新增内容该放哪里，省略处用 ... 占位）。
//! 不生成整段站点配置：每个人的配置都不一样，整段替换有覆盖别人原有内容的风险。
//! MVP 阶段只生成 + 展示 + 一键复制：不改服务器、不自动 reload。
//!
//! 所有外部输入（域名、用户名、哈希、白名单）都先校验再拼接：
//! 域名只收字母/数字/点/横杠；用户名不收冒号/空白；白名单只收合法 IP 或 CIDR
//! （`all`、`0.0.0.0/0` 都不算保护，另有提醒）。校验不过就不往片段里写，并把原因写进步骤。

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

/// 域名只允许字母、数字、点、横杠（不允许斜杠、冒号、空格、分号等）。
fn valid_domain(s: &str) -> bool {
    !s.is_empty()
        && s.chars().any(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// 用户名拦掉冒号、空白和换行：htpasswd 是「user:hash」格式，冒号会被当成新用户。
fn valid_user(s: &str) -> bool {
    !s.is_empty() && !s.contains(':') && !s.chars().any(char::is_whitespace)
}

/// 密码哈希得是 $ 开头的正常哈希，而且不能带空白/换行（防往 htpasswd 里塞行）。
fn valid_hash(s: &str) -> bool {
    s.starts_with('$') && !s.chars().any(char::is_whitespace)
}

/// 白名单的一条：单独的 IP 或 CIDR 才算数。
/// `all`、`0.0.0.0/0`、`::/0`、空值都不算（它们等于没设防）。
pub(crate) fn valid_allow_entry(s: &str) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};

    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("all") {
        return false;
    }
    if let Some((addr, prefix)) = s.split_once('/') {
        let Ok(prefix) = prefix.parse::<u32>() else {
            return false;
        };
        if addr.parse::<Ipv4Addr>().is_ok() {
            return (1..=32).contains(&prefix);
        }
        if addr.parse::<Ipv6Addr>().is_ok() {
            return (1..=128).contains(&prefix);
        }
        return false;
    }
    s.parse::<Ipv4Addr>().is_ok() || s.parse::<Ipv6Addr>().is_ok()
}

/// 切白名单：英文逗号、中文逗号、空格、换行都算分隔符，空项丢掉。
fn split_allow_list(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == '，' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 把网址洗成域名：去掉 http(s)://、端口和后面的路径。
fn normalize_domain(raw: &str) -> String {
    let mut d = raw.trim();
    for prefix in ["https://", "http://"] {
        if d.len() >= prefix.len()
            && d.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            d = &d[prefix.len()..];
            break;
        }
    }
    let end = d
        .find(|c: char| c == '/' || c == ':' || c.is_whitespace())
        .unwrap_or(d.len());
    d[..end].trim().to_string()
}

/// 密码文件（站点目录）解析后的目录：服务器配置里的 `vhostDir` → 环境变量
/// `SHANGXIAN_HTPASSWD_DIR` → 小皮面板默认 `/xp/panel/vhost`。
/// nginx 配置在它下面的 `nginx/` 里，htpasswd 直接放在它下面。
pub(crate) fn htpasswd_dir_for(server: &ServerConfig) -> String {
    let from_env = std::env::var("SHANGXIAN_HTPASSWD_DIR").unwrap_or_default();
    resolve_htpasswd_dir(server.vhost_dir.as_deref(), Some(from_env.as_str()))
}

/// [`htpasswd_dir_for`] 的纯函数核心：不读环境变量，方便单测，也方便别处复用。
/// 空串、全空白、只剩斜杠的目录都算「没填」。
fn resolve_htpasswd_dir(from_server: Option<&str>, from_env: Option<&str>) -> String {
    fn clean(raw: &str) -> Option<String> {
        let trimmed = raw.trim().trim_end_matches(|c| c == '/' || c == '\\');
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
    from_server
        .and_then(clean)
        .or_else(|| from_env.and_then(clean))
        .unwrap_or_else(|| "/xp/panel/vhost".to_string())
}

pub fn generate(svc: &Tunnel, _local_port: u16, server: &ServerConfig) -> NginxSnippet {
    // 先把外部输入过一遍校验，问题记进 warnings，最后拼成「⚠ …」步骤给人看。
    let mut warnings: Vec<String> = Vec::new();

    let raw_url = svc
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (domain, url_missing) = match raw_url {
        None => ("你的域名".to_string(), true),
        Some(raw) => {
            let clean = normalize_domain(raw);
            if valid_domain(&clean) {
                (clean, false)
            } else {
                warnings.push(format!(
                    "公网网址「{raw}」不能直接当域名用：只允许字母、数字、点、横杠（不要带 http://、端口、路径）。这里先用「你的域名」占位，请回应用里改成纯域名再重新生成。"
                ));
                ("你的域名".to_string(), false)
            }
        }
    };
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
    let dir = htpasswd_dir_for(server);
    let file = format!("{dir}/{domain}.htpasswd");

    if want_password {
        let user = svc
            .access_user
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "shangxian".into());
        if !valid_user(&user) {
            // 用户名有冒号/空白/换行：不能冒险拼 htpasswd，也不生成密码块。
            warnings.push(format!(
                "访问用户名「{user}」里有冒号、空白或换行，不能用来做密码保护（会有注入风险）。这块没有生成，请回应用里改成普通用户名。"
            ));
        } else {
            blocks.push(format!(
                "    # 访问密码（由「上线助手」新增）\n    auth_basic \"需要密码\";\n    auth_basic_user_file {file};"
            ));
            acme_exempt = Some("auth_basic off;".into());
            match svc.access_hash.as_deref() {
                Some(hash) if valid_hash(hash) => htpasswd = Some(format!("{user}:{hash}")),
                Some(_) => warnings.push(
                    "密码哈希里有空白或不是正常哈希，htpasswd 内容没有生成；请回应用里重新设置一次访问密码。"
                        .into(),
                ),
                None => {}
            }
            preview.push(gray(""));
            preview.push(add("    # ① 新增：访问密码（放在 server_name 下面）"));
            preview.push(add("    auth_basic \"需要密码\";"));
            preview.push(add(format!("    auth_basic_user_file {file};")));
        }
    }

    if want_ip {
        let mut block = String::new();
        if want_password {
            preview.push(gray(""));
            preview.push(add("    # ② 新增：只允许白名单里的 IP（放在 server_name 下面）"));
        } else {
            preview.push(add("    # 新增：只允许白名单里的 IP（放在 server_name 下面）"));
        }
        // 白名单逐条校验：非法写法跳过并提醒；一条有效的都没有就不生成（免得只剩 deny all 把所有人挡在外面）。
        let raw_list = svc.access_detail.as_deref().unwrap_or("");
        let mut valid: Vec<String> = Vec::new();
        let mut bad: Vec<String> = Vec::new();
        for entry in split_allow_list(raw_list) {
            if !valid_allow_entry(&entry) {
                bad.push(entry);
            } else if !valid.contains(&entry) {
                valid.push(entry);
            }
        }
        if !bad.is_empty() {
            warnings.push(format!(
                "IP 白名单里有不认识的写法：{}。只认单个 IP 或 CIDR（例如 1.2.3.4、10.0.0.0/8）；all、0.0.0.0/0 等于没设防，已经跳过。",
                bad.join("、")
            ));
        }
        if valid.is_empty() {
            warnings.push(
                "IP 白名单是空的（或者全是无效写法），IP 限制没有生成；也不会只放一行 deny all; ——那会把所有人都挡在外面。请回应用里填上 IP 再重新生成。"
                    .into(),
            );
        } else {
            for ip in &valid {
                block.push_str(&format!("    allow {ip};\n"));
                preview.push(add(format!("    allow {ip};")));
            }
            block.push_str("    deny all;");
            preview.push(add("    deny all;"));
            blocks.push(block);
        }
    }

    if !want_password && !want_ip {
        preview.push(add(
            "    # ⚠ 没有访问保护：任何人知道网址都能访问（建议回应用里设一个访问密码）",
        ));
    }

    // 证书续期目录只给密码保护开口子；IP 白名单时那里本来就有面板自带的 allow all;，不动它。
    if acme_exempt.is_some() {
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

    // ---------- 使用说明（人话，只留 4 条短句 + 按需追加） ----------
    let mut steps: Vec<String> = vec![
        format!(
            "站点和证书先在面板建好；「反向代理」目标填 http://127.0.0.1:{}。（配过就跳过）",
            svc.remote_port
        ),
        "把下面高亮的行加进「站点 → 配置修改」：只加这几行，不替换整段；保存后重载 nginx，再从外网打开确认。".into(),
    ];
    if want_password {
        if htpasswd.is_some() {
            steps.push(format!("密码文件内容存到服务器上的 {file}（没有就新建）。"));
        } else {
            steps.push("这个服务还没有可用的密码哈希：回应用里重新设置一次访问密码，再回来生成。".into());
        }
        steps.push(format!(
            "密码文件要存到 {dir}/ 下面。面板不是小皮的话，回应用里把服务器的「站点目录」改成你面板的目录（比如宝塔填 /www/server/panel/vhost），再回来重新生成；临时也可以先设环境变量 SHANGXIAN_HTPASSWD_DIR 再重新生成。"
        ));
        steps.push("证书续期那两行（auth_basic off;）别漏——漏了证书到期不能自动续。".into());
    }
    match (want_password, want_ip) {
        (true, true) => steps.push(
            "白名单和密码两道门都要过；换网络或换手机时记得回应用里加 IP。".into(),
        ),
        (false, true) => {
            steps.push("只有白名单里的 IP 能访问；换网络或换手机时记得回应用里加 IP。".into())
        }
        (false, false) => {
            steps.push("⚠ 这个服务目前没有访问保护，建议在应用里给它设一个访问密码。".into())
        }
        (true, false) => {}
    }
    if url_missing {
        steps.insert(
            0,
            "⚠ 这条隧道还没填公网网址：先回应用里把网址填上再回来重新生成（现在示意里的域名是占位符）。".into(),
        );
    }

    // 校验发现的问题放最前面，界面上最先看到。
    if !warnings.is_empty() {
        let mut all: Vec<String> = warnings
            .into_iter()
            .map(|w| format!("⚠ {w}"))
            .collect();
        all.extend(steps);
        steps = all;
    }

    NginxSnippet {
        snippet,
        acme_exempt,
        preview,
        htpasswd,
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_entries_are_validated() {
        assert!(valid_allow_entry("1.2.3.4"));
        assert!(valid_allow_entry("10.0.0.0/8"));
        assert!(valid_allow_entry("2001:db8::1"));
        assert!(valid_allow_entry("2001:db8::/32"));
        assert!(!valid_allow_entry("all"));
        assert!(!valid_allow_entry("ALL"));
        assert!(!valid_allow_entry("0.0.0.0/0"));
        assert!(!valid_allow_entry("::/0"));
        assert!(!valid_allow_entry("1.2.3.4/0"));
        assert!(!valid_allow_entry("1.2.3.999"));
        assert!(!valid_allow_entry("1.2.3.4/33"));
        assert!(!valid_allow_entry(""));
        assert!(!valid_allow_entry("1.2.3.4; deny all;"));
    }

    #[test]
    fn split_allow_list_handles_all_separators() {
        assert_eq!(
            split_allow_list("1.2.3.4, 5.6.7.8\n9.9.9.9，10.0.0.1"),
            vec!["1.2.3.4", "5.6.7.8", "9.9.9.9", "10.0.0.1"]
        );
    }

    #[test]
    fn domain_and_user_are_checked() {
        assert!(valid_domain("album.example.com"));
        assert!(valid_domain("a-b.example.com"));
        assert!(!valid_domain("example.com; }"));
        assert!(!valid_domain("example.com/x"));
        assert!(!valid_domain(""));
        assert!(valid_user("shangxian"));
        assert!(!valid_user("a:b"));
        assert!(!valid_user("a b"));
        assert!(!valid_user("a\nb"));
    }

    #[test]
    fn normalize_strips_scheme_port_and_path() {
        assert_eq!(normalize_domain("https://album.example.com/x"), "album.example.com");
        assert_eq!(normalize_domain("HTTP://album.example.com:8443"), "album.example.com");
        assert_eq!(normalize_domain(" album.example.com "), "album.example.com");
    }

    fn tunnel_with(
        access: Vec<&str>,
        user: Option<&str>,
        hash: Option<&str>,
        detail: Option<&str>,
    ) -> Tunnel {
        Tunnel {
            id: "x".into(),
            name: "测试".into(),
            service_id: "svc".into(),
            server_id: "srv".into(),
            remote_port: 18080,
            url: Some("example.com; }".into()),
            key_path: None,
            access: access.into_iter().map(str::to_string).collect(),
            access_user: user.map(str::to_string),
            access_hash: hash.map(str::to_string),
            access_detail: detail.map(str::to_string),
            enabled: true,
        }
    }

    fn fake_server() -> ServerConfig {
        ServerConfig {
            id: "srv".into(),
            name: "srv".into(),
            host: "h".into(),
            ssh_port: 22,
            user: "u".into(),
            key_path: None,
            vhost_dir: None,
        }
    }

    #[test]
    fn htpasswd_dir_precedence() {
        // 服务器配置里的 vhostDir 最优先，其次环境变量，最后小皮默认
        assert_eq!(
            resolve_htpasswd_dir(Some("/www/server/panel/vhost"), Some("/env/dir")),
            "/www/server/panel/vhost"
        );
        assert_eq!(resolve_htpasswd_dir(None, Some("/env/dir")), "/env/dir");
        assert_eq!(
            resolve_htpasswd_dir(Some("  "), Some("")),
            "/xp/panel/vhost"
        );
        // 末尾的斜杠要去掉，免得拼出 /data/vhost//nginx
        assert_eq!(
            resolve_htpasswd_dir(Some("/data/vhost/"), None),
            "/data/vhost"
        );
        assert_eq!(resolve_htpasswd_dir(None, None), "/xp/panel/vhost");
    }

    #[test]
    fn server_vhost_dir_lands_in_snippet() {
        // 服务器上填了「站点目录」，生成片段和密码文件说明都得跟着走
        let mut server = fake_server();
        server.vhost_dir = Some("/www/server/panel/vhost".into());
        let mut tun = tunnel_with(
            vec!["password"],
            Some("shangxian"),
            Some("$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0"),
            None,
        );
        tun.url = Some("album.example.com".into());
        let snip = generate(&tun, 8080, &server);
        let add = snip.snippet.as_deref().unwrap();
        assert!(
            add.contains(
                "auth_basic_user_file /www/server/panel/vhost/album.example.com.htpasswd;"
            ),
            "add={add}"
        );
        assert!(
            snip.steps.iter().any(|s| s.contains("/www/server/panel/vhost")),
            "steps={:?}",
            snip.steps
        );
    }

    #[test]
    fn invalid_inputs_do_not_reach_the_snippet() {
        let tun = tunnel_with(
            vec!["password", "ip"],
            Some("bad:user"),
            Some("$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0"),
            Some("1.2.3.4; all"),
        );
        let snip = generate(&tun, 8080, &fake_server());
        let text = snip
            .preview
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("bad:user"), "非法用户名不能进片段");
        assert!(!text.contains("1.2.3.4;"), "非法白名单不能进片段");
        assert!(snip.htpasswd.is_none(), "用户名非法就不生成 htpasswd");
        assert!(snip.snippet.is_none(), "没有任何保护块应该可以生成");
        assert!(
            snip.steps.iter().any(|s| s.contains("注入")),
            "要有提醒：{:?}",
            snip.steps
        );
    }

    #[test]
    fn valid_whitelist_keeps_good_entries_and_drops_all() {
        let tun = tunnel_with(
            vec!["ip"],
            None,
            None,
            Some("1.2.3.4, 10.0.0.0/8, all, 1.2.3.4"),
        );
        let snip = generate(&tun, 8080, &fake_server());
        let add = snip.snippet.as_deref().unwrap();
        assert!(add.contains("allow 1.2.3.4;"));
        assert!(add.contains("allow 10.0.0.0/8;"));
        assert!(add.contains("deny all;"));
        assert!(!add.contains("allow all;"));
        assert_eq!(add.matches("allow 1.2.3.4;").count(), 1, "重复项要去重：{add}");
        assert!(
            snip.steps.iter().any(|s| s.contains("all")),
            "要提醒 all 被跳过：{:?}",
            snip.steps
        );
    }

    #[test]
    fn empty_whitelist_does_not_emit_deny_all_alone() {
        let tun = tunnel_with(vec!["ip"], None, None, Some("all"));
        let snip = generate(&tun, 8080, &fake_server());
        assert!(snip.snippet.is_none(), "只有 deny all 会把所有人挡住，不能生成");
        assert!(snip.steps.iter().any(|s| s.contains("挡在外面")));
    }
}
