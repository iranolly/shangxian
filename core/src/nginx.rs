//! 生成云端 nginx 的「新增配置」片段（面板格式，人工应用；对齐 DESIGN.md §3.3「nginx 下发」）。
//!
//! 只生成两样东西：
//! 1. 要加到用户现有站点配置里的那几行（复制按钮复制的就是它）；
//! 2. 一段简短示意（灰色＝原配置的样子，高亮＝新增内容该放哪里，省略处用 ... 占位）。
//! 不生成整段站点配置：每个人的配置都不一样，整段替换有覆盖别人原有内容的风险。
//! 只生成 + 展示 + 一键复制：不改服务器、不自动 reload。
//!
//! 所有外部输入（域名、用户名、哈希、白名单）都先校验再拼接：
//! 域名只收字母/数字/点/横杠；用户名不收冒号/空白；白名单只收合法 IP 或 CIDR
//! （`all`、`0.0.0.0/0` 都不算保护，另有提醒）。校验不过就不往片段里写，并把原因写进步骤。
//!
//! 按 D0 仲裁：
//! - `noindex` **不默认生成**，只在 `optional`（界面「可选附加行」）里给一行提示；
//! - `acmeExempt` 字段名不变，内容升级为多行：有密码给 `auth_basic off;`，有白名单给
//!   `allow all;`（白名单模式也必须生成），并带说明注释；
//! - `Tunnel.preset` 是历史字段（新版本界面/CLI 不再设置）：`ws`/`upload` 决定哪组行保持激活，
//!   其余情况两组都以「按需启用」注释菜单出现，只放进已有的 `location / { }`，
//!   WebSocket 的 `Connection` 行按环境识别到的 `WS_MAP` 优先用 `$connection_upgrade`。

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
    /// 要新增的几行（复制按钮复制的就是它）；没有内容时为 None
    pub snippet: Option<String>,
    /// 证书自动续期要加的行（放进 location /.well-known/ 里）；没有保护时为 None。
    /// 多行：注释 + `auth_basic off;` / `allow all;`，没有该 location 时还会附整块提示。
    pub acme_exempt: Option<String>,
    /// 简短示意：灰 = 原配置的样子，高亮 = 新增内容的位置
    pub preview: Vec<SnippetLine>,
    /// 密码文件内容（user:hash）；没有密码保护时为 None
    pub htpasswd: Option<String>,
    /// 使用说明（人话）
    pub steps: Vec<String>,
    /// 可选附加行（默认不生成、由用户自己决定加不加），如不被搜索引擎收录的 noindex 行。
    #[serde(default)]
    pub optional: Vec<String>,
}

