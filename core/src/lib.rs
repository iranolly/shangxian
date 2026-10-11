//! 上线助手核心库（不依赖界面）。
//!
//! 分层约定（见 DESIGN.md §3.1）：
//! - `job`：Windows 作业对象，保证本进程退出/崩溃时子进程（ssh.exe）一起被结束，
//!   不会留下孤儿进程。
//! - `ssh`：ssh.exe 命令行参数构造（写死系统路径、不依赖连接复用）。
//! - `supervisor`：单个隧道的状态机（启动、退出检测、退避重试、停止）。
//! - `config`：应用配置（唯一事实来源，能手改、能备份）。
//! - `guardian`：按配置管理全部隧道，输出给界面用的状态。
//! - `proc_detail`：读取运行中进程的程序路径/命令行/工作目录（添加本机服务自动填充用）。

pub mod cleanup;
pub mod config;
pub mod ctl;
pub mod guardian;
pub mod import;
pub mod job;
pub mod keyboot;
pub mod local_ports;
pub mod localservice;
pub mod nginx;
pub mod notify;
pub mod proc_detail;
pub mod protection;
pub mod ssh;
pub mod supervisor;
