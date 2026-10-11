//! 云端访问保护核对：解析服务器上 nginx 站点配置的全文，判断实际开着的保护。
//!
//! 纯文本解析（不联网、不改服务器），解析规则对着真实的小皮面板配置：
//! - 密码保护看 `auth_basic`：值不是 off 才算开着（`location /.well-known/` 里会有一行
//!   `auth_basic off;`，不能因为它就判成「关」）；
//! - IP 白名单看 `deny all;`；`allow all;`（面板默认）不算白名单；
//! - `allow X;` 里的 X（不是 all）收进白名单，去重、按配置里的先后顺序保留。
//!
//! 只看 server 块顶层（大括号层级 == 1）的规则：`location` / `if` 等子块里的
//! `deny all;` / `allow 1.2.3.4;` 是局部的，不能当成整站的保护。引号里的
//! `{` `}` `#` 只当普通字符，不算语法。
//!
//! 远端脚本会把站点配置夹在 `===SHANGXIAN-CONF-BEGIN/END===` 两条标记之间：
//! 标记之间只认规则行，标记之外只认 CONF_FOUND / CONF_MISSING / HTPASSWD_* 标记，
//! 这样正文里恰好写着「HTPASSWD_NO」之类的字样也不会翻转核对结果。

use serde::Serialize;

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
    /// 有没有 IP 白名单（server 顶层存在 deny all;）
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
    // server 块内的花括号层级：server { 之后是 1，location/if 等子块里是 2 及以上。
    let mut depth: i32 = 0;

    for line in output.lines() {
        let line = line.trim();
        if in_conf {
            if line == CONF_END {
                in_conf = false;
                continue;
            }
            scan_conf_line(line, &mut check, &mut depth);
            continue;
        }

        // 正文之外只认标记行（正文里出现的同样字样不作数）。
        match line {
            CONF_BEGIN => {
                in_conf = true;
                depth = 0;
            }
            "CONF_FOUND" => check.conf_found = true,
            "CONF_MISSING" => check.conf_found = false,
            "HTPASSWD_YES" => check.htpasswd_exists = true,
            "HTPASSWD_NO" => check.htpasswd_exists = false,
            _ => {}
        }
    }

    check
}

/// 逐字符扫一行配置：数花括号层级，只在层级 == 1 时把 `;` 结束的语句当规则解析。
///
/// - 引号里的 `{` `}` `#` 和 `;` 都只是普通字符；
/// - `#` 到行尾算注释；
/// - 一行写完的 `location x { deny all; }`：`{` 先把层级 +1，后面的语句在层级 2，
///   不会被误当成整站规则。
fn scan_conf_line(line: &str, check: &mut AccessCheck, depth: &mut i32) {
    let mut stmt = String::new();
    let mut quote: Option<char> = None;

    for ch in line.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                stmt.push(ch);
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '#' => break, // 行尾注释，剩下的都不算
            '{' => {
                *depth += 1;
                stmt.clear();
            }
            '}' => {
                *depth = (*depth - 1).max(0);
                stmt.clear();
            }
            ';' => {
                if *depth == 1 {
                    parse_directive(&stmt, check);
                }
                stmt.clear();
            }
            _ => stmt.push(ch),
        }
    }
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
            if !value.is_empty()
                && !value.eq_ignore_ascii_case("all")
                && !check.allow_ips.iter().any(|ip| ip == &value)
            {
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
}
