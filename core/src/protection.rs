//! 云端访问保护核对：解析服务器上 nginx 站点配置的全文，判断实际开着的保护。
//!
//! 纯文本解析（不联网、不改服务器），解析规则对着真实的小皮面板配置：
//! - 密码保护看 `auth_basic`：值不是 off 才算开着（`location /.well-known/` 里会有一行
//!   `auth_basic off;`，不能因为它就判成「关」）；
//! - IP 白名单看 `deny all;` **加上至少一条有效的 `allow`**；`allow all;`、`0.0.0.0/0`
//!   都不算白名单（等于没设防）；
//! - `allow X;` 里的 X 通过 IP/CIDR 校验才收进来，去重、按配置里的先后顺序保留。也只
//!   有同时有有效 allow 时 `ip_on` 才为真，避免界面上把「只有 deny all」报成已开。
//!
//! 只看 server 块顶层（大括号层级 == 1）的规则：`location` / `if` 等子块里的
//! `deny all;` / `allow 1.2.3.4;` 是局部的，不能当成整站的保护。引号里的
//! `{` `}` `#` 只当普通字符，不算语法。
//!
//! 远端脚本会把站点配置夹在 `===SHANGXIAN-CONF-BEGIN/END===` 两条标记之间：
//! 标记之间只认规则行，标记之外只认 CONF_FOUND / CONF_MISSING / HTPASSWD_* 标记，
//! 这样正文里恰好写着「HTPASSWD_NO」之类的字样也不会翻转核对结果。

use serde::Serialize;

use crate::nginx::valid_allow_entry;

/// 站点配置正文的开始标记（由 `guardian::access_check_script` 打印）。
const CONF_BEGIN: &str = "===SHANGXIAN-CONF-BEGIN===";
/// 站点配置正文的结束标记。
const CONF_END: &str = "===SHANGXIAN-CONF-END===";

/// 一条隧道网站实际生效的访问保护（给界面/命令行看）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessCheck {
    /// 云端站点配置文件在不在（/xp/panel/vhost/nginx/<域名>.conf）
    pub conf_found: bool,
    /// 有没有开密码保护（server 顶层存在不是 off 的 auth_basic）
    pub password_on: bool,
    /// 有没有 IP 白名单（server 顶层存在 deny all; 且至少有一条有效 allow）
    pub ip_on: bool,
    /// 白名单里实际写着的 IP（去重、按配置里的先后顺序）
    pub allow_ips: Vec<String>,
    /// 密码文件在不在（/xp/panel/vhost/<域名>.htpasswd）
    pub htpasswd_exists: bool,
}

/// 解析远端脚本的输出：两条正文标记之间的站点配置全文，加上标记外的
/// CONF_FOUND/CONF_MISSING/HTPASSWD_YES/HTPASSWD_NO 结果行。
pub fn parse(output: &str) -> AccessCheck {
    let mut check = AccessCheck {
        conf_found: false,
        password_on: false,
        ip_on: false,
        allow_ips: Vec::new(),
        htpasswd_exists: false,
    };

    let mut in_conf = false;
    let mut conf = String::new();

    for line in output.lines() {
        let line = line.trim();
        if in_conf {
            if line == CONF_END {
                in_conf = false;
                continue;
            }
            conf.push_str(line);
            conf.push('\n');
            continue;
        }

        // 正文之外只认标记行（正文里出现的同样字样不作数）。
        match line {
            CONF_BEGIN => in_conf = true,
            "CONF_FOUND" => check.conf_found = true,
            "CONF_MISSING" => check.conf_found = false,
            "HTPASSWD_YES" => check.htpasswd_exists = true,
            "HTPASSWD_NO" => check.htpasswd_exists = false,
            _ => {}
        }
    }

    // 只看 server 块顶层（大括号层级 == 1）的规则；location / if 等子块里的
    // `deny all;` / `allow 1.2.3.4;` 是局部的，不能当成整站的保护。
    for block in parse_block_tree(&conf) {
        if !head_is(&block.head, "server") {
            continue;
        }
        for stmt in &block.directives {
            parse_directive(stmt, &mut check);
        }
    }

    // 白名单要「deny all; + 至少一条有效 allow」同时成立才算开；只有 deny all; 等于全挡，不算保护。
    check.ip_on = check.ip_on && !check.allow_ips.is_empty();

    check
}

// ---------- server 块扫描器（D3 §4：HTTPS / 证书 / 跳转 / 日志 / .well-known 判断用） ----------

/// 一个 `listen` 指令：端口 + 是不是带 `ssl` 参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    pub port: u16,
    pub ssl: bool,
    pub args: Vec<String>,
}

/// 一条 `return <code> <目标>;`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnRule {
    pub code: u16,
    pub target: String,
}

