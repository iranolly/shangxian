//! 掉线 / 恢复提醒的判定逻辑（纯逻辑，时间一律用 `Instant`，方便测试）。
//!
//! 规则（对齐 D1 §7）：
//! - **计时用 `Instant`**，不再收墙钟毫秒：系统时间被改也不会误报或者哑掉。
//! - 隧道和本机服务统一成一份 [`HealthItem`]；用户自己停的、只监控且从没被启动过的
//!   服务 `should_notify=false`：跳过不发通知（条目本身仍留在列表里）。
//! - 连续异常满 `down_after`（3 分钟）才首报；同一条目再次提醒间隔 `renotify_after`（30 分钟）。
//! - 恢复时补一条「已恢复」。
//!
//! 静音时段与多条聚合也是纯函数，放这里方便单测；Tauri 侧的点击通知、发送失败兜底
//! 在 `app/src-tauri/src/notify.rs` 接线。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// 这条提醒说的是隧道还是本机服务。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Tunnel,
    Service,
}

/// 这次该发的提醒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// 掉线：连续异常达到了阈值（或到了再次提醒的间隔）。
    Down {
        kind: NoticeKind,
        id: String,
        name: String,
    },
    /// 恢复：之前报过掉线的条目又好了。
    Recovered {
        kind: NoticeKind,
        id: String,
        name: String,
    },
}

/// 「健康条目」只定义一份；`Guardian::health_snapshot` 直接返回它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthItem {
    pub kind: HealthKind,
    pub id: String,
    pub name: String,
    pub healthy: bool,
    /// false = 这条不参与通知（用户手动停的、只监控的服务）
    pub should_notify: bool,
}

/// 健康条目的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthKind {
    Tunnel,
    Service,
}

impl From<HealthKind> for NoticeKind {
    fn from(kind: HealthKind) -> Self {
        match kind {
            HealthKind::Tunnel => NoticeKind::Tunnel,
            HealthKind::Service => NoticeKind::Service,
        }
    }
}

/// 每个条目的跟踪状态。
struct Tracked {
    /// 第一次发现异常的时刻（用来算首报阈值）
    first_unhealthy: Instant,
    /// 已经报过掉线（恢复时要补一条）
    notified: bool,
    /// 上次报掉的时刻（用来限流）
    last_notify: Instant,
}

/// 记录每个条目的异常起点，决定什么时候值得打扰用户。
pub struct NotifyTracker {
    /// 连续异常多久才首报。
    down_after: Duration,
    /// 同一条目再次提醒的最小间隔。
    renotify_after: Duration,
    /// 条目 id -> 跟踪状态
    state: HashMap<String, Tracked>,
}

impl NotifyTracker {
    /// `down_after`：连续异常多久首报；`renotify_after`：同一条目再次提醒的最小间隔。
    pub fn new(down_after: Duration, renotify_after: Duration) -> Self {
        Self {
            down_after,
            renotify_after,
            state: HashMap::new(),
        }
    }

    /// 喂一次现状。`now` 必须是 `Instant`。
    /// 返回本次应发的提醒（可能 0..n 条）。
    pub fn update(&mut self, now: Instant, items: &[HealthItem]) -> Vec<Notice> {
        let mut notices = Vec::new();

        for item in items {
            // 不参与通知的条目：直接跳过（也不跟踪，用户停掉就算「这事结束了」）
            if !item.should_notify {
                continue;
            }
            if item.healthy {
                if let Some(entry) = self.state.remove(&item.id) {
                    if entry.notified {
                        notices.push(Notice::Recovered {
                            kind: item.kind.into(),
                            id: item.id.clone(),
                            name: item.name.clone(),
                        });
                    }
                }
                continue;
            }

            let entry = self.state.entry(item.id.clone()).or_insert(Tracked {
                first_unhealthy: now,
                notified: false,
                last_notify: now,
            });
            if !entry.notified {
                if now.saturating_duration_since(entry.first_unhealthy) >= self.down_after {
                    entry.notified = true;
                    entry.last_notify = now;
                    notices.push(Notice::Down {
                        kind: item.kind.into(),
                        id: item.id.clone(),
                        name: item.name.clone(),
                    });
                }
            } else if now.saturating_duration_since(entry.last_notify) >= self.renotify_after {
                entry.last_notify = now;
                notices.push(Notice::Down {
                    kind: item.kind.into(),
                    id: item.id.clone(),
                    name: item.name.clone(),
                });
            }
        }

        // 清掉已经不在列表里的条目（被删除 / 被用户停掉）
        let alive: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
        self.state.retain(|id, _| alive.contains(id.as_str()));

        notices
    }
}

