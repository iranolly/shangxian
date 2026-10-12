//! 子进程执行统一入口：带超时、并发读管道、输出上限（D1 §11）。
//!
//! 背景：旧实现是先等进程退出、之后再读 stdout 管道。远端命令输出超过管道缓冲
//! （约 64KB）时，子进程会卡在写 stdout，主进程一直等到超时，形成假超时、输出丢失。
//! 这里 spawn 之后立刻用两条线程并发读 stdout/stderr，主线程只按 deadline 等退出，
//! 所以输出再多也不会把子进程堵死。
//!
//! 调用方负责设 `stdin(Stdio::null())`、`creation_flags(CREATE_NO_WINDOW)`；
//! 本模块强制把 stdout/stderr 设成管道。

use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// 默认输出上限：256KB。只防意外刷屏，正常命令输出远小于它。
pub const DEFAULT_OUTPUT_LIMIT: usize = 256 * 1024;

/// `run_capture` 启动失败时的错误前缀。调用方可用 [`spawn_error_with_context`]
/// 把它换成本场景的说法（如「无法启动 ssh：…」）。
pub const SPAWN_ERROR_PREFIX: &str = "无法启动命令：";

/// 输出被截断时在文本尾部补的一行说明。
const TRUNCATED_NOTE: &str = "（输出太多，后面被截断了）";

/// 主循环轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// 收读线程结果的有界等待。
const RECV_TIMEOUT: Duration = Duration::from_millis(500);

/// 一个跑完的子进程的全部输出。
#[derive(Debug)]
pub struct CapturedOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    /// 超过 limit_bytes，输出被截断（会在文本尾加一行说明）
    pub output_truncated: bool,
}

/// 跑一个子进程：
/// - stdout/stderr 设为管道并在两条线程里并发读（不会因管道写满卡死）；
/// - 到 deadline 还没退出：kill + wait，然后有界等待读线程（500ms），返回 Err；
/// - 成功返回时保证两条读线程已结束；
/// - 输出上限 limit_bytes：超出部分丢弃并置 output_truncated。
pub fn run_capture(
    cmd: &mut Command,
    timeout: Duration,
    limit_bytes: usize,
) -> Result<CapturedOutput, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{SPAWN_ERROR_PREFIX}{e}"))?;

    // 尽量把子进程放进 kill-on-close 作业：超时时整树结束，孙进程不会攥着管道不放。
    // 放不进也不影响主流程：超时退回只杀直接子进程（属于少数受限环境）。
    let job = match crate::job::Job::new() {
        Ok(job) => {
            if job.assign_process(child.as_raw_handle()).is_ok() {
                Some(job)
            } else {
                None
            }
        }
        Err(_) => None,
    };

    let (out_tx, out_rx) = mpsc::channel::<StreamResult>();
    let (err_tx, err_rx) = mpsc::channel::<StreamResult>();

    let out_thread = match child.stdout.take() {
        Some(pipe) => match spawn_reader(pipe, limit_bytes, out_tx) {
            Ok(handle) => Some(handle),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        },
        None => None,
    };
    let err_thread = match child.stderr.take() {
        Some(pipe) => match spawn_reader(pipe, limit_bytes, err_tx) {
            Ok(handle) => Some(handle),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        },
        None => None,
    };

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    if let Some(job) = &job {
                        job.terminate();
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    // 有界等待读线程收尾（管道此时应该已经关闭）；等不到就放弃线程。
                    let _ = out_rx.recv_timeout(RECV_TIMEOUT);
                    let _ = err_rx.recv_timeout(RECV_TIMEOUT);
                    return Err(format!(
                        "命令超时（{} 秒），已经把它结束。",
                        secs_text(timeout)
                    ));
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                if let Some(job) = &job {
                    job.terminate();
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("等待子进程失败：{e}"));
            }
        }
    };

    // 进程已经退出；收两条读线程的结果。有界等待：万一孙进程占着管道不放手，
    // 等不到就丢弃线程，绝不把调用方卡在这里。
    let (stdout, out_truncated) = collect(out_rx, out_thread);
    let (stderr, err_truncated) = collect(err_rx, err_thread);

    // 进程已经退出，作业可以释放（kill-on-close 对已结束的进程是空操作）。
    drop(job);

    Ok(CapturedOutput {
        status,
        stdout,
        stderr,
        output_truncated: out_truncated || err_truncated,
    })
}

/// 把 `run_capture` 的启动失败错误换成本场景的说法；超时等其他错误原样保留。
pub fn spawn_error_with_context(err: String, context: &str) -> String {
    match err.strip_prefix(SPAWN_ERROR_PREFIX) {
        Some(raw) => format!("{context}：{raw}"),
        None => err,
    }
}

