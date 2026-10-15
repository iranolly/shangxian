//! 探测「这台服务器的 nginx 能不能验 bcrypt（`$2y$`）」，给访问密码选择格式用。
//!
//! 背景：nginx 自己只认得 apr1 等少数格式，bcrypt 要交给系统加密库去做校验；
//! 有的面板（例如小皮）给 nginx 打包了自带的老库并让它优先加载，bcrypt 会让站点
//! 直接返回 500。判定的唯一可靠办法是拿 nginx **真正加载的那个库**做一次真实计算
//! （不能用字符串猜：`libcrypt` 这个词里就包含 `bcrypt`）。
//!
//! 探测只读：找 nginx → `ldd` 看它加载哪个 libcrypt → 用 python ctypes 真算一次
//! `$2y$12$…`；没有 python 就退回看符号粗判。任何一项拿不到就报「说不准」，调用方
//! 按「说不准」处理（用兼容的 apr1），宁可弱一点也不能把站点打成 500。

use crate::cleanup::{run, SshExec};
use std::time::Duration;

/// 探测结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BcryptSupport {
    /// 这个 nginx 能验 bcrypt，可以放心用 `$2y$…`
    Yes,
    /// 明确不支持（例如 nginx 加载的是老 glibc 自带的 libcrypt）
    No,
    /// 没探出来（没找到 nginx / 没有 python 也看不准 / 命令失败）
    #[default]
    Unknown,
}

/// 一次探测的完整结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BcryptProbe {
    pub support: BcryptSupport,
    /// 探测用到的 nginx 可执行文件
    pub nginx_bin: Option<String>,
    /// nginx 真正加载的 libcrypt 路径
    pub crypt_lib: Option<String>,
    /// 给人看的证据（`nginx 路径|库路径`）
    pub evidence: Option<String>,
}

/// 找 nginx 可执行文件（设置 shell 变量 `nginx_bin`）。
///
/// `@BIN@` 是调用方已经探到的路径，优先用；探不到再按面板常见位置、`PATH`、
/// 正在跑的 nginx 主进程依次兜底（小皮的实际路径是 `/xp/server/nginx/sbin/nginx`，
/// 面板目录 `/xp/panel/nginx/...` 反而可能不存在）。
pub const NGINX_FIND: &str = r#"nginx_bin=@BIN@
if [ -z "$nginx_bin" ]; then
  for _shx_b in /usr/sbin/nginx /usr/local/sbin/nginx /usr/local/nginx/sbin/nginx /usr/local/openresty/nginx/sbin/nginx /www/server/nginx/sbin/nginx /xp/server/nginx/sbin/nginx /xp/panel/nginx/sbin/nginx /opt/nginx/sbin/nginx /opt/openresty/nginx/sbin/nginx; do
    if [ -x "$_shx_b" ]; then nginx_bin="$_shx_b"; break; fi
  done
fi
if [ -z "$nginx_bin" ] && command -v nginx >/dev/null 2>&1; then nginx_bin=$(command -v nginx); fi
if [ -z "$nginx_bin" ]; then
  nginx_bin=$(ps -eo args 2>/dev/null | sed -n 's#^nginx: master process ##p' | awk '{print $1}' | head -1)
fi
if [ -z "$nginx_bin" ]; then
  nginx_bin=$(ps aux 2>/dev/null | grep '[n]ginx: master process' | head -1 | sed 's#.*nginx: master process ##' | awk '{print $1}')
fi"#;

/// 用 nginx 真正加载的 libcrypt 做一次真实 bcrypt 计算。
///
/// 输出：`CRYPT_LIB=` / `BCRYPT=yes|no|unknown` / `BCRYPT_EVIDENCE=`。
pub const BCRYPT_PROBE: &str = r#"crypt_lib=''
if [ -n "$nginx_bin" ] && [ -x "$nginx_bin" ] && command -v ldd >/dev/null 2>&1; then
  crypt_lib=$(ldd "$nginx_bin" 2>/dev/null | awk '/libcrypt/ { for (i = 1; i <= NF; i++) if ($i ~ /^\//) { print $i; exit } }')
