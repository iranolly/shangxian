//! 错误原因码与中文文案的**唯一**翻译器（D2 §7）。
//!
//! - `ReasonCode` 是 JSON 里唯一的机器值（`"authDenied"` 等），中文文案只在这里出现一份。
//! - `guardian::exit_reason_from_log`、app 的 `ssh_error_human`、`test_server` 的错误翻译
//!   都要改成调用这里，不许在别处再存关键字表。
//! - [`ReasonCode::from_ssh_text`]：ssh 输出 → 原因码；[`ReasonCode::from_service_text`]：
//!   服务侧人话原因 → 原因码；认不出来时按约定兜底。

use serde::Serialize;

/// 全部 42 个错误/事件码（D2 §7.2 + §19.4 追加的 `RetryStopped`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReasonCode {
    // SSH / 网络（隧道退出、test_server、配免密）
    SshMissing,
    NetTimeout,
    AuthDenied,
    HostKeyChanged,
    ConnRefused,
    DnsFail,
    KeyUnusable,
    RemotePortBusy,
    ConnReset,
    SpawnFail,
    ExitUnknown,
    /// 连续 20 次没连上后停止自动重试（隧道）。
    RetryStopped,
    SignalExit,
    // 引用 / 配置
    ServerMissing,
    ServiceMissing,
    ConfigUntrusted,
    ConfigUnsupported,
    CleanupFail,
    // 本机服务
    StartFail,
    CrashLoop,
    Hung,
    Exited,
    AutostartFail,
    // 事件专用
    AppStarted,
    AppExited,
    ConfigSaved,
    ConfigMigrated,
    ConfigNormalized,
    ConfigCorrupt,
    StateReset,
    Deleted,
    Restored,
    UpdateChecked,
    UpdateAvailable,
    ProtectionChecked,
    ProtectionDrift,
    ProtectionError,
    DoctorRan,
    TunnelConnected,
    TunnelDown,
    ServiceUp,
    ServiceDown,
}

impl ReasonCode {
    /// 全枚举（顺序即 §7.2 表顺序 + §19.4 追加项）。
    pub const ALL: [ReasonCode; 42] = [
        ReasonCode::SshMissing,
        ReasonCode::NetTimeout,
        ReasonCode::AuthDenied,
        ReasonCode::HostKeyChanged,
        ReasonCode::ConnRefused,
        ReasonCode::DnsFail,
        ReasonCode::KeyUnusable,
        ReasonCode::RemotePortBusy,
        ReasonCode::ConnReset,
        ReasonCode::SpawnFail,
        ReasonCode::ExitUnknown,
        ReasonCode::RetryStopped,
        ReasonCode::SignalExit,
        ReasonCode::ServerMissing,
        ReasonCode::ServiceMissing,
        ReasonCode::ConfigUntrusted,
        ReasonCode::ConfigUnsupported,
        ReasonCode::CleanupFail,
        ReasonCode::StartFail,
        ReasonCode::CrashLoop,
        ReasonCode::Hung,
        ReasonCode::Exited,
        ReasonCode::AutostartFail,
        ReasonCode::AppStarted,
        ReasonCode::AppExited,
        ReasonCode::ConfigSaved,
        ReasonCode::ConfigMigrated,
        ReasonCode::ConfigNormalized,
        ReasonCode::ConfigCorrupt,
        ReasonCode::StateReset,
        ReasonCode::Deleted,
        ReasonCode::Restored,
        ReasonCode::UpdateChecked,
        ReasonCode::UpdateAvailable,
        ReasonCode::ProtectionChecked,
        ReasonCode::ProtectionDrift,
        ReasonCode::ProtectionError,
        ReasonCode::DoctorRan,
        ReasonCode::TunnelConnected,
        ReasonCode::TunnelDown,
        ReasonCode::ServiceUp,
        ReasonCode::ServiceDown,
    ];