/// 一条管道读线程回传的结果。
struct StreamResult {
    text: String,
    truncated: bool,
}

/// 起一条读线程，读到 EOF 后把结果发回来。
fn spawn_reader<R: Read + Send + 'static>(
    mut pipe: R,
    limit_bytes: usize,
    tx: mpsc::Sender<StreamResult>,
) -> Result<thread::JoinHandle<()>, String> {
    thread::Builder::new()
        .name("proc-reader".into())
        .spawn(move || {
            let result = read_stream(&mut pipe, limit_bytes);
            let _ = tx.send(result);
        })
        .map_err(|e| format!("内部错误：起不了子进程输出读取线程（{e}）"))
}

/// 收一条读线程的结果；等不到（极端情况）就当空输出，不 join 卡死。
fn collect(
    rx: mpsc::Receiver<StreamResult>,
    handle: Option<thread::JoinHandle<()>>,
) -> (String, bool) {
    match rx.recv_timeout(RECV_TIMEOUT) {
        Ok(result) => {
            if let Some(handle) = handle {
                let _ = handle.join();
            }
            (result.text, result.truncated)
        }
        Err(_) => (String::new(), false),
    }
}

/// 把一条管道读到 EOF；超过上限的部分丢掉，并继续读（保持管道排空，别堵住子进程）。
fn read_stream<R: Read>(pipe: &mut R, limit_bytes: usize) -> StreamResult {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let room = limit_bytes.saturating_sub(bytes.len());
                if room == 0 {
                    truncated = true;
                    continue;
                }
                if n <= room {
                    bytes.extend_from_slice(&buf[..n]);
                } else {
                    bytes.extend_from_slice(&buf[..room]);
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push('\n');
        text.push_str(TRUNCATED_NOTE);
    }
    StreamResult { text, truncated }
}

/// 超时文案里用的秒数：整秒不带小数点，零头保留一位。
fn secs_text(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs.fract() == 0.0 {
        format!("{}", duration.as_secs())
    } else {
        format!("{secs:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn cmd(script: &str) -> Command {
        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", script])
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        command
    }

    #[test]
    fn captures_stdout_and_stderr() {
        let out = run_capture(
            &mut cmd("echo out & echo err 1>&2"),
            Duration::from_secs(10),
            DEFAULT_OUTPUT_LIMIT,
        )
        .expect("跑 echo 不该失败");
        assert!(out.status.success(), "状态：{:?}", out.status);
        assert!(out.stdout.contains("out"), "stdout：{:?}", out.stdout);
        assert!(out.stderr.contains("err"), "stderr：{:?}", out.stderr);
        assert!(!out.output_truncated);
    }

    #[test]
    fn times_out_and_kills() {
        let started = Instant::now();
        let err = run_capture(
            &mut cmd("ping -n 30 127.0.0.1 >NUL"),
            Duration::from_millis(300),
            DEFAULT_OUTPUT_LIMIT,
        )
        .expect_err("长命令应该超时");
        assert!(err.contains("命令超时"), "错误文案：{err}");
        // 上界放宽到 3 秒：并行满载时进程启动/回收可能被拖慢，断言意图仍是「超时有界返回」。
        assert!(
            started.elapsed() < Duration::from_millis(3000),
            "超时后要有界返回：{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn big_output_does_not_deadlock() {
        // 约 128KB，超过管道缓冲（约 64KB）：并发读的进程应正常退出，输出被截断。
        let line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let script = format!("for /L %i in (1,1,2000) do @echo {line}");
        let out = run_capture(&mut cmd(&script), Duration::from_secs(10), 4096)
            .expect("大输出不该卡死");
        assert!(out.status.success(), "状态：{:?}", out.status);
        assert!(out.output_truncated, "超过上限要标记截断");
        assert!(out.stdout.ends_with(TRUNCATED_NOTE), "要带截断说明：{:?}", out.stdout.len());
        assert!(out.stdout.len() < 8192, "截断后文本不该太大：{}", out.stdout.len());
    }

    #[test]
    fn spawn_error_can_be_contextualized() {
        let mut command = Command::new("definitely-missing-shangxian-proc.exe");
        command.stdin(Stdio::null());
        let err = run_capture(&mut command, Duration::from_secs(1), 1024)
            .expect_err("不存在的程序应该起不来");
        assert!(err.starts_with(SPAWN_ERROR_PREFIX), "错误：{err}");
        let mapped = spawn_error_with_context(err, "无法启动 ssh");
        assert!(mapped.starts_with("无法启动 ssh："), "映射后：{mapped}");
    }
}