fi
echo "CRYPT_LIB=$crypt_lib"
bcrypt=''
if [ -n "$nginx_bin" ]; then
  if [ -z "$crypt_lib" ]; then
    bcrypt=no
  else
    py=''
    if command -v python3 >/dev/null 2>&1; then py=python3
    elif command -v python >/dev/null 2>&1; then py=python
    fi
    if [ -n "$py" ]; then
      bcrypt=$(LIB_PATH="$crypt_lib" "$py" - <<'PY' 2>/dev/null
import ctypes, os
try:
    lib = ctypes.CDLL(os.environ.get('LIB_PATH', ''))
    lib.crypt.restype = ctypes.c_char_p
    lib.crypt.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
    out = lib.crypt(b'test', b'$2y$12$abcdefghijklmnopqrstuv')
    print('yes' if out and out.startswith(b'$2y$12$') else 'no')
except Exception:
    print('unknown')
PY
)
    fi
    if [ -z "$bcrypt" ]; then
      if command -v nm >/dev/null 2>&1 && nm -D "$crypt_lib" 2>/dev/null | grep -q 'crypt_gensalt.*XCRYPT'; then
        bcrypt=unknown
      else
        bcrypt=no
      fi
    fi
  fi
fi
case "$bcrypt" in
  yes|no) ;;
  *) bcrypt=unknown ;;
esac
echo "BCRYPT=$bcrypt"
echo "BCRYPT_EVIDENCE=$nginx_bin|$crypt_lib""#;

/// 独立跑一次探测的脚本（不改服务器上的任何东西）。
pub fn script(nginx_hint: Option<&str>) -> String {
    let hint = match nginx_hint {
        Some(s) if !s.trim().is_empty() => crate::cleanup::shell_quote(s.trim()),
        _ => "''".to_string(),
    };
    let body = format!("{}\n{}", NGINX_FIND, BCRYPT_PROBE).replace("@BIN@", &hint);
    format!("echo SHANGXIAN-BCRYPT-PROBE-V1\n{body}\necho \"NGINX_BIN=$nginx_bin\"\n")
}

/// 解析探测输出（只看 `KEY=value` 行，认不出的安静忽略）。
pub fn parse(output: &str) -> BcryptProbe {
    let mut probe = BcryptProbe::default();
    for line in output.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("NGINX_BIN=") {
            let v = v.trim();
            if !v.is_empty() {
                probe.nginx_bin = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("CRYPT_LIB=") {
            let v = v.trim();
            if !v.is_empty() {
                probe.crypt_lib = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("BCRYPT=") {
            probe.support = match v.trim() {
                "yes" => BcryptSupport::Yes,
                "no" => BcryptSupport::No,
                _ => BcryptSupport::Unknown,
            };
        } else if let Some(v) = line.strip_prefix("BCRYPT_EVIDENCE=") {
            let v = v.trim();
            if !v.is_empty() {
                probe.evidence = Some(v.to_string());
            }
        }
    }
    probe
}

/// 真跑一次探测；网络/命令失败返回 Err（调用方按「说不准」处理，不打扰用户）。
pub fn probe(
    exec: &SshExec,
    nginx_hint: Option<&str>,
    timeout: Duration,
) -> Result<BcryptProbe, String> {
    let out = run(exec, &script(nginx_hint), timeout)?;
    Ok(parse(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_probe_result() {
        let out = "SHANGXIAN-BCRYPT-PROBE-V1\nNGINX_BIN=/xp/server/nginx/sbin/nginx\nCRYPT_LIB=/xp/server/nginx/sbin/libs/libcrypt.so.1\nBCRYPT=no\nBCRYPT_EVIDENCE=/xp/server/nginx/sbin/nginx|/xp/server/nginx/sbin/libs/libcrypt.so.1\n";
        let p = parse(out);
        assert_eq!(p.support, BcryptSupport::No);
        assert_eq!(p.nginx_bin.as_deref(), Some("/xp/server/nginx/sbin/nginx"));
        assert_eq!(
            p.crypt_lib.as_deref(),
            Some("/xp/server/nginx/sbin/libs/libcrypt.so.1")
        );
        assert!(p.evidence.unwrap().contains("|"));
    }

    #[test]
    fn parse_yes_and_unknown() {
        assert_eq!(parse("BCRYPT=yes\n").support, BcryptSupport::Yes);
        assert_eq!(parse("").support, BcryptSupport::Unknown);
        assert_eq!(parse("BCRYPT=\n").support, BcryptSupport::Unknown);
        assert_eq!(parse("BCRYPT=maybe\n").support, BcryptSupport::Unknown);
    }

    #[test]
    fn script_quotes_hint_and_contains_real_test() {
        let s = script(Some("/xp/server/nginx/sbin/nginx"));
        assert!(s.contains("/xp/server/nginx/sbin/nginx"), "要带上提示路径：{s}");
        assert!(s.contains("libcrypt"), "要找 nginx 加载的库");
        assert!(s.contains("$2y$12$"), "要真算一次 bcrypt");
        assert!(script(None).contains("nginx_bin=''"), "没有提示也安全");
    }
}
