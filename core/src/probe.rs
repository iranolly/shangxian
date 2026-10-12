//! 本机端口探测：防抖 + 出锁（D1 §5）。
//!
//! 为什么要有这个模块（照 D1 §5.1 的结论）：
//! - 探测原来在看护 tick 里同步做，一次最长 400ms，还占着全局锁，界面跟着一起卡。
//!   现在每个服务一条轻量探测线程，结果写进 [`ProbeSlot`] 原子量；看护线程只读。
//! - 单次探测超时 TCP 100ms、HTTP 500ms；**连续 3 次失败**才判 down（约 6 秒），
//!   任何一次成功立即清零。防抖吸收偶发抖动，别误杀。
//! - HTTP 模式：写一次请求，**收到任何字节就算活着**（401/403/404/500 都算），
//!   只有连接被拒、超时、或请求发出去读不到字节才算死。先 HEAD，服务器不认 HEAD
//!   （或干脆不回）时退回 GET。
//! - [`ProbeHandle`] 的 Drop 只置停止位、不 join：删服务/换端口时不能占着全局锁等探测超时。

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 探测线程的间隔（2 秒一次，见 D1 §5.1）。
pub const PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// TCP 探测一次的最长等待。
pub const TCP_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
/// HTTP 探测一次的最长等待（要等第一个响应字节，放宽到 500ms）。
pub const HTTP_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// 连续失败这么多次才判 down。
pub const PROBE_FAILS_BEFORE_DOWN: u32 = 3;
/// 探测线程停止位的轮询步长：Drop 后最多这么久线程就退出。
const STOP_POLL_STEP: Duration = Duration::from_millis(100);
/// 一次 HTTP 探测最多读多少字节（状态行一般几十字节，读一点就够）。
const HTTP_READ_MAX: usize = 512;

/// 探测方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeKind {
    /// 只确认 TCP 能连上。
    Tcp,
    /// 发一个 HTTP 请求，收到响应就算活着。
    Http,
}

impl Default for ProbeKind {
    fn default() -> Self {
        ProbeKind::Tcp
    }
}

/// 一次探测的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// TCP 连上了（不关心服务说什么）。
    TcpOpen,
    /// HTTP 有响应（任何状态码都算活，含 401/403/404/500）。
    /// `code` 为 0 表示收到了字节但状态行认不出来（例如服务说的不是 HTTP）。
    HttpAlive { code: u16 },
    /// 连接被拒 / 超时 / 连上但没等到任何字节。
    Down { error: String },
}

impl ProbeOutcome {
    /// 这次探测算不算「活着」。
    pub fn is_alive(&self) -> bool {
        !matches!(self, ProbeOutcome::Down { .. })
    }

    /// 失败时的人话原因（成功返回 None）。
    pub fn error(&self) -> Option<&str> {
        match self {
            ProbeOutcome::Down { error } => Some(error.as_str()),
            _ => None,
        }
    }
}

/// 按探测方式取超时。
pub fn timeout_for(kind: ProbeKind) -> Duration {
    match kind {
        ProbeKind::Tcp => TCP_PROBE_TIMEOUT,
        ProbeKind::Http => HTTP_PROBE_TIMEOUT,
    }
}

/// 一次同步探测（最多阻塞 `timeout`）。
pub fn probe_once(port: u16, kind: ProbeKind, timeout: Duration) -> ProbeOutcome {
    match kind {
        ProbeKind::Tcp => tcp_probe(port, timeout),
        ProbeKind::Http => http_probe(port, timeout),
    }
}

fn local_addr(port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
}

fn tcp_probe(port: u16, timeout: Duration) -> ProbeOutcome {
    match TcpStream::connect_timeout(&local_addr(port).into(), timeout) {
        Ok(_) => ProbeOutcome::TcpOpen,
        Err(err) => ProbeOutcome::Down {
            error: format!("连不上本机端口 {port}：{err}"),
        },
    }
}

