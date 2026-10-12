//! 云端核对 V2 协议：远端只读脚本生成 + 文本协议解析 + 红黄灯判定（纯函数，可单测）。
//!
//! 对齐 D3-cloud.md §2–§11（D0 仲裁优先）：
//! - 远端输出协议「版本行 + 分节 + KEY=value + `LOG|`/`PORT|` 原始行」；
//!   与现有 `===SHANGXIAN-CONF-BEGIN/END===` 标记兼容（正文继续走 `protection::parse`）；
//! - 解析规则见 [`parse_v2`]：第一非空行必须是 `SHANGXIAN-CHECK-V2`，否则回退 V1 并给警告；
//!   配置正文标记优先级最高（正文里伪造 `===SECTION:XXX===` 不会把解析带偏）；
//! - 云端只贴证据，判断全在本地：环境识别、HTTPS/证书/跳转、端口回环、访问审计、
//!   公网 IP、`nginx -T` 文件定位都在这里解析成结构；
//! - 红黄灯（[`Light`]）由纯函数给出，方便单测和回放。
//!
//! 本模块**不联网、不写服务器**：生成的是让用户/看护进程拿去跑的脚本文本。

use std::collections::HashMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::nginx::valid_domain;
use crate::protection::{self, AccessCheck, RedirectKind, ServerBlock, ServerPick};
// 持久化结构以 state 模块为准（D2 §4.1 / D0 第 2 条）；本模块只负责组装。
use crate::state::{CloudProtection, ExpectedProtection, ProtectionCheck};

/// V2 协议的第一行版本标记。
pub const CHECK_V2_MARKER: &str = "SHANGXIAN-CHECK-V2";
/// 站点配置正文开始标记（沿用老脚本的标记）。
pub const CONF_BEGIN: &str = "===SHANGXIAN-CONF-BEGIN===";
/// 站点配置正文结束标记。
pub const CONF_END: &str = "===SHANGXIAN-CONF-END===";

/// 协议里单个值的最长字符数，超长一律忽略（D3 §11.2）。
const MAX_VALUE_LEN: usize = 512;
/// 配置正文最多收多少字节。
const MAX_CONF_BYTES: usize = 256 * 1024;
/// 审计统计的输入上限（2MB）。
const MAX_AUDIT_BYTES: usize = 2 * 1024 * 1024;
/// 审计样本最多 30 行。
const MAX_LOG_SAMPLES: usize = 30;
/// `LOG|` 行最多收 30 条。
const MAX_LOG_LINES: usize = 30;
/// `PORT|` 行最多收 64 条。
const MAX_PORT_LINES: usize = 64;
/// `AUDIT_IP=` 行最多收 128 条。
const MAX_IP_LINES: usize = 128;
/// 定位到的配置文件最多列 10 条。
const MAX_FILE_LINES: usize = 10;
/// 审计里被挡 IP 只留前 5 个。
const MAX_TOP_IPS: usize = 5;

// ---------- 检查模式 ----------

/// 一次云端检查的模式（对应 D3 §11.3 的节组合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    /// 完整核对：CONF/HTPASSWD/HTTPS/PORT/FILES/NET
    Full,
    /// 漂移复检（轻量）：CONF/HTPASSWD/HTTPS/PORT
    Drift,
    /// 访问审计：CONF/HTPASSWD/AUDIT
    Audit,
    /// 环境识别：ENV/FILES
    Env,
}

impl CheckMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckMode::Full => "full",
            CheckMode::Drift => "drift",
            CheckMode::Audit => "audit",
            CheckMode::Env => "env",
        }
    }

    /// 这个模式包含的节（顺序即脚本里的输出顺序）。
    pub fn sections(self) -> &'static [&'static str] {
        match self {
            CheckMode::Full => &["CONF", "HTPASSWD", "HTTPS", "PORT", "FILES", "NET"],
            CheckMode::Drift => &["CONF", "HTPASSWD", "HTTPS", "PORT"],
            CheckMode::Audit => &["CONF", "HTPASSWD", "AUDIT"],
            CheckMode::Env => &["ENV", "FILES"],
        }
    }
}

// ---------- 脚本生成（只读，写死的命令） ----------

/// 生成一次检查脚本需要的输入。
#[derive(Debug, Clone)]
pub struct ScriptInput<'a> {
    pub mode: CheckMode,
    /// 已规范化的域名（必须过 `valid_domain`）
    pub domain: &'a str,
    /// 面板站点目录（nginx 配置在它下面的 nginx/ 里）
    pub vhost_dir: &'a str,
    /// 本隧道的云端端口
    pub remote_port: u16,
    /// 环境识别到的 nginx 可执行文件（没有就让脚本自己找）
    pub nginx_bin: Option<&'a str>,
    /// 已经解析出来的访问日志路径（审计用；没有就让脚本按 conf/候选路径找）
    pub log_path: Option<&'a str>,
}

fn sanitize_remote_path(raw: &str, what: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(format!("{what}为空，无法拼进检查脚本"));
    }
    if text.chars().count() > 512 {
        return Err(format!("{what}过长（超过 512 个字符）"));
    }
    if text
        .chars()
        .any(|c| c == '\n' || c == '\r' || c == '`' || c == '\0')
    {
        return Err(format!("{what}中包含换行或反引号，为安全起见无法拼进检查脚本"));
    }
    Ok(text.to_string())
}

/// 生成远端检查脚本（只读命令；域名/路径都先校验再用单引号包好）。
pub fn build_script(input: &ScriptInput<'_>) -> Result<String, String> {
    if !valid_domain(input.domain) {
        return Err(format!(
            "域名「{}」不能直接拼进脚本：只允许字母、数字、点、横杠",
            input.domain
        ));
    }
    let dir = sanitize_remote_path(input.vhost_dir, "站点目录")?;
    let domain = input.domain.trim();
    let dir_q = crate::cleanup::shell_quote(&dir);
    let bin_q = match input.nginx_bin {
        Some(bin) if !bin.trim().is_empty() => {
            crate::cleanup::shell_quote(&sanitize_remote_path(bin, "nginx 路径")?)
        }
        _ => "''".to_string(),
    };
    let log_q = match input.log_path {
        Some(path) if !path.trim().is_empty() => {
            crate::cleanup::shell_quote(&sanitize_remote_path(path, "日志路径")?)
        }
        _ => "''".to_string(),
    };
    let domain_q = crate::cleanup::shell_quote(domain);
    let fill = |tmpl: &str| {
        tmpl.replace("@DOMAIN@", &domain_q)
            .replace("@DIR@", &dir_q)
            .replace("@BIN@", &bin_q)
            .replace("@LOG@", &log_q)
            .replace("@PORT@", &input.remote_port.to_string())
    };

    let mut out = String::new();
    out.push_str("echo SHANGXIAN-CHECK-V2\n");
    out.push_str(SHX_EXPAND_FN);
    for section in input.mode.sections() {
        let tmpl = match *section {
            "ENV" => ENV_TMPL,
            "CONF" => CONF_TMPL,
            "HTPASSWD" => HTPASSWD_TMPL,
            "HTTPS" => HTTPS_TMPL,
            "PORT" => PORT_TMPL,
            "FILES" => FILES_TMPL,
            "NET" => NET_TMPL,
            "AUDIT" => AUDIT_TMPL,
            _ => continue,
        };
        out.push_str(&fill(tmpl));
        out.push('\n');
    }
    out.push_str("echo \"===END===\"\n");
    Ok(out)
}

const ENV_TMPL: &str = r#"echo "===SECTION:ENV==="
panel=unknown; pev=''
if [ -d /www/server/panel ]; then panel=bt; pev='/www/server/panel'
elif [ -d /xp/panel ]; then panel=xp; pev='/xp/panel'
else
  if [ -d /www/wwwroot ] || [ -d @DIR@/nginx ]; then panel=none; else panel=unknown; fi
fi
echo "PANEL=$panel"
echo "PANEL_EVIDENCE=$pev"
has_nginx=0; has_apache=0; nginx_bin=''
if command -v nginx >/dev/null 2>&1; then has_nginx=1; nginx_bin=$(command -v nginx)
elif [ -x /www/server/nginx/sbin/nginx ]; then has_nginx=1; nginx_bin=/www/server/nginx/sbin/nginx
elif [ -x /xp/panel/nginx/sbin/nginx ]; then has_nginx=1; nginx_bin=/xp/panel/nginx/sbin/nginx
fi
if command -v httpd >/dev/null 2>&1 || command -v apache2 >/dev/null 2>&1; then has_apache=1; fi
case "$has_nginx$has_apache" in
  11) web=both;; 10) web=nginx;; 01) web=apache;; *) web=unknown;;
esac
echo "WEB=$web"
[ -n "$nginx_bin" ] && echo "NGINX_BIN=$nginx_bin"
echo "WEB_EVIDENCE=$nginx_bin"
ve=0; [ -d @DIR@ ] && ve=1
echo "VHOST_DIR=@DIR@"
echo "VHOST_DIR_EXISTS=$ve"
nf=0
[ -d /etc/nginx ] && nf=1
[ -x "$nginx_bin" ] && nf=1
[ -d @DIR@/nginx ] && nf=1
echo "WEB_CONF_EXISTS=$nf"
ws=unknown; wsf=0
for f in /etc/nginx/nginx.conf /www/server/nginx/conf/nginx.conf /xp/panel/nginx/conf/nginx.conf; do
  if [ -f "$f" ]; then
    wsf=1
    if grep -q 'connection_upgrade' "$f" 2>/dev/null; then ws=1; break; else ws=0; fi
  fi
done
echo "WS_MAP=$ws""#;

