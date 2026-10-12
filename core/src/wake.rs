//! 睡眠唤醒 / 网络恢复 / 时钟跳变的检测（D1 §1）。
//!
//! 三层检测：
//! 1. **tick 漂移检测（主机制）**：看护每轮比较墙钟（`SystemTime`）和单调钟
//!    （`Instant`）的增量差。墙钟比单调钟多走 ≥5 秒 → 电脑睡过一觉（或系统时间被
//!    大改）。纯 `std`，时钟可注入，方便单测。
//! 2. **Windows 电源回调**（app 的 `power.rs`）：回调线程只调 [`signal_resume`] 置位。
//! 3. **网卡变化回调**：只调 [`signal_network_change`] 置位。
//!
//! 回调线程只碰原子量，不抢任何锁；看护线程每 tick 用 [`take_signals`] 取走。
//! 同一轮只返回一个最优先的事件：`Resumed > NetworkChanged > ClockJump`。

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// 墙钟比单调钟多走这么多就认为"睡过一觉"。
pub const WAKE_DRIFT_THRESHOLD: Duration = Duration::from_secs(5);

/// 一次系统级事件。`slept_ms` / `delta_ms` 只用于文案/日志，不参与判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeKind {
    /// 电脑从睡眠里醒过来（墙钟漂移达到阈值，或电源回调置位）。
    Resumed { slept_ms: u64 },
    /// 网卡 / 地址变化（换 Wi-Fi、插网线、VPN 起落）。
    NetworkChanged,
    /// 系统时间被改动（墙钟往回走超过阈值）。
    ClockJump { delta_ms: i64 },
}

/// 电源回调置的位。
const BIT_RESUMED: u8 = 0b01;
/// 网卡变化回调置的位。
const BIT_NETWORK: u8 = 0b10;

/// 全局信号位：Win32 回调线程置位，看护线程取走。
static SIGNALS: AtomicU8 = AtomicU8::new(0);

/// 看护线程每 tick 取走系统回调置的位。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WakeSignals {
    pub resumed: bool,
    pub network_changed: bool,
}

/// Win32 电源回调线程调用：置位，不阻塞、不加锁（内部只有一个原子量）。
pub fn signal_resume() {
    SIGNALS.fetch_or(BIT_RESUMED, Ordering::SeqCst);
}

/// Win32 网卡变化回调线程调用：置位，不阻塞、不加锁。
pub fn signal_network_change() {
    SIGNALS.fetch_or(BIT_NETWORK, Ordering::SeqCst);
}

/// 取走系统回调置的位（同一位只取一次）。
pub fn take_signals() -> WakeSignals {
    let bits = SIGNALS.swap(0, Ordering::SeqCst);
    WakeSignals {
        resumed: bits & BIT_RESUMED != 0,
        network_changed: bits & BIT_NETWORK != 0,
    }
}

/// 只看两个时钟的差值，不碰系统 API；时钟可注入，方便单测。
pub struct WakeMonitor {
    last_wall: SystemTime,
    last_mono: Instant,
}

impl Default for WakeMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl WakeMonitor {
    pub fn new() -> Self {
        Self {
            last_wall: SystemTime::now(),
            last_mono: Instant::now(),
        }
    }

    /// 注入时钟（测试用）：`wall`/`mono` 是本轮观察值。
    ///
    /// - 墙钟多走（≥阈值）而单调钟没走 → [`WakeKind::Resumed`]；
    /// - 墙钟往回走（≥阈值）→ [`WakeKind::ClockJump`]；
    /// - 正常 tick 返回 `None`。
    pub fn observe(&mut self, wall: SystemTime, mono: Instant) -> Option<WakeKind> {
        let prev_wall = self.last_wall;
        let prev_mono = self.last_mono;
        self.last_wall = wall;
        self.last_mono = mono;

        // 带符号的墙钟增量（可能为负）。
        let delta_ms: i128 = match wall.duration_since(prev_wall) {
            Ok(d) => d.as_millis() as i128,
            Err(e) => -(e.duration().as_millis() as i128),
        };
        let mono_ms = mono.duration_since(prev_mono).as_millis() as i128;
        let drift = delta_ms - mono_ms;
        let threshold = WAKE_DRIFT_THRESHOLD.as_millis() as i128;

        if drift >= threshold {
            return Some(WakeKind::Resumed {
                slept_ms: drift.min(u64::MAX as i128) as u64,
            });
        }
        if drift <= -threshold {
            return Some(WakeKind::ClockJump {
                delta_ms: delta_ms.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
            });
        }
        None
    }