/// 生成片段时的可选项（都有默认；调用方拿到环境识别/上次检查结果后再传进来）。
#[derive(Debug, Clone, Default)]
pub struct GenOptions<'a> {
    /// 环境识别到的 `WS_MAP`：全局配置里有没有 `connection_upgrade` 的 map。
    /// `Some(true)` = 有（优先用 `$connection_upgrade`）；`Some(false)`/`None` = 用 `"upgrade"` 并加注释。
    pub ws_map: Option<bool>,
    /// 上次完整检查：站点配置里有没有 `location /.well-known/`。
    /// `Some(false)` 时续期片段里附上「整块怎么加」。
    pub wellknown_present: Option<bool>,
    /// 环境识别缓存的站点目录（D0 第 12 条解析链：服务器填写 → 识别结果 → 环境变量 → 默认）。
    pub detected_vhost_dir: Option<&'a str>,
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
pub(crate) fn valid_domain(s: &str) -> bool {
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
pub(crate) fn normalize_domain(raw: &str) -> String {
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

/// 密码文件（站点目录）解析后的目录，按 D0 第 12 条：
/// `server.vhostDir` → 环境识别结果（state 缓存）→ 环境变量
/// `SHANGXIAN_HTPASSWD_DIR` → 小皮面板默认 `/xp/panel/vhost`。
/// nginx 配置在它下面的 `nginx/` 里，htpasswd 直接放在它下面。
pub(crate) fn htpasswd_dir_for(server: &ServerConfig) -> String {
    htpasswd_dir_with(server, None)
}

/// [`htpasswd_dir_for`] 的带「环境识别结果」版本。
pub(crate) fn htpasswd_dir_with(server: &ServerConfig, detected: Option<&str>) -> String {
    let from_env = std::env::var("SHANGXIAN_HTPASSWD_DIR").unwrap_or_default();
    resolve_htpasswd_dir(server.vhost_dir.as_deref(), detected, Some(from_env.as_str()))
}

/// [`htpasswd_dir_for`] 的纯函数核心：不读环境变量，方便单测，也方便别处复用。
/// 空串、全空白、只剩斜杠的目录都算「没填」。
fn resolve_htpasswd_dir(
    from_server: Option<&str>,
    from_detected: Option<&str>,
    from_env: Option<&str>,
) -> String {
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
        .or_else(|| from_detected.and_then(clean))
        .or_else(|| from_env.and_then(clean))
        .unwrap_or_else(|| "/xp/panel/vhost".to_string())
}

/// 「按需启用」菜单的公共头（这几行放进已有的 `location / { }` 里）。
const PRESET_HEAD: &str =
    "# ↓↓↓ 下面几行放进你已有的 location / { } 里（不要贴在 server 里）↓↓↓";
/// 菜单用法说明。
const PRESET_MENU_HINT: &str = "# 需要哪一组就把那组行开头的 # 去掉；两组都用不上就整段别贴。";
/// WebSocket 组前的说明（只在该组是注释菜单时出现）。
const WS_MENU_HINT: &str =
    "# 网站有实时进度、长连接（比如 ComfyUI、AI 聊天）→ 去掉这组行开头的 #";
/// 大文件上传组前的说明（只在该组是注释菜单时出现）。
const UPLOAD_MENU_HINT: &str = "# 需要传大文件（最大 2GB）→ 去掉这组行开头的 #";

/// 把 `Tunnel.preset` 归一成三种之一；非法值按 `web` 处理（D2 契约）。
fn normalize_preset(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "ws" => "ws",
        "upload" => "upload",
        _ => "web",
    }
}

/// WebSocket 组（长连接）的指令行。
fn ws_group(ws_map: Option<bool>) -> Vec<String> {
    let mut lines = vec![
        "proxy_http_version 1.1;".to_string(),
        "proxy_set_header Upgrade $http_upgrade;".to_string(),
    ];
    if ws_map == Some(true) {
        lines.push("proxy_set_header Connection $connection_upgrade;".to_string());
    } else {
        lines.push("proxy_set_header Connection \"upgrade\";".to_string());
        lines.push(
            "# 如果服务器全局配置里已有 connection_upgrade 的 map，把这行换成 $connection_upgrade 更稳定（宝塔默认有）。"
                .to_string(),
        );
    }
    lines.push("proxy_read_timeout 3600s;".to_string());
    lines.push("proxy_send_timeout 3600s;".to_string());
    lines
}

/// 大文件上传组的指令行。
fn upload_group() -> Vec<String> {
    vec![
        "client_max_body_size 2048m;".to_string(),
        "proxy_request_buffering off;".to_string(),
        "proxy_read_timeout 600s;".to_string(),
        "proxy_send_timeout 600s;".to_string(),
    ]
}

/// 把一组指令行变成注释菜单形态：指令行前面加 `# `，原本就是注释的行原样保留。
fn as_menu(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            if line.starts_with('#') {
                line.clone()
            } else {
                format!("# {line}")
            }
        })
        .collect()
}