fn http_probe(port: u16, timeout: Duration) -> ProbeOutcome {
    // HEAD 先试：正常网站/接口都会回状态行，不用拉 body，最省。
    let head = http_attempt(port, timeout, "HEAD");
    // 服务器不认 HEAD（直接掐断、或压根不回）时退回 GET：只要回一个字节就算活着。
    let (got, error) = match head {
        Ok(code) => (Some(code), None),
        Err(err) => match http_attempt(port, timeout, "GET") {
            Ok(code) => (Some(code), None),
            Err(err2) => (None, Some(format!("{err}；改用 GET 再试：{err2}"))),
        },
    };
    match got {
        Some(code) => ProbeOutcome::HttpAlive { code },
        None => ProbeOutcome::Down {
            error: error.unwrap_or_else(|| format!("本机端口 {port} 没有 HTTP 响应")),
        },
    }
}

/// 发一次请求并读响应；读到字节返回 Ok(状态码，认不出状态行时是 0)，
/// 连不上 / 超时 / 一个字节都没读到返回 Err(人话)。
fn http_attempt(port: u16, timeout: Duration, method: &str) -> Result<u16, String> {
    let mut stream = TcpStream::connect_timeout(&local_addr(port).into(), timeout)
        .map_err(|err| format!("连不上本机端口 {port}：{err}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| format!("设置读取超时失败:{err}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| format!("设置写入超时失败:{err}"))?;

    let request = format!(
        "{method} / HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: shangxian-probe\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("请求写不出去：{err}"))?;
    // 关上写方向，服务端才知道请求完了，会尽快回包。
    let _ = stream.shutdown(Shutdown::Write);

    let mut buf = [0u8; HTTP_READ_MAX];
    let read = stream
        .read(&mut buf)
        .map_err(|err| format!("等响应超时或读失败:{err}"))?;
    if read == 0 {
        return Err("连上了但一个字节都没等到".into());
    }
    Ok(parse_status_code(&buf[..read]).unwrap_or(0))
}

/// 从响应头里找 `HTTP/1.x 200 ...` 的 200。找不到返回 None（不影响「收到字节 = 活着」）。
fn parse_status_code(head: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(head);
    let first_line = text.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse::<u16>().ok()
}

/// 一个服务的探测结果槽（Send + Sync，探测线程写、看护线程读）。
pub struct ProbeSlot {
    alive: AtomicBool,
    checked: AtomicBool,
    fail_streak: AtomicU32,
    last_ok_at: Mutex<Option<Instant>>,
    last_error: Mutex<Option<String>>,
}

impl Default for ProbeSlot {
    fn default() -> Self {
        Self::new_inner()
    }
}

impl ProbeSlot {
    /// 建一个共享槽（返回 Arc，探测线程和 runner 各持一份）。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::new_inner())
    }

    fn new_inner() -> Self {
        Self {
            alive: AtomicBool::new(false),
            checked: AtomicBool::new(false),
            fail_streak: AtomicU32::new(0),
            last_ok_at: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    /// 最近一次探测是否活着（防抖后的值：连续失败达到阈值才转 false）。
    /// 从没探过是 false。
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// 至少探过一次（用来区分「没探过」和「探过不通」）。
    pub fn ever_checked(&self) -> bool {
        self.checked.load(Ordering::SeqCst)
    }

    /// 连续失败次数（任何一次成功立即清零）。
    pub fn fail_streak(&self) -> u32 {
        self.fail_streak.load(Ordering::SeqCst)
    }

    /// 确认判 down：至少测过一次，且连续失败 ≥ [`PROBE_FAILS_BEFORE_DOWN`]。
    pub fn is_down(&self) -> bool {
        self.ever_checked() && self.fail_streak() >= PROBE_FAILS_BEFORE_DOWN
    }

    /// 最近一次探测成功的时刻。
    pub fn last_ok_at(&self) -> Option<Instant> {
        *self.last_ok_at.lock().unwrap()
    }

    /// 最近一次失败的人话原因（成功清空）。
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    /// 喂一次结果（探测线程 / [`ProbeSlot::check_now`] 用）。
    /// `alive` 是防抖后的值：失败 1–2 次先不动，第 [`PROBE_FAILS_BEFORE_DOWN`] 次才转 false；
    /// 任何一次成功立即转 true 并清零。
    pub fn feed(&self, outcome: ProbeOutcome) {
        if outcome.is_alive() {
            self.alive.store(true, Ordering::SeqCst);
            self.checked.store(true, Ordering::SeqCst);
            self.fail_streak.store(0, Ordering::SeqCst);
            *self.last_ok_at.lock().unwrap() = Some(Instant::now());
            *self.last_error.lock().unwrap() = None;
        } else {
            self.checked.store(true, Ordering::SeqCst);
            let streak = self.fail_streak.fetch_add(1, Ordering::SeqCst) + 1;
            if streak >= PROBE_FAILS_BEFORE_DOWN {
                self.alive.store(false, Ordering::SeqCst);
            }
            *self.last_error.lock().unwrap() = outcome.error().map(str::to_string);
        }
    }

    /// 同步探一次并写回；返回是否活。
    /// 用于启动前/认领时「端口是不是已经在听」——只在启动、用户操作时发生（最多 100ms）。
    pub fn check_now(&self, port: u16, kind: ProbeKind) -> bool {
        let outcome = probe_once(port, kind, timeout_for(kind));
        let alive = outcome.is_alive();
        self.feed(outcome);
        alive
    }
}

/// 探测线程句柄：Drop 只置停止位、不 join（避免删服务时占着全局锁等探测超时）。
pub struct ProbeHandle {
    stop: Arc<AtomicBool>,
}

impl ProbeHandle {
    /// 主动让探测线程停下（Drop 也会做，重复调用无害）。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for ProbeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 起一条探测线程：每 `interval` 探一次，结果写进 `slot`。
/// 线程只做 connect/read，不持任何锁；`stop` 置位后最多 100ms 退出。
pub fn spawn_probe(
    port: u16,
    kind: ProbeKind,
    interval: Duration,
    slot: Arc<ProbeSlot>,
) -> ProbeHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = ProbeHandle {
        stop: Arc::clone(&stop),
    };
    std::thread::spawn(move || {
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            slot.feed(probe_once(port, kind, timeout_for(kind)));
            let deadline = Instant::now() + interval;
            while Instant::now() < deadline {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(STOP_POLL_STEP);
            }
        }
    });
    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// 起一个只回一次固定响应的假 HTTP 服务。
    fn spawn_http_server(response: &'static [u8]) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response);
            }
        });
        port
    }

    #[test]
    fn tcp_open_on_listener() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let outcome = probe_once(port, ProbeKind::Tcp, TCP_PROBE_TIMEOUT);
        assert_eq!(outcome, ProbeOutcome::TcpOpen);
        assert!(outcome.is_alive());
    }

    #[test]
    fn refused_is_down() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let outcome = probe_once(port, ProbeKind::Tcp, TCP_PROBE_TIMEOUT);
        assert!(matches!(outcome, ProbeOutcome::Down { .. }), "{outcome:?}");
    }

    #[test]
    fn http_401_is_alive() {
        let port = spawn_http_server(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let outcome = probe_once(port, ProbeKind::Http, HTTP_PROBE_TIMEOUT);
        assert_eq!(outcome, ProbeOutcome::HttpAlive { code: 401 });
        assert!(outcome.is_alive(), "401 也算活着");
    }

    #[test]
    fn http_404_and_500_are_alive() {
        for (response, want) in [
            (
                &b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"[..],
                404u16,
            ),
            (
                &b"HTTP/1.0 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n"[..],
                500u16,
            ),
        ] {
            let port = spawn_http_server(response);
            assert_eq!(
                probe_once(port, ProbeKind::Http, HTTP_PROBE_TIMEOUT),
                ProbeOutcome::HttpAlive { code: want }
            );
        }
    }

    #[test]
    fn http_head_falls_back_to_get() {
        // 第一次（HEAD）直接掐断不回；第二次（GET）才回 200。
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // HEAD：读一下请求就关，一个字节都不回
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
            }
            // GET：正常回一个状态行
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let outcome = probe_once(port, ProbeKind::Http, HTTP_PROBE_TIMEOUT);
        assert_eq!(outcome, ProbeOutcome::HttpAlive { code: 200 });
    }

    #[test]
    fn three_fails_before_down() {
        let slot = ProbeSlot::new();
        assert!(!slot.ever_checked(), "没探过不算有结果");
        assert!(!slot.is_down(), "没探过绝不能判 down");

        slot.feed(ProbeOutcome::TcpOpen);
        assert!(slot.alive());

        slot.feed(ProbeOutcome::Down {
            error: "第一次".into(),
        });
        assert!(slot.ever_checked());
        assert!(!slot.is_down(), "1 次失败还不到阈值");
        assert!(slot.alive(), "防抖：1 次失败不让 alive 转 false");
        slot.feed(ProbeOutcome::Down {
            error: "第二次".into(),
        });
        assert!(!slot.is_down(), "2 次失败还不到阈值");
        assert!(slot.alive(), "防抖：2 次失败也不让 alive 转 false");
        assert_eq!(slot.fail_streak(), 2);
        assert_eq!(slot.last_error().as_deref(), Some("第二次"));

        slot.feed(ProbeOutcome::Down {
            error: "第三次".into(),
        });
        assert!(slot.is_down(), "连续 3 次失败才判 down");
        assert!(!slot.alive(), "到阈值才转 false");
        assert_eq!(slot.fail_streak(), 3);

        slot.feed(ProbeOutcome::TcpOpen);
        assert!(!slot.is_down(), "成功一次立即恢复");
        assert_eq!(slot.fail_streak(), 0);
        assert!(slot.alive());
        assert!(slot.last_ok_at().is_some());
        assert_eq!(slot.last_error(), None);
    }

    #[test]
    fn check_now_writes_result() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let slot = ProbeSlot::new();
        assert!(slot.check_now(port, ProbeKind::Tcp));
        assert!(slot.alive());
        assert!(slot.ever_checked());
        assert_eq!(slot.fail_streak(), 0);

        drop(listener);
        assert!(!slot.check_now(port, ProbeKind::Tcp), "同步探测返回原始结果");
        assert_eq!(slot.fail_streak(), 1);
        assert!(!slot.is_down(), "单次失败还不算 down");
        assert!(slot.alive(), "防抖：单次失败不让 alive 转 false");
    }

    #[test]
    fn spawned_probe_feeds_slot() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let slot = ProbeSlot::new();
        let handle = spawn_probe(
            port,
            ProbeKind::Tcp,
            Duration::from_millis(50),
            Arc::clone(&slot),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while !slot.alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(slot.alive(), "探测线程应该把结果写进 slot");
        drop(handle); // 只置停止位，不 join
        drop(listener);
    }

    #[test]
    fn stopped_probe_keeps_last_result_and_exits() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let slot = ProbeSlot::new();
        let handle = spawn_probe(
            port,
            ProbeKind::Tcp,
            Duration::from_millis(20),
            Arc::clone(&slot),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while !slot.alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(slot.alive());
        handle.stop();
        drop(handle);
        drop(listener);
        // 停止后不再更新：结果保持停止前那一次
        std::thread::sleep(Duration::from_millis(120));
        assert!(slot.alive(), "停止只是不再探，不抹掉最后一次结果");
    }
}