/// 一条 `rewrite <正则> <目标> [flag];`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteRule {
    pub target: String,
    pub flag: String,
}

/// 一个 `location` 子块（只收我们关心的那几样）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationBlock {
    /// `location` 后面的参数（已去引号），例：`/.well-known/`、`^~ /.well-known/`
    pub args: String,
    /// 子块里的语句原文（已去分号）
    pub directives: Vec<String>,
    /// 子块里有 `auth_basic off;`
    pub auth_basic_off: bool,
    /// 子块里有 `allow all;`
    pub allow_all: bool,
}

/// 一个 `server { }` 块里我们关心的内容。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerBlock {
    /// `server_name` 列表（已转小写、去掉结尾的点）
    pub server_names: Vec<String>,
    pub listens: Vec<Listen>,
    /// 有 `ssl on;`
    pub ssl_on: bool,
    /// `ssl_certificate` 的路径（含 $ 变量的跳过）
    pub ssl_certificates: Vec<String>,
    pub returns: Vec<ReturnRule>,
    pub rewrites: Vec<RewriteRule>,
    /// `if` 子块里挂 $server_port / $scheme 条件的整站 https 跳转
    /// （面板「强制 HTTPS」的标准写法，如 `if ($server_port !~ 443) { rewrite … permanent; }`）。
    pub if_redirects: Vec<RedirectKind>,
    /// `access_log` 的第一个参数；`off` 原样记成 "off"
    pub access_logs: Vec<String>,
    /// 直接的 `location` 子块
    pub locations: Vec<LocationBlock>,
    /// 块里（含子块）有没有 `proxy_pass`
    pub has_proxy_pass: bool,
    /// 块顶层的语句原文（已去分号）
    pub directives: Vec<String>,
}

/// 命中这个网站的 server 块选择结果。
#[derive(Debug)]
pub enum ServerPick<'a> {
    /// server_name 和域名对上的块（可能多个）
    Matched(Vec<&'a ServerBlock>),
    /// 没对上，但整个配置里只有一个 server 块：按它判断（调用方要给黄字提醒）
    Single(&'a ServerBlock),
    /// 对不上（或一个 server 块都没有）
    None,
}

/// 80 跳 443 的跳转类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    /// 301 / 308 / rewrite 到 https
    Permanent,
    /// 302 / 307（临时跳转，要提醒用户换 301）
    Temporary,
}

impl ServerBlock {
    /// 这个块开了 ssl：listen 带 ssl 参数，或有 `ssl on;`。
    pub fn has_ssl(&self) -> bool {
        self.ssl_on || self.listens.iter().any(|l| l.ssl)
    }

    /// 这个块是不是在监听指定端口；一条 `listen` 都没写时 nginx 默认监听 80。
    pub fn listens_on(&self, port: u16) -> bool {
        if self.listens.is_empty() {
            port == 80
        } else {
            self.listens.iter().any(|l| l.port == port)
        }
    }

    /// 块里第一个 `/.well-known/` 的 location。
    pub fn wellknown(&self) -> Option<&LocationBlock> {
        self.locations.iter().find(|l| is_wellknown_args(&l.args))
    }

    pub fn has_wellknown(&self) -> bool {
        self.wellknown().is_some()
    }

    /// 块里有没有「跳到 https」的 return / rewrite / 整站 if。
    ///
    /// return / rewrite 只在块顶层看（和 D3 §4 一致）；`if` 子块另看：条件挂在
    /// `$server_port` / `$scheme` 上的（面板「强制 HTTPS」的标准写法）算整站跳转，
    /// 按路径写的 `if` 不算。rewrite 按 nginx 的真实行为分档：`permanent` 就是 301，
    /// `redirect` 是 302；其它 flag 不看。
    pub fn redirect_to_https(&self) -> Option<RedirectKind> {
        let mut permanent = false;
        let mut temporary = false;
        for r in &self.returns {
            if !target_is_https(&r.target) {
                continue;
            }
            match r.code {
                301 | 308 => permanent = true,
                302 | 307 => temporary = true,
                _ => {}
            }
        }
        for r in &self.rewrites {
            if !target_is_https(&r.target) {
                continue;
            }
            match r.flag.as_str() {
                "permanent" => permanent = true,
                "redirect" => temporary = true,
                _ => {}
            }
        }
        for kind in &self.if_redirects {
            match kind {
                RedirectKind::Permanent => permanent = true,
                RedirectKind::Temporary => temporary = true,
            }
        }
        if permanent {
            Some(RedirectKind::Permanent)
        } else if temporary {
            Some(RedirectKind::Temporary)
        } else {
            None
        }
    }

