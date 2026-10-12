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
                "本机未找到 ssh 命令：请在「设置 → 应用 → 可选功能」中安装「OpenSSH 客户端」后重试"
            }
            ReasonCode::NetTimeout => {
                "连接服务器超时或不通：请先检查网络，再核对服务器地址和 SSH 端口是否填写正确"
            }
            ReasonCode::AuthDenied => {
                "登录被服务器拒绝了：账号或密钥不正确，也可能服务器已禁用这种登录方式"
            }
            ReasonCode::HostKeyChanged => {
                "服务器指纹已变更：可能是更换了机器，也可能存在冒名顶替；请核对清楚后再连接"
            }
            ReasonCode::ConnRefused => {
                "服务器拒绝连接：SSH 端口可能不正确，或者服务器未开启 SSH 服务"
            }
            ReasonCode::DnsFail => "服务器地址无法解析：域名可能写错，或本机网络/DNS 异常",
            ReasonCode::KeyUnusable => "密钥文件不可用：请重新用密码配置一次免密登录，或核对密钥路径",
            ReasonCode::RemotePortBusy => "云端端口已被其他程序占用：请更换一个云端端口后重试",
            ReasonCode::ConnReset => {
                "服务器断开了连接：可能是网络抖动或服务器限流，稍后将自动重连"
            }
            ReasonCode::SpawnFail => "连接程序启动失败：详情请查看这条隧道的日志",
            ReasonCode::ExitUnknown => {
                "本轮未连接成功：可能是网络、服务器地址或账号密钥问题；详情请查看这条隧道的日志"
            }
            ReasonCode::RetryStopped => {
                "多次尝试仍无法连接，已停止自动重试；请先点击「暂停」，再点击「开始发布」重新尝试"
            }
            ReasonCode::SignalExit => "连接程序意外退出（退出码 {code}）：稍后将按计划重连",
            ReasonCode::ServerMissing => {
                "这条隧道关联的云端服务器已不存在：请在「云端服务器」页重新选择一台"
            }
            ReasonCode::ServiceMissing => {
                "这条隧道对应的本机服务已不存在：请先恢复服务，或删除这条隧道"
            }
            ReasonCode::ConfigUntrusted => {
                "配置文件上次读取失败（已备份为 config.json.bad），新设置暂时无法保存；请先修复配置文件再重开应用"
            }
            ReasonCode::ConfigUnsupported => {
                "这份设置由新版「上线助手」保存（版本 {found}），当前版本仅支持到 {supported}。请先升级到最新版再打开；设置文件未被修改"
            }
            ReasonCode::CleanupFail => {
                "云端残留连接未清理干净：下次将自动重试；若反复出现，请手动检查"
            }
            ReasonCode::StartFail => "本机服务启动失败：{原因}",
            ReasonCode::CrashLoop => "连续启动失败，已停止自动重试；点击「启动」可再试一次",
            ReasonCode::Hung => "程序运行中但端口不通：可能已卡住；看护开启时会自动重启",
            ReasonCode::Exited => "程序已退出（退出码 {code}）：看护开启时会自动启动",
            ReasonCode::AutostartFail => "开机自启未启动：{原因}",
            ReasonCode::AppStarted => "应用已启动",
            ReasonCode::AppExited => "应用已退出",
            ReasonCode::ConfigSaved => "设置已保存",
            ReasonCode::ConfigMigrated => {
                "旧版设置已升级到新版格式（原文件备份成 config.json.v5.bak）"
            }
            ReasonCode::ConfigNormalized => "设置中存在无法识别的取值，已按默认值处理：{字段}",
            ReasonCode::ConfigCorrupt => "配置文件无法读取，原文件已备份；将先用空白设置运行",
            ReasonCode::StateReset => "运行记录已重新开始（不影响现有设置）",
            ReasonCode::Deleted => "已删除「{name}」（10 分钟内可以撤销）",
            ReasonCode::Restored => "已恢复「{name}」；隧道将保持暂停，需要连接时请点击开始",
            ReasonCode::UpdateChecked => "更新检查完成：{结果}",
            ReasonCode::UpdateAvailable => "发现新版本 {latest}，可前往发布页下载",
            ReasonCode::ProtectionChecked => "云端设置已核对，结果一致",
            ReasonCode::ProtectionDrift => "云端设置与本地设置不一致：{差异}",
            ReasonCode::ProtectionError => "云端无法核对：{原因}",
            ReasonCode::DoctorRan => "体检完成：{pass} 项通过、{fail} 项未通过、{skip} 项跳过",
            ReasonCode::TunnelConnected => "发布已连接",
            ReasonCode::TunnelDown => "{reason}",
            ReasonCode::ServiceUp => "本机服务已就绪",
            ReasonCode::ServiceDown => "{reason}",
        }
    }

    /// 建议动作（doctor / 界面修复带用）。事件专用码没有建议，返回 None。
    pub fn advice(self) -> Option<&'static str> {
        match self {
            ReasonCode::SshMissing => Some("安装 OpenSSH 客户端后重试"),
            ReasonCode::NetTimeout => Some("检查网络、服务器地址与 SSH 端口"),
            ReasonCode::AuthDenied => Some("用密码重新配置一次免密，或核对账号密钥"),
            ReasonCode::HostKeyChanged => Some("请先核对服务器身份，不要直接连接"),
            ReasonCode::ConnRefused => Some("核对 SSH 端口与服务器 sshd 服务"),
            ReasonCode::DnsFail => Some("检查主机名与 DNS"),
            ReasonCode::KeyUnusable => Some("重新配置免密或修改密钥路径"),
            ReasonCode::RemotePortBusy => Some("更换一个云端端口"),
            ReasonCode::ConnReset => Some("等待自动重连；若持续出现，请检查服务器"),
            ReasonCode::SpawnFail => Some("查看日志；通常是 ssh 路径或权限问题"),
            ReasonCode::ExitUnknown => Some("查看日志定位"),
            ReasonCode::RetryStopped => Some("修复地址、网络或密钥后重新开始发布"),
            ReasonCode::SignalExit => Some("等待自动重连；若连续出现，请查看日志"),
            ReasonCode::ServerMissing => Some("重新选择服务器"),
            ReasonCode::ServiceMissing => Some("恢复服务或删除隧道"),
            ReasonCode::ConfigUntrusted => Some("修复配置或恢复备份"),
            ReasonCode::ConfigUnsupported => Some("升级应用"),
            ReasonCode::CleanupFail => Some("稍后重试"),
            ReasonCode::StartFail => Some("查看程序日志/路径"),
            ReasonCode::CrashLoop => Some("修复程序后点击启动"),
            ReasonCode::Hung => Some("看护开启时会自动重启"),
            ReasonCode::Exited => Some("看护开启时会自动启动"),
            ReasonCode::AutostartFail => Some("手动启动一次"),
            ReasonCode::ConfigCorrupt => Some("修复或恢复 .bad 文件"),
            ReasonCode::Deleted => Some("撤销用 `cli undo last`"),
            ReasonCode::DoctorRan => Some("有问题的条目请查看各自建议"),
            ReasonCode::ProtectionDrift => Some("重新粘贴云端配置"),
            ReasonCode::ProtectionError => Some("检查服务器连接后重试"),
            ReasonCode::TunnelDown | ReasonCode::ServiceDown => Some("查看原因"),
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
        if text.contains("无响应") {
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
            ReasonCode::from_service_text("进程活着但端口不通（无响应）；看护没开，可以点启动重试"),
            ReasonCode::Hung
        );
        assert_eq!(
            ReasonCode::from_service_text("连续无响应，已经停止自动重试"),
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
            "多次尝试仍无法连接，已停止自动重试；请先点击「暂停」，再点击「开始发布」重新尝试"
        );
        assert_eq!(
            ReasonCode::RetryStopped.advice(),
            Some("修复地址、网络或密钥后重新开始发布")
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
            "连接程序意外退出（退出码 1）：稍后将按计划重连"
        );
        assert_eq!(
            ReasonCode::DoctorRan.render(&[("pass", "7"), ("fail", "1"), ("skip", "2")]),
            "体检完成：7 项通过、1 项未通过、2 项跳过"
        );
        assert_eq!(
            ReasonCode::Deleted.render(&[("name", "旧站")]),
            "已删除「旧站」（10 分钟内可以撤销）"
        );
        assert_eq!(
            ReasonCode::ProtectionDrift.render(&[("差异", "云端没开密码")]),
            "云端设置与本地设置不一致：云端没开密码"
        );
        // 没给的占位符保持原样
        assert!(ReasonCode::StartFail.render(&[]).contains("{原因}"));
        assert!(ReasonCode::SshMissing.advice().is_some());
        assert!(ReasonCode::AppStarted.advice().is_none());
    }
}