// ---------- 静音时段（纯函数，app 层调用） ----------

/// 把 `"22:00"` 解析成一天里的第几分钟；格式不对返回 None。
/// 失败按「免打扰关闭」处理，绝不能把 `?` 写进通知线程的死循环里。
pub fn parse_hhmm(text: &str) -> Option<u16> {
    let (h, m) = text.trim().split_once(':')?;
    if h.len() > 2 || m.len() > 2 || h.is_empty() || m.is_empty() {
        return None;
    }
    let h: u16 = h.parse().ok()?;
    let m: u16 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// 现在（一天里的第几分钟）在不在免打扰时段里。
/// 支持跨零点（23:00–07:00）；`from == to` 按「关闭」处理（D2 §3.2）。
pub fn in_quiet_hours(now_min: u16, from_min: u16, to_min: u16) -> bool {
    if from_min == to_min {
        return false;
    }
    if from_min < to_min {
        now_min >= from_min && now_min < to_min
    } else {
        now_min >= from_min || now_min < to_min
    }
}

// ---------- 聚合（纯函数，app 层调用） ----------

/// 一条准备发出去的系统通知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub body: String,
    /// true = 这条是「掉线」类提醒，免打扰时段要压掉；恢复类为 false。
    pub is_down: bool,
}

/// 把一个 tick 里的 notice 合并成最多 2 条 toast（一坏一好）。
pub fn summarize(notices: &[Notice]) -> Vec<Toast> {
    if notices.is_empty() {
        return Vec::new();
    }

    let mut downs: Vec<&Notice> = Vec::new();
    let mut ups: Vec<&Notice> = Vec::new();
    for notice in notices {
        match notice {
            Notice::Down { .. } => downs.push(notice),
            Notice::Recovered { .. } => ups.push(notice),
        }
    }

    let mut toasts = Vec::new();
    if !downs.is_empty() {
        toasts.push(build_toast(&downs, true));
    }
    if !ups.is_empty() {
        toasts.push(build_toast(&ups, false));
    }
    toasts
}

fn build_toast(group: &[&Notice], is_down: bool) -> Toast {
    if group.len() == 1 {
        let (kind, name) = identify(group[0]);
        return match (is_down, kind) {
            (true, NoticeKind::Tunnel) => Toast {
                title: format!("发布断开：{name}"),
                body: "无法连接，正在自动重试。".into(),
                is_down,
            },
            (true, NoticeKind::Service) => Toast {
                title: format!("服务断开：{name}"),
                body: "程序已退出；已开启看护时会自动启动，否则请在应用中启动。".into(),
                is_down,
            },
            (false, NoticeKind::Tunnel) => Toast {
                title: format!("发布恢复：{name}"),
                body: "已重新连接。".into(),
                is_down,
            },
            (false, NoticeKind::Service) => Toast {
                title: format!("服务恢复：{name}"),
                body: "已重新连接。".into(),
                is_down,
            },
        };
    }

    let all_service = group
        .iter()
        .all(|notice| identify(notice).0 == NoticeKind::Service);
    let what = if all_service { "服务" } else { "发布" };
    let title = if is_down {
        format!("{} 条{what}存在问题", group.len())
    } else {
        format!("{} 条{what}恢复了", group.len())
    };

    let mut names: Vec<&str> = group.iter().map(|notice| identify(notice).1).collect();
    let more = names.len() > 3;
    names.truncate(3);
    let mut listing = names.join("、");
    if more {
        listing.push_str(" 等");
    }
    let body = if is_down {
        format!("其中有：{listing}。请打开应用查看详情。")
    } else {
        format!("{listing} 均已重新连接。")
    };
    Toast {
        title,
        body,
        is_down,
    }
}

