//! 应用运行日志：注入式接收器 + 文件/标准错误/空实现 + 时间戳（D1 §3/§8）。
//!
//! 用法约定：
//! - release 应用启动时装 [`FileReporter`]（落 `<data_dir>/logs/app.log`）；cli 启动时装
//!   [`StderrReporter`]（core 的日志走 stderr，CLI 自己的 stdout 输出一个字节都不动）；
//! - core 内部一律用 [`log_info`] / [`log_warn`] / [`log_error`]，不再用 eprintln；
//! - 时间戳有两套，别混：
//!   - app.log：每条记录带本地时间戳 [`timestamp_now`]（D1 §3.2/§8）；
//!   - 隧道/服务日志里「程序自己写的行」：带 `[<epoch-ms>] ` 前缀 [`epoch_prefix`]
//!     （D0 第 14 条 / D2 §5.4，方便 logs_query 读出行时间）；ssh/服务进程自己的
//!     原始输出一个字节都不动。
//!
//! 线程安全：所有函数都能从任意线程调用；未注册时用 [`NullReporter`] 兜底，不 panic。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

/// app.log 文件名。
pub const APP_LOG_FILE_NAME: &str = "app.log";
/// app.log 单文件上限：2MB。
pub const APP_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// app.log 保留的备份份数：app.log.1、app.log.2。
pub const APP_LOG_BACKUPS: usize = 2;

// ---------- 级别 ----------

/// 日志级别：只有三档（D1 §3.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// 行里的大写级别字（`INFO` / `WARN` / `ERROR`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

// ---------- 接收器 ----------

/// 运行日志接收器。实现必须：不 panic、短时间完成、不反向调用 core 逻辑。
pub trait Reporter: Send + Sync + 'static {
    /// `tag`：见 TAG_*；`text`：一行大白话（可以含换行，实现负责折叠）。
    fn report(&self, level: LogLevel, tag: &str, text: &str);
}

/// 文件接收器：`<log_dir>/app.log`，2MB × 3 份轮转。
///
/// 不缓存句柄（每条 append 一次），所以 panic 时也不会因为锁死而写不进去；
/// 只有「查大小 + 轮转」这一小段加锁，避免多线程同时轮转打架。
pub struct FileReporter {
    log_dir: PathBuf,
    rotate_lock: Mutex<()>,
}

impl FileReporter {
    /// 建好日志目录（已存在就复用）。
    pub fn new(log_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(log_dir)?;
        Ok(Self {
            log_dir: log_dir.to_path_buf(),
            rotate_lock: Mutex::new(()),
        })
    }

    /// app.log 的完整路径。
    pub fn app_log_path(&self) -> PathBuf {
        self.log_dir.join(APP_LOG_FILE_NAME)
    }
}

impl Reporter for FileReporter {
    fn report(&self, level: LogLevel, tag: &str, text: &str) {
        let path = self.app_log_path();
        if crate::logfile::size_of(&path) >= APP_LOG_MAX_BYTES {
            let _guard = self
                .rotate_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // 拿到锁后再看一次：可能别的线程已经轮转过了。
            if crate::logfile::size_of(&path) >= APP_LOG_MAX_BYTES {
                let _ = crate::logfile::rotate_if_needed(&path, APP_LOG_MAX_BYTES, APP_LOG_BACKUPS);
            }
        }
        let line = format!(
            "{}[{}] [{}] {}\n",
            timestamp_now(),
            level.as_str(),
            tag,
            fold_newlines(text)
        );
        // 写失败不能反过来影响业务：直接 append 一次，错了就算了（D1 §3.7）。
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// cli 用：写 stderr，绝不动 stdout。
pub struct StderrReporter;

impl Reporter for StderrReporter {
    fn report(&self, level: LogLevel, tag: &str, text: &str) {
        eprintln!(
            "{}[{}] [{}] {}",
            timestamp_now(),
            level.as_str(),
            tag,
            fold_newlines(text)
        );
    }
}

/// 测试/兜底用：什么都不做。
pub struct NullReporter;

impl Reporter for NullReporter {
    fn report(&self, _level: LogLevel, _tag: &str, _text: &str) {}
}

// ---------- 进程级注册 ----------

static REPORTER: OnceLock<RwLock<Arc<dyn Reporter>>> = OnceLock::new();

fn reporter_slot() -> &'static RwLock<Arc<dyn Reporter>> {
    REPORTER.get_or_init(|| RwLock::new(Arc::new(NullReporter)))
}

/// 进程级注册一次；重复调用覆盖旧的（单测可用）。
pub fn set_reporter(reporter: Arc<dyn Reporter>) {
    let mut guard = reporter_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = reporter;
}

/// 未注册时返回 NullReporter，保证 log_* 永不 panic。
pub fn reporter() -> Arc<dyn Reporter> {
    let guard = reporter_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.clone()
}

/// 记一条 INFO。
pub fn log_info(tag: &str, text: &str) {
    reporter().report(LogLevel::Info, tag, text);
}

/// 记一条 WARN。
pub fn log_warn(tag: &str, text: &str) {
    reporter().report(LogLevel::Warn, tag, text);
}

/// 记一条 ERROR。
pub fn log_error(tag: &str, text: &str) {
    reporter().report(LogLevel::Error, tag, text);
}

// ---------- 标签（硬契约，全项目共用） ----------

pub const TAG_START: &str = "启动";
pub const TAG_CONFIG: &str = "配置";
pub const TAG_GUARD: &str = "看护";
pub const TAG_TUNNEL: &str = "隧道";
pub const TAG_SERVICE: &str = "服务";
pub const TAG_NOTIFY: &str = "通知";
pub const TAG_CTL: &str = "命令行";
pub const TAG_POWER: &str = "电源";
pub const TAG_CRASH: &str = "崩溃";
pub const TAG_WINDOW: &str = "窗口";
pub const TAG_SESSION: &str = "多会话";

// ---------- 时间戳 ----------

/// 拼一个本地时间戳：`2026-01-02 03:04:05.006`（不带末尾空格；D1 §8）。
pub fn format_timestamp(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16, ms: u16) -> String {
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{ms:03}")
}

/// 当前本地时间戳，格式 `YYYY-MM-DD HH:MM:SS.mmm `（末尾一个空格，方便直接拼文本；D1 §8）。
/// GetLocalTime 拿不到有效时间时兜底 `[epoch {ms}ms] `。
pub fn timestamp_now() -> String {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    if st.wYear == 0 {
        return format!("[epoch {}ms] ", epoch_ms_now());
    }
    let mut text = format_timestamp(
        st.wYear,
        st.wMonth,
        st.wDay,
        st.wHour,
        st.wMinute,
        st.wSecond,
        st.wMilliseconds,
    );
    text.push(' ');
    text
}

/// 当前 Unix epoch 毫秒（UTC；给机器读的时间）。
pub fn epoch_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 隧道/服务日志里「程序自己写的行」的 `[<epoch-ms>] ` 前缀（D0 第 14 条 / D2 §5.4）。
///
/// 用法：`writeln!(f, "{}{}", crate::log::epoch_prefix(), text)`。
pub fn epoch_prefix() -> String {
    format!("[{}] ", epoch_ms_now())
}

/// 把文本里的换行折成 `⏎`，保证一条记录一行（D1 §3.2）。
fn fold_newlines(text: &str) -> String {
    text.replace("\r\n", "⏎").replace('\n', "⏎")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn format_timestamp_pads() {
        assert_eq!(
            format_timestamp(2026, 1, 2, 3, 4, 5, 6),
            "2026-01-02 03:04:05.006"
        );
    }

    #[test]
    fn timestamp_now_is_local_shape() {
        let ts = timestamp_now();
        assert!(ts.ends_with(' '), "时间戳末尾要有空格：{ts:?}");
        let body = ts.trim_end();
        assert_eq!(body.len(), 23, "格式长度不对：{body:?}");
        let bytes = body.as_bytes();
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b' ');
        assert_eq!(bytes[13], b':');
        assert_eq!(bytes[16], b':');
        assert_eq!(bytes[19], b'.');
        assert_eq!(
            body.chars().filter(|c| c.is_ascii_digit()).count(),
            17,
            "应该是 17 位数字：{body:?}"
        );
    }