    /// 第一个不是 off 的 `access_log` 路径。
    pub fn access_log_path(&self) -> Option<&str> {
        self.access_logs
            .iter()
            .map(String::as_str)
            .find(|p| !p.eq_ignore_ascii_case("off"))
    }
}

/// `if` 子块里的整站 https 跳转：条件必须挂在 `$server_port` 或 `$scheme` 上，
/// 这是「来的是 http 就跳」的整站判断；按路径写的 `if`（`$uri`、`$request_uri` 等）
/// 是局部跳转，不算整站。rewrite 按 nginx 真实行为分档：`permanent` = 301，
/// `redirect` = 302，其它 flag 不看。
fn if_block_redirect(head: &str, directives: &[String]) -> Option<RedirectKind> {
    let cond = head.to_ascii_lowercase();
    if !cond.contains("$server_port") && !cond.contains("$scheme") {
        return None;
    }
    let mut permanent = false;
    let mut temporary = false;
    for stmt in directives {
        let (name, args) = split_directive(stmt);
        match name.to_ascii_lowercase().as_str() {
            "return" => {
                if let Some(code) = args.first().and_then(|a| a.parse::<u16>().ok()) {
                    let target = args.get(1).map(String::as_str).unwrap_or("");
                    if target_is_https(target) {
                        match code {
                            301 | 308 => permanent = true,
                            302 | 307 => temporary = true,
                            _ => {}
                        }
                    }
                }
            }
            "rewrite" => {
                if let Some(target) = args.get(1) {
                    if target_is_https(target) {
                        match args.get(2).map(|f| f.to_ascii_lowercase()).as_deref() {
                            Some("permanent") => permanent = true,
                            Some("redirect") => temporary = true,
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if permanent {
        Some(RedirectKind::Permanent)
    } else if temporary {
        Some(RedirectKind::Temporary)
    } else {
        None
    }
}

/// 在「主配置 + include 展开」的合并文本里，找根级（server 块外面）的整站 https 跳转。
///
/// 小皮面板把 `if ($server_port !~ 443) { rewrite … permanent; }` 单独写在
/// `vhost/https/to_https.conf` 里、被证书文件 include 进站点配置。远端脚本把
/// include 的内容拼回文本后，这段会落在 server 块外面，所以要在根级单独找一遍。
/// 条件判定和块内 `if` 一样：只认 $server_port / $scheme。
pub fn root_if_https_redirect(conf: &str) -> Option<RedirectKind> {
    let mut permanent = false;
    let mut temporary = false;
    for node in parse_block_tree(conf) {
        if !head_is(&node.head, "if") {
            continue;
        }
        match if_block_redirect(&node.head, &node.directives) {
            Some(RedirectKind::Permanent) => permanent = true,
            Some(RedirectKind::Temporary) => temporary = true,
            None => {}
        }
    }
    if permanent {
        Some(RedirectKind::Permanent)
    } else if temporary {
        Some(RedirectKind::Temporary)
    } else {
        None
    }
}

/// 域名和一条 server_name 是否匹配（忽略大小写、忽略结尾的点；支持 *./. 通配）。
///
/// - `a.example.com` 精确匹配；
/// - `*.example.com` 匹配子域名（不匹配裸域名，和 nginx 一致）；
/// - `.example.com` 匹配裸域名和所有子域名。
/// - `_`（nginx 的默认块占位）永远不匹配。
pub fn server_name_matches(domain: &str, name: &str) -> bool {
    let d = normalize_name(domain);
    let n = normalize_name(name);
    if d.is_empty() || n.is_empty() || n == "_" {
        return false;
    }
    if let Some(suffix) = n.strip_prefix("*.") {
        if suffix.is_empty() || !d.ends_with(suffix) || d.len() <= suffix.len() {
            return false;
        }
        return d.as_bytes()[d.len() - suffix.len() - 1] == b'.';
    }
    if let Some(suffix) = n.strip_prefix('.') {
        return d == suffix
            || (d.len() > suffix.len() && d.ends_with(suffix));
    }
    d == n
}

/// 从一份站点配置里挑出「本隧道的块」。
pub fn pick_server<'a>(blocks: &'a [ServerBlock], domain: &str) -> ServerPick<'a> {
    let matched: Vec<&ServerBlock> = blocks
        .iter()
        .filter(|b| b.server_names.iter().any(|n| server_name_matches(domain, n)))
        .collect();
    if !matched.is_empty() {
        return ServerPick::Matched(matched);
    }
    if blocks.len() == 1 {
        return ServerPick::Single(&blocks[0]);
    }
    ServerPick::None
}

/// 扫一份 nginx 配置（站点 conf 或 `nginx -T` 的输出都行），列出所有 server 块。
pub fn scan_servers(conf: &str) -> Vec<ServerBlock> {
    let mut out = Vec::new();
    for root in parse_block_tree(conf) {
        collect_servers(&root, &mut out);
    }
    out
}

fn collect_servers(node: &RawBlock, out: &mut Vec<ServerBlock>) {
    if head_is(&node.head, "server") {
        out.push(build_server(node));
        return;
    }
    for child in &node.children {
        collect_servers(child, out);
    }
}

fn build_server(node: &RawBlock) -> ServerBlock {
    let mut sb = ServerBlock::default();
    sb.has_proxy_pass = subtree_has_directive(node, "proxy_pass");

    for stmt in &node.directives {
        let (name, args) = split_directive(stmt);
        match name.to_ascii_lowercase().as_str() {
            "server_name" => {
                for a in &args {
                    let token = normalize_name(a);
                    if !token.is_empty() {
                        sb.server_names.push(token);
                    }
                }
            }
            "listen" => {
                if let Some(port) = parse_listen_port(args.first().map(String::as_str)) {
                    let ssl = args.iter().any(|a| a.eq_ignore_ascii_case("ssl"));
                    sb.listens.push(Listen {
                        port,
                        ssl,
                        args: args.clone(),
                    });
                }
            }
            "ssl" => {
                if args.first().map(|a| a.eq_ignore_ascii_case("on")).unwrap_or(false) {
                    sb.ssl_on = true;
                }
            }
            "ssl_certificate" => {
                if let Some(path) = args.first() {
                    if !path.is_empty() && !path.contains('$') {
                        sb.ssl_certificates.push(path.clone());
                    }
                }
            }
            "return" => {
                if let Some(code) = args.first().and_then(|a| a.parse::<u16>().ok()) {
                    sb.returns.push(ReturnRule {
                        code,
                        target: args.get(1).cloned().unwrap_or_default(),
                    });
                }
            }
            "rewrite" => {
                if let Some(target) = args.get(1) {
                    sb.rewrites.push(RewriteRule {
                        target: target.clone(),
                        flag: args
                            .get(2)
                            .map(|f| f.to_ascii_lowercase())
                            .unwrap_or_default(),
                    });
                }
            }
            "access_log" => {
                if let Some(path) = args.first() {
                    if path.eq_ignore_ascii_case("off") {
                        sb.access_logs.push("off".to_string());
                    } else {
                        sb.access_logs.push(path.clone());
                    }
                }
            }
            _ => {}
        }
        sb.directives.push(stmt.clone());
    }

    for child in &node.children {
        if head_is(&child.head, "if") {
            if let Some(kind) = if_block_redirect(&child.head, &child.directives) {
                sb.if_redirects.push(kind);
            }
            continue;
        }
        if !head_is(&child.head, "location") {
            continue;
        }
        let args = child
            .head
            .split_once(char::is_whitespace)
            .map(|(_, rest)| rest.trim().to_string())
            .unwrap_or_default();
        let mut lb = LocationBlock {
            args,
            directives: child.directives.clone(),
            auth_basic_off: false,
            allow_all: false,
        };
        for stmt in &child.directives {
            let (name, args) = split_directive(stmt);
            match name.to_ascii_lowercase().as_str() {
                "auth_basic" => {
                    if args
                        .first()
                        .map(|v| v.eq_ignore_ascii_case("off"))
                        .unwrap_or(false)
                    {
                        lb.auth_basic_off = true;
                    }
                }
                "allow" => {
                    if args
                        .first()
                        .map(|v| v.eq_ignore_ascii_case("all"))
                        .unwrap_or(false)
                    {
                        lb.allow_all = true;
                    }
                }
                _ => {}
            }
        }
        sb.locations.push(lb);
    }

    sb
}

/// 一层一层地把配置拆成块树（引号、注释、花括号的规则和现有解析器一致）。
#[derive(Debug, Clone, Default)]
struct RawBlock {
    head: String,
    directives: Vec<String>,
    children: Vec<RawBlock>,
}

fn parse_block_tree(text: &str) -> Vec<RawBlock> {
    let mut stack: Vec<RawBlock> = vec![RawBlock::default()];
    let mut stmt = String::new();
    let mut quote: Option<char> = None;
    let mut comment = false;

    for ch in text.chars() {
        if comment {
            if ch == '\n' {
                comment = false;
            }
            continue;
        }
        if let Some(q) = quote {
            stmt.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                stmt.push(ch);
            }
            '#' => comment = true,
            '{' => {
                let head = stmt.trim().to_string();
                stmt.clear();
                stack.push(RawBlock {
                    head,
                    ..RawBlock::default()
                });
            }
            '}' => {
                flush_stmt(&mut stmt, &mut stack);
                if stack.len() > 1 {
                    let done = stack.pop().unwrap();
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(done);
                    }
                }
            }
            ';' => flush_stmt(&mut stmt, &mut stack),
            _ => {
                if ch.is_whitespace() {
                    if !stmt.is_empty() && !stmt.ends_with(' ') {
                        stmt.push(' ');
                    }
                } else {
                    stmt.push(ch);
                }
            }
        }
    }
    flush_stmt(&mut stmt, &mut stack);
    // 最后一个大括号没闭合也照样收拢（配置文件被截断时不丢内容）
    while stack.len() > 1 {
        let done = stack.pop().unwrap();
        if let Some(parent) = stack.last_mut() {
            parent.children.push(done);
        }
    }
    stack.remove(0).children
}

fn flush_stmt(stmt: &mut String, stack: &mut [RawBlock]) {
    let text = stmt.trim().to_string();
    stmt.clear();
    if text.is_empty() {
        return;
    }
    if let Some(block) = stack.last_mut() {
        block.directives.push(text);
    }
}

fn subtree_has_directive(node: &RawBlock, name: &str) -> bool {
    if node
        .directives
        .iter()
        .any(|d| split_directive(d).0.eq_ignore_ascii_case(name))
    {
        return true;
    }
    node.children.iter().any(|c| subtree_has_directive(c, name))
}

fn head_is(head: &str, word: &str) -> bool {
    head.split_whitespace()
        .next()
        .map(|w| w.eq_ignore_ascii_case(word))
        .unwrap_or(false)
}

fn split_directive(text: &str) -> (String, Vec<String>) {
    let text = text.trim();
    let (head, rest) = match text.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest),
        None => (text, ""),
    };
    let args = rest
        .split_whitespace()
        .map(unquote)
        .filter(|a| !a.is_empty())
        .collect();
    (head.to_string(), args)
}

