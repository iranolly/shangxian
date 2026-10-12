//! 应用运行日志：装文件接收器（`<数据目录>/logs/app.log`）+ 崩溃 panic hook。
//!
//! 启动最早就调一次（顺序见 lib.rs::run）。日志要是建不出来，退回写 stderr，
//! 并把一句人话错误交给调用方（顶栏提示用），绝不让程序因为日志起不来。

use std::sync::Arc;

use shangxian_core::log;

/// 建目录、装 `FileReporter`、装 panic hook。
///
/// 返回 `Err(人话)` 表示文件日志不可用（已经退回 stderr），调用方只提示、不阻断。
pub fn init(data_dir: &std::path::Path) -> Result<(), String> {
    let log_dir = data_dir.join("logs");
    match log::FileReporter::new(&log_dir) {
        Ok(reporter) => {
            log::set_reporter(Arc::new(reporter));
            install_panic_hook();
            Ok(())
        }
        Err(e) => {
            log::set_reporter(Arc::new(log::StderrReporter));
            install_panic_hook();
            Err(format!(
                "日志文件建不出来（{e}）：{}。程序先用控制台日志跑，其它功能不受影响。",
                log_dir.display()
            ))
        }
    }
}

/// panic 时在 abort 之前写下最后一笔（`panic = "abort"` 下 hook 仍会执行）。
fn install_panic_hook() {
    std::panic::set_hook(Box::new(move |info| {
        let text = format!(
            "程序内部出错：{info}\n位置：{}\n{}",
            info.location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_default(),
            std::backtrace::Backtrace::force_capture()
        );
        log::log_error(log::TAG_CRASH, &text);
    }));
}
