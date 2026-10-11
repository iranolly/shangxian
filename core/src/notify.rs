//! 掉线提醒的判定逻辑（纯逻辑，时间由外部传入，方便测试）。
//!
//! 规则（对齐 DESIGN.md「掉线通知」）：
//! - 服务连续异常达到阈值才提醒一次，避免重连抖动狂轰滥炸；
//! - 恢复连接时补一条"已恢复"；
//! - 用户自己暂停的服务不参与判定（由调用方从列表里排除）。

use std::collections::{HashMap, HashSet};

/// 这次该发的提醒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// 掉线：服务连续异常达到了阈值。
    Down { name: String },
    /// 恢复：之前报过掉线的服务又跑起来了。
    Recovered { name: String },
}

/// 记录每个服务的异常起点，决定什么时候值得打扰用户。
pub struct NotifyTracker {
    /// 连续异常多久才提醒（毫秒）
    down_after_ms: u64,
    /// 服务 id -> (第一次发现异常的时刻, 是否已经提醒过掉线)
    state: HashMap<String, (u64, bool)>,
}

impl NotifyTracker {
    pub fn new(down_after_ms: u64) -> Self {
        Self {
            down_after_ms,
            state: HashMap::new(),
        }
    }

    /// 喂一次现状：`services` 为 (id, 显示名, 是否正常)。
    /// 返回这次该发的提醒（0~2 条）。
    pub fn update(&mut self, now_ms: u64, services: &[(String, String, bool)]) -> Vec<Notice> {
        let mut notices = Vec::new();

        for (id, name, healthy) in services {
            if *healthy {
                if let Some((_, notified)) = self.state.remove(id) {
                    if notified {
                        notices.push(Notice::Recovered { name: name.clone() });
                    }
                }
            } else {
                let entry = self.state.entry(id.clone()).or_insert((now_ms, false));
                if !entry.1 && now_ms.saturating_sub(entry.0) >= self.down_after_ms {
                    entry.1 = true;
                    notices.push(Notice::Down { name: name.clone() });
                }
            }
        }

        // 清掉已经不在列表里的服务（被删除或被用户暂停的）
        let alive: HashSet<&String> = services.iter().map(|(id, _, _)| id).collect();
        self.state.retain(|id, _| alive.contains(id));

        notices
    }
}