fn parse_listen_port(token: Option<&str>) -> Option<u16> {
    let token = token?.trim();
    if token.is_empty() || token.contains('/') {
        return None;
    }
    if let Ok(port) = token.parse::<u16>() {
        return Some(port);
    }
    // 127.0.0.1:8080 / [::]:8080 / *:8080
    token.rsplit(':').next()?.parse::<u16>().ok()
}

fn normalize_name(raw: &str) -> String {
    raw.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn target_is_https(target: &str) -> bool {
    let t = target.trim();
    t.len() >= 8 && t.as_bytes()[..8].eq_ignore_ascii_case(b"https://")
}

/// `location` 的参数是不是 `/.well-known/` 这一块（前缀匹配，去掉 = ^~ ~ 等修饰）。
fn is_wellknown_args(args: &str) -> bool {
    args.split_whitespace().any(|part| {
        let p = part.trim_matches(|c| c == '=' || c == '~' || c == '^');
        p.starts_with("/.well-known/")
    })
}

/// 把层级 1 的一条语句（`auth_basic "需要密码"` 这种）按规则记进结果。
fn parse_directive(stmt: &str, check: &mut AccessCheck) {
    let text = stmt.trim();
    if text.is_empty() {
        return;
    }
    let (head, rest) = match text.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (text, ""),
    };
    let directive = head.trim_end_matches(';').to_ascii_lowercase();
    // 取到第一个分号为止，后面可能是行尾注释
    let value = unquote(rest.split(';').next().unwrap_or("").trim());

    match directive.as_str() {
        "auth_basic" => {
            if !value.is_empty() && !value.eq_ignore_ascii_case("off") {
                check.password_on = true;
            }
        }
        "deny" => {
            if value.eq_ignore_ascii_case("all") {
                check.ip_on = true;
            }
        }
        "allow" => {
            // 只收合法的 IP / CIDR；all / 0.0.0.0/0 / 乱写的都不算白名单。
            if valid_allow_entry(&value) && !check.allow_ips.iter().any(|ip| ip == &value) {
                check.allow_ips.push(value);
            }
        }
        _ => {}
    }
}

