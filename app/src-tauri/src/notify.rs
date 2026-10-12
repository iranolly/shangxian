//! 掉线 / 恢复通知的后台线程：静音时段、多条聚合、点击打开主窗口、发送失败兜底。
//!
//! 判断「该不该报」「合并成几条 toast」「现在是不是免打扰」的纯逻辑在
//! `core::notify`（K2 已实现并有单测）；这里只负责：每 5 秒取一次健康状况 →
//! 交给 core 判定 → 按结果发系统通知。
//!
//! 计时全部用 `Instant`，系统时间被改也不会误报或哑掉。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shangxian_core::guardian::Guardian;
use shangxian_core::log::{TAG_NOTIFY, log_info, log_warn};
use shangxian_core::notify::{self, HealthItem, HealthKind, NotifyTracker};
use tauri::AppHandle;

/// 连续异常多久首次提醒（3 分钟，防重连抖动轰炸）。
pub const DOWN_AFTER: Duration = Duration::from_secs(3 * 60);
/// 同一条目再次提醒的最小间隔（30 分钟）。
pub const RENOTIFY_AFTER: Duration = Duration::from_secs(30 * 60);
/// 巡检周期。
pub const TICK: Duration = Duration::from_secs(5);

/// 起通知线程（每 5 秒查一次）。
pub fn spawn_guard(guardian: Arc<Mutex<Guardian>>, app: AppHandle) {
    std::thread::spawn(move || {
        let mut tracker = NotifyTracker::new(DOWN_AFTER, RENOTIFY_AFTER);
        loop {
            std::thread::sleep(TICK);
            let (enabled, quiet, health) = {
                let g = match guardian.lock() {
                    Ok(g) => g,
                    Err(_) => continue, // 锁中毒：这一轮跳过，下一轮再来
                };
                (g.notify_enabled(), g.notify_quiet(), g.health_snapshot())
            };
            let notices = tracker.update(Instant::now(), &health);
            if !enabled || notices.is_empty() {
                continue;
            }
            // 时间配置错了按「不免打扰」处理，不能把解析失败变成哑掉。
            let (from, to) = match (
                notify::parse_hhmm(&quiet.start),
                notify::parse_hhmm(&quiet.end),
            ) {
                (Some(f), Some(t)) => (f, t),
                _ => (0, 0),
            };
            let quiet_on =
                quiet.enabled && notify::in_quiet_hours(local_minutes_of_day(), from, to);
            for toast in notify::summarize(&notices) {
                if quiet_on && toast.is_down {
                    log_info(TAG_NOTIFY, "免打扰时段，已跳过一条断开提醒");
                    continue;
                }
                send_toast(&app, &toast.title, &toast.body);
            }
        }
    });
}

/// 注册通知点击 → 打开主窗口。setup 里尽早调一次（注册前发出的通知不报点击）。
pub fn install_click_handler(app: &AppHandle) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    let handle = app.clone();
    app.notification()
        .on_action(move |action| {
            if action.action_id() == "tap" {
                crate::mainwindow::show_or_create(&handle);
            }
        })
        .map_err(|e| format!("通知点击注册失败：{e}"))
}

/// 统一发送入口：失败写 app.log + 发 `sys-notice` 提示用户。返回是否成功。
pub fn send_toast(app: &AppHandle, title: &str, body: &str) -> bool {
    use tauri_plugin_notification::NotificationExt;
    match app.notification().builder().title(title).body(body).show() {
        Ok(()) => {
            log_info(TAG_NOTIFY, &format!("已发送通知：{body}"));
            true
        }
        Err(e) => {
            log_warn(TAG_NOTIFY, &format!("通知发送失败：{e}"));
            crate::sys_notice(
                app,
                "warn",
                "系统通知发送失败，断开和恢复事件仅会记录在应用内；可到系统设置中查看通知权限。",
                false,
            );
            false
        }
    }
}

/// 本机时间的「今天过了多少分钟」（0..1439）。
pub fn local_minutes_of_day() -> u16 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut st = SYSTEMTIME::default();
        unsafe { GetLocalTime(&mut st) };
        st.wHour * 60 + st.wMinute
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// 健康条目里只数隧道的（托盘 tooltip、退出确认只用隧道口径）。
/// 返回 (正在发布, 掉线)。
pub fn tunnel_health(health: &[HealthItem]) -> (usize, usize) {
    let tunnels: Vec<&HealthItem> = health
        .iter()
        .filter(|h| h.kind == HealthKind::Tunnel)
        .collect();
    let publishing = tunnels.iter().filter(|h| h.healthy).count();
    (publishing, tunnels.len() - publishing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_wraps_midnight() {
        assert!(notify::in_quiet_hours(23 * 60 + 30, 23 * 60, 7 * 60));
        assert!(!notify::in_quiet_hours(8 * 60, 23 * 60, 7 * 60));
        assert!(notify::in_quiet_hours(6 * 60 + 59, 23 * 60, 7 * 60));
        assert!(
            !notify::in_quiet_hours(12 * 60, 0, 0),
            "没配时间 = 不免打扰"
        );
        assert!(
            !notify::in_quiet_hours(12 * 60, 13 * 60, 13 * 60),
            "起点=终点 = 关闭"
        );
    }

    #[test]
    fn summarize_merges() {
        use shangxian_core::notify::{Notice, NoticeKind};
        let notices = vec![
            Notice::Down {
                kind: NoticeKind::Service,
                id: "a".into(),
                name: "相册".into(),
            },
            Notice::Down {
                kind: NoticeKind::Service,
                id: "b".into(),
                name: "博客".into(),
            },
            Notice::Down {
                kind: NoticeKind::Tunnel,
                id: "c".into(),
                name: "发布".into(),
            },
            Notice::Recovered {
                kind: NoticeKind::Service,
                id: "a".into(),
                name: "相册".into(),
            },
        ];
        let toasts = notify::summarize(&notices);
        assert_eq!(toasts.len(), 2);
        assert!(toasts[0].is_down);
        // 混合了隧道 + 服务：core 的合并标题用「发布」（全是服务时才写「服务」）
        assert_eq!(toasts[0].title, "3 条发布存在问题");
        assert!(toasts[0].body.contains("相册"));
        assert!(!toasts[1].is_down);
    }

    #[test]
    fn tunnel_health_counts_only_tunnels() {
        use shangxian_core::notify::HealthKind;
        let items = vec![
            HealthItem {
                kind: HealthKind::Tunnel,
                id: "t1".into(),
                name: "a".into(),
                healthy: true,
                should_notify: true,
            },
            HealthItem {
                kind: HealthKind::Tunnel,
                id: "t2".into(),
                name: "b".into(),
                healthy: false,
                should_notify: true,
            },
            HealthItem {
                kind: HealthKind::Service,
                id: "s1".into(),
                name: "c".into(),
                healthy: true,
                should_notify: false,
            },
        ];
        assert_eq!(tunnel_health(&items), (1, 1));
    }
}