/// 站点配置展开函数：小皮 / 宝塔把证书、强制 HTTPS 写进被 `include` 的独立文件里，
/// 检查脚本要跟着 include 把正文拼回来看——不然会误报「证书文件找不到」、也看不到 80 跳 443。
/// 只读、限 3 层、只认绝对路径、去重防循环，读不到的引用文件安静跳过。放在脚本开头，CONF 与 HTTPS 两节共用。
const SHX_EXPAND_FN: &str = r#"# 展开站点配置里的 include：先打印主文件正文，再按层追加引用到的文件
_shx_expand_conf() {
  _shx_root=$1
  { [ -n "$_shx_root" ] && [ -f "$_shx_root" ]; } || return 0
  cat -- "$_shx_root" 2>/dev/null
  echo
  _shx_seen=" $_shx_root "
  _shx_level="$_shx_root"
  _shx_depth=0
  while [ "$_shx_depth" -lt 3 ] && [ -n "$_shx_level" ]; do
    _shx_depth=$(( _shx_depth + 1 ))
    _shx_next=''
    for _shx_file in $_shx_level; do
      _shx_includes=$(sed -n 's/^[[:space:]]*include[[:space:]]*//p' "$_shx_file" 2>/dev/null | sed 's/#.*$//' | cut -d';' -f1)
      for _shx_item in $_shx_includes; do
        _shx_item=$(printf '%s' "$_shx_item" | tr -d '"')
        case "$_shx_item" in
          /*) ;;
          *) continue ;;
        esac
        case "$_shx_item" in
          *'$'*) continue ;;
        esac
        for _shx_hit in $_shx_item; do
          [ -f "$_shx_hit" ] || continue
          case "$_shx_seen" in
            *" $_shx_hit "*) continue ;;
          esac
          _shx_seen="$_shx_seen$_shx_hit "
          cat -- "$_shx_hit" 2>/dev/null
          echo
          _shx_next="$_shx_next $_shx_hit"
        done
      done
    done
    _shx_level=$_shx_next
  done
}
"#;

const CONF_TMPL: &str = r#"echo "===SECTION:CONF==="
d=@DOMAIN@; dir=@DIR@; f="$dir/nginx/$d.conf"
if [ -f "$f" ]; then
  echo "CONF_FOUND=1"
  echo "CONF_PATH=$f"
  echo '===SHANGXIAN-CONF-BEGIN==='
  _shx_expand_conf "$f" | head -c 65536
  echo
  echo '===SHANGXIAN-CONF-END==='
  echo CONF_FOUND
else
  echo "CONF_FOUND=0"
  echo CONF_MISSING
fi"#;

const HTPASSWD_TMPL: &str = r#"echo "===SECTION:HTPASSWD==="
d=@DOMAIN@; dir=@DIR@
if [ -f "$dir/$d.htpasswd" ]; then
  echo "HTPASSWD_EXISTS=1"
  echo "HTPASSWD_PATH=$dir/$d.htpasswd"
  echo HTPASSWD_YES
else
  echo "HTPASSWD_EXISTS=0"
  echo HTPASSWD_NO
fi"#;

const HTTPS_TMPL: &str = r#"echo "===SECTION:HTTPS==="
d=@DOMAIN@; dir=@DIR@; f="$dir/nginx/$d.conf"; cert=''
if [ -f "$f" ]; then
  certs=$(_shx_expand_conf "$f" | sed -n 's/^[[:space:]]*ssl_certificate[[:space:]]*//p' | cut -d';' -f1)
  for c in $certs; do
    c=$(printf '%s' "$c" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//; s/^"//; s/"$//')
    [ -f "$c" ] && { cert="$c"; break; }
  done
fi
if [ -n "$cert" ]; then
  echo "SSL_CERT_PATH=$cert"
  if command -v openssl >/dev/null 2>&1; then
    end=$(LC_ALL=C openssl x509 -enddate -noout -in "$cert" 2>/dev/null | sed 's/^notAfter=//')
    [ -n "$end" ] && echo "CERT_END_RAW=$end"
    end_epoch=$(LC_ALL=C date -d "$end" +%s 2>/dev/null)
    if [ -n "$end_epoch" ]; then now=$(date +%s); echo "CERT_DAYS_LEFT=$(( (end_epoch - now) / 86400 ))"; fi
    openssl x509 -checkend $((7*86400)) -noout -in "$cert" >/dev/null 2>&1 && echo "CERT_CHECKEND_7=1" || echo "CERT_CHECKEND_7=0"
    openssl x509 -checkend $((30*86400)) -noout -in "$cert" >/dev/null 2>&1 && echo "CERT_CHECKEND_30=1" || echo "CERT_CHECKEND_30=0"
  else
    echo "CERT_OPENSSL=0"
  fi
elif [ -f "$f" ]; then
  echo "CERT_FILE_MISSING=1"
fi"#;

const PORT_TMPL: &str = r#"echo "===SECTION:PORT==="
port=@PORT@
echo "PORT=$port"
if command -v ss >/dev/null 2>&1; then
  echo "PORT_TOOL=ss"
  ss -ltn 2>/dev/null | grep -v '^State' | awk -v p=":$port" '$4 ~ p "$" { print "PORT|" $4 }'
elif command -v netstat >/dev/null 2>&1; then
  echo "PORT_TOOL=netstat"
  netstat -ltn 2>/dev/null | grep -v '^Proto' | awk -v p=":$port" '$4 ~ p "$" { print "PORT|" $4 }'
else
  echo "PORT_TOOL=none"
fi"#;

const FILES_TMPL: &str = r#"echo "===SECTION:FILES==="
d=@DOMAIN@; dir=@DIR@; f="$dir/nginx/$d.conf"
[ -f "$f" ] && echo "NGINX_CONF_FILE=$f"
bin=@BIN@
[ -n "$bin" ] || bin=nginx
if [ -x "$bin" ] || command -v "$bin" >/dev/null 2>&1; then
  out=$("$bin" -T 2>/dev/null | head -c 262144 | awk -v d="$d" '
    /^# configuration file / { file=$0 }
    /server_name/ && index($0, d) { print file }
  ' | sort -u | head -5)
  if [ -n "$out" ]; then
    printf '%s\n' "$out" | sed 's/^/NGINX_CONF_FILE=/'
  else
    echo "NGINX_T_FAILED=1"
  fi
fi
if [ -d "$dir/nginx" ]; then
  grep -rl --include='*.conf' -- "$d" "$dir/nginx" 2>/dev/null | head -5 | sed 's/^/CONF_GREP_HIT=/'
fi"#;

const NET_TMPL: &str = r#"echo "===SECTION:NET==="
ip=$(printf '%s' "${SSH_CONNECTION:-}" | awk '{print $1}')
[ -z "$ip" ] && ip=$(printf '%s' "${SSH_CLIENT:-}" | awk '{print $1}')
[ -n "$ip" ] && echo "SSH_CLIENT_IP=$ip" || echo "SSH_CLIENT_IP=unknown""#;

const AUDIT_TMPL: &str = r#"echo "===SECTION:AUDIT==="
d=@DOMAIN@; dir=@DIR@; f="$dir/nginx/$d.conf"; log=@LOG@
if [ -z "$log" ] && [ -f "$f" ]; then
  # 站点配置里常常有多条 access_log（不少 location 里是 /dev/null）：跳过 dev/null 和 off，取第一条真日志。
  log=$(sed -n 's/^[[:space:]]*access_log[[:space:]]*//p' "$f" | cut -d';' -f1 \
    | sed 's/^[[:space:]]*//; s/[[:space:]]*$//; s/^"//; s/"$//' \
    | grep -v '^/dev/null$' | grep -v '^off$' | head -1)
fi
if [ -z "$log" ] || [ ! -f "$log" ]; then
  log=''
  for c in "$dir/logs/$d.log" "/www/wwwlogs/$d.log" "/xp/panel/vhost/logs/$d.log" "/xp/wwwlogs/$d.log"; do
    [ -f "$c" ] && { log="$c"; break; }
  done
fi
if [ -z "$log" ]; then
  echo "AUDIT_SKIP=no_log_path"
elif [ ! -r "$log" ]; then
  echo "AUDIT_SKIP=log_unreadable"
else
  echo "AUDIT_LOG=$log"
  echo "AUDIT_WINDOW=1000"
  tail -n 1000 -- "$log" 2>/dev/null | LC_ALL=C head -c 2097152 | LC_ALL=C awk '
    { bytes += length($0) + 1
      line = $0
      if (line ~ /"[ \t]+401[ \t]/) { c401++; n = split(line, a, " "); ip = a[1]; if (ip ~ /^[0-9a-fA-F:.]+$/) m[ip]++ }
      else if (line ~ /"[ \t]+403[ \t]/) { c403++ }
    }
    END {
      printf "AUDIT_401=%d\nAUDIT_403=%d\nAUDIT_BYTES=%d\n", c401 + 0, c403 + 0, bytes + 0
      for (k in m) printf "AUDIT_IP=%s:%d\n", k, m[k]
    }' | head -c 131072
  tail -n 30 -- "$log" 2>/dev/null | LC_ALL=C tr -d '\000-\010\013\014\016-\037' | cut -c1-300 | sed 's/^/LOG|/'
fi"#;

// ---------- 结构 ----------

/// 环境识别结果（缓存在 state 的 `servers: {id: CloudEnv}`，D0 第 2 条 / D2 §19.3-A.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudEnv {
    /// `bt`（宝塔）/ `xp`（小皮）/ `none`（有网站目录但没面板）/ `unknown`
    pub panel: String,
    /// `nginx` / `apache` / `both` / `none` / `unknown`
    pub web: String,
    /// `high` / `medium` / `low` / `unknown`
    pub panel_confidence: String,
    /// `high` / `medium` / `low` / `unknown`
    pub web_confidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nginx_bin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vhost_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vhost_dir_exists: Option<bool>,
    /// 全局配置里有没有 `connection_upgrade` 的 map；`None` = 没探出来
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_map: Option<bool>,
    /// 缓存时间（epoch 毫秒）；解析时不填，state 落盘时由调用方填
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_at: Option<i64>,
}

impl Default for CloudEnv {
    fn default() -> Self {
        Self {
            panel: "unknown".into(),
            web: "unknown".into(),
            panel_confidence: "unknown".into(),
            web_confidence: "unknown".into(),
            panel_evidence: None,
            web_evidence: None,
            nginx_bin: None,
            vhost_dir: None,
            vhost_dir_exists: None,
            ws_map: None,
            detected_at: None,
        }
    }
}

impl CloudEnv {
    /// 从 ENV 节的 KEY=value 里拼出结果，并按 D3 §3 的规则算置信度。
    pub fn from_keys(keys: &HashMap<String, String>) -> CloudEnv {
        let panel = match key_str(keys, "PANEL").as_deref() {
            Some("bt") => "bt",
            Some("xp") => "xp",
            Some("none") => "none",
            _ => "unknown",
        }
        .to_string();
        let web = match key_str(keys, "WEB").as_deref() {
            Some("nginx") => "nginx",
            Some("apache") => "apache",
            Some("both") => "both",
            Some("none") => "none",
            _ => "unknown",
        }
        .to_string();
        let panel_evidence = key_str(keys, "PANEL_EVIDENCE");
        let web_evidence = key_str(keys, "WEB_EVIDENCE");
        let nginx_bin = key_str(keys, "NGINX_BIN");
        // 脚本回显 VHOST_DIR 用的是 shell 转义（外层带单引号），这里剥掉再存：
        // 否则界面会显示 '/xp/panel/vhost' 这种带引号的怪值，用户抄进配置还会真的带上引号。
        let vhost_dir = key_str(keys, "VHOST_DIR").map(|d| d.trim_matches('\'').to_string());
        let vhost_dir_exists = key_bool(keys, "VHOST_DIR_EXISTS");
        let ws_map = key_bool(keys, "WS_MAP");
        let conf_dir_exists = key_bool(keys, "WEB_CONF_EXISTS").unwrap_or(false);

        // 置信度规则按 D3 §3 在本地算；如果将来脚本自己带置信度，就优先用脚本给的（只认四个合法值）。
        let explicit_confidence = |name: &str| {
            key_str(keys, name).filter(|v| {
                matches!(v.as_str(), "high" | "medium" | "low" | "unknown")
            })
        };
        let panel_confidence = explicit_confidence("PANEL_CONFIDENCE")
            .unwrap_or_else(|| panel_confidence(&panel, panel_evidence.as_deref(), vhost_dir_exists));
        let has_bin = nginx_bin.is_some() || matches!(web.as_str(), "apache" | "both");
        let web_confidence = explicit_confidence("WEB_CONFIDENCE").unwrap_or_else(|| {
            web_confidence(
                &web,
                has_bin,
                conf_dir_exists,
                panel_evidence.as_deref(),
                vhost_dir_exists,
            )
        });

        CloudEnv {
            panel,
            web,
            panel_confidence,
            web_confidence,
            panel_evidence,
            web_evidence,
            nginx_bin,
            vhost_dir,
            vhost_dir_exists,
            ws_map,
            detected_at: None,
        }
    }
}

/// 面板置信度规则（D3 §3）：
/// - `high`：面板目录存在，而且站点目录也存在；
/// - `medium`：面板目录存在，但站点目录不在；
/// - `low`：没认出面板，但能看到网站目录（`PANEL=none`）；
/// - `unknown`：什么证据都没有。
fn panel_confidence(
    panel: &str,
    panel_evidence: Option<&str>,
    vhost_exists: Option<bool>,
) -> String {
    if panel_evidence.is_some() {
        if vhost_exists == Some(true) {
            "high".to_string()
        } else {
            "medium".to_string()
        }
    } else if panel == "none" {
        "low".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Web 服务置信度规则（D3 §3）：
/// - `high`：面板目录存在，而且站点目录也存在；
/// - `medium`：面板目录存在但站点目录不在；或 web 二进制存在且有 conf 目录；
/// - `low`：只有 web 二进制；或只有站点目录；
/// - `unknown`：什么都没找到。
fn web_confidence(
    web: &str,
    has_bin: bool,
    conf_dir: bool,
    panel_evidence: Option<&str>,
    vhost_exists: Option<bool>,
) -> String {
    if web == "unknown" {
        return "unknown".to_string();
    }
    if panel_evidence.is_some() && vhost_exists == Some(true) {
        return "high".to_string();
    }
    if (panel_evidence.is_some() && vhost_exists != Some(true)) || (has_bin && conf_dir) {
        return "medium".to_string();
    }
    if has_bin || vhost_exists == Some(true) {
        return "low".to_string();
    }
    "unknown".to_string()
}

/// HTTPS 节里远端算好的证书信息（原样值）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HttpsKeys {
    pub ssl_cert_path: Option<String>,
    pub cert_end_raw: Option<String>,
    pub cert_days_left: Option<i64>,
    pub checkend7: Option<bool>,
    pub checkend30: Option<bool>,
    /// 服务器上没装 openssl
    pub openssl_missing: bool,
    /// 配置里写的证书文件在服务器上找不到
    pub cert_file_missing: bool,
}

impl HttpsKeys {
    pub fn from_keys(keys: &HashMap<String, String>, warnings: &mut Vec<String>) -> HttpsKeys {
        let mut out = HttpsKeys {
            ssl_cert_path: key_str(keys, "SSL_CERT_PATH"),
            cert_end_raw: key_str(keys, "CERT_END_RAW"),
            checkend7: key_bool(keys, "CERT_CHECKEND_7"),
            checkend30: key_bool(keys, "CERT_CHECKEND_30"),
            openssl_missing: keys.get("CERT_OPENSSL").map(|v| v.trim()) == Some("0"),
            cert_file_missing: key_bool(keys, "CERT_FILE_MISSING").unwrap_or(false),
            ..HttpsKeys::default()
        };
        if let Some(raw) = keys
            .get("CERT_DAYS_LEFT")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            match raw.parse::<i64>() {
                Ok(days) => out.cert_days_left = Some(days),
                Err(_) => warnings.push(
                    "检查结果中 CERT_DAYS_LEFT 无法解析（服务器日期格式可能不常见），已忽略。"
                        .into(),
                ),
            }
        }
        out
    }
}

/// HTTPS / 证书 / 80 跳转的判断结果（写进 `protectionCheck.extra.https`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpsCheck {
    /// 云端有 443 + ssl（listen 443 ssl 或 ssl on;）
    pub enabled: bool,
    /// 证书剩余天数；None = 没算出来
    pub cert_days: Option<i64>,
    /// 80 会跳到 https（301/308 强跳；302/307 也算，但带黄字）
    pub redirect80: bool,
    /// 80 不是只跳转，还能直接打开（有 proxy_pass）
    pub plain_open: bool,
    /// 用的是临时跳转（302/307 或 rewrite）
    pub temporary_redirect: bool,
    /// `matched`（server_name 对上）/ `single`（只有一个块）/ `none`（没找到）
    pub server_match: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_end_raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkend7: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkend30: Option<bool>,
    pub openssl_missing: bool,
    pub cert_file_missing: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// 云端端口回环核对结果（D3 §8）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortCheck {
    /// `loopback` / `exposed` / `missing` / `unknown`（两者都有按 `exposed`，宁严不松）
    pub bound: String,
    /// `ss` / `netstat` / `none`
    pub tool: String,
    pub port: u16,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl PortCheck {
    /// 没跑 PORT 节时的兜底值。
    pub fn unknown(port: u16) -> PortCheck {
        PortCheck {
            bound: "unknown".into(),
            tool: "none".into(),
            port,
            evidence: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// 从 PORT 节的 KEY 和 `PORT|` 行解析。
    pub fn from_section(
        keys: &HashMap<String, String>,
        lines: &[String],
        expected_port: u16,
        warnings: &mut Vec<String>,
    ) -> PortCheck {
        let tool = match key_str(keys, "PORT_TOOL").as_deref() {
            Some("ss") => "ss",
            Some("netstat") => "netstat",
            _ => "none",
        }
        .to_string();
        let key_port = keys.get("PORT").and_then(|v| v.trim().parse::<u16>().ok());
        if let (Some(reported), true) = (key_port, expected_port != 0) {
            if reported != expected_port {
                warnings.push(format!(
                    "检查脚本回报的云端端口为 {reported}，与应用中填写的 {expected_port} 不一致，已按应用中的端口核对。"
                ));
            }
        }
        let port = if expected_port != 0 {
            expected_port
        } else {
            key_port.unwrap_or(0)
        };

        let mut evidence: Vec<String> = Vec::new();
        let mut any_open = false;
        let mut loopback = false;
        for line in lines {
            let Some(kind) = classify_listen_line(line, port) else {
                continue;
            };
            evidence.push(line.trim().to_string());
            match kind {
                AddrKind::Any | AddrKind::Specific => any_open = true,
                AddrKind::Loopback => loopback = true,
            }
        }
        let bound = if tool == "none" {
            "unknown"
        } else if any_open {
            // 有对外听的（具体网卡也算）：宁严不松
            "exposed"
        } else if loopback {
            "loopback"
        } else {
            "missing"
        };
        PortCheck {
            bound: bound.to_string(),
            tool,
            port,
            evidence,
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddrKind {
    Any,
    Specific,
    Loopback,
}

/// 解析一行 `ss/netstat` 的本地地址（脚本已经只打印地址列）。
fn classify_listen_line(line: &str, port: u16) -> Option<AddrKind> {
    let addr = line.trim();
    if addr.is_empty() {
        return None;
    }
    let last = addr.rsplit(':').next()?;
    if last.parse::<u16>().ok()? != port {
        return None;
    }
    let host_part = addr[..addr.len().saturating_sub(last.len())].trim_end_matches(':');
    let host = host_part
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if host.is_empty() || host == "*" || host == "0.0.0.0" || host == "::" {
        return Some(AddrKind::Any);
    }
    if host == "localhost" {
        return Some(AddrKind::Loopback);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(if ip.is_loopback() {
            AddrKind::Loopback
        } else {
            AddrKind::Specific
        });
    }
    Some(AddrKind::Specific)
}

/// 一条访问日志样本（解析不出来时保留 `raw`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessEntry {
    pub ip: Option<String>,
    pub time: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<u16>,
    pub bytes: Option<i64>,
    pub raw: Option<String>,
}

/// 访问审计结果（D3 §7）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpCount {
    pub ip: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessAudit {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub window: u32,
    pub count401: u64,
    pub count403: u64,
    pub top_ips: Vec<IpCount>,
    pub entries: Vec<AccessEntry>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl AccessAudit {
    pub fn skipped(reason: &str) -> AccessAudit {
        AccessAudit {
            available: false,
            path: None,
            window: 1000,
            count401: 0,
            count403: 0,
            top_ips: Vec::new(),
            entries: Vec::new(),
            truncated: false,
            skip_reason: Some(reason.to_string()),
            warnings: Vec::new(),
        }
    }

    pub fn from_section(
        keys: &HashMap<String, String>,
        log_lines: &[String],
        ip_lines: &[String],
    ) -> AccessAudit {
        let mut warnings: Vec<String> = Vec::new();
        let path = key_str(keys, "AUDIT_LOG");
        let skip_reason = key_str(keys, "AUDIT_SKIP");
        let available = path.is_some() && skip_reason.is_none();
        let window = keys
            .get("AUDIT_WINDOW")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(1000);
        let count401 = key_num(keys, "AUDIT_401", &mut warnings).unwrap_or(0).max(0) as u64;
        let count403 = key_num(keys, "AUDIT_403", &mut warnings).unwrap_or(0).max(0) as u64;
        let bytes = key_num(keys, "AUDIT_BYTES", &mut warnings).unwrap_or(0);
        let truncated = bytes >= MAX_AUDIT_BYTES as i64;

        let mut agg: Vec<(String, u64)> = Vec::new();
        for line in ip_lines {
            let Some((ip_raw, count_raw)) = line.rsplit_once(':') else {
                warnings.push("日志中有一条被挡 IP 统计无法解析，已跳过。".into());
                continue;
            };
            let Ok(count) = count_raw.trim().parse::<u64>() else {
                warnings.push("日志中有一条被挡 IP 统计无法解析，已跳过。".into());
                continue;
            };
            let ip = ip_raw.trim();
            if ip.parse::<IpAddr>().is_err() {
                warnings.push("日志中有一条被挡 IP 统计无法解析，已跳过。".into());
                continue;
            }
            if let Some(slot) = agg.iter_mut().find(|(k, _)| k == ip) {
                slot.1 = slot.1.saturating_add(count);
            } else {
                agg.push((ip.to_string(), count));
            }
        }
        agg.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let top_ips = agg
            .into_iter()
            .take(MAX_TOP_IPS)
            .map(|(ip, count)| IpCount { ip, count })
            .collect();

        let entries: Vec<AccessEntry> = log_lines
            .iter()
            .take(MAX_LOG_SAMPLES)
            .map(|l| parse_access_entry(l))
            .collect();
        if truncated {
            warnings.push("日志仅统计最近 2MB，更早的内容未计入；下面的次数为近似值。".into());
        }
        AccessAudit {
            available,
            path,
            window,
            count401,
            count403,
            top_ips,
            entries,
            truncated,
            skip_reason,
            warnings,
        }
    }
}

/// 解析一条 combined 格式的访问日志；认不出来就保留 `raw` 让界面原样显示。
pub fn parse_access_entry(line: &str) -> AccessEntry {
    let raw = sanitize_log_line(line);
    let mut entry = AccessEntry {
        ip: None,
        time: None,
        method: None,
        path: None,
        status: None,
        bytes: None,
        raw: None,
    };
    let mut rest = raw.as_str();
    if let Some((first, tail)) = rest.split_once(char::is_whitespace) {
        if first.parse::<IpAddr>().is_ok() {
            entry.ip = Some(first.to_string());
        }
        rest = tail;
    }
    let mut after_time = rest;
    if let Some(open) = rest.find('[') {
        if let Some(close) = rest[open + 1..].find(']') {
            let time = rest[open + 1..open + 1 + close].trim();
            if !time.is_empty() {
                entry.time = Some(time.to_string());
            }
            after_time = &rest[open + 1 + close + 1..];
        }
    }
    let mut after_req = after_time;
    let mut request_ok = false;
    if let Some(open) = after_time.find('"') {
        if let Some(close) = after_time[open + 1..].find('"') {
            let request = &after_time[open + 1..open + 1 + close];
            let mut parts = request.split_whitespace();
            if let Some(method) = parts.next() {
                entry.method = Some(method.to_string());
            }
            if let Some(path) = parts.next() {
                entry.path = Some(path.to_string());
            }
            request_ok = entry.method.is_some();
            after_req = &after_time[open + 1 + close + 1..];
        }
    }
    let mut status_ok = false;
    let mut parts = after_req.split_whitespace();
    if let Some(status) = parts.next() {
        if let Ok(code) = status.parse::<u16>() {
            entry.status = Some(code);
            status_ok = true;
        }
    }
    if let Some(bytes) = parts.next() {
        if let Ok(size) = bytes.parse::<i64>() {
            entry.bytes = Some(size);
        }
    }
    if !(request_ok && status_ok && entry.time.is_some()) {
        entry.raw = Some(raw);
    }
    entry
}

fn sanitize_log_line(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect()
}

// ---------- V2 协议解析 ----------

/// V2 解析结果（各节可缺）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct V2Output {
    /// 第一行是不是 `SHANGXIAN-CHECK-V2`；false = 老脚本输出（回退 V1）
    pub protocol_v2: bool,
    /// 配置正文（两条标记之间的内容）
    pub conf: Option<String>,
    pub conf_found: Option<bool>,
    pub conf_path: Option<String>,
    pub htpasswd_exists: Option<bool>,
    pub htpasswd_path: Option<String>,
    pub env: Option<CloudEnv>,
    pub https: HttpsKeys,
    pub port: Option<PortCheck>,
    pub client_ip: Option<String>,
    /// 包含该 server_name 的配置文件路径（去重、已知路径优先）
    pub files: Vec<String>,
    pub nginx_t_failed: bool,
    pub audit: Option<AccessAudit>,
    pub warnings: Vec<String>,
}

impl V2Output {
    /// 这份输出算不算「读到了东西」：完全认不出来时整体按 error 处理。
    pub fn usable(&self) -> bool {
        self.protocol_v2
            || self.conf.is_some()
            || self.conf_found.is_some()
            || self.htpasswd_exists.is_some()
    }
}

#[derive(Debug, Clone, Default)]
struct SectionBuf {
    name: String,
    keys: HashMap<String, String>,
    port_lines: Vec<String>,
    audit_ip_lines: Vec<String>,
    log_lines: Vec<String>,
    file_lines: Vec<String>,
}

/// 解析 V2 检查输出。
///
/// `expected_port` 是本隧道的云端端口（0 = 未知）：`PORT|` 行里的端口必须和它相等，
/// 避免 `:80` 匹配上 `:8080`。
///
/// 规则（D3 §11.2）：未知节/未知键/超长值/乱码一律忽略并给警告；配置正文标记优先级最高，
/// 正文里伪造 `===SECTION:XXX===` 不会把解析带偏；重复键最后一个生效（定位文件/日志/端口行除外）。
pub fn parse_v2(output: &str, expected_port: u16) -> V2Output {
    let mut out = V2Output::default();
    let mut buf = SectionBuf::default();
    let mut in_conf = false;
    let mut conf_seen = false;
    let mut conf = String::new();
    let mut conf_overflow = false;
    let mut header_seen = false;
    let mut legacy_conf: Option<bool> = None;
    let mut legacy_htpasswd: Option<bool> = None;

    if output.trim().is_empty() {
        out.warnings
            .push("云端检查未返回任何内容，连接可能已中断。".into());
        return out;
    }

    for raw_line in output.lines() {
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim();

        // 4) 配置正文优先级最高：正文里的一切都按原始正文处理
        if in_conf {
            if trimmed == CONF_END {
                in_conf = false;
            } else if conf.len() < MAX_CONF_BYTES {
                conf.push_str(line);
                conf.push('\n');
            } else if !conf_overflow {
                conf_overflow = true;
                out.warnings
                    .push("云端配置过长，仅读取了前面一部分。".into());
            }
            continue;
        }

        // 1) 第一非空行必须是版本标记
        if !header_seen {
            if trimmed.is_empty() {
                continue;
            }
            header_seen = true;
            if trimmed == CHECK_V2_MARKER {
                out.protocol_v2 = true;
                continue;
            }
            out.warnings
                .push("检查脚本版本较旧，部分结果无法解析。".into());
        }

        if trimmed == CONF_BEGIN {
            flush_section(&mut buf, &mut out, expected_port);
            in_conf = true;
            conf_seen = true;
            continue;
        }

        // 2) 节头；未知节整节忽略（前向兼容）
        if let Some(name) = section_header(trimmed) {
            flush_section(&mut buf, &mut out, expected_port);
            buf.name = name.to_string();
            continue;
        }

        // 7) 老标记行 V2 解析器忽略（只给 V1 兜底用）
        match trimmed {
            "CONF_FOUND" => {
                legacy_conf = Some(true);
                continue;
            }
            "CONF_MISSING" => {
                legacy_conf = Some(false);
                continue;
            }
            "HTPASSWD_YES" => {
                legacy_htpasswd = Some(true);
                continue;
            }
            "HTPASSWD_NO" => {
                legacy_htpasswd = Some(false);
                continue;
            }
            _ => {}
        }

        if buf.name.is_empty() {
            continue;
        }

        // 5) 累积行：定位文件 / 审计 IP / 日志样本 / 端口证据
        if let Some(rest) = trimmed.strip_prefix("PORT|") {
            if buf.name == "PORT" && buf.port_lines.len() < MAX_PORT_LINES {
                buf.port_lines.push(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("LOG|") {
            if buf.name == "AUDIT" && buf.log_lines.len() < MAX_LOG_LINES {
                buf.log_lines.push(rest.to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("AUDIT_IP=") {
            if buf.name == "AUDIT" && buf.audit_ip_lines.len() < MAX_IP_LINES {
                buf.audit_ip_lines.push(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("NGINX_CONF_FILE=") {
            if buf.name == "FILES" && buf.file_lines.len() < MAX_FILE_LINES {
                buf.file_lines.push(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("CONF_GREP_HIT=") {
            if buf.name == "FILES" && buf.file_lines.len() < MAX_FILE_LINES {
                buf.file_lines.push(rest.trim().to_string());
            }
            continue;
        }

        // 3) 节内 KEY=value
        if let Some((key, value)) = split_key(trimmed) {
            let value = value.trim();
            if value.chars().count() > MAX_VALUE_LEN {
                out.warnings.push(format!(
                    "检查结果中 {key} 的值过长，已忽略（可能存在异常数据）。"
                ));
                continue;
            }
            buf.keys.insert(key.to_string(), value.to_string());
        }
    }

    flush_section(&mut buf, &mut out, expected_port);

    if conf_seen {
        out.conf = Some(conf);
    }
    if !out.protocol_v2 {
        if out.conf_found.is_none() {
            out.conf_found = legacy_conf;
        }
        if out.htpasswd_exists.is_none() {
            out.htpasswd_exists = legacy_htpasswd;
        }
    }
    out
}

fn flush_section(buf: &mut SectionBuf, out: &mut V2Output, expected_port: u16) {
    match buf.name.as_str() {
        "ENV" => out.env = Some(CloudEnv::from_keys(&buf.keys)),
        "CONF" => {
            out.conf_found = key_bool(&buf.keys, "CONF_FOUND").or(out.conf_found);
            out.conf_path = key_str(&buf.keys, "CONF_PATH").or_else(|| out.conf_path.take());
        }
        "HTPASSWD" => {
            out.htpasswd_exists =
                key_bool(&buf.keys, "HTPASSWD_EXISTS").or(out.htpasswd_exists);
            out.htpasswd_path =
                key_str(&buf.keys, "HTPASSWD_PATH").or_else(|| out.htpasswd_path.take());
        }
        "HTTPS" => out.https = HttpsKeys::from_keys(&buf.keys, &mut out.warnings),
        "PORT" => {
            out.port = Some(PortCheck::from_section(
                &buf.keys,
                &buf.port_lines,
                expected_port,
                &mut out.warnings,
            ))
        }
        "FILES" => {
            out.files = merge_files(&buf.file_lines);
            out.nginx_t_failed = key_bool(&buf.keys, "NGINX_T_FAILED").unwrap_or(false);
        }
        "NET" => {
            out.client_ip = parse_client_ip(&buf.keys, &mut out.warnings);
        }
        "AUDIT" => {
            out.audit = Some(AccessAudit::from_section(
                &buf.keys,
                &buf.log_lines,
                &buf.audit_ip_lines,
            ))
        }
        _ => {}
    }
    *buf = SectionBuf::default();
}

fn section_header(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("===SECTION:")?;
    let name = rest.strip_suffix("===")?;
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        Some(name)
    } else {
        None
    }
}

fn split_key(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    if key.is_empty() || !key.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    Some((key, value))
}

fn parse_client_ip(keys: &HashMap<String, String>, warnings: &mut Vec<String>) -> Option<String> {
    let raw = keys.get("SSH_CLIENT_IP")?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("unknown") {
        return None;
    }
    if raw.parse::<IpAddr>().is_ok() {
        Some(raw.to_string())
    } else {
        warnings.push(format!("服务器回报的上网 IP「{raw}」不是有效 IP，已忽略。"));
        None
    }
}

fn merge_files(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let text = line.trim();
        if text.is_empty() || text.chars().any(|c| c.is_control()) {
            continue;
        }
        if !out.iter().any(|x| x == text) {
            out.push(text.to_string());
        }
        if out.len() >= MAX_FILE_LINES {
            break;
        }
    }
    out
}

fn key_str(keys: &HashMap<String, String>, name: &str) -> Option<String> {
    keys.get(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn key_bool(keys: &HashMap<String, String>, name: &str) -> Option<bool> {
    parse_bool(keys.get(name).map(String::as_str).unwrap_or(""))
}

fn key_num(keys: &HashMap<String, String>, name: &str, warnings: &mut Vec<String>) -> Option<i64> {
    let raw = keys.get(name)?.trim();
    match raw.parse::<i64>() {
        Ok(n) => Some(n),
        Err(_) => {
            warnings.push(format!("检查结果中 {name} 不是数字，已忽略。"));
            None
        }
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

// ---------- HTTPS 判断 ----------

fn selected_blocks<'a>(
    blocks: &'a [ServerBlock],
    domain: &str,
) -> (Vec<&'a ServerBlock>, &'static str) {
    match protection::pick_server(blocks, domain) {
        ServerPick::Matched(blocks) => (blocks, "matched"),
        ServerPick::Single(block) => (vec![block], "single"),
        ServerPick::None => (Vec::new(), "none"),
    }
}

/// 从站点配置 + HTTPS 节的证书信息里算出 HTTPS 三件套（D3 §4）。
pub fn analyze_https(conf: &str, domain: &str, keys: &HttpsKeys) -> HttpsCheck {
    let blocks = protection::scan_servers(conf);
    let (selected, server_match) = selected_blocks(&blocks, domain);
    let mut warnings: Vec<String> = Vec::new();
    match server_match {
        "single" => warnings.push("站点配置中只有一个 server 块，下面的 HTTPS 判断将以它为准。".into()),
        "none" if !blocks.is_empty() => warnings.push(
            "未找到 server_name 与网址匹配的站点块，下面的 HTTPS 判断可能不准确。".into(),
        ),
        _ => {}
    }
    if blocks.is_empty() {
        warnings.push("云端站点配置中未读取到 server 块，HTTPS 判断跳过。".into());
    }

    let enabled = selected
        .iter()
        .any(|b| b.ssl_on || b.listens.iter().any(|l| l.ssl && l.port == 443));
    let http_blocks: Vec<&ServerBlock> = selected
        .iter()
        .copied()
        .filter(|b| b.listens_on(80))
        .collect();
    let mut redirect80 = false;
    let mut temporary_redirect = false;
    for block in &http_blocks {
        match block.redirect_to_https() {
            Some(RedirectKind::Permanent) => redirect80 = true,
            Some(RedirectKind::Temporary) => {
                redirect80 = true;
                temporary_redirect = true;
            }
            None => {}
        }
    }
    // 小皮面板把「强制 HTTPS」的 if 块放在被 include 的独立文件里；脚本把 include
    // 的内容拼回文本后，这段会落在 server 块外面——这里兜一下根级的跳转 if。
    // 块里面的 if 已经在 redirect_to_https() 里看过了。
    if !redirect80 && !http_blocks.is_empty() {
        if let Some(kind) = protection::root_if_https_redirect(conf) {
            redirect80 = true;
            temporary_redirect = kind == RedirectKind::Temporary;
        }
    }
    let plain_open = http_blocks
        .iter()
        .any(|b| b.has_proxy_pass && b.redirect_to_https().is_none());

    let conf_cert = selected
        .iter()
        .flat_map(|b| b.ssl_certificates.iter())
        .next()
        .cloned();
    let cert_path = keys.ssl_cert_path.clone().or(conf_cert);

    if temporary_redirect {
        warnings.push(
            "80 的跳转是临时的（302/307 或 rewrite），建议改为 301 永久跳转。".into(),
        );
    }
    if keys.openssl_missing {
        warnings.push(
            "服务器未安装 openssl，证书剩余天数未计算；不影响其他检查，请在面板中查看证书到期时间。"
                .into(),
        );
    }
    if keys.cert_file_missing {
        warnings.push(
            "配置中写的证书文件在服务器上未找到，可能是续期后路径已变更；请在面板「SSL」页确认。"
                .into(),
        );
    }

    HttpsCheck {
        enabled,
        cert_days: keys.cert_days_left,
        redirect80,
        plain_open,
        temporary_redirect,
        server_match: server_match.to_string(),
        cert_path,
        cert_end_raw: keys.cert_end_raw.clone(),
        checkend7: keys.checkend7,
        checkend30: keys.checkend30,
        openssl_missing: keys.openssl_missing,
        cert_file_missing: keys.cert_file_missing,
        warnings,
    }
}

/// 续期豁免行在不在（D3 §5 / §12）：有 `location /.well-known/` 且（有密码时）有
/// `auth_basic off`、（有白名单时）有 `allow all`。
pub fn wellknown_exempt_ok(
    conf: &str,
    domain: &str,
    need_password: bool,
    need_ip: bool,
) -> bool {
    if !need_password && !need_ip {
        return true;
    }
    let blocks = protection::scan_servers(conf);
    let (selected, _) = selected_blocks(&blocks, domain);
    selected.iter().any(|b| {
        b.wellknown()
            .map(|loc| {
                (!need_password || loc.auth_basic_off) && (!need_ip || loc.allow_all)
            })
            .unwrap_or(false)
    })
}

// ---------- 红黄灯（纯函数） ----------

/// 红黄绿灯。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Light {
    Green,
    Yellow,
    Red,
}

/// 一条带灯色的判断（界面按 level 上色，text 是大白话）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightItem {
    pub level: Light,
    pub text: String,
}

impl LightItem {
    fn new(level: Light, text: impl Into<String>) -> LightItem {
        LightItem {
            level,
            text: text.into(),
        }
    }
}

/// HTTPS / 证书 / 明文访问的红黄灯（D3 §4 表）。
pub fn https_lights(
    https: &HttpsCheck,
    url_is_https: bool,
    password_on: bool,
    ip_on: bool,
) -> Vec<LightItem> {
    let mut red: Vec<LightItem> = Vec::new();
    let mut yellow: Vec<LightItem> = Vec::new();
    let mut green: Vec<LightItem> = Vec::new();

    if url_is_https && !https.enabled {
        red.push(LightItem::new(
            Light::Red,
            "网址写的是 https，但服务器上未找到 443 站点。外部无法访问，请先在面板中开启证书和 https。",
        ));
    }
    if https.plain_open && !https.redirect80 {
        if password_on {
            red.push(LightItem::new(
                Light::Red,
                "密码目前可通过 http:// 明文提交，相当于以明文传输密码。请在面板中打开「强制 HTTPS」（80 跳 443）。",
            ));
        } else if ip_on {
            yellow.push(LightItem::new(
                Light::Yellow,
                "网站用 http:// 打开时内容不加密。虽然只有白名单中的 IP 能访问，仍建议在面板中打开「强制 HTTPS」。",
            ));
        } else {
            yellow.push(LightItem::new(
                Light::Yellow,
                "这个网站 http:// 和 https:// 都能打开。建议在面板中打开「强制 HTTPS」，让 http:// 自动跳转到 https://。",
            ));
        }
    }
    match https.cert_days {
        Some(days) if days <= 0 => red.push(LightItem::new(
            Light::Red,
            "证书已过期，网页会显示警告。请在面板中点击「续签证书」。",
        )),
        Some(days) if days <= 7 => red.push(LightItem::new(
            Light::Red,
            format!(
                "证书剩余 {days} 天，即将到期。请在面板中点击「续签证书」；续期用的 /.well-known 那一小块不要删除。"
            ),
        )),
        Some(days) if days <= 30 => yellow.push(LightItem::new(
            Light::Yellow,
            format!("证书剩余 {days} 天，请注意。面板通常会自动续期，续期失败会显示红灯。"),
        )),
        Some(_) => {}
        None => match https.checkend30 {
            Some(true) => green.push(LightItem::new(
                Light::Green,
                "证书至少 30 天内有效（具体剩余天数未计算出：服务器 date 命令无法解析该日期格式）。",
            )),
            Some(false) => yellow.push(LightItem::new(
                Light::Yellow,
                "证书 30 天内到期（可能已过期）。请在面板中查看到期时间。",
            )),
            None => {}
        },
    }
    if https.openssl_missing {
        yellow.push(LightItem::new(
            Light::Yellow,
            "服务器未安装 openssl，证书剩余天数未计算。不影响其他检查，请在面板中查看证书到期时间。",
        ));
    }
    if https.cert_file_missing {
        yellow.push(LightItem::new(
            Light::Yellow,
            "配置中写的证书文件在服务器上未找到，可能是续期后路径已变更。请在面板「SSL」页确认。",
        ));
    }
    if !url_is_https && https.enabled && https.redirect80 {
        yellow.push(LightItem::new(
            Light::Yellow,
            "服务器已支持 https，建议把网址改为以 https:// 开头的地址。",
        ));
    }
    if https.enabled && https.redirect80 && red.is_empty() && yellow.is_empty() {
        green.push(LightItem::new(
            Light::Green,
            "HTTPS 正常：443 端口已开启，http:// 会自动跳转到 https://。",
        ));
    }

    red.into_iter().chain(yellow).chain(green).collect()
}

/// 云端环境横幅（D3 §3 文案）。
pub fn env_banner(env: &CloudEnv) -> EnvBanner {
    let dir = env
        .vhost_dir
        .as_deref()
        .unwrap_or("/xp/panel/vhost")
        .to_string();
    match env.web.as_str() {
        "apache" => EnvBanner {
            level: Light::Red,
            text: "这台服务器运行的是 Apache，不是 nginx。本应用生成的是 nginx 配置片段，粘贴后不会生效。两个办法：① 在面板中新建站点时选择 nginx（宝塔/小皮都支持）；② 先保留片段，切换为 nginx 后再使用。".into(),
        },
        "both" => EnvBanner {
            level: Light::Yellow,
            text: "这台服务器上 nginx 和 Apache 都在。粘贴前请先确认站点由哪一个管理（面板的网站列表通常会标注 nginx 还是 Apache）。".into(),
        },
        "none" => EnvBanner {
            level: Light::Yellow,
            text: "未找到 nginx，也没有 Apache。可能未安装网站服务，或安装在特殊路径。这种情况下先不要粘贴片段。".into(),
        },
        "unknown" => EnvBanner {
            level: Light::Yellow,
            text: "无法识别这台服务器使用的面板和网站服务（nginx 和 Apache 都未找到）。先不要粘贴片段，以免出错；可以在服务器上执行 nginx -v 或 httpd -v 查看，再点击「重新识别」。".into(),
        },
        _ => match env.panel.as_str() {
            "bt" => EnvBanner {
                level: Light::Green,
                text: format!(
                    "已识别云服务器：宝塔面板 + nginx。站点目录已自动填写为 {dir}，不正确可以修改。"
                ),
            },
            "xp" => EnvBanner {
                level: Light::Green,
                text: format!(
                    "已识别云服务器：小皮面板 + nginx。站点目录已自动填写为 {dir}，不正确可以修改。"
                ),
            },
            "none" => EnvBanner {
                level: Light::Yellow,
                text: "这台服务器未发现面板，但已安装 nginx。需要自行修改 nginx 的站点配置文件；下面给出的片段仍然可以使用。".into(),
            },
            _ => EnvBanner {
                level: Light::Yellow,
                text: format!(
                    "已找到服务器上的 nginx，但无法识别使用的面板。站点目录先按 {dir} 使用，不正确可在服务器设置中修改。"
                ),
            },
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvBanner {
    pub level: Light,
    pub text: String,
}

/// 端口回环的红黄灯（D3 §8 表）。
pub fn port_lights(port: &PortCheck, tunnel_running: bool) -> Vec<LightItem> {
    match port.bound.as_str() {
        "exposed" => vec![LightItem::new(
            Light::Red,
            format!(
                "云端端口 {} 当前对所有网卡开放。发布本身只绑定 127.0.0.1（回环地址＝只有服务器自身能连接的地址），说明有其他程序也占用了这个端口、并且对外开放。请检查该程序，或用防火墙关闭这个端口。",
                port.port
            ),
        )],
        "missing" => {
            if tunnel_running {
                vec![LightItem::new(
                    Light::Yellow,
                    format!(
                        "云端未看到 {} 在监听。可能是转发未建立（请查看隧道日志），也可能端口填写错误。",
                        port.port
                    ),
                )]
            } else {
                vec![LightItem::new(
                    Light::Yellow,
                    format!(
                        "云端未看到 {} 在监听；隧道当前未运行，请先启动隧道再核对。",
                        port.port
                    ),
                )]
            }
        }
        "loopback" => vec![LightItem::new(
            Light::Green,
            "端口只绑定在回环地址（127.0.0.1），外部无法连接，符合预期。",
        )],
        _ => vec![LightItem::new(
            Light::Yellow,
            "服务器上未安装 ss 和 netstat，回环检查跳过；不影响其他检查。",
        )],
    }
}

/// 访问审计的摘要（大白话；次数是近似值）。
pub fn audit_summary(audit: &AccessAudit) -> String {
    if !audit.available {
        return "未找到访问日志（也可能日志仅 root 可读），本步骤跳过。".into();
    }
    let mut text = if audit.count401 == 0 && audit.count403 == 0 {
        format!(
            "最近 {} 行日志中没有被挡的访问。",
            audit.window
        )
    } else {
        let mut s = format!(
            "最近 {} 行日志中，密码错误被拦截 {} 次，白名单限制被拦截 {} 次。",
            audit.window, audit.count401, audit.count403
        );
        if let Some(top) = audit.top_ips.first() {
            s.push_str(&format!(
                "次数最多的是 {}（{} 次）。",
                top.ip, top.count
            ));
        }
        s
    };
    text.push_str("这些记录仅在本地显示，不会上传到任何地方。");
    text
}

/// 公网 IP 助手的文案（D3 §9）。
pub fn public_ip_message(ip: Option<&str>, already_listed: bool) -> String {
    let Some(ip) = ip else {
        return "本次未读取到本机上网 IP（服务器未提供 $SSH_CONNECTION）；可点击「重新识别」再试。".into();
    };
    let listed = if already_listed {
        "这个 IP 已在白名单中。"
    } else {
        "如需将该 IP 加入白名单，点击即可（仅修改本机配置）。"
    };
    format!(
        "服务器识别到的本机上网 IP 是 {ip}。{listed}注意：手机流量、公司网络、重启宽带后这个 IP 可能会变化；如果添加后仍无法访问，可能是运营商共享 IP，改用密码保护更省心。"
    )
}

/// 这个公网 IP 是不是已经在白名单里（本地计算，不改服务器）。
pub fn public_ip_already_listed(ip: &str, allow_entries: &[String]) -> bool {
    let Ok(addr) = ip.trim().parse::<IpAddr>() else {
        return false;
    };
    allow_entries
        .iter()
        .any(|entry| ip_in_allow_entry(&addr, entry))
}

/// 一个 IP 是否落在某条 `allow` 项里（单个 IP 或 CIDR）。
pub fn ip_in_allow_entry(ip: &IpAddr, entry: &str) -> bool {
    if !crate::nginx::valid_allow_entry(entry) {
        return false;
    }
    if let Some((addr, prefix)) = entry.trim().split_once('/') {
        let (Ok(net), Ok(prefix)) = (addr.parse::<IpAddr>(), prefix.parse::<u32>()) else {
            return false;
        };
        prefix_matches(ip, &net, prefix)
    } else {
        entry
            .trim()
            .parse::<IpAddr>()
            .map(|net| net == *ip)
            .unwrap_or(false)
    }
}

fn prefix_matches(ip: &IpAddr, net: &IpAddr, prefix: u32) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            if prefix > 32 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(*a) & mask) == (u32::from(*b) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            if prefix > 128 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(*a) & mask) == (u128::from(*b) & mask)
        }
        _ => false,
    }
}

// ---------- 一次核对的完整返回（D2 §4.1 的结构定义在 state 模块） ----------

/// 一次云端核对的完整返回（D3 §2.2）：
/// 外层是瞬态对象，需要留下次打开还在的部分写进 `check.extra`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCheckResult {
    pub check: ProtectionCheck,
    pub actual: AccessCheck,
    pub port: PortCheck,
    pub files: Vec<String>,
    pub env: Option<CloudEnv>,
    pub client_ip: Option<String>,
    pub audit: Option<AccessAudit>,
}

/// 漂移判定的纯函数（D3 §12 / D2 §4.1 的 `driftFields` 取值）。
///
/// 只看「配置类」差异：配置找不找得到、密码开没开、白名单开没开、白名单 IP 集合、
/// 密码文件在不在、`.well-known` 续期豁免行在不在（映射到 "conf"）。
/// HTTPS/端口只进 warnings，不算漂移。
pub fn drift_fields(
    expected: &ExpectedProtection,
    actual: &AccessCheck,
    wellknown_exempt_ok: bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !expected.url.trim().is_empty() && !actual.conf_found {
        out.push("conf".into());
    }
    if expected.password != actual.password_on {
        out.push("password".into());
    }
    if expected.ip != actual.ip_on {
        out.push("ip".into());
    }
    if expected.ip && actual.ip_on {
        let want = normalize_allow_list(&expected.allow_ips);
        let got = normalize_allow_list(&actual.allow_ips);
        if want != got {
            out.push("allowIps".into());
        }
    }
    if expected.password && !actual.htpasswd_exists {
        out.push("htpasswd".into());
    }
    if (expected.password || expected.ip) && !wellknown_exempt_ok {
        out.push("conf".into());
    }
    out.sort();
    out.dedup();
    out
}

fn normalize_allow_list(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = list
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 组装一次检查需要的输入（从本机配置/参数来，不联网）。
#[derive(Debug, Clone)]
pub struct CheckInput<'a> {
    /// 规范化后的域名
    pub domain: &'a str,
    /// 应用里填的网址是不是 https://
    pub url_is_https: bool,
    /// 应用里开着密码保护
    pub expected_password: bool,
    /// 应用里开着 IP 白名单
    pub expected_ip: bool,
    pub expected_allow_ips: &'a [String],
    /// 本隧道的云端端口（0 = 未知）
    pub remote_port: u16,
    /// `"manual" | "auto"`
    pub source: &'a str,
    /// 检查时刻（epoch 毫秒）
    pub ts: i64,
    /// 环境识别的缓存结果（full 模式脚本不带 ENV，用缓存补）
    pub cached_env: Option<CloudEnv>,
}

/// 把远端输出组装成 [`CloudCheckResult`]（含 D2 骨架、漂移判定、extra 透传）。
pub fn assemble_v2(output: &str, input: &CheckInput<'_>) -> CloudCheckResult {
    let parsed = parse_v2(output, input.remote_port);
    let conf = parsed.conf.clone().unwrap_or_default();
    // 复用现有纯解析器：它只认两条正文标记之间的配置和标记外的结果行。
    let actual = protection::parse(output);
    let https = analyze_https(&conf, input.domain, &parsed.https);
    let port = parsed
        .port
        .clone()
        .unwrap_or_else(|| PortCheck::unknown(input.remote_port));
    let files = parsed.files.clone();
    let env = parsed.env.clone().or_else(|| input.cached_env.clone());
    let client_ip = parsed.client_ip.clone();
    let audit = parsed.audit.clone();

    let expected = ExpectedProtection {
        password: input.expected_password,
        ip: input.expected_ip,
        allow_ips: input.expected_allow_ips.to_vec(),
        url: input.domain.to_string(),
    };
    let wellknown_ok = wellknown_exempt_ok(
        &conf,
        input.domain,
        input.expected_password,
        input.expected_ip,
    );
    let drift = drift_fields(&expected, &actual, wellknown_ok);

    let mut warnings: Vec<String> = Vec::new();
    warnings.extend(parsed.warnings.iter().cloned());
    warnings.extend(https.warnings.iter().cloned());
    warnings.extend(port.warnings.iter().cloned());
    if let Some(audit) = &audit {
        warnings.extend(audit.warnings.iter().cloned());
    }
    if !wellknown_ok && (input.expected_password || input.expected_ip) {
        warnings.push(
            "云端配置中未找到证书续期的豁免行。证书续期可能失败，建议按上面片段补充。".into(),
        );
    }

    let usable = parsed.usable();
    let (status, error) = if !usable {
        (
            "error".to_string(),
            Some(
                "本次云端检查未完整读取（无法识别脚本输出）。已保留上次结果；可以再点击一次「检查云端」。"
                    .to_string(),
            ),
        )
    } else if !drift.is_empty() {
        ("drift".to_string(), None)
    } else {
        ("ok".to_string(), None)
    };

    let cloud = if usable {
        Some(CloudProtection {
            conf_found: actual.conf_found,
            password_on: actual.password_on,
            ip_on: actual.ip_on,
            allow_ips: actual.allow_ips.clone(),
            htpasswd_exists: actual.htpasswd_exists,
        })
    } else {
        None
    };

    let extra = serde_json::json!({
        "https": https,
        "port": port,
        "files": files,
        "env": env,
        "clientIp": client_ip,
        "audit": audit,
        "warnings": warnings,
    });

    CloudCheckResult {
        check: ProtectionCheck {
            status,
            ts: Some(input.ts),
            source: Some(input.source.to_string()),
            cloud,
            expected,
            drift_fields: drift,
            error,
            extra: Some(extra),
        },
        actual,
        port,
        files,
        env,
        client_ip,
        audit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ---------- golden 回放 ----------

    /// D3 §11.1 的协议样例（full + audit 合在一起，方便一次覆盖所有节）。
    const GOLDEN: &str = r#"SHANGXIAN-CHECK-V2
===SECTION:ENV===
PANEL=bt
PANEL_EVIDENCE=/www/server/panel
WEB=nginx
WEB_CONFIDENCE=high
NGINX_BIN=/www/server/nginx/sbin/nginx
VHOST_DIR=/www/server/panel/vhost
VHOST_DIR_EXISTS=1
WEB_CONF_EXISTS=1
WS_MAP=1
===SECTION:CONF===
CONF_FOUND=1
CONF_PATH=/www/server/panel/vhost/nginx/album.example.com.conf
===SHANGXIAN-CONF-BEGIN===
server {
    listen 80;
    server_name album.example.com;
    access_log /www/wwwlogs/album.example.com.log;
    return 301 https://$host$request_uri;
}
server {
    listen 443 ssl http2;
    server_name album.example.com;
    ssl_certificate /www/server/panel/vhost/cert/album.example.com/fullchain.pem;
    auth_basic "需要密码";
    auth_basic_user_file /www/server/panel/vhost/album.example.com.htpasswd;
    allow 1.2.3.4;
    deny all;
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
}
===SHANGXIAN-CONF-END===
CONF_FOUND
===SECTION:HTPASSWD===
HTPASSWD_EXISTS=1
HTPASSWD_PATH=/www/server/panel/vhost/album.example.com.htpasswd
HTPASSWD_YES
===SECTION:HTTPS===
SSL_CERT_PATH=/www/server/panel/vhost/cert/album.example.com/fullchain.pem
CERT_END_RAW=Nov 18 12:00:00 2026 GMT
CERT_DAYS_LEFT=67
CERT_CHECKEND_7=1
CERT_CHECKEND_30=1
===SECTION:PORT===
PORT=18080
PORT_TOOL=ss
PORT|127.0.0.1:18080
===SECTION:FILES===
NGINX_CONF_FILE=/www/server/panel/vhost/nginx/album.example.com.conf
===SECTION:NET===
SSH_CLIENT_IP=203.0.113.7
===SECTION:AUDIT===
AUDIT_LOG=/www/wwwlogs/album.example.com.log
AUDIT_WINDOW=1000
AUDIT_401=12
AUDIT_403=3
AUDIT_IP=1.2.3.4:9
AUDIT_IP=5.6.7.8:4
LOG|1.2.3.4 - - [12/Oct/2026:03:10:00 +0800] "GET / HTTP/1.1" 401 0 "-" "curl/8.0"
LOG|这台机器的日志格式很奇怪
===END===
"#;

    #[test]
    fn golden_output_parses_all_sections() {
        let out = parse_v2(GOLDEN, 18080);
        assert!(out.protocol_v2, "第一行版本标记要认出来");
        assert!(out.usable());

        let env = out.env.as_ref().expect("要有环境识别");
        assert_eq!(env.panel, "bt");
        assert_eq!(env.web, "nginx");
        assert_eq!(env.panel_evidence.as_deref(), Some("/www/server/panel"));
        assert_eq!(env.panel_confidence, "high");
        assert_eq!(env.web_confidence, "high");
        assert_eq!(env.ws_map, Some(true));
        assert_eq!(
            env.nginx_bin.as_deref(),
            Some("/www/server/nginx/sbin/nginx")
        );

        assert_eq!(out.conf_found, Some(true));
        assert_eq!(
            out.conf_path.as_deref(),
            Some("/www/server/panel/vhost/nginx/album.example.com.conf")
        );
        let conf = out.conf.as_deref().unwrap();
        assert!(conf.contains("listen 443 ssl http2;"));
        assert!(conf.contains("location /.well-known/"));

        assert_eq!(out.htpasswd_exists, Some(true));
        assert_eq!(out.https.cert_days_left, Some(67));
        assert_eq!(out.https.checkend30, Some(true));

        let port = out.port.as_ref().unwrap();
        assert_eq!(port.bound, "loopback");
        assert_eq!(port.tool, "ss");
        assert_eq!(port.evidence, vec!["127.0.0.1:18080"]);

        assert_eq!(out.files, vec!["/www/server/panel/vhost/nginx/album.example.com.conf"]);
        assert_eq!(out.client_ip.as_deref(), Some("203.0.113.7"));

        let audit = out.audit.as_ref().unwrap();
        assert!(audit.available);
        assert_eq!(audit.count401, 12);
        assert_eq!(audit.count403, 3);
        assert_eq!(audit.top_ips[0].ip, "1.2.3.4");
        assert_eq!(audit.top_ips[0].count, 9);
        assert_eq!(audit.entries.len(), 2);
        assert_eq!(audit.entries[0].status, Some(401));
        assert_eq!(audit.entries[0].method.as_deref(), Some("GET"));
        assert!(audit.entries[1].raw.is_some(), "认不出来的行要保留原文");
    }

    #[test]
    fn golden_conf_produces_expected_checks() {
        let input = CheckInput {
            domain: "album.example.com",
            url_is_https: true,
            expected_password: true,
            expected_ip: true,
            expected_allow_ips: &["1.2.3.4".to_string()],
            remote_port: 18080,
            source: "manual",
            ts: 1791234567890,
            cached_env: None,
        };
        let result = assemble_v2(GOLDEN, &input);
        assert_eq!(result.check.status, "ok", "配置和应用里一致：{:?}", result.check);
        assert!(result.check.drift_fields.is_empty());
        assert!(result.actual.password_on && result.actual.ip_on);
        assert!(result.actual.htpasswd_exists);
        assert!(!result.port.evidence.is_empty());
        assert_eq!(result.client_ip.as_deref(), Some("203.0.113.7"));

        let extra = result.check.extra.as_ref().unwrap();
        assert_eq!(extra["https"]["enabled"], serde_json::json!(true));
        assert_eq!(extra["https"]["redirect80"], serde_json::json!(true));
        assert_eq!(extra["https"]["certDays"], serde_json::json!(67));
        assert_eq!(extra["port"]["bound"], serde_json::json!("loopback"));
    }

    // ---------- 对抗用例：配置正文伪造 SECTION 标记 ----------

    const TRAP: &str = r#"SHANGXIAN-CHECK-V2
===SECTION:ENV===
PANEL=xp
WEB=nginx
VHOST_DIR=/xp/panel/vhost
VHOST_DIR_EXISTS=1
WEB_CONF_EXISTS=1
WS_MAP=0
===SECTION:CONF===
CONF_FOUND=1
===SHANGXIAN-CONF-BEGIN===
server {
    server_name trap.example.com;
    # ===SECTION:AUDIT===
    # PANEL=bt
    # PANEL_EVIDENCE=/www/server/panel
    # LOG|9.9.9.9 - - [01/Jan/2020:00:00:00 +0000] "GET / HTTP/1.1" 401 0
    # PORT|0.0.0.0:18080
    # CONF_FOUND=0
    # HTPASSWD_NO
    return 200 "===SECTION:HTTPS===
CERT_DAYS_LEFT=1
===END===";
}
===SHANGXIAN-CONF-END===
CONF_FOUND
===SECTION:HTTPS===
CERT_DAYS_LEFT=42
===SECTION:PORT===
PORT=18080
PORT_TOOL=ss
PORT|127.0.0.1:18080
PORT|0.0.0.0:9999
===SECTION:NET===
SSH_CLIENT_IP=not-an-ip
===SECTION:AUDIT===
AUDIT_LOG=/var/log/real.log
AUDIT_401=0
AUDIT_IP=1.2.3.4:2
LOG|1.2.3.4 - - [12/Oct/2026:03:10:00 +0800] "GET / HTTP/1.1" 403 12
===END===
"#;

    #[test]
    fn conf_body_cannot_hijack_sections() {
        let out = parse_v2(TRAP, 18080);
        // 正文里的 SECTION 标记整段按正文处理，不能把后面的节解析带偏
        let conf = out.conf.as_deref().unwrap();
        assert!(conf.contains("===SECTION:AUDIT==="), "正文要原样保留");
        assert!(conf.contains(r#""===SECTION:HTTPS==="#));

        let env = out.env.as_ref().expect("ENV 节要按真实的解析");
        assert_eq!(env.panel, "xp", "正文里的 PANEL=bt 不能覆盖真实节");
        assert_eq!(env.panel_evidence, None);
        assert_eq!(out.https.cert_days_left, Some(42), "正文里的 CERT_DAYS_LEFT 不能覆盖");
        assert_eq!(out.conf_found, Some(true), "正文里的 CONF_FOUND=0 不能覆盖");

        let port = out.port.as_ref().unwrap();
        assert_eq!(port.bound, "loopback");
        assert_eq!(port.evidence, vec!["127.0.0.1:18080"], "别的端口行不能进证据");

        let audit = out.audit.as_ref().unwrap();
        assert_eq!(audit.entries.len(), 1, "正文里的 LOG| 不能被收进审计");
        assert_eq!(audit.count401, 0);
        assert_eq!(audit.top_ips.len(), 1);

        assert!(out.client_ip.is_none());
        assert!(
            out.warnings.iter().any(|w| w.contains("不是有效 IP")),
            "非法 IP 要有警告：{:?}",
            out.warnings
        );
    }

    // ---------- V1 兼容 ----------

    #[test]
    fn legacy_v1_output_still_parses() {
        let legacy = r#"===SHANGXIAN-CONF-BEGIN===
server {
    server_name album.example.com;
    auth_basic "需要密码";
    auth_basic_user_file /xp/panel/vhost/album.example.com.htpasswd;
}
===SHANGXIAN-CONF-END===
CONF_FOUND
HTPASSWD_YES
"#;
        let out = parse_v2(legacy, 18080);
        assert!(!out.protocol_v2, "老脚本没有版本行");
        assert!(
            out.warnings.iter().any(|w| w.contains("版本较旧")),
            "要有版本警告：{:?}",
            out.warnings
        );
        assert_eq!(out.conf_found, Some(true));
        assert_eq!(out.htpasswd_exists, Some(true));
        assert!(out.conf.as_deref().unwrap().contains("auth_basic"));
        let check = protection::parse(legacy);
        assert!(check.password_on);
    }

    #[test]
    fn missing_conf_markers_give_conf_false() {
        let text = format!("{CHECK_V2_MARKER}\n===SECTION:CONF===\nCONF_FOUND=0\nCONF_MISSING\n===END===\n");
        let out = parse_v2(&text, 18080);
        assert_eq!(out.conf_found, Some(false));
        assert!(out.conf.is_none() || out.conf.as_deref() == Some(""));
    }

    #[test]
    fn empty_and_garbage_output_never_panic() {
        assert!(!parse_v2("", 1).usable());
        assert!(!parse_v2("\u{4e2d}\u{6587}\u{4e71}\u{7801}\n".repeat(50).as_str(), 1).usable());
        assert!(!parse_v2("hello world\nKEY=value\n", 1).usable());
        let weird = "SHANGXIAN-CHECK-V2\n===SECTION:PORT===\nPORT=\nPORT|:::\nPORT|:::::18080\n";
        let out = parse_v2(weird, 18080);
        assert!(out.usable());
    }

    // ---------- 解析规则细节 ----------

    #[test]
    fn duplicate_keys_last_wins() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:HTTPS===\nCERT_DAYS_LEFT=5\nCERT_DAYS_LEFT=9\n===END===\n"
        );
        let out = parse_v2(&text, 1);
        assert_eq!(out.https.cert_days_left, Some(9));
    }

    #[test]
    fn number_and_bool_parse_failures_are_ignored_with_warnings() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:HTTPS===\nCERT_DAYS_LEFT=很多天\nCERT_CHECKEND_30=unknown\n===END===\n"
        );
        let out = parse_v2(&text, 1);
        assert_eq!(out.https.cert_days_left, None);
        assert_eq!(out.https.checkend30, None);
        assert!(
            out.warnings.iter().any(|w| w.contains("CERT_DAYS_LEFT")),
            "数字解析失败要有警告：{:?}",
            out.warnings
        );
    }

    #[test]
    fn overlong_values_are_dropped() {
        let long = "x".repeat(600);
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:CONF===\nCONF_PATH={long}\nCONF_FOUND=1\n===END===\n"
        );
        let out = parse_v2(&text, 1);
        assert_eq!(out.conf_path, None, "超长值要忽略");
        assert_eq!(out.conf_found, Some(true), "同一节里正常的值还要保留");
        assert!(out.warnings.iter().any(|w| w.contains("过长")));
    }

    #[test]
    fn unknown_sections_and_keys_are_ignored() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:FUTURE===\nWHATEVER=1\n===SECTION:NET===\nNOPE=1\nSSH_CLIENT_IP=1.2.3.4\n===END===\n"
        );
        let out = parse_v2(&text, 1);
        assert!(out.usable());
        assert_eq!(out.client_ip.as_deref(), Some("1.2.3.4"));
        assert_eq!(out.env, None);
    }

    #[test]
    fn file_locations_accumulate_dedupe_and_keep_order() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:FILES===\nNGINX_CONF_FILE=/a.conf\nCONF_GREP_HIT=/b.conf\nNGINX_CONF_FILE=/a.conf\nNGINX_T_FAILED=1\n===END===\n"
        );
        let out = parse_v2(&text, 1);
        assert_eq!(out.files, vec!["/a.conf", "/b.conf"]);
        assert!(out.nginx_t_failed);
    }

    #[test]
    fn log_and_port_lines_are_capped() {
        let mut text = format!("{CHECK_V2_MARKER}\n===SECTION:AUDIT===\nAUDIT_LOG=/x.log\n");
        for i in 0..40 {
            text.push_str(&format!(
                "LOG|1.2.3.4 - - [12/Oct/2026:03:10:00 +0800] \"GET /{i} HTTP/1.1\" 401 0\n"
            ));
        }
        text.push_str("===END===\n");
        let out = parse_v2(&text, 1);
        assert_eq!(out.audit.as_ref().unwrap().entries.len(), MAX_LOG_SAMPLES);

        let mut pt = format!("{CHECK_V2_MARKER}\n===SECTION:PORT===\nPORT_TOOL=ss\n");
        for _ in 0..100 {
            pt.push_str("PORT|127.0.0.1:18080\n");
        }
        pt.push_str("===END===\n");
        let out = parse_v2(&pt, 18080);
        assert_eq!(out.port.as_ref().unwrap().evidence.len(), MAX_PORT_LINES);
    }

    #[test]
    fn port_lines_must_match_the_expected_port() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:PORT===\nPORT=18080\nPORT_TOOL=netstat\nPORT|0.0.0.0:80\nPORT|0.0.0.0:8080\nPORT|[::]:18080\n===END===\n"
        );
        let out = parse_v2(&text, 18080);
        let port = out.port.as_ref().unwrap();
        assert_eq!(port.evidence, vec!["[::]:18080"], ":80 / :8080 不能匹配 :18080");
        assert_eq!(port.bound, "exposed");
        assert_eq!(port.tool, "netstat");
    }

    #[test]
    fn port_classification_rules() {
        let check = |lines: &[&str]| {
            let mut text = format!("{CHECK_V2_MARKER}\n===SECTION:PORT===\nPORT_TOOL=ss\n");
            for l in lines {
                text.push_str(&format!("PORT|{l}\n"));
            }
            text.push_str("===END===\n");
            parse_v2(&text, 18080).port.unwrap()
        };
        assert_eq!(check(&["127.0.0.1:18080", "::1:18080"]).bound, "loopback");
        assert_eq!(check(&["0.0.0.0:18080", "127.0.0.1:18080"]).bound, "exposed");
        assert_eq!(check(&["127.0.0.1:18080", "192.168.1.9:18080"]).bound, "exposed");
        assert_eq!(check(&["192.168.1.9:18080"]).bound, "exposed");
        assert_eq!(check(&["*:18080"]).bound, "exposed");
        assert_eq!(check(&[":::18080"]).bound, "exposed");
        assert_eq!(check(&[]).bound, "missing");
        let mut text = format!("{CHECK_V2_MARKER}\n===SECTION:PORT===\nPORT_TOOL=none\n===END===\n");
        text.push_str("");
        assert_eq!(parse_v2(&text, 18080).port.unwrap().bound, "unknown");
    }

    #[test]
    fn port_number_mismatch_warns_but_uses_app_port() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:PORT===\nPORT=9999\nPORT_TOOL=ss\nPORT|127.0.0.1:18080\n===END===\n"
        );
        let out = parse_v2(&text, 18080);
        let port = out.port.as_ref().unwrap();
        assert_eq!(port.port, 18080);
        assert!(port.warnings.is_empty(), "警告记在整体输出里：{:?}", out.warnings);
        assert!(out.warnings.iter().any(|w| w.contains("9999")));
    }

    // ---------- 环境识别 ----------

    #[test]
    fn env_confidence_rules() {
        let high = CloudEnv::from_keys(&keys(&[
            ("PANEL", "bt"),
            ("PANEL_EVIDENCE", "/www/server/panel"),
            ("WEB", "nginx"),
            ("NGINX_BIN", "/www/server/nginx/sbin/nginx"),
            ("VHOST_DIR_EXISTS", "1"),
            ("WEB_CONF_EXISTS", "1"),
        ]));
        assert_eq!(high.panel_confidence, "high");
        assert_eq!(high.web_confidence, "high");

        let medium = CloudEnv::from_keys(&keys(&[
            ("PANEL", "bt"),
            ("PANEL_EVIDENCE", "/www/server/panel"),
            ("WEB", "nginx"),
            ("NGINX_BIN", "/usr/sbin/nginx"),
            ("VHOST_DIR_EXISTS", "0"),
            ("WEB_CONF_EXISTS", "1"),
        ]));
        assert_eq!(medium.panel_confidence, "medium");
        assert_eq!(medium.web_confidence, "medium");

        let low = CloudEnv::from_keys(&keys(&[
            ("PANEL", "none"),
            ("WEB", "nginx"),
            ("NGINX_BIN", "/usr/sbin/nginx"),
            ("VHOST_DIR_EXISTS", "1"),
        ]));
        assert_eq!(low.panel_confidence, "low");
        assert_eq!(low.web_confidence, "low");

        let unknown = CloudEnv::from_keys(&keys(&[("WS_MAP", "unknown")]));
        assert_eq!(unknown.panel_confidence, "unknown");
        assert_eq!(unknown.web_confidence, "unknown");
        assert_eq!(unknown.ws_map, None);
    }

    #[test]
    fn env_vhost_dir_quotes_stripped() {
        // 脚本里 VHOST_DIR 是 shell 转义回显（带上外层单引号），解析要剥掉。
        let e = CloudEnv::from_keys(&keys(&[("VHOST_DIR", "'/xp/panel/vhost'")]));
        assert_eq!(e.vhost_dir.as_deref(), Some("/xp/panel/vhost"));
    }

    #[test]
    fn env_banner_covers_every_case() {
        let env = |panel: &str, web: &str| CloudEnv {
            panel: panel.into(),
            web: web.into(),
            ..CloudEnv::default()
        };
        assert_eq!(env_banner(&env("bt", "nginx")).level, Light::Green);
        assert!(env_banner(&env("bt", "nginx")).text.contains("宝塔"));
        assert_eq!(env_banner(&env("xp", "nginx")).level, Light::Green);
        assert_eq!(env_banner(&env("none", "nginx")).level, Light::Yellow);
        assert_eq!(env_banner(&env("unknown", "nginx")).level, Light::Yellow);
        assert_eq!(env_banner(&env("bt", "apache")).level, Light::Red);
        assert_eq!(env_banner(&env("bt", "both")).level, Light::Yellow);
        assert_eq!(env_banner(&env("bt", "none")).level, Light::Yellow);
        assert_eq!(env_banner(&env("unknown", "unknown")).level, Light::Yellow);
    }

    // ---------- HTTPS ----------

    const HTTPS_CONF: &str = r#"
server {
    listen 80;
    server_name album.example.com;
    access_log /www/wwwlogs/album.example.com.log;
    return 301 https://$host$request_uri;
}
server {
    listen 443 ssl http2;
    server_name album.example.com;
    ssl_certificate /etc/ssl/album.pem;
    auth_basic "x";
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
}
"#;

    #[test]
    fn analyze_https_matches_block_and_detects_redirect() {
        let check = analyze_https(HTTPS_CONF, "album.example.com", &HttpsKeys::default());
        assert_eq!(check.server_match, "matched");
        assert!(check.enabled);
        assert!(check.redirect80);
        assert!(!check.temporary_redirect);
        assert!(!check.plain_open, "80 只有跳转，没有 proxy_pass");
        assert_eq!(check.cert_path.as_deref(), Some("/etc/ssl/album.pem"));
    }

    /// 小皮面板布局：脚本把 include 的文件拼回文本后，证书行和「强制 HTTPS」的
    /// if 块都落在 server 块外面。这条要整体认出来，不能再误报。
    const XP_MERGED_CONF: &str = r#"
server {
    listen 80;
    listen 443 ssl http2;
    server_name album.example.com;
    auth_basic "要求密码";
    auth_basic_user_file /xp/panel/vhost/album.example.com.htpasswd;
    include /xp/panel/vhost/ssl/album.example.com/ssl.conf;
    location /.well-known/ {
        auth_basic off;
        allow all;
    }
}
    ssl_certificate /xp/panel/vhost/ssl/album.example.com/cert/fullchain.pem;
    ssl_certificate_key /xp/panel/vhost/ssl/album.example.com/cert/privkey.key;
    if ($server_port !~ 443) {
        rewrite ^(/.*)$ https://$host$1 permanent;
    }
"#;

    #[test]
    fn analyze_https_reads_merged_include_layout() {
        let keys = HttpsKeys {
            ssl_cert_path: Some("/xp/panel/vhost/ssl/album.example.com/cert/fullchain.pem".into()),
            cert_days_left: Some(78),
            ..HttpsKeys::default()
        };
        let check = analyze_https(XP_MERGED_CONF, "album.example.com", &keys);
        assert_eq!(check.server_match, "matched");
        assert!(check.enabled, "listen 443 ssl 要认出来");
        assert!(check.redirect80, "include 里的 80 跳 443 要认出来");
        assert!(
            !check.temporary_redirect,
            "rewrite permanent 是 301，不该报临时跳转"
        );
        assert_eq!(check.cert_days, Some(78));
        assert!(!check.cert_file_missing);
        assert_eq!(
            check.cert_path.as_deref(),
            Some("/xp/panel/vhost/ssl/album.example.com/cert/fullchain.pem")
        );
        assert!(
            check.warnings.iter().all(|w| !w.contains("证书文件")),
            "不能再误报证书找不到：{:?}",
            check.warnings
        );
    }

    #[test]
    fn analyze_https_plain_open_and_temporary_redirect() {
        let conf = r#"
server {
    listen 80;
    server_name a.example.com;
    return 302 https://$host$request_uri;
    location / { proxy_pass http://127.0.0.1:1; }
}
"#;
        let check = analyze_https(conf, "a.example.com", &HttpsKeys::default());
        assert!(check.temporary_redirect, "302 要标出来");
        assert!(!check.plain_open, "server 级 return 会让所有路径先跳转");
        assert!(check.warnings.iter().any(|w| w.contains("301")));
    }

    #[test]
    fn analyze_https_plain_open_without_redirect() {
        let conf = r#"
server {
    listen 80;
    server_name a.example.com;
    location / { proxy_pass http://127.0.0.1:1; }
}
"#;
        let check = analyze_https(conf, "a.example.com", &HttpsKeys::default());
        assert!(check.plain_open);
        assert!(!check.redirect80);
        assert!(!check.enabled);
    }

    #[test]
    fn analyze_https_single_block_fallback_and_none() {
        let one = "server { listen 80; server_name other.example.com; return 301 https://x/; }";
        let check = analyze_https(one, "album.example.com", &HttpsKeys::default());
        assert_eq!(check.server_match, "single");
        assert!(check.warnings.iter().any(|w| w.contains("只有一个")));

        let two = "server { server_name a.example.com; }\nserver { server_name b.example.com; }";
        let check = analyze_https(two, "album.example.com", &HttpsKeys::default());
        assert_eq!(check.server_match, "none");
        assert!(check.warnings.iter().any(|w| w.contains("未找到 server_name")));
    }

    #[test]
    fn https_lights_follow_the_table() {
        let base = HttpsCheck {
            enabled: true,
            cert_days: Some(67),
            redirect80: true,
            plain_open: false,
            temporary_redirect: false,
            server_match: "matched".into(),
            cert_path: None,
            cert_end_raw: None,
            checkend7: None,
            checkend30: None,
            openssl_missing: false,
            cert_file_missing: false,
            warnings: Vec::new(),
        };
        // 全部正常：绿
        let lights = https_lights(&base, true, true, false);
        assert_eq!(lights.last().unwrap().level, Light::Green);

        // https 网址但没开 443：红
        let mut no_https = base.clone();
        no_https.enabled = false;
        let lights = https_lights(&no_https, true, true, false);
        assert_eq!(lights[0].level, Light::Red);

        // 密码 + 明文可开 + 不跳转：红
        let mut plain = base.clone();
        plain.plain_open = true;
        plain.redirect80 = false;
        plain.enabled = false;
        let lights = https_lights(&plain, false, true, false);
        assert_eq!(lights[0].level, Light::Red);
        assert!(lights[0].text.contains("明文传输"));

        // 白名单 + 明文：黄
        let lights = https_lights(&plain, false, false, true);
        assert_eq!(lights[0].level, Light::Yellow);

        // 无保护 + 明文：黄
        let lights = https_lights(&plain, false, false, false);
        assert_eq!(lights[0].level, Light::Yellow);

        // 证书过期 / 快到期：红
        let mut expired = base.clone();
        expired.cert_days = Some(0);
        assert_eq!(https_lights(&expired, true, true, false)[0].level, Light::Red);
        expired.cert_days = Some(3);
        let lights = https_lights(&expired, true, true, false);
        assert_eq!(lights[0].level, Light::Red);
        assert!(lights[0].text.contains("3 天"));
        expired.cert_days = Some(20);
        let lights = https_lights(&expired, true, true, false);
        assert_eq!(lights[0].level, Light::Yellow);

        // 天数算不出来但有 checkend 档位
        let mut no_days = base.clone();
        no_days.cert_days = None;
        no_days.checkend30 = Some(true);
        assert!(https_lights(&no_days, true, true, false)
            .iter()
            .any(|l| l.level == Light::Green));
        no_days.checkend30 = Some(false);
        assert!(https_lights(&no_days, true, true, false)
            .iter()
            .any(|l| l.level == Light::Yellow));

        // 没装 openssl / 证书文件找不到：黄
        let mut missing = base.clone();
        missing.cert_days = None;
        missing.openssl_missing = true;
        missing.cert_file_missing = true;
        let lights = https_lights(&missing, true, true, false);
        assert!(lights.iter().any(|l| l.text.contains("openssl")));
        assert!(lights.iter().any(|l| l.text.contains("证书文件")));

        // 网址还是 http，但服务器已经支持 https
        let lights = https_lights(&base, false, true, false);
        assert!(lights.iter().any(|l| l.text.contains("改为以 https://")));
    }

    #[test]
    fn wellknown_exemption_detection() {
        assert!(wellknown_exempt_ok(HTTPS_CONF, "album.example.com", true, true));
        assert!(wellknown_exempt_ok(HTTPS_CONF, "album.example.com", false, false));

        let missing_off = r#"
server {
    listen 443 ssl;
    server_name a.example.com;
    location /.well-known/ { allow all; }
}
"#;
        assert!(!wellknown_exempt_ok(missing_off, "a.example.com", true, false));
        assert!(wellknown_exempt_ok(missing_off, "a.example.com", false, true));
    }

    // ---------- 端口红黄灯 ----------

    #[test]
    fn port_lights_follow_the_table() {
        let mk = |bound: &str| PortCheck {
            bound: bound.into(),
            tool: "ss".into(),
            port: 18080,
            evidence: vec![],
            warnings: vec![],
        };
        assert_eq!(port_lights(&mk("exposed"), true)[0].level, Light::Red);
        assert!(port_lights(&mk("exposed"), true)[0].text.contains("18080"));
        assert_eq!(port_lights(&mk("missing"), true)[0].level, Light::Yellow);
        assert!(port_lights(&mk("missing"), true)[0].text.contains("隧道日志"));
        assert_eq!(port_lights(&mk("missing"), false)[0].level, Light::Yellow);
        assert_eq!(port_lights(&mk("unknown"), true)[0].level, Light::Yellow);
        assert_eq!(port_lights(&mk("loopback"), true)[0].level, Light::Green);
    }

    // ---------- 访问审计 ----------

    #[test]
    fn access_entry_parses_combined_and_falls_back_to_raw() {
        let entry = parse_access_entry(
            "1.2.3.4 - - [12/Oct/2026:03:10:00 +0800] \"GET /a?b=1 HTTP/1.1\" 401 0 \"-\" \"curl/8.0\"",
        );
        assert_eq!(entry.ip.as_deref(), Some("1.2.3.4"));
        assert_eq!(entry.time.as_deref(), Some("12/Oct/2026:03:10:00 +0800"));
        assert_eq!(entry.method.as_deref(), Some("GET"));
        assert_eq!(entry.path.as_deref(), Some("/a?b=1"));
        assert_eq!(entry.status, Some(401));
        assert_eq!(entry.bytes, Some(0));
        assert_eq!(entry.raw, None);

        let weird = parse_access_entry("这不是日志");
        assert_eq!(weird.raw.as_deref(), Some("这不是日志"));
        assert_eq!(weird.status, None);
    }

    #[test]
    fn audit_summary_texts() {
        let mut a = AccessAudit::skipped("no_log_path");
        assert!(audit_summary(&a).contains("跳过"));

        a.available = true;
        a.path = Some("/x.log".into());
        assert!(audit_summary(&a).contains("没有被挡的访问"));

        a.count401 = 12;
        a.count403 = 3;
        a.top_ips = vec![IpCount {
            ip: "1.2.3.4".into(),
            count: 9,
        }];
        let text = audit_summary(&a);
        assert!(text.contains("12 次") && text.contains("3 次"), "{text}");
        assert!(text.contains("1.2.3.4（9 次）"), "{text}");
        assert!(text.contains("不会上传"), "{text}");
    }

    #[test]
    fn audit_top_ips_sorted_deduped_and_capped() {
        let mut text = format!("{CHECK_V2_MARKER}\n===SECTION:AUDIT===\nAUDIT_LOG=/x.log\nAUDIT_BYTES=10\n");
        for i in 0..8 {
            text.push_str(&format!("AUDIT_IP=10.0.0.{i}:{}\n", i + 1));
        }
        text.push_str("AUDIT_IP=10.0.0.0:5\nAUDIT_IP=坏数据\nAUDIT_IP=1.2.3.4:-2\n");
        text.push_str("===END===\n");
        let audit = parse_v2(&text, 1).audit.unwrap();
        assert_eq!(audit.top_ips.len(), MAX_TOP_IPS);
        assert_eq!(audit.top_ips[0].ip, "10.0.0.7");
        assert_eq!(audit.top_ips[0].count, 8);
        let merged = audit
            .top_ips
            .iter()
            .find(|t| t.ip == "10.0.0.0")
            .expect("10.0.0.0 的两条要累加到一起");
        assert_eq!(merged.count, 6);
        assert!(!audit.truncated);
        assert!(!audit.warnings.is_empty(), "坏行要有警告");
    }

    #[test]
    fn audit_truncated_at_two_megabytes() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:AUDIT===\nAUDIT_LOG=/x.log\nAUDIT_BYTES=2097152\n===END===\n"
        );
        let audit = parse_v2(&text, 1).audit.unwrap();
        assert!(audit.truncated);
        assert!(audit.warnings.iter().any(|w| w.contains("2MB")));
    }

    #[test]
    fn audit_skip_reasons() {
        let text = format!(
            "{CHECK_V2_MARKER}\n===SECTION:AUDIT===\nAUDIT_SKIP=log_unreadable\n===END===\n"
        );
        let audit = parse_v2(&text, 1).audit.unwrap();
        assert!(!audit.available);
        assert_eq!(audit.skip_reason.as_deref(), Some("log_unreadable"));
    }

    // ---------- 公网 IP ----------

    #[test]
    fn public_ip_messages_and_listing() {
        assert!(public_ip_message(Some("203.0.113.7"), false).contains("203.0.113.7"));
        assert!(public_ip_message(Some("203.0.113.7"), false).contains("加入白名单"));
        assert!(public_ip_message(Some("203.0.113.7"), true).contains("已在白名单"));
        assert!(public_ip_message(None, false).contains("未读取到"));

        assert!(public_ip_already_listed("1.2.3.4", &["1.2.3.4".into()]));
        assert!(public_ip_already_listed("1.2.3.4", &["1.2.3.0/24".into()]));
        assert!(!public_ip_already_listed("1.2.4.4", &["1.2.3.0/24".into()]));
        assert!(public_ip_already_listed(
            "2001:db8::5",
            &["2001:db8::/32".into()]
        ));
        assert!(!public_ip_already_listed("2001:db9::5", &["2001:db8::/32".into()]));
        assert!(!public_ip_already_listed("1.2.3.4", &["all".into()]));
        assert!(!public_ip_already_listed("not-ip", &["1.2.3.4".into()]));
    }

    // ---------- 漂移判定 ----------

    #[test]
    fn drift_fields_cover_the_config_side() {
        fn access(password: bool, ip: bool, ips: &[&str], htpasswd: bool) -> AccessCheck {
            AccessCheck {
                conf_found: true,
                password_on: password,
                ip_on: ip,
                allow_ips: ips.iter().map(|s| s.to_string()).collect(),
                htpasswd_exists: htpasswd,
            }
        }
        let expected = ExpectedProtection {
            password: true,
            ip: true,
            allow_ips: vec!["1.2.3.4".into()],
            url: "album.example.com".into(),
        };
        assert!(drift_fields(&expected, &access(true, true, &["1.2.3.4"], true), true).is_empty());
        assert_eq!(
            drift_fields(&expected, &access(false, true, &["1.2.3.4"], true), true),
            vec!["password"]
        );
        assert_eq!(
            drift_fields(&expected, &access(true, false, &[], false), true),
            vec!["htpasswd", "ip"]
        );
        assert_eq!(
            drift_fields(&expected, &access(true, true, &["5.6.7.8"], true), true),
            vec!["allowIps"]
        );
        let mut no_conf = access(true, true, &["1.2.3.4"], true);
        no_conf.conf_found = false;
        assert!(drift_fields(&expected, &no_conf, true).contains(&"conf".to_string()));
        assert!(drift_fields(&expected, &access(true, true, &["1.2.3.4"], true), false)
            .contains(&"conf".to_string()));
    }

    #[test]
    fn assemble_reports_drift_and_error() {
        let expected_ips = vec!["1.2.3.4".to_string()];
        let input = CheckInput {
            domain: "trap.example.com",
            url_is_https: false,
            expected_password: true,
            expected_ip: true,
            expected_allow_ips: &expected_ips,
            remote_port: 18080,
            source: "auto",
            ts: 123,
            cached_env: None,
        };
        let result = assemble_v2(TRAP, &input);
        assert_eq!(result.check.status, "drift");
        assert_eq!(result.check.source.as_deref(), Some("auto"));
        assert!(!result.check.drift_fields.is_empty());
        assert!(result.check.cloud.is_some());
        assert!(result.actual.conf_found, "TRAP 里有配置");

        let garbage = assemble_v2("完全不认识的输出\n", &input);
        assert_eq!(garbage.check.status, "error");
        assert!(garbage.check.error.as_deref().unwrap().contains("未完整读取"));
        assert!(garbage.check.cloud.is_none());
    }

    #[test]
    fn cached_env_is_used_when_output_has_none() {
        let cached = CloudEnv {
            panel: "bt".into(),
            web: "nginx".into(),
            panel_confidence: "high".into(),
            web_confidence: "high".into(),
            ..CloudEnv::default()
        };
        let input = CheckInput {
            domain: "album.example.com",
            url_is_https: true,
            expected_password: true,
            expected_ip: false,
            expected_allow_ips: &[],
            remote_port: 18080,
            source: "manual",
            ts: 1,
            cached_env: Some(cached.clone()),
        };
        let result = assemble_v2(GOLDEN, &input);
        // full 模式脚本不带 ENV 节时用缓存；GOLDEN 有 ENV，所以以输出为准
        assert_eq!(result.env.unwrap().panel, "bt");
        let no_env = format!("{CHECK_V2_MARKER}\n===SECTION:CONF===\nCONF_FOUND=0\n===END===\n");
        let result = assemble_v2(&no_env, &input);
        assert_eq!(result.env.unwrap().panel, "bt");
    }

    // ---------- 脚本生成 ----------

    fn script_input<'a>(mode: CheckMode, domain: &'a str, dir: &'a str) -> ScriptInput<'a> {
        ScriptInput {
            mode,
            domain,
            vhost_dir: dir,
            remote_port: 18080,
            nginx_bin: None,
            log_path: None,
        }
    }

    #[test]
    fn script_has_version_line_and_mode_sections() {
        let full = build_script(&script_input(CheckMode::Full, "album.example.com", "/xp/panel/vhost"))
            .unwrap();
        assert_eq!(full.lines().next().unwrap(), "echo SHANGXIAN-CHECK-V2");
        for section in CheckMode::Full.sections() {
            assert!(
                full.contains(&format!("===SECTION:{section}===")),
                "full 应包含 {section}：{full}"
            );
        }
        assert!(!full.contains("===SECTION:ENV==="), "full 不带 ENV");
        assert!(!full.contains("===SECTION:AUDIT==="), "full 不带 AUDIT");
        assert!(full.contains("===END==="));
        assert!(!full.contains('@'), "占位符都要替换掉");

        let env = build_script(&script_input(CheckMode::Env, "album.example.com", "/xp/panel/vhost"))
            .unwrap();
        assert!(env.contains("===SECTION:ENV===") && env.contains("===SECTION:FILES==="));
        assert!(!env.contains("===SECTION:CONF==="));

        let drift = build_script(&script_input(CheckMode::Drift, "album.example.com", "/xp/panel/vhost"))
            .unwrap();
        assert!(drift.contains("===SECTION:HTTPS===") && drift.contains("===SECTION:PORT==="));
        assert!(!drift.contains("===SECTION:FILES===") && !drift.contains("===SECTION:NET==="));
        assert!(!drift.contains("===SECTION:ENV==="));

        let audit = build_script(&script_input(CheckMode::Audit, "album.example.com", "/xp/panel/vhost"))
            .unwrap();
        assert!(audit.contains("===SECTION:AUDIT==="));
        assert!(audit.contains("AUDIT_WINDOW=1000") || audit.contains("AUDIT_WINDOW"));
        assert!(!audit.contains("===SECTION:NET==="));
    }

    #[test]
    fn script_expands_site_includes() {
        let full = build_script(&script_input(CheckMode::Full, "album.example.com", "/xp/panel/vhost"))
            .unwrap();
        assert!(
            full.contains("_shx_expand_conf()"),
            "要带上 include 展开函数：{full}"
        );
        assert_eq!(
            full.matches("_shx_expand_conf \"$f\"").count(),
            2,
            "CONF 和 HTTPS 两节都要用它：{full}"
        );
        assert!(
            full.contains("_shx_expand_conf \"$f\" | head -c 65536"),
            "CONF 正文要用展开后的内容：{full}"
        );
    }

    #[test]
    fn script_rejects_bad_inputs_and_quotes_paths() {
        let mut input = script_input(CheckMode::Full, "album.example.com", "/xp/panel/vhost");
        assert!(build_script(&input).is_ok());

        input.domain = "album.example.com; rm -rf /";
        assert!(build_script(&input).is_err());

        input.domain = "album.example.com";
        input.vhost_dir = "/tmp/a\nrm -rf /";
        assert!(build_script(&input).is_err());

        input.vhost_dir = "/tmp/a`whoami`";
        assert!(build_script(&input).is_err());

        input.vhost_dir = "/xp/panel/vhost";
        assert!(build_script(&input).is_ok());

        input.log_path = Some("/var/log/x.log\nrm -rf /".into());
        assert!(build_script(&input).is_err());

        // 日志路径拼接在 AUDIT 节，nginx 路径拼接在 FILES 节，分别验
        let mut audit_input = script_input(CheckMode::Audit, "album.example.com", "/xp/panel/vhost");
        audit_input.log_path = Some("/var/log/我的 日志.log".into());
        let audit_script = build_script(&audit_input).unwrap();
        assert!(
            audit_script.contains("'/var/log/我的 日志.log'"),
            "路径要单引号包好：{audit_script}"
        );

        let mut env_input = script_input(CheckMode::Env, "album.example.com", "/xp/panel/vhost");
        env_input.nginx_bin = Some("/www/server/nginx/sbin/nginx".into());
        let env_script = build_script(&env_input).unwrap();
        assert!(env_script.contains("'/www/server/nginx/sbin/nginx'"));
    }

    #[test]
    fn script_has_no_write_operations() {
        // 红线：远端脚本只读。`2>/dev/null` 是允许的丢弃输出，其它重定向/改配置命令都不许出现。
        for mode in [CheckMode::Full, CheckMode::Drift, CheckMode::Audit, CheckMode::Env] {
            let script =
                build_script(&script_input(mode, "album.example.com", "/xp/panel/vhost")).unwrap();
            for bad in ["sed -i", "systemctl", "rm ", "kill ", "curl ", "wget "] {
                assert!(
                    !script.contains(bad),
                    "{} 模式脚本里不该出现「{bad}」：\n{script}",
                    mode.as_str()
                );
            }
            for (idx, _) in script.match_indices('>') {
                let rest = &script[idx..];
                assert!(
                    rest.starts_with(">/dev/null") || rest.starts_with(">&1"),
                    "只允许 >/dev/null 或 2>&1 这种丢弃/合流输出，其它重定向不许写：\n{script}"
                );
            }
        }
    }

    #[test]
    fn protection_check_serde_matches_d2_shape() {
        let check = ProtectionCheck::default();
        let value = serde_json::to_value(&check).unwrap();
        assert_eq!(value["status"], serde_json::json!("unknown"));
        assert!(value["ts"].is_null());
        assert!(value["cloud"].is_null());
        assert_eq!(value["expected"]["password"], serde_json::json!(false));
        assert_eq!(value["driftFields"], serde_json::json!([]));
        // extra 为 None 时不落键（D2 的 skip_serializing_if）
        assert!(value.get("extra").is_none());
    }
}
