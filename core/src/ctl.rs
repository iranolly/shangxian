//! 本机控制通道：让 cli.exe（以及别的程序、AI agent）能跟运行中的「上线助手」说话。
//!
//! 怎么工作：
//! 1. 应用启动时在 127.0.0.1（只有本机能连的地址）上开一个随机端口；
//! 2. 把端口号和一张通行证写进 `<数据目录>/ctl.json`；
//! 3. 别的程序读这个文件、连上去、发一行 JSON，再收一行 JSON。
//!
//! 请求：`{"token":"...","id":1,"method":"status","params":{}}`
//! 成功：`{"id":1,"ok":true,"result":{...}}`
//! 失败：`{"id":1,"ok":false,"error":"人话原因"}`
//!
//! 应用没在运行时 ctl.json 可能还在（上次退出没来得及删）——连不上就是没运行。

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 通行证文件名（放在数据目录里，只有本用户能读）。
pub const CTL_FILE_NAME: &str = "ctl.json";

/// 应用没在运行时给调用方的统一提示。
pub const APP_NOT_RUNNING: &str =
    "『上线助手』未在运行（无法连接其控制通道）。请先打开应用，或运行：cli app start";

/// ctl.json 里存的东西。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CtlInfo {
    /// 应用监听的端口（每次启动都换，只在本机回环上）
    pub port: u16,
    /// 通行证：请求里要带上，防止同机器上别的程序乱调
    pub token: String,
    /// 应用进程号
    pub pid: u32,
    /// 应用版本
    #[serde(default)]
    pub app_version: String,
}

/// ctl.json 的完整路径。
pub fn ctl_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CTL_FILE_NAME)
}

/// 应用启动时写；先建目录再写，失败就当控制通道没开。
pub fn write_ctl_info(data_dir: &Path, info: &CtlInfo) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let text = serde_json::to_string_pretty(info).unwrap_or_default();
    std::fs::write(ctl_file_path(data_dir), text)
}

/// 读 ctl.json；没有或者内容坏了都返回 None。
pub fn read_ctl_info(data_dir: &Path) -> Option<CtlInfo> {
    let text = std::fs::read_to_string(ctl_file_path(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 应用退出时删掉（删不掉也没事，连不上就是没运行）。
pub fn remove_ctl_info(data_dir: &Path) {
    let _ = std::fs::remove_file(ctl_file_path(data_dir));
}

/// 生成一张通行证：只在本机回环上用，随机够长就行。
pub fn new_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..4).map(|_| format!("{:016x}", rng.gen::<u64>())).collect()
}

#[derive(Serialize)]
struct Request<'a> {
    token: &'a str,
    id: u64,
    method: &'a str,
    params: &'a Value,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<String>,
}

/// 给运行中的应用发一条请求；成功返回 result，失败返回人话错误。
pub fn request(info: &CtlInfo, method: &str, params: &Value) -> Result<Value, String> {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, info.port);
    let mut stream = TcpStream::connect_timeout(&addr.into(), Duration::from_secs(3))
        .map_err(|_| APP_NOT_RUNNING.to_string())?;
    // 配免密、连服务器这类操作要等十几秒，读取超时给宽一点
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));

    let req = serde_json::to_string(&Request {
        token: &info.token,
        id: 1,
        method,
        params,
    })
    .map_err(|e| format!("请求编码失败：{e}"))?;
    stream
        .write_all(req.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|_| "与『上线助手』通信失败（连接已断开）".to_string())?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|_| "『上线助手』未响应（可能刚退出）".to_string())?;
    let resp: Response =
        serde_json::from_str(&line).map_err(|e| format!("无法解析它的响应：{e}"))?;
    if resp.ok {
        Ok(resp.result)
    } else {
        Err(resp.error.unwrap_or_else(|| "操作失败".into()))
    }
}

/// 应用在不在跑：读 ctl.json + 试连一下（顺便验证通行证）。
pub fn app_running(data_dir: &Path) -> Option<CtlInfo> {
    let info = read_ctl_info(data_dir)?;
    match request(&info, "ping", &serde_json::json!({})) {
        Ok(_) => Some(info),
        Err(_) => None,
    }
}