/// 去掉值两边成对的引号（nginx 里 `auth_basic "需要密码";` 这种写法）。
fn unquote(value: &str) -> String {
    let text = value.trim();
    if text.len() >= 2 {
        let quoted = (text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\''));
        if quoted {
            return text[1..text.len() - 1].trim().to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 密码 + 白名单同开（顺序和真实配置一致：密码在 server 块里，.well-known 里有 off）。
    const BOTH: &str = r#"===SHANGXIAN-CONF-BEGIN===
server {
    listen 443 ssl;
    server_name karryn.iranol1.xyz;

    # 访问密码（由「上线助手」新增）
    auth_basic "需要密码";
    auth_basic_user_file /xp/panel/vhost/karryn.iranol1.xyz.htpasswd;

    # 新增：只允许白名单里的 IP（放在 server_name 下面）
    allow 1.2.3.4;
    allow 5.6.7.8;
    deny all;

    # SSL证书验证目录
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;

    #[test]
    fn parses_password_and_whitelist_together() {
        let check = parse(BOTH);
        assert!(check.conf_found);
        assert!(check.password_on, "有非 off 的 auth_basic 就是开了密码");
        assert!(check.ip_on, "有 deny all; 就是开了白名单");
        assert_eq!(check.allow_ips, vec!["1.2.3.4", "5.6.7.8"]);
        assert!(check.htpasswd_exists);
    }

    #[test]
    fn parses_password_only() {
        // 只有密码：没有 allow/deny；.well-known 里的 auth_basic off 不能把结果判成关
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name album.example.com;
    auth_basic "需要密码";
    auth_basic_user_file /xp/panel/vhost/album.example.com.htpasswd;
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;
        let check = parse(text);
        assert!(check.conf_found && check.htpasswd_exists);
        assert!(check.password_on, "auth_basic off 只出现在 .well-known 里，不能判成关");
        assert!(!check.ip_on);
        assert!(check.allow_ips.is_empty(), "allow all 不算白名单");
    }

    #[test]
    fn parses_whitelist_only() {
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name ip.example.com;
    auth_basic off;
    allow 10.0.0.1;
    allow 10.0.0.1;
    deny all;
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_NO
"#;
        let check = parse(text);
        assert!(check.conf_found && !check.htpasswd_exists);
        assert!(!check.password_on, "auth_basic off 不算密码保护");
        assert!(check.ip_on);
        assert_eq!(check.allow_ips, vec!["10.0.0.1"], "同一 IP 去重");
    }

    #[test]
    fn parses_no_protection() {
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name open.example.com;
    proxy_pass http://127.0.0.1:18080;
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_NO
"#;
        let check = parse(text);
        assert!(check.conf_found);
        assert!(!check.password_on && !check.ip_on);
        assert!(check.allow_ips.is_empty());
        assert!(!check.htpasswd_exists);
    }

    #[test]
    fn parses_missing_conf() {
        let check = parse("CONF_MISSING\nHTPASSWD_NO\n");
        assert!(!check.conf_found);
        assert!(!check.password_on && !check.ip_on);
        assert!(check.allow_ips.is_empty());
        assert!(!check.htpasswd_exists);
    }

    #[test]
    fn server_level_rules_count() {
        // server 顶层（层级 1）的规则才算：真实配置的 server 级 auth_basic 在这里
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    auth_basic "需要密码";
    allow 1.2.3.4;
    deny all;
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;
        let check = parse(text);
        assert!(check.password_on);
        assert!(check.ip_on);
        assert_eq!(check.allow_ips, vec!["1.2.3.4"]);
    }

    #[test]
    fn child_block_rules_are_ignored() {
        // 子块里的 deny/allow/auth_basic 是局部的，不能当成整站保护
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name child.example.com;
    location ~ /\. {
        deny all;
        allow 1.2.3.4;
        auth_basic "子块密码";
    }
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;
        let check = parse(text);
        assert!(!check.password_on, "location 块里的 auth_basic 不算");
        assert!(!check.ip_on, "location 块里的 deny all 不算");
        assert!(check.allow_ips.is_empty(), "location 块里的 allow 不算");
    }

    #[test]
    fn one_line_location_block_is_ignored() {
        // 一行写完的块：`{` 后面的内容层级已经是 2，不能当成 server 顶层
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    location ~ /\. { deny all; allow 1.2.3.4; auth_basic "x"; }
    deny all;
    allow 9.9.9.9;
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_NO
"#;
        let check = parse(text);
        assert!(!check.password_on, "一行式 location 里的 auth_basic 不算");
        assert!(check.ip_on, "server 顶层的 deny all 才算");
        assert_eq!(check.allow_ips, vec!["9.9.9.9"]);
    }

    #[test]
    fn braces_in_quotes_do_not_change_depth() {
        // 引号里的 { } # 只是普通字符；注释里的 } 也不能把层级带偏
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    auth_basic "有{花}括#号";
    # 这一行有 } } } 和 { { {，还有 deny all;
    allow "1.2.3.4";
    deny all; # 行尾注释
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;
        let check = parse(text);
        assert!(check.password_on, "引号里的花括号不影响规则识别");
        assert!(check.ip_on);
        assert_eq!(check.allow_ips, vec!["1.2.3.4"]);
    }

    #[test]
    fn body_text_does_not_flip_result_flags() {
        // 正文里恰好写着 HTPASSWD_NO / CONF_MISSING，也不能翻转标记行给的结果
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name trap.example.com;
    # HTPASSWD_NO
    return 200 "HTPASSWD_NO";
    auth_basic "需要密码";
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;
        let check = parse(text);
        assert!(check.conf_found, "正文里的 CONF_MISSING 不算数");
        assert!(check.password_on);
        assert!(check.htpasswd_exists, "正文里的 HTPASSWD_NO 不能把结果翻转成没有");
    }

    #[test]
    fn markers_are_required_for_rules() {
        // 没有正文标记时（老脚本输出），规则行不参与解析，避免正文里字样翻转结果
        let text = r#"server {
    auth_basic "需要密码";
    deny all;
}
CONF_FOUND
HTPASSWD_YES
"#;
        let check = parse(text);
        assert!(check.conf_found && check.htpasswd_exists);
        assert!(!check.password_on && !check.ip_on);
    }

    #[test]
    fn deny_all_alone_is_not_a_whitelist() {
        // 只有 deny all; 没有有效 allow：等于把所有人都挡住，不能报「白名单已开」
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name x.example.com;
    deny all;
    allow all;
    allow 0.0.0.0/0;
    allow 1.2.3.4/0;
    allow 乱写的;
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_NO
"#;
        let check = parse(text);
        assert!(!check.ip_on, "没有有效 allow 就不算白名单：{check:?}");
        assert!(check.allow_ips.is_empty(), "无效项不能收进白名单：{check:?}");
    }

    #[test]
    fn invalid_allow_entries_are_ignored() {
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name x.example.com;
    allow 1.2.3.4;
    allow 999.999.999.999;
    allow 10.0.0.0/8;
    deny all;
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_NO
"#;
        let check = parse(text);
        assert!(check.ip_on);
        assert_eq!(check.allow_ips, vec!["1.2.3.4", "10.0.0.0/8"]);
    }

    #[test]
    fn allow_after_deny_still_counts() {
        // 顺序反过来也算：只要 deny all; 和有效 allow 都在
        let text = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name x.example.com;
    deny all;
    allow 1.2.3.4;
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_NO
"#;
        let check = parse(text);
        assert!(check.ip_on);
        assert_eq!(check.allow_ips, vec!["1.2.3.4"]);
    }

    // ---------- server 块扫描器 ----------

    const SITE: &str = r#"
# 站点注释
server {
    listen 80;
    listen [::]:80;
    server_name album.example.com www.album.example.com;
    access_log /www/wwwlogs/album.example.com.log main;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    server_name album.example.com;
    ssl_certificate /www/server/panel/vhost/cert/album.example.com/fullchain.pem;
    access_log /www/wwwlogs/album.example.com.log;
    proxy_pass http://127.0.0.1:18080;
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
    location / {
        proxy_pass http://127.0.0.1:18080;
    }
}
"#;

    #[test]
    fn scans_server_blocks() {
        let blocks = scan_servers(SITE);
        assert_eq!(blocks.len(), 2, "应该扫到两个 server 块");
        let http = &blocks[0];
        assert_eq!(http.server_names, vec!["album.example.com", "www.album.example.com"]);
        assert_eq!(http.listens.len(), 2);
        assert!(http.listens_on(80) && !http.listens_on(443));
        assert!(!http.has_ssl());
        assert_eq!(http.redirect_to_https(), Some(RedirectKind::Permanent));
        assert_eq!(http.access_log_path(), Some("/www/wwwlogs/album.example.com.log"));

        let https = &blocks[1];
        assert!(https.has_ssl(), "listen 443 ssl 要算开了 ssl");
        assert!(https.listens_on(443) && !https.listens_on(80));
        assert_eq!(
            https.ssl_certificates,
            vec!["/www/server/panel/vhost/cert/album.example.com/fullchain.pem"]
        );
        assert!(https.has_proxy_pass);
        assert!(https.has_wellknown());
        let wk = https.wellknown().unwrap();
        assert!(wk.auth_basic_off && wk.allow_all);
    }

    #[test]
    fn default_listen_is_80() {
        let blocks = scan_servers("server { server_name a.example.com; proxy_pass http://127.0.0.1:1; }");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].listens_on(80), "没写 listen 时 nginx 默认监听 80");
        assert!(!blocks[0].listens_on(443));
    }

    #[test]
    fn ssl_on_shortcut_counts() {
        let blocks = scan_servers("server { ssl on; server_name a.example.com; }");
        assert!(blocks[0].has_ssl());
    }

    #[test]
    fn picks_wildcard_and_dot_names() {
        let blocks = scan_servers(
            r#"server { server_name *.example.com; }
server { server_name .dot.example.com; }
server { server_name _; }"#,
        );
        assert!(server_name_matches("a.example.com", "*.example.com"));
        assert!(!server_name_matches("example.com", "*.example.com"), "*. 不匹配裸域名");
        assert!(server_name_matches("example.com", ".dot.example.com") == false);
        assert!(server_name_matches("dot.example.com", ".dot.example.com"));
        assert!(server_name_matches("x.dot.example.com", ".dot.example.com"));
        assert!(!server_name_matches("a.example.com", "_"));
        assert!(server_name_matches("ALBUM.example.com.", "album.example.com"));

        match pick_server(&blocks, "demo.example.com") {
            ServerPick::Matched(v) => assert_eq!(v.len(), 1),
            other => panic!("应该按通配符命中：{other:?}"),
        }
        match pick_server(&blocks, "nothing.org") {
            ServerPick::None => {}
            other => panic!("多条但都对不上应该 None：{other:?}"),
        }
    }

    #[test]
    fn single_block_fallback() {
        let blocks = scan_servers("server { server_name other.example.com; }");
        match pick_server(&blocks, "album.example.com") {
            ServerPick::Single(b) => assert_eq!(b.server_names, vec!["other.example.com"]),
            other => panic!("只有一个块时应该直接用它：{other:?}"),
        }
    }

    #[test]
    fn temporary_redirect_is_flagged() {
        let blocks = scan_servers(
            "server { listen 80; server_name a.example.com; return 302 https://$host$request_uri; }",
        );
        assert_eq!(blocks[0].redirect_to_https(), Some(RedirectKind::Temporary));
    }

    #[test]
    fn rewrite_permanent_counts_as_redirect() {
        let blocks = scan_servers(
            "server { listen 80; server_name a.example.com; rewrite ^ https://$host$request_uri permanent; }",
        );
        assert!(blocks[0].redirect_to_https().is_some());
    }

    #[test]
    fn rewrite_permanent_is_classified_permanent() {
        let blocks = scan_servers(
            "server { listen 80; server_name a.example.com; rewrite ^(/.*)$ https://$host$1 permanent; }",
        );
        assert_eq!(blocks[0].redirect_to_https(), Some(RedirectKind::Permanent));
    }

    #[test]
    fn force_https_if_inside_server_block_is_recognized() {
        // 面板「强制 HTTPS」的标准写法：条件挂 $server_port
        let blocks = scan_servers(
            "server {\n  listen 80;\n  listen 443 ssl http2;\n  server_name a.example.com;\n  if ($server_port !~ 443) {\n    rewrite ^(/.*)$ https://$host$1 permanent;\n  }\n}",
        );
        assert_eq!(blocks[0].redirect_to_https(), Some(RedirectKind::Permanent));
    }

    #[test]
    fn path_specific_if_redirect_is_not_a_whole_site_redirect() {
        let blocks = scan_servers(
            "server { listen 80; server_name a.example.com; if ($request_uri ~ ^/old) { return 301 https://$host/new; } }",
        );
        assert_eq!(blocks[0].redirect_to_https(), None);
    }

    #[test]
    fn root_level_if_redirect_from_merged_include_is_found() {
        // 脚本把 include 的文件拼回文本后，跳转 if 会落在 server 块外面
        let conf = "server {\n  listen 80;\n  listen 443 ssl;\n  server_name a.example.com;\n}\nif ($server_port !~ 443) {\n  rewrite ^(/.*)$ https://$host$1 permanent;\n}\n";
        assert_eq!(root_if_https_redirect(conf), Some(RedirectKind::Permanent));
    }

    #[test]
    fn root_level_if_without_https_redirect_is_ignored() {
        let conf =
            "server { listen 80; server_name a.example.com; }\nif ($scheme = http) { return 404; }\n";
        assert_eq!(root_if_https_redirect(conf), None);
    }

    #[test]
    fn wellknown_modifiers_are_recognized() {
        let blocks = scan_servers(
            "server { server_name a.example.com; location ^~ /.well-known/ { auth_basic off; } }",
        );
        let wk = blocks[0].wellknown().expect("^~ 的 location 也要认出来");
        assert!(wk.auth_basic_off);
        assert!(!wk.allow_all);
    }

    #[test]
    fn certificates_with_variables_are_skipped() {
        let blocks = scan_servers(
            "server { server_name a.example.com; ssl_certificate $ssl_server_name.crt; ssl_certificate /etc/ssl/real.pem; }",
        );
        assert_eq!(blocks[0].ssl_certificates, vec!["/etc/ssl/real.pem"]);
    }

    #[test]
    fn http_wrapped_conf_is_scanned() {
        // nginx -T 的整份输出是 http { server { ... } } 套起来的
        let blocks = scan_servers(
            "http {\n  server { listen 443 ssl; server_name a.example.com; }\n}\n",
        );
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].has_ssl());
    }

    #[test]
    fn truncated_block_does_not_lose_directives() {
        let blocks = scan_servers("server { listen 443 ssl; server_name a.example.com;");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].has_ssl());
        assert_eq!(blocks[0].server_names, vec!["a.example.com"]);
    }
}