    /// JSON 里的机器值。必须等于 serde 序列化结果（有单测锁死）。
    pub fn as_str(self) -> &'static str {
        match self {
            ReasonCode::SshMissing => "sshMissing",
            ReasonCode::NetTimeout => "netTimeout",
            ReasonCode::AuthDenied => "authDenied",
            ReasonCode::HostKeyChanged => "hostKeyChanged",
            ReasonCode::ConnRefused => "connRefused",
            ReasonCode::DnsFail => "dnsFail",
            ReasonCode::KeyUnusable => "keyUnusable",
            ReasonCode::RemotePortBusy => "remotePortBusy",
            ReasonCode::ConnReset => "connReset",
            ReasonCode::SpawnFail => "spawnFail",
            ReasonCode::ExitUnknown => "exitUnknown",
            ReasonCode::RetryStopped => "retryStopped",
            ReasonCode::SignalExit => "signalExit",
            ReasonCode::ServerMissing => "serverMissing",
            ReasonCode::ServiceMissing => "serviceMissing",
            ReasonCode::ConfigUntrusted => "configUntrusted",
            ReasonCode::ConfigUnsupported => "configUnsupported",
            ReasonCode::CleanupFail => "cleanupFail",
            ReasonCode::StartFail => "startFail",
            ReasonCode::CrashLoop => "crashLoop",
            ReasonCode::Hung => "hung",
            ReasonCode::Exited => "exited",
            ReasonCode::AutostartFail => "autostartFail",
            ReasonCode::AppStarted => "appStarted",
            ReasonCode::AppExited => "appExited",
            ReasonCode::ConfigSaved => "configSaved",
            ReasonCode::ConfigMigrated => "configMigrated",
            ReasonCode::ConfigNormalized => "configNormalized",
            ReasonCode::ConfigCorrupt => "configCorrupt",
            ReasonCode::StateReset => "stateReset",
            ReasonCode::Deleted => "deleted",
            ReasonCode::Restored => "restored",
            ReasonCode::UpdateChecked => "updateChecked",
            ReasonCode::UpdateAvailable => "updateAvailable",
            ReasonCode::ProtectionChecked => "protectionChecked",
            ReasonCode::ProtectionDrift => "protectionDrift",
            ReasonCode::ProtectionError => "protectionError",
            ReasonCode::DoctorRan => "doctorRan",
            ReasonCode::TunnelConnected => "tunnelConnected",
            ReasonCode::TunnelDown => "tunnelDown",
            ReasonCode::ServiceUp => "serviceUp",
            ReasonCode::ServiceDown => "serviceDown",
        }
    }

    /// 默认中文文案（大白话、带下一步；`{…}` 用 [`ReasonCode::render`] 填）。
    pub fn text(self) -> &'static str {
        match self {
            ReasonCode::SshMissing => {
                "这台电脑上找不到 ssh 命令：去「设置 → 应用 → 可选功能」里装上「OpenSSH 客户端」再试"
            }
            ReasonCode::NetTimeout => {
                "连服务器超时或不通：先检查网络，再看服务器地址和 SSH 端口填得对不对"
            }
            ReasonCode::AuthDenied => {
                "登录被服务器拒绝了：账号或密钥不对，也可能服务器关掉了这种登录方式"
            }
            ReasonCode::HostKeyChanged => {
                "服务器指纹变了：可能换了机器，也可能有人在冒名顶替；核对清楚再连"
            }
            ReasonCode::ConnRefused => {
                "服务器拒绝连接：SSH 端口可能不对，或者服务器上没开 SSH 服务"
            }
            ReasonCode::DnsFail => "服务器地址解析不了：域名写错了，或者本机网络/DNS 有问题",
            ReasonCode::KeyUnusable => "密钥文件用不了：重新用密码配一次免密，或者核对密钥路径",
            ReasonCode::RemotePortBusy => "云端端口被别的东西占了：换一个云端端口再试",
            ReasonCode::ConnReset => {
                "服务器把连接掐断了：可能是网络抖动或服务器限流，稍后会自动重连"
            }
            ReasonCode::SpawnFail => "连接程序启动失败：详情看这条隧道的日志",
            ReasonCode::ExitUnknown => {
                "这一轮没连上：网络、服务器地址或账号密钥都可能；详情看这条隧道的日志"
            }
            ReasonCode::RetryStopped => {
                "试了很多次还是没连上，已经停止自动重试；点「再试一次」重新开始"
            }
            ReasonCode::SignalExit => "连接程序意外退出了（退出码 {code}）：稍后会按计划重连",
            ReasonCode::ServerMissing => {
                "这条隧道挂的云端服务器不在了：去「云端服务器」页重新选一台"
            }
            ReasonCode::ServiceMissing => {
                "这条隧道对应的本机服务不在了：先把服务补回来，或者删掉这条隧道"
            }
            ReasonCode::ConfigUntrusted => {
                "配置文件上次没读成功（已留底成 config.json.bad），新设置暂时不能保存；请先修好配置文件再重开应用"
            }
            ReasonCode::ConfigUnsupported => {
                "这份设置是新版「上线助手」存的（版本 {found}），当前版本只认到 {supported}。请先升级到最新版再打开；你的设置文件没有被改过"
            }
            ReasonCode::CleanupFail => {
                "云端残留连接没清干净：下次会自动再试；反复出现就手动检查一下"
            }
            ReasonCode::StartFail => "本机服务启动失败：{原因}",
            ReasonCode::CrashLoop => "连续启动失败，已经停止自动重试；点「启动」可以再试一次",
            ReasonCode::Hung => "程序在跑但端口不通：可能卡住了；看护开着的话会自动重启",
            ReasonCode::Exited => "程序退出了（退出码 {code}）：看护开着的话会自动拉起",
            ReasonCode::AutostartFail => "开机自启没拉起来：{原因}",
            ReasonCode::AppStarted => "应用已启动",
            ReasonCode::AppExited => "应用已退出",
            ReasonCode::ConfigSaved => "设置已保存",
            ReasonCode::ConfigMigrated => {
                "旧版设置已升级到新版格式（原文件备份成 config.json.v5.bak）"
            }
            ReasonCode::ConfigNormalized => "设置里有不认识的取值，已经按默认值处理：{字段}",
            ReasonCode::ConfigCorrupt => "配置文件读不出来，原文件已留底；先用空白设置运行",
            ReasonCode::StateReset => "运行记录重新开始了（不影响你的设置）",
            ReasonCode::Deleted => "已删除「{name}」（10 分钟内可以撤销）",
            ReasonCode::Restored => "已恢复「{name}」；隧道会保持暂停，想连上再点开始",
            ReasonCode::UpdateChecked => "更新检查完成：{结果}",
            ReasonCode::UpdateAvailable => "发现新版本 {latest}，可以去发布页下载",
            ReasonCode::ProtectionChecked => "云端设置核对过了，对得上",
            ReasonCode::ProtectionDrift => "云端设置和你的设置对不上：{差异}",
            ReasonCode::ProtectionError => "云端核不上了：{原因}",
            ReasonCode::DoctorRan => "自检完成：{pass} 项通过、{fail} 项有问题、{skip} 项跳过",
            ReasonCode::TunnelConnected => "发布已连上",
            ReasonCode::TunnelDown => "{reason}",
            ReasonCode::ServiceUp => "本机服务已就绪",
            ReasonCode::ServiceDown => "{reason}",
        }
    }

    /// 建议动作（doctor / 界面修复带用）。事件专用码没有建议，返回 None。
    pub fn advice(self) -> Option<&'static str> {
        match self {
            ReasonCode::SshMissing => Some("安装 OpenSSH 客户端后重试"),
            ReasonCode::NetTimeout => Some("检查网络与服务器地址、SSH 端口"),
            ReasonCode::AuthDenied => Some("用密码重配一次免密，或核对账号密钥"),
            ReasonCode::HostKeyChanged => Some("先核对服务器身份，别直接连"),
            ReasonCode::ConnRefused => Some("核对 SSH 端口与服务器 sshd"),
            ReasonCode::DnsFail => Some("检查主机名与 DNS"),
            ReasonCode::KeyUnusable => Some("重新配免密或改密钥路径"),
            ReasonCode::RemotePortBusy => Some("换一个云端端口"),
            ReasonCode::ConnReset => Some("等自动重连；持续出现查服务器"),
            ReasonCode::SpawnFail => Some("看日志；多为 ssh 路径或权限问题"),
            ReasonCode::ExitUnknown => Some("看日志定位"),
            ReasonCode::RetryStopped => Some("修好地址、网络或密钥后点「再试一次」"),
            ReasonCode::SignalExit => Some("等自动重连；连续出现看日志"),
            ReasonCode::ServerMissing => Some("重新选服务器"),
            ReasonCode::ServiceMissing => Some("补回服务或删隧道"),
            ReasonCode::ConfigUntrusted => Some("修好配置或恢复备份"),
            ReasonCode::ConfigUnsupported => Some("升级应用"),
            ReasonCode::CleanupFail => Some("稍后重试"),
            ReasonCode::StartFail => Some("看程序日志/路径"),
            ReasonCode::CrashLoop => Some("修好程序再点启动"),
            ReasonCode::Hung => Some("看护开着就等它重启"),
            ReasonCode::Exited => Some("看护开着就等自动拉起"),
            ReasonCode::AutostartFail => Some("手动启动一次"),
            ReasonCode::ConfigCorrupt => Some("修好或恢复 .bad 文件"),
            ReasonCode::Deleted => Some("撤销用 `cli undo last`"),
            ReasonCode::DoctorRan => Some("有问题看每条的 advice"),
            ReasonCode::ProtectionDrift => Some("重新贴云端配置"),
            ReasonCode::ProtectionError => Some("检查服务器连接后重试"),
            ReasonCode::TunnelDown | ReasonCode::ServiceDown => Some("见原因"),
            _ => None,
        }
    }

    /// 把 `{code}` / `{原因}` / `{字段}` / `{差异}` / `{结果}` / `{latest}` /
    /// `{pass}` / `{fail}` / `{skip}` / `{found}` / `{supported}` / `{name}` /
    /// `{reason}` 填进文案里（没给的占位符保持原样，方便排查）。
    pub fn render(self, values: &[(&str, &str)]) -> String {
        let mut out = self.text().to_string();
        for (key, value) in values {
            out = out.replace(&format!("{{{key}}}"), value);
        }
        out
    }

    /// 字符串 → 原因码（读 state/events 的 code 字段用）；不认识返回 None。
    pub fn from_code(code: &str) -> Option<ReasonCode> {
        ReasonCode::ALL.iter().copied().find(|c| c.as_str() == code)
    }

    /// ssh 输出/退出日志 → 原因码（原 `exit_reason_from_log` 关键字表）。
    /// 认不出返回 None，由调用方按退出码兜底（255 → [`ReasonCode::ExitUnknown`]，其余 →
    /// [`ReasonCode::SignalExit`]）。
    pub fn from_ssh_text(tail: &str) -> Option<ReasonCode> {
        let lower = tail.to_ascii_lowercase();
        let has = |needle: &str| lower.contains(needle);

        if has("无法启动 ssh") || has("cannot find the file") {
            return Some(ReasonCode::SshMissing);
        }
        if has("permission denied") || has("authentication failed") || has("no supported authentication")
        {
            return Some(ReasonCode::AuthDenied);
        }
        if has("remote host identification has changed") || has("host key verification failed") {
            return Some(ReasonCode::HostKeyChanged);
        }
        if has("could not resolve hostname")
            || has("name or service not known")
            || has("temporary failure in name resolution")
        {
            return Some(ReasonCode::DnsFail);
        }
        if has("connection refused") {
            return Some(ReasonCode::ConnRefused);
        }
        if has("connection timed out")
            || has("operation timed out")
            || has("network is unreachable")
            || has("no route to host")
            || has("timed out")
        {
            return Some(ReasonCode::NetTimeout);
        }
        if has("no such file") || has("identity file") {
            return Some(ReasonCode::KeyUnusable);
        }
        if has("remote port forwarding failed") {
            return Some(ReasonCode::RemotePortBusy);
        }
        if has("connection closed by remote host") || has("connection reset by peer") {
            return Some(ReasonCode::ConnReset);
        }
        None
    }

    /// 本机服务侧的人话原因 → 原因码（服务失败记录用）。
    /// 认不出来按 [`ReasonCode::ExitUnknown`] 兜底。
    pub fn from_service_text(text: &str) -> ReasonCode {
        if text.contains("假死") {
            if text.contains("停止自动重试") {
                return ReasonCode::CrashLoop;
            }
            return ReasonCode::Hung;
        }
        if text.contains("停止自动重试") || text.contains("连续启动失败") {
            return ReasonCode::CrashLoop;
        }
        if text.contains("自启") {
            return ReasonCode::AutostartFail;
        }
        if text.contains("启动失败") || text.contains("启动程序") {
            return ReasonCode::StartFail;
        }
        if text.contains("退出") {
            return ReasonCode::Exited;
        }
        ReasonCode::ExitUnknown
    }

    /// 隧道退出时，ssh 文本 + 退出码 → 原因码（单一入口，guardian 直接调）。
    /// 文本认不出来时：255 → `exitUnknown`，其他非 0 → `signalExit`。
    pub fn from_ssh_exit(tail: &str, exit_code: i32) -> ReasonCode {
        if let Some(code) = ReasonCode::from_ssh_text(tail) {
            return code;
        }
        if exit_code == 255 {
            ReasonCode::ExitUnknown
        } else {
            ReasonCode::SignalExit
        }
    }
}

