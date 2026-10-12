//! 崩溃自恢复：向 Windows 登记「崩溃后帮我重新打开」，并防止崩溃循环。
//!
//! 用的是 `RegisterApplicationRestart`（不需要管理员，也不建计划任务）。
//! 登记的命令行固定为 `--hidden --recovered`：重启后静默进托盘，并且知道自己是被
//! 「捞回来」的。窗口是否弹出来由启动矩阵决定（见 mainwindow.rs）。
//!
//! 防崩溃循环：每次带 `--recovered` 启动都在 `<数据目录>/recovery.json` 记一笔；
//! 10 分钟窗口内第 3 次（含）以后暂停自动恢复（注销登记），由用户决定要不要继续。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 被 WER 捞回来时带上的启动参数（与 `--hidden` 一起写进注册命令行）。
pub const RECOVER_FLAG: &str = "--recovered";
/// 10 分钟窗口内允许的自动恢复次数；第 3 次就暂停。
pub const MAX_RECOVERIES_PER_WINDOW: u32 = 3;
/// 计数窗口长度（10 分钟）。
pub const RECOVERY_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// 崩溃恢复的当前状态（给设置页/控制通道看）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryState {
    /// 本窗口内已经自动恢复过的次数。
    pub restarts_in_window: u32,
    /// true = 连续崩溃太多，已暂停自动恢复。
    pub paused: bool,
}

/// recovery.json 的磁盘形状。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryFile {
    window_start_ms: u64,
    count: u32,
    #[serde(default)]
    paused: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("recovery.json")
}

fn read_file(data_dir: &Path) -> Option<RecoveryFile> {
    let text = std::fs::read_to_string(state_path(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 原子写计数文件；写不进去就只记日志，不打断启动。
fn write_file(data_dir: &Path, file: &RecoveryFile) {
    let path = state_path(data_dir);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(text) = serde_json::to_string(file) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text.as_bytes()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// 开关崩溃自动恢复：登记 / 注销 `RegisterApplicationRestart`。
///
/// 登记的命令行固定是 `--hidden --recovered`（D1 §2.3）。返回 `Err` 时给的是
/// 人话原因，调用方把它显示成顶栏警告即可，不要阻断启动。
pub fn install(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Recovery::{
            RESTART_NO_PATCH, RESTART_NO_REBOOT, RegisterApplicationRestart,
            UnregisterApplicationRestart,
        };
        if enabled {
            let cmdline: Vec<u16> = "--hidden --recovered\0".encode_utf16().collect();
            let hr = unsafe {
                RegisterApplicationRestart(cmdline.as_ptr(), RESTART_NO_REBOOT | RESTART_NO_PATCH)
            };
            if hr < 0 {
                let msg = format!("系统不允许登记崩溃恢复（错误码 {hr:#x}）");
                shangxian_core::log::log_warn(shangxian_core::log::TAG_CRASH, &msg);
                return Err(msg);
            }
            shangxian_core::log::log_info(
                shangxian_core::log::TAG_CRASH,
                "已登记崩溃自动恢复（重启参数 --hidden --recovered）",
            );
        } else {
            unsafe { UnregisterApplicationRestart() };
            shangxian_core::log::log_info(shangxian_core::log::TAG_CRASH, "已注销崩溃自动恢复");
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(())
    }
}

/// 每次 `--recovered` 启动调一次：更新窗口计数并返回状态。
///
/// 窗口过期（距 windowStartMs 超过 10 分钟）就从 1 重新计数；计数达到上限即暂停。
/// 坏文件按「新窗口」处理，绝不因为这一笔记录让程序起不来。
pub fn note_recovered_start(data_dir: &Path) -> RecoveryState {
    let now = now_ms();
    let prev = read_file(data_dir);
    let (window_start, count) = match prev {
        Some(p) if now.saturating_sub(p.window_start_ms) < RECOVERY_WINDOW.as_millis() as u64 => {
            (p.window_start_ms, p.count.saturating_add(1))
        }
        _ => (now, 1),
    };
    let paused = count >= MAX_RECOVERIES_PER_WINDOW;
    write_file(
        data_dir,
        &RecoveryFile {
            window_start_ms: window_start,
            count,
            paused,
        },
    );
    RecoveryState {
        restarts_in_window: count,
        paused,
    }
}

/// 读当前状态（不写文件）：窗口过期就报 0 次、未暂停。
pub fn current_state(data_dir: &Path) -> RecoveryState {
    match read_file(data_dir) {
        Some(p)
            if now_ms().saturating_sub(p.window_start_ms) < RECOVERY_WINDOW.as_millis() as u64 =>
        {
            RecoveryState {
                restarts_in_window: p.count,
                paused: p.paused,
            }
        }
        _ => RecoveryState {
            restarts_in_window: 0,
            paused: false,
        },
    }
}

/// 用户的「程序崩溃后自动恢复」开关存哪：D2 冻结的 config v6 里没有
/// `restartOnCrash` 字段（D1 §2 有、D2 §3.1 没收录），所以应用层用一个小标记文件
/// `<数据目录>/crash-recovery.off` 记「关」；文件不在 = 默认开。
fn off_marker(data_dir: &Path) -> PathBuf {
    data_dir.join("crash-recovery.off")
}

/// 崩溃自动恢复开关当前是开的吗（默认开）。
pub fn crash_recovery_enabled(data_dir: &Path) -> bool {
    !off_marker(data_dir).exists()
}

/// 写入开关标记（`enabled=true` 时删掉标记）。
pub fn set_enabled_marker(data_dir: &Path, enabled: bool) -> Result<(), String> {
    let marker = off_marker(data_dir);
    if enabled {
        if marker.exists() {
            std::fs::remove_file(&marker).map_err(|e| format!("保存开关失败：{e}"))?;
        }
        Ok(())
    } else {
        if let Some(dir) = marker.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("保存开关失败：{e}"))?;
        }
        std::fs::write(&marker, b"off").map_err(|e| format!("保存开关失败：{e}"))
    }
}

/// 用户点「继续自动恢复」：清零计数（下次崩溃还能被捞回来）。
pub fn clear_recovery_state(data_dir: &Path) {
    let _ = std::fs::remove_file(state_path(data_dir));
    shangxian_core::log::log_info(shangxian_core::log::TAG_CRASH, "崩溃恢复计数已清零");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shangxian-recovery-test-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn third_recovery_pauses() {
        let dir = temp_dir("pause");
        assert_eq!(note_recovered_start(&dir).restarts_in_window, 1);
        assert!(!note_recovered_start(&dir).paused);
        assert_eq!(note_recovered_start(&dir).restarts_in_window, 3);
        assert!(note_recovered_start(&dir).paused);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_window_resets() {
        let dir = temp_dir("expired");
        write_file(
            &dir,
            &RecoveryFile {
                window_start_ms: now_ms().saturating_sub(11 * 60 * 1000),
                count: 9,
                paused: true,
            },
        );
        let state = note_recovered_start(&dir);
        assert_eq!(state.restarts_in_window, 1);
        assert!(!state.paused);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_starts_fresh() {
        let dir = temp_dir("corrupt");
        std::fs::write(state_path(&dir), b"{ not json").unwrap();
        assert_eq!(note_recovered_start(&dir).restarts_in_window, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_resets_count() {
        let dir = temp_dir("clear");
        note_recovered_start(&dir);
        note_recovered_start(&dir);
        clear_recovery_state(&dir);
        assert_eq!(current_state(&dir).restarts_in_window, 0);
        assert!(!current_state(&dir).paused);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