/// 生成「按需启用」注释菜单（放进已有的 `location / { }` 里）：
/// 历史 preset 对应的那组行保持激活（老隧道行为不变），另一组以注释菜单出现；
/// 其余情况两组都注释。两组之间留一个空行。
fn preset_lines(preset: &str, ws_map: Option<bool>) -> Vec<String> {
    let ws = ws_group(ws_map);
    let upload = upload_group();
    let mut lines = vec![PRESET_HEAD.to_string(), PRESET_MENU_HINT.to_string()];
    if preset == "ws" {
        lines.extend(ws.iter().cloned());
    } else {
        lines.push(WS_MENU_HINT.to_string());
        lines.extend(as_menu(&ws));
    }
    lines.push(String::new());
    if preset == "upload" {
        lines.extend(upload.iter().cloned());
    } else {
        lines.push(UPLOAD_MENU_HINT.to_string());
        lines.extend(as_menu(&upload));
    }
    lines
}

pub fn generate(svc: &Tunnel, local_port: u16, server: &ServerConfig) -> NginxSnippet {
    generate_with(svc, local_port, server, &GenOptions::default())
}

pub fn generate_with(
    svc: &Tunnel,
    _local_port: u16,
    server: &ServerConfig,
    opts: &GenOptions,
) -> NginxSnippet {
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
                    "公网网址「{raw}」不能直接作为域名使用：只允许字母、数字、点、横杠（不要包含 http://、端口、路径）。这里先用「你的域名」占位，请回到应用中改为纯域名后重新生成。"
                ));
                ("你的域名".to_string(), false)
            }
        }
    };
    let want_password = svc.wants_password();
    let want_ip = svc.wants_ip();
    let preset = normalize_preset(&svc.preset);

    let mut preview: Vec<SnippetLine> = vec![
        gray("server {"),
        gray("    ..."),
        gray(format!("    server_name {domain};")),
    ];

    // 两种保护可以同时要：先出密码块，再出 IP 白名单块（nginx 默认 satisfy all，两块会叠加生效）。
    let mut blocks: Vec<String> = Vec::new();
    let mut acme_lines: Vec<String> = Vec::new();
    let mut htpasswd = None;
    let dir = htpasswd_dir_with(server, opts.detected_vhost_dir);
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
                "访问用户名「{user}」包含冒号、空白或换行，无法用于密码保护（存在注入风险）。这项未生成，请回到应用中改为普通用户名。"
            ));
        } else {
            blocks.push(format!(
                "    # 访问密码（由「上线助手」新增）\n    auth_basic \"需要密码\";\n    auth_basic_user_file {file};"
            ));
            acme_lines.push("auth_basic off;".into());
            match svc.access_hash.as_deref() {
                Some(hash) if valid_hash(hash) => htpasswd = Some(format!("{user}:{hash}")),
                Some(_) => warnings.push(
                    "密码哈希包含空白或不是有效哈希，htpasswd 内容未生成；请回到应用中重新设置一次访问密码。"
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
                "IP 白名单中存在无法识别的写法：{}。仅支持单个 IP 或 CIDR（例如 1.2.3.4、10.0.0.0/8）；all、0.0.0.0/0 等于未设防，已跳过。",
                bad.join("、")
            ));
        }
        if valid.is_empty() {
            warnings.push(
                "IP 白名单为空（或全部为无效写法），IP 限制未生成；也不会只放一行 deny all; ——那会阻止所有访问。请回到应用中填写 IP 后重新生成。"
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
            acme_lines.push("allow all;".into());
        }
    }

    if !want_password && !want_ip {
        preview.push(add(
            "    # ⚠ 没有访问保护：任何人知道网址都能访问（建议回应用里设一个访问密码）",
        ));
    }

    // ---------- 「按需启用」菜单（历史 preset 的 ws / upload 保持激活，其余都注释） ----------
    let preset_extra = preset_lines(preset, opts.ws_map);

    // ---------- 证书续期豁免（多行；白名单模式也要 allow all;） ----------
    let acme_exempt = if acme_lines.is_empty() {
        None
    } else {
        let mut out: Vec<String> = vec![
            "# 证书续期专用：把下面要用的行放进已有的 location /.well-known/ { } 里".into(),
            "# （续 Let's Encrypt 证书要用；只放开这一小块，别的地址还是要过密码/白名单）".into(),
        ];
        out.extend(acme_lines.iter().cloned());
        if opts.wellknown_present == Some(false) {
            out.push(String::new());
            out.push(
                "# 你的站点配置里没有 /.well-known/ 这一块；没有的话整块加上（已经有就别重复加，nginx 会报 location 重复）"
                    .into(),
            );
            out.push("location /.well-known/ {".into());
            for line in &acme_lines {
                out.push(format!("    {line}"));
            }
            out.push("}".into());
        }
        Some(out.join("\n"))
    };

    // 示意里补上 .well-known 块和预设行的位置。
    if !acme_lines.is_empty() {
        preview.push(gray("    ..."));
        preview.push(gray("    # SSL证书验证目录"));
        preview.push(gray("    location /.well-known/ {"));
        preview.push(add("        # 新增：证书续期用（放进这个目录里）"));
        for line in &acme_lines {
            preview.push(add(format!("        {line}")));
        }
        preview.push(gray("    }"));
    }
    if !preset_extra.is_empty() {
        preview.push(gray("    ..."));
        preview.push(gray("    location / {"));
        for line in &preset_extra {
            if line.is_empty() {
                preview.push(add(""));
            } else {
                preview.push(add(format!("        {line}")));
            }
        }
        preview.push(gray("    }"));
    }

    let snippet = {
        let mut parts = blocks;
        parts.push(preset_extra.join("\n"));
        Some(parts.join("\n"))
    };

    preview.push(gray("    ..."));
    preview.push(gray("}"));

    // ---------- 可选附加行（默认不生成） ----------
    let optional: Vec<String> = vec![
        "# 可选：告诉搜索引擎「别收录这个网站」（礼貌提示，不是门锁；门锁还是密码/白名单）".into(),
        "add_header X-Robots-Tag \"noindex, nofollow\" always;".into(),
    ];

    // ---------- 使用说明（人话，只留 4 条短句 + 按需追加） ----------
    let mut steps: Vec<String> = vec![
        format!(
            "请先在面板建好站点和证书；「反向代理」目标填写 http://127.0.0.1:{}。（已配置可跳过）",
            svc.remote_port
        ),
        "把下面高亮的行加入「站点 → 配置修改」：只添加这几行，不替换整段；保存后重载 nginx，再从外网打开确认。".into(),
    ];
    if want_password {
        if htpasswd.is_some() {
            steps.push(format!("密码文件内容保存到服务器上的 {file}（不存在则新建）。"));
        } else {
            steps.push("这个服务还没有可用的密码哈希：请回到应用中重新设置一次访问密码，再回来生成。".into());
        }
        steps.push(format!(
            "密码文件要保存到 {dir}/ 下面。如果面板不是小皮，请回到应用中把服务器的「站点目录」改为所用面板的目录（例如宝塔填 /www/server/panel/vhost），再回来重新生成；临时也可先设置环境变量 SHANGXIAN_HTPASSWD_DIR 再重新生成。"
        ));
    }
    if !acme_lines.is_empty() {
        let what = match (want_password, want_ip) {
            (true, true) => "证书续期那两行（auth_basic off; 和 allow all;）",
            (true, false) => "证书续期那行（auth_basic off;）",
            (false, true) => "证书续期那行（allow all;）",
            (false, false) => "",
        };
        steps.push(format!(
            "{what}不要遗漏——遗漏后证书到期无法自动续期；它只放开 /.well-known/ 这个地址，其他地址仍然需要密码/白名单。"
        ));
    }
    steps.push(
        "「可选附加行」里的 noindex 默认未添加：添加后会告诉搜索引擎不要收录这个网站（礼貌提示，不是门锁）；网站面向所有人公开时无需添加。".into(),
    );
    match (want_password, want_ip) {
        (true, true) => steps.push(
            "白名单和密码两道门都要过；更换网络或手机时请记得回到应用中添加 IP。".into(),
        ),
        (false, true) => {
            steps.push("只有白名单中的 IP 能访问；更换网络或手机时请记得回到应用中添加 IP。".into())
        }
        (false, false) => {
            steps.push("⚠ 这个服务目前没有访问保护，建议在应用中为它设置访问密码。".into())
        }
        (true, false) => {}
    }
    if url_missing {
        steps.insert(
            0,
            "⚠ 这条隧道尚未填写公网网址：请先回到应用中填写网址，再回来重新生成（当前示意中的域名为占位符）。".into(),
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
        optional,
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
            preset: "web".into(),
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
        // 服务器配置里的 vhostDir 最优先，其次环境识别结果，再其次环境变量，最后小皮默认
        assert_eq!(
            resolve_htpasswd_dir(Some("/www/server/panel/vhost"), Some("/detected"), Some("/env/dir")),
            "/www/server/panel/vhost"
        );
        assert_eq!(
            resolve_htpasswd_dir(None, Some("/detected"), Some("/env/dir")),
            "/detected"
        );
        assert_eq!(resolve_htpasswd_dir(None, None, Some("/env/dir")), "/env/dir");
        assert_eq!(
            resolve_htpasswd_dir(Some("  "), Some(""), Some("")),
            "/xp/panel/vhost"
        );
        // 末尾的斜杠要去掉，免得拼出 /data/vhost//nginx
        assert_eq!(
            resolve_htpasswd_dir(Some("/data/vhost/"), None, None),
            "/data/vhost"
        );
        assert_eq!(resolve_htpasswd_dir(None, None, None), "/xp/panel/vhost");
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
    fn detected_vhost_dir_used_when_server_field_empty() {
        let mut tun = tunnel_with(
            vec!["password"],
            Some("shangxian"),
            Some("$2y$12$aBcDeFgHiJkLmNoPqRsTuVwXyZaBcDeFgHiJkLmNoPqRsTuVwXyZ0"),
            None,
        );
        tun.url = Some("album.example.com".into());
        let mut opts = GenOptions::default();
        opts.detected_vhost_dir = Some("/www/server/panel/vhost");
        let snip = generate_with(&tun, 8080, &fake_server(), &opts);
        assert!(
            snip.snippet
                .as_deref()
                .unwrap()
                .contains("/www/server/panel/vhost/album.example.com.htpasswd;")
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
        assert!(
            !snip.snippet.as_deref().unwrap().contains("auth_basic"),
            "没有任何保护块应该可以生成（只剩按需启用菜单）：{:?}",
            snip.snippet
        );
        assert!(snip.acme_exempt.is_none(), "没有保护就不生成续期豁免");
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
        assert!(
            !snip.snippet.as_deref().unwrap().contains("deny all;"),
            "只有 deny all 会把所有人挡住，不能生成：{:?}",
            snip.snippet
        );
        assert!(snip.steps.iter().any(|s| s.contains("阻止所有访问")));
        assert!(snip.acme_exempt.is_none(), "没有真正生效的白名单就不生成续期豁免");
    }

    // ---------- .well-known 豁免（D0 第 13 条 / D3 §5） ----------

    #[test]
    fn acme_exempt_password_only_is_multiline() {
        let tun = tunnel_with(
            vec!["password"],
            Some("u"),
            Some("$2y$12$abc"),
            None,
        );
        let snip = generate(&tun, 8080, &fake_server());
        let acme = snip.acme_exempt.as_deref().unwrap();
        assert!(acme.contains("auth_basic off;"), "acme={acme}");
        assert!(!acme.contains("allow all;"), "只密码时不该有 allow all：{acme}");
        assert!(
            acme.contains("# 证书续期专用"),
            "要带说明注释（多行）：{acme}"
        );
    }

    #[test]
    fn acme_exempt_whitelist_only_has_allow_all() {
        // D0 第 13 条：白名单模式也必须生成 allow all;（不再假设面板自带）
        let tun = tunnel_with(vec!["ip"], None, None, Some("1.2.3.4"));
        let snip = generate(&tun, 8080, &fake_server());
        let acme = snip.acme_exempt.as_deref().unwrap();
        assert!(acme.contains("allow all;"), "acme={acme}");
        assert!(!acme.contains("auth_basic off;"), "acme={acme}");
    }

    #[test]
    fn acme_exempt_both_modes_has_both_lines() {
        let tun = tunnel_with(
            vec!["password", "ip"],
            Some("u"),
            Some("$2y$12$abc"),
            Some("1.2.3.4"),
        );
        let snip = generate(&tun, 8080, &fake_server());
        let acme = snip.acme_exempt.as_deref().unwrap();
        let off_at = acme.find("auth_basic off;").expect("要有 auth_basic off");
        let allow_at = acme.find("allow all;").expect("要有 allow all");
        assert!(off_at < allow_at, "按 D3 顺序：先 auth_basic off 再 allow all：{acme}");
    }

    #[test]
    fn acme_exempt_adds_block_hint_when_wellknown_missing() {
        let tun = tunnel_with(vec!["password", "ip"], Some("u"), Some("$2y$12$abc"), Some("1.2.3.4"));
        let mut opts = GenOptions::default();
        opts.wellknown_present = Some(false);
        let snip = generate_with(&tun, 8080, &fake_server(), &opts);
        let acme = snip.acme_exempt.as_deref().unwrap();
        assert!(acme.contains("location /.well-known/ {"), "acme={acme}");
        assert!(acme.contains("    auth_basic off;") && acme.contains("    allow all;"), "acme={acme}");
    }

    // ---------- 「按需启用」菜单（历史 preset 决定哪组激活） ----------

    /// 片段里有没有按「行」算的指令：`commented=true` 要求该行是 `# 指令`，
    /// `false` 要求是激活行（行首无 #）。逐行判断，避免子串误判。
    fn has_instruction(add: &str, prefix: &str, commented: bool) -> bool {
        add.lines().any(|line| {
            let text = line.trim_start();
            match text.strip_prefix('#') {
                Some(rest) => commented && rest.trim_start().starts_with(prefix),
                None => !commented && text.starts_with(prefix),
            }
        })
    }

    #[test]
    fn web_menu_comments_both_groups() {
        let tun = tunnel_with(vec!["password"], Some("u"), Some("$2y$12$abc"), None);
        let snip = generate(&tun, 8080, &fake_server());
        let add = snip.snippet.as_deref().unwrap();
        assert!(add.contains(PRESET_HEAD), "add={add}");
        assert!(add.contains(PRESET_MENU_HINT), "add={add}");
        assert!(add.contains(WS_MENU_HINT), "add={add}");
        assert!(add.contains(UPLOAD_MENU_HINT), "add={add}");
        assert!(has_instruction(add, "proxy_http_version 1.1;", true), "add={add}");
        assert!(has_instruction(add, "client_max_body_size 2048m;", true), "add={add}");
        assert!(!has_instruction(add, "proxy_http_version 1.1;", false), "add={add}");
        assert!(!has_instruction(add, "client_max_body_size 2048m;", false), "add={add}");
        // 示意图里同一套菜单行都是新增行
        assert!(snip
            .preview
            .iter()
            .any(|l| l.is_new && l.text.contains("# proxy_http_version 1.1;")));
        assert!(snip
            .preview
            .iter()
            .any(|l| l.is_new && l.text.contains("# client_max_body_size 2048m;")));
    }

    #[test]
    fn ws_preset_keeps_ws_group_active() {
        let mut tun = tunnel_with(vec!["password"], Some("u"), Some("$2y$12$abc"), None);
        tun.preset = "ws".into();
        let mut opts = GenOptions::default();
        opts.ws_map = Some(true);
        let snip = generate_with(&tun, 8080, &fake_server(), &opts);
        let add = snip.snippet.as_deref().unwrap();
        assert!(has_instruction(add, "proxy_http_version 1.1;", false), "add={add}");
        assert!(has_instruction(add, "proxy_set_header Upgrade $http_upgrade;", false), "add={add}");
        assert!(
            has_instruction(add, "proxy_set_header Connection $connection_upgrade;", false),
            "add={add}"
        );
        assert!(has_instruction(add, "proxy_read_timeout 3600s;", false), "add={add}");
        assert!(!add.contains("\"upgrade\""), "有 map 时不该再用 \"upgrade\"：{add}");
        // 另一组（大文件上传）仍是注释菜单
        assert!(has_instruction(add, "client_max_body_size 2048m;", true), "add={add}");
        assert!(add.contains(UPLOAD_MENU_HINT), "add={add}");
        assert!(!add.contains(WS_MENU_HINT), "WS 组已激活，不该再带菜单说明：{add}");
    }

    #[test]
    fn ws_preset_without_map_uses_upgrade_with_advice() {
        let mut tun = tunnel_with(vec!["password"], Some("u"), Some("$2y$12$abc"), None);
        tun.preset = "ws".into();
        // ws_map 未知（None）也要能用，但要提示怎么换更好
        let snip = generate(&tun, 8080, &fake_server());
        let add = snip.snippet.as_deref().unwrap();
        assert!(
            has_instruction(add, "proxy_set_header Connection \"upgrade\";", false),
            "add={add}"
        );
        assert!(add.contains("connection_upgrade 的 map"), "add={add}");
        assert!(has_instruction(add, "client_max_body_size 2048m;", true), "add={add}");
    }

    #[test]
    fn upload_preset_keeps_upload_group_active() {
        let mut tun = tunnel_with(vec!["password"], Some("u"), Some("$2y$12$abc"), None);
        tun.preset = "upload".into();
        let snip = generate(&tun, 8080, &fake_server());
        let add = snip.snippet.as_deref().unwrap();
        assert!(has_instruction(add, "client_max_body_size 2048m;", false), "add={add}");
        assert!(has_instruction(add, "proxy_request_buffering off;", false), "add={add}");
        assert!(has_instruction(add, "proxy_read_timeout 600s;", false), "add={add}");
        // 另一组（WebSocket）仍是注释菜单
        assert!(has_instruction(add, "proxy_http_version 1.1;", true), "add={add}");
        assert!(add.contains(WS_MENU_HINT), "add={add}");
        assert!(!add.contains(UPLOAD_MENU_HINT), "上传组已激活，不该再带菜单说明：{add}");
    }

    #[test]
    fn unknown_preset_falls_back_to_web_menu() {
        let mut tun = tunnel_with(vec!["password"], Some("u"), Some("$2y$12$abc"), None);
        tun.preset = "spa".into();
        let snip = generate(&tun, 8080, &fake_server());
        let add = snip.snippet.as_deref().unwrap();
        assert!(has_instruction(add, "proxy_http_version 1.1;", true), "add={add}");
        assert!(has_instruction(add, "client_max_body_size 2048m;", true), "add={add}");
        assert_eq!(normalize_preset("WS"), "ws");
        assert_eq!(normalize_preset("upload"), "upload");
        assert_eq!(normalize_preset("SPA"), "web");
    }

    // ---------- noindex（D0 第 3 条：只在「可选附加行」里提示） ----------

    #[test]
    fn noindex_is_only_in_optional() {
        let tun = tunnel_with(vec!["password"], Some("u"), Some("$2y$12$abc"), None);
        let snip = generate(&tun, 8080, &fake_server());
        let add = snip.snippet.as_deref().unwrap();
        assert!(!add.contains("X-Robots-Tag"), "默认不生成 noindex：{add}");
        assert!(
            !snip.preview.iter().any(|l| l.text.contains("X-Robots-Tag")),
            "示意图里也不默认画 noindex"
        );
        assert!(
            snip.optional.iter().any(|l| l.contains("X-Robots-Tag \"noindex, nofollow\" always;")),
            "optional={:?}",
            snip.optional
        );
        assert!(snip.steps.iter().any(|s| s.contains("noindex")), "steps={:?}", snip.steps);
    }
}