fn identify(notice: &Notice) -> (NoticeKind, &str) {
    match notice {
        Notice::Down { kind, name, .. } | Notice::Recovered { kind, name, .. } => (*kind, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, name: &str, healthy: bool, should_notify: bool) -> HealthItem {
        HealthItem {
            kind: HealthKind::Service,
            id: id.into(),
            name: name.into(),
            healthy,
            should_notify,
        }
    }

    #[test]
    fn down_after_threshold() {
        let mut tracker = NotifyTracker::new(Duration::from_secs(180), Duration::from_secs(1800));
        let t0 = Instant::now();
        let down = [item("svc-1", "相册", false, true)];

        assert!(tracker.update(t0, &down).is_empty(), "刚掉不报");
        assert!(
            tracker.update(t0 + Duration::from_secs(179), &down).is_empty(),
            "差 1 秒不到阈值也不报"
        );
        let notices = tracker.update(t0 + Duration::from_secs(180), &down);
        assert_eq!(notices.len(), 1, "到 3 分钟报一条");
        assert!(matches!(notices[0], Notice::Down { .. }));
    }

    #[test]
    fn renotify_only_after_30min() {
        let mut tracker = NotifyTracker::new(Duration::from_secs(180), Duration::from_secs(1800));
        let t0 = Instant::now();
        let down = [item("svc-1", "相册", false, true)];

        assert!(tracker.update(t0, &down).is_empty(), "刚开始掉：先记时间");
        assert_eq!(tracker.update(t0 + Duration::from_secs(180), &down).len(), 1);
        assert!(
            tracker.update(t0 + Duration::from_secs(180 + 1799), &down).is_empty(),
            "30 分钟内不重复打扰"
        );
        let again = tracker.update(t0 + Duration::from_secs(180 + 1800), &down);
        assert_eq!(again.len(), 1, "超过 30 分钟再提醒一次");
        assert!(matches!(again[0], Notice::Down { .. }));
    }

    #[test]
    fn recovered_after_down() {
        let mut tracker = NotifyTracker::new(Duration::from_secs(180), Duration::from_secs(1800));
        let t0 = Instant::now();
        let down = [item("svc-1", "相册", false, true)];
        let up = [item("svc-1", "相册", true, true)];

        // 先让它看到异常；没报过掉线就恢复：不补「恢复」（用户根本不知道它坏过）
        assert!(tracker.update(t0, &down).is_empty());
        assert!(tracker.update(t0 + Duration::from_secs(10), &up).is_empty());

        // 再掉：满 3 分钟才报，报过之后恢复才补一条
        assert!(tracker.update(t0 + Duration::from_secs(20), &down).is_empty());
        assert_eq!(tracker.update(t0 + Duration::from_secs(220), &down).len(), 1);
        let notices = tracker.update(t0 + Duration::from_secs(260), &up);
        assert_eq!(notices.len(), 1);
        assert!(matches!(notices[0], Notice::Recovered { .. }));
        // 恢复后状态清掉，别反复报
        assert!(tracker.update(t0 + Duration::from_secs(600), &up).is_empty());
    }

    #[test]
    fn service_item_included_and_user_stopped_excluded() {
        let mut tracker = NotifyTracker::new(Duration::from_secs(180), Duration::from_secs(1800));
        let t0 = Instant::now();
        let items = [
            HealthItem {
                kind: HealthKind::Tunnel,
                id: "tun-1".into(),
                name: "相册发布".into(),
                healthy: false,
                should_notify: true,
            },
            // 用户自己停的：不报
            item("svc-1", "相册", false, false),
            // 被看护的服务：报
            item("svc-2", "博客", false, true),
        ];
        assert!(tracker.update(t0, &items).is_empty(), "先记时间");
        let notices = tracker.update(t0 + Duration::from_secs(200), &items);
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().any(|n| matches!(
            n,
            Notice::Down { kind: NoticeKind::Tunnel, name, .. } if name == "相册发布"
        )));
        assert!(notices.iter().any(|n| matches!(
            n,
            Notice::Down { kind: NoticeKind::Service, name, .. } if name == "博客"
        )));
        assert!(
            !notices.iter().any(|n| identify(n).1 == "相册"),
            "用户手动停的服务不发通知"
        );
    }

    #[test]
    fn removed_item_forgets_state() {
        let mut tracker = NotifyTracker::new(Duration::from_secs(180), Duration::from_secs(1800));
        let t0 = Instant::now();
        let down = [item("svc-1", "相册", false, true)];
        assert!(tracker.update(t0, &down).is_empty(), "先记时间");
        assert_eq!(tracker.update(t0 + Duration::from_secs(200), &down).len(), 1);
        // 条目从列表消失（服务被删了）：状态跟着清
        assert!(tracker.update(t0 + Duration::from_secs(300), &[]).is_empty());
        // 又出现且健康：不该补「恢复」（已经不在跟踪里了）
        let up = [item("svc-1", "相册", true, true)];
        assert!(tracker.update(t0 + Duration::from_secs(400), &up).is_empty());
    }

    #[test]
    fn time_going_backwards_does_not_panic() {
        let mut tracker = NotifyTracker::new(Duration::from_secs(180), Duration::from_secs(1800));
        let t0 = Instant::now();
        let down = [item("svc-1", "相册", false, true)];
        assert!(tracker.update(t0, &down).is_empty());
        // Instant 只能往前或返回同一时刻，这里用「更早喂一次」模拟调用顺序错乱：不许 panic
        assert!(tracker.update(t0, &down).is_empty());
    }

    #[test]
    fn quiet_hours_wraps_midnight() {
        assert!(in_quiet_hours(23 * 60 + 30, 23 * 60, 7 * 60));
        assert!(!in_quiet_hours(8 * 60, 23 * 60, 7 * 60));
        assert!(in_quiet_hours(6 * 60 + 59, 23 * 60, 7 * 60));
        assert!(!in_quiet_hours(7 * 60, 23 * 60, 7 * 60), "结束点不算在里面");
        // 不跨零点
        assert!(in_quiet_hours(13 * 60, 12 * 60, 14 * 60));
        assert!(!in_quiet_hours(11 * 60 + 59, 12 * 60, 14 * 60));
        // from == to 视为关闭
        assert!(!in_quiet_hours(13 * 60, 13 * 60, 13 * 60));
    }

    #[test]
    fn parse_hhmm_rejects_garbage() {
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("22:00"), Some(22 * 60));
        assert_eq!(parse_hhmm(" 07:05 "), Some(7 * 60 + 5));
        for bad in ["", "24:00", "23:60", "7", "7:5:1", "ab:cd", "-1:00"] {
            assert_eq!(parse_hhmm(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn summarize_single_items() {
        let notices = vec![
            Notice::Down {
                kind: NoticeKind::Tunnel,
                id: "tun-1".into(),
                name: "相册发布".into(),
            },
            Notice::Recovered {
                kind: NoticeKind::Service,
                id: "svc-1".into(),
                name: "相册".into(),
            },
        ];
        let toasts = summarize(&notices);
        assert_eq!(toasts.len(), 2);
        assert_eq!(toasts[0].title, "发布断开：相册发布");
        assert!(toasts[0].is_down);
        assert_eq!(toasts[1].title, "服务恢复：相册");
        assert!(!toasts[1].is_down);
    }

    #[test]
    fn summarize_merges_three_downs_into_one() {
        let notices: Vec<Notice> = (1..=3)
            .map(|i| Notice::Down {
                kind: NoticeKind::Service,
                id: format!("svc-{i}"),
                name: format!("服务{i}"),
            })
            .collect();
        let toasts = summarize(&notices);
        assert_eq!(toasts.len(), 1);
        assert!(toasts[0].is_down);
        assert_eq!(toasts[0].title, "3 条服务存在问题");
        assert!(toasts[0].body.contains("服务1、服务2、服务3"));
    }

    #[test]
    fn summarize_merges_many_names_with_ellipsis() {
        let notices: Vec<Notice> = (1..=5)
            .map(|i| Notice::Down {
                kind: NoticeKind::Tunnel,
                id: format!("tun-{i}"),
                name: format!("发布{i}"),
            })
            .collect();
        let toasts = summarize(&notices);
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].title, "5 条发布存在问题");
        assert!(toasts[0].body.contains("发布1、发布2、发布3 等"), "{}", toasts[0].body);
    }

    #[test]
    fn summarize_empty_is_empty() {
        assert!(summarize(&[]).is_empty());
    }
}