    #[test]
    fn epoch_prefix_is_bracketed_ms() {
        let prefix = epoch_prefix();
        assert!(prefix.starts_with('['), "{prefix}");
        assert!(prefix.ends_with("] "), "{prefix}");
        let value: i64 = prefix[1..prefix.len() - 2]
            .parse()
            .expect("前缀中间应该是整数");
        assert!(value > 1_500_000_000_000, "epoch 毫秒看着不对：{value}");
    }

    #[test]
    fn newline_is_folded() {
        assert_eq!(fold_newlines("第一行\n第二行\r\n第三行"), "第一行⏎第二行⏎第三行");
    }

    #[test]
    fn file_reporter_writes_one_line_then_rotates() {
        let dir = std::env::temp_dir().join(format!("sx-log-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let reporter = FileReporter::new(&dir).expect("建 FileReporter");
        assert_eq!(reporter.app_log_path(), dir.join(APP_LOG_FILE_NAME));

        reporter.report(LogLevel::Info, TAG_START, "第一行\n第二行\r\n第三行");
        let text = std::fs::read_to_string(reporter.app_log_path()).unwrap();
        assert_eq!(text.lines().count(), 1, "换行要被折叠：{text:?}");
        assert!(text.contains('⏎'), "换行要折成 ⏎：{text:?}");
        assert!(text.contains("[INFO] [启动]"), "行格式不对：{text:?}");
        assert!(text.starts_with("20"), "行首要带本地时间戳：{text:?}");

        // 预置超过上限的内容，再写一条 → 轮转出 app.log.1，新文件只留新记录。
        let big = vec![b'x'; (APP_LOG_MAX_BYTES + 1) as usize];
        std::fs::write(reporter.app_log_path(), &big).unwrap();
        reporter.report(LogLevel::Warn, TAG_GUARD, "轮转测试");
        assert!(
            dir.join(format!("{APP_LOG_FILE_NAME}.1")).exists(),
            "应该出现 .1 备份"
        );
        let text = std::fs::read_to_string(reporter.app_log_path()).unwrap();
        assert!(text.contains("轮转测试"), "新记录要写进新文件：{text:?}");
        assert!(text.len() < 1024, "新文件不该带旧内容：{}", text.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn null_reporter_never_panics() {
        set_reporter(Arc::new(NullReporter));
        log_info(TAG_START, "随便写");
        log_warn(TAG_GUARD, "带\n换行的警告");
        log_error(TAG_CRASH, "崩溃");
    }

    #[test]
    fn set_reporter_replaces_global() {
        struct Counter(AtomicUsize);
        impl Reporter for Counter {
            fn report(&self, _level: LogLevel, _tag: &str, _text: &str) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        set_reporter(counter.clone());
        log_info(TAG_CTL, "计数");
        assert!(counter.0.load(Ordering::SeqCst) >= 1, "注册的接收器要被调用");
        // 收尾：别把计数接收器留给别的测试。
        set_reporter(Arc::new(NullReporter));
    }
}