    /// 看护每 tick 调一次（内部用真实时钟调 [`Self::observe`]，并合并
    /// [`take_signals`] 里的 Win32 回调信号；同一轮只返回一个最优先的事件：
    /// `Resumed > NetworkChanged > ClockJump`）。
    pub fn poll(&mut self) -> Option<WakeKind> {
        let drift = self.observe(SystemTime::now(), Instant::now());
        let signals = take_signals();

        if signals.resumed {
            // 电源回调没有睡眠时长：漂移检测同时认出来就用它的，认不出记 0。
            if let Some(WakeKind::Resumed { slept_ms }) = drift {
                return Some(WakeKind::Resumed { slept_ms });
            }
            return Some(WakeKind::Resumed { slept_ms: 0 });
        }
        if signals.network_changed {
            return Some(WakeKind::NetworkChanged);
        }
        drift
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_tick_no_event() {
        let mut monitor = WakeMonitor::new();
        let wall = monitor.last_wall;
        let mono = monitor.last_mono;
        assert_eq!(
            monitor.observe(wall + Duration::from_millis(150), mono + Duration::from_millis(150)),
            None
        );
    }

    #[test]
    fn drift_detects_resume() {
        let mut monitor = WakeMonitor::new();
        let wall = monitor.last_wall;
        let mono = monitor.last_mono;
        let kind = monitor.observe(wall + Duration::from_secs(600), mono);
        assert_eq!(
            kind,
            Some(WakeKind::Resumed { slept_ms: 600_000 }),
            "wall +10min / mono +0ms 应该认成唤醒"
        );
    }

    #[test]
    fn backward_clock_jump() {
        let mut monitor = WakeMonitor::new();
        let wall = monitor.last_wall;
        let mono = monitor.last_mono;
        let kind = monitor.observe(
            wall - Duration::from_secs(3600),
            mono + Duration::from_millis(150),
        );
        match kind {
            Some(WakeKind::ClockJump { delta_ms }) => assert!(delta_ms < 0, "delta_ms={delta_ms}"),
            other => panic!("期望 ClockJump，实际 {other:?}"),
        }
    }

    #[test]
    fn small_clock_adjustment_is_ignored() {
        let mut monitor = WakeMonitor::new();
        let wall = monitor.last_wall;
        let mono = monitor.last_mono;
        // NTP 小调整（1 秒）不该触发任何事件
        assert_eq!(
            monitor.observe(wall + Duration::from_secs(1), mono + Duration::from_millis(150)),
            None
        );
    }

    #[test]
    fn poll_prefers_resume_signal_over_network() {
        let _ = take_signals(); // 清掉别的测试可能留下的位
        let mut monitor = WakeMonitor::new();
        signal_network_change();
        // 模拟电源回调：漂移没到阈值时也要报 Resumed
        let wall = monitor.last_wall;
        let mono = monitor.last_mono;
        monitor.last_wall = wall;
        monitor.last_mono = mono;
        signal_resume();
        assert_eq!(
            monitor.poll(),
            Some(WakeKind::Resumed { slept_ms: 0 }),
            "Resumed 比 NetworkChanged 优先"
        );
        // 位取走后同一类不再重复
        assert_eq!(monitor.poll(), None);
    }

    #[test]
    fn signals_are_taken_once() {
        let _ = take_signals();
        signal_network_change();
        let first = take_signals();
        assert!(first.network_changed && !first.resumed);
        let second = take_signals();
        assert!(!second.network_changed && !second.resumed);
    }

    #[test]
    fn poll_returns_network_change_when_no_resume() {
        let _ = take_signals();
        let mut monitor = WakeMonitor::new();
        signal_network_change();
        assert_eq!(monitor.poll(), Some(WakeKind::NetworkChanged));
    }
}