impl std::fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ReasonCode {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ReasonCode::from_code(s).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单测锁死 as_str 与 serde 序列化逐值一致，并且不多不少正好 42 个。
    #[test]
    fn as_str_matches_serde_for_all_codes() {
        assert_eq!(ReasonCode::ALL.len(), 42);
        let mut seen = std::collections::BTreeSet::new();
        for code in ReasonCode::ALL {
            let serialized = serde_json::to_value(code).unwrap();
            let text = serialized.as_str().expect("枚举必须序列化成字符串");
            assert_eq!(code.as_str(), text, "as_str 与 serde 不一致：{code:?}");
            assert!(seen.insert(text.to_string()), "重复的 code：{text}");
            assert_eq!(ReasonCode::from_code(text), Some(code));
            assert_eq!(text.parse::<ReasonCode>(), Ok(code));
            // 文案不能是空的，也不能太长（大白话短句）
            assert!(!code.text().is_empty(), "{text} 没有文案");
            assert!(code.text().chars().count() <= 100, "{text} 文案太长了");
        }
        assert_eq!(seen.len(), 42, "42 个码必须互不相同");
        assert_eq!(ReasonCode::from_code("没有这个码"), None);
    }

    /// ssh_error_human 的现有 7 类样例输入，输出必须与文案表一致。
    #[test]
    fn ssh_text_maps_to_expected_codes() {
        let cases: &[(&str, ReasonCode)] = &[
            ("Permission denied (publickey).", ReasonCode::AuthDenied),
            ("Host key verification failed.", ReasonCode::HostKeyChanged),
            (
                "ssh: Could not resolve hostname bad: Name or service not known",
                ReasonCode::DnsFail,
            ),
            (
                "connect to host x port 22: Connection refused",
                ReasonCode::ConnRefused,
            ),
            (
                "remote port forwarding failed for listen port 18080",
                ReasonCode::RemotePortBusy,
            ),
            ("ssh: connect to host x port 22: Connection timed out", ReasonCode::NetTimeout),
            ("Read from socket failed: Connection reset by peer", ReasonCode::ConnReset),
        ];
        for (tail, want) in cases {
            assert_eq!(ReasonCode::from_ssh_text(tail), Some(*want), "输入：{tail}");
            assert_eq!(ReasonCode::from_ssh_exit(tail, 255), *want);
        }
        assert_eq!(ReasonCode::from_ssh_text("everything is fine"), None);
        assert_eq!(
            ReasonCode::from_ssh_exit("everything is fine", 255),
            ReasonCode::ExitUnknown
        );
        assert_eq!(
            ReasonCode::from_ssh_exit("everything is fine", 1),
            ReasonCode::SignalExit
        );
        assert_eq!(
            ReasonCode::from_ssh_text("This program cannot find the file specified"),
            Some(ReasonCode::SshMissing)
        );
    }

    #[test]
    fn service_text_maps_to_expected_codes() {
        assert_eq!(
            ReasonCode::from_service_text("连续启动失败，已经停止自动重试"),
            ReasonCode::CrashLoop
        );
        assert_eq!(
            ReasonCode::from_service_text("启动失败：拒绝访问（os error 5）"),
            ReasonCode::StartFail
        );
        assert_eq!(
            ReasonCode::from_service_text("进程活着但端口不通（假死）；看护没开，可以点启动重试"),
            ReasonCode::Hung
        );
        assert_eq!(
            ReasonCode::from_service_text("连续假死，已经停止自动重试"),
            ReasonCode::CrashLoop
        );
        assert_eq!(
            ReasonCode::from_service_text("程序退出了（退出码 1）"),
            ReasonCode::Exited
        );
        assert_eq!(
            ReasonCode::from_service_text("开机自启没拉起来：路径不对"),
            ReasonCode::AutostartFail
        );
        assert_eq!(
            ReasonCode::from_service_text("看不懂的一句话"),
            ReasonCode::ExitUnknown
        );
    }

    #[test]
    fn retry_stopped_copy_is_frozen() {
        assert_eq!(ReasonCode::RetryStopped.as_str(), "retryStopped");
        assert_eq!(
            ReasonCode::RetryStopped.text(),
            "试了很多次还是没连上，已经停止自动重试；点「再试一次」重新开始"
        );
        assert_eq!(
            ReasonCode::RetryStopped.advice(),
            Some("修好地址、网络或密钥后点「再试一次」")
        );
        assert_eq!(
            ReasonCode::from_code("retryStopped"),
            Some(ReasonCode::RetryStopped)
        );
    }

    #[test]
    fn render_fills_placeholders() {
        assert_eq!(
            ReasonCode::SignalExit.render(&[("code", "1")]),
            "连接程序意外退出了（退出码 1）：稍后会按计划重连"
        );
        assert_eq!(
            ReasonCode::DoctorRan.render(&[("pass", "7"), ("fail", "1"), ("skip", "2")]),
            "自检完成：7 项通过、1 项有问题、2 项跳过"
        );
        assert_eq!(
            ReasonCode::Deleted.render(&[("name", "旧站")]),
            "已删除「旧站」（10 分钟内可以撤销）"
        );
        assert_eq!(
            ReasonCode::ProtectionDrift.render(&[("差异", "云端没开密码")]),
            "云端设置和你的设置对不上：云端没开密码"
        );
        // 没给的占位符保持原样
        assert!(ReasonCode::StartFail.render(&[]).contains("{原因}"));
        assert!(ReasonCode::SshMissing.advice().is_some());
        assert!(ReasonCode::AppStarted.advice().is_none());
    }
}
