# 上线助手

把你电脑上的服务发布到公网：自动管好 SSH 隧道、云端 nginx、域名和访问保护。
完整设计见 [DESIGN.md](DESIGN.md)。

- **下载安装包**：[Releases](https://github.com/iranolly/shangxian/releases)（安装版；不想装就用便携包）
- 给人看的使用说明：[docs/使用手册.md](docs/使用手册.md)
- 给 AI agent 看的：[docs/AI-AGENT.md](docs/AI-AGENT.md)（配 `cli.exe` 用）

## 两个概念

- **本机服务**：你电脑上的一个东西（比如 comfyui 在 8188）。可以填好启动程序/参数/工作目录，让助手帮你启动、看着它（掉了自动拉起来）、开机自启。**不发布也能用**——只想让它帮你看着本机服务也行。
- **发布隧道**：把某台本机服务接到某台云服务器上，让外网能访问。一条服务可以发到多台服务器；一台服务器上可以挂多条隧道。

删本机服务时如果还有隧道在用它，助手会拦住并告诉你先删哪几条（不自动连带删）。

## 命令行（给 AI agent 和脚本用）

应用是真正干活的那个（常驻托盘），`cli.exe` 是遥控器：查状态、改配置、起停服务与隧道。

```bash
cli status                 # 概览：应用 + 本机服务 + 隧道 + 服务器
cli tunnels                # 隧道清单（服务 → 服务器、云端端口、状态）
cli ports                  # 本机正在监听的端口（带进程名和程序路径）
cli tunnel start <id>      # 开始发布；stop 暂停；restart 重连
cli service start <id>     # 拉起本机服务；stop 停止；edit 改配置
cli nginx <隧道id>         # 生成要贴到云端面板的 nginx 片段
cli app start              # 应用没在跑时把它叫起来
cli methods                # 列出所有能调的方法（自描述）
cli guide                  # 给 AI agent 的速查说明
```

- 默认输出中文人话；加 `--json` 输出机器格式（agent 一律加）。
- 应用没在跑时，只读命令会退回直接读配置文件（状态标 `unknown`）；要改东西先 `cli app start`。
- 它是通过本机回环端口 + 一张通行证（`%APPDATA%\Shangxian\ctl.json`）跟应用说话的，外面连不进来。

## 目录结构

```
上线助手/
├─ DESIGN.md            设计方案（v0.4，含功能/架构/界面/路线）
├─ docs/                使用手册（给人）+ AI-AGENT.md（给 agent）
├─ Cargo.toml           工作区（core + app/src-tauri + cli）
├─ core/                Rust 核心库（不依赖界面）
│  ├─ src/config.rs     配置与 v3→v4 自动迁移（能手改、能备份）
│  ├─ src/job.rs        作业对象：主程序崩溃也不留孤儿进程
│  ├─ src/ssh.rs        ssh.exe 参数构造（写死系统路径，每条隧道独立连接）
│  ├─ src/supervisor.rs 单隧道看护状态机（退出检测、退避重连、停止）
│  ├─ src/localservice.rs 本机服务：启动/停止/看护/端口探测
│  ├─ src/keyboot.rs    密码登录一次 → 自动配好免密
│  ├─ src/ctl.rs        控制通道协议与客户端（命令行从这儿连应用）
│  └─ src/bin/spike.rs  第 0 步验证程序（对本地 sshd 靶机跑真实隧道）
├─ cli/                 命令行（cli.exe）：给 AI agent 和脚本用
├─ app/                 Tauri 桌面应用
│  ├─ src/              Web 界面（React + TypeScript）
│  └─ src-tauri/        Tauri 壳（Rust）
└─ .setup/              本地测试环境（sshd 容器脚本、测试密钥；不进版本库）
```

## 构建与运行

发布（一键出安装包 + 把 cli.exe 一起打进去）：

```bash
bash build-release.sh          # Windows 上也可以双击 build-release.bat
```

前置：Rust（`~/.cargo/bin`）、Node.js、系统自带 OpenSSH。

```bash
# 核心库 + 验证程序（快）
cargo build -p shangxian-core --bins

# 命令行（target/debug/cli.exe）
cargo build -p shangxian-cli

# 桌面应用（开发模式，自动起前端 dev server 并打开窗口）
cd app && npm install && npm run tauri dev

# 桌面应用（打安装包）
cd app && npm run tauri build
```

## 第 0 步验证结果（2026-10-11，本地 Docker sshd 靶机）

| 验证项 | 结果 |
|---|---|
| 双隧道并存 | 19101/19102 两条反向隧道同时工作，穿透访问返回本机服务内容 ✓ |
| 按端口精确清理 | 只杀 19101 的持有者：A 自动重连（2s 退避），B 全程未受影响 ✓ |
| 断线自动重连 | 退出检测 ~0.2s，退避 2→4→8s 递增、稳定后重置 ✓ |
| 主程序崩溃零残留 | 强杀主程序后，两条 ssh 子进程被作业对象带走，云端端口立即释放 ✓ |
| 服务端能力探测的必要性 | linuxserver 镜像默认 `AllowTcpForwarding no`，不探测就会一直失败 ✓（真实服务器已允许转发） |

## 当前进度（2026-10-11）

- ✅ 第 0 步验证（见上表）
- ✅ 界面骨架：设计系统、五状态卡片、修复带、深色/浅色模式、托盘常驻（关闭=缩托盘）
- ✅ 配置存储：`%APPDATA%\Shangxian\config.json`（能手改、能备份；密钥只存路径，绝不存内容）
- ✅ 隧道守护接入应用：界面上的状态、开始/暂停发布、立刻重连都是真实操作（单实例锁）
- ✅ 远端残留端口精确清理：撞上"端口被占"时自动清掉残留 sshd 会话再重连（端到端测试通过）
- ✅ **本机服务与发布隧道拆开**（v0.4）：本机服务可单独登记/启动/停止/看护/开机自启，隧道从已登记的服务里选；老配置自动迁移（备份成 `config.json.v3.bak`）
- ✅ **密码登录一次 → 自动配好免密**（v0.4）：填服务器地址+账号+密码，助手生成密钥、写进服务器、用密钥验证，密码用完即丢
- ✅ 新建隧道向导、导入接管（扫描旧脚本）、日志查看、服务器管理
- ✅ 访问保护的 nginx 配置生成：按隧道生成 server 片段 + 密码文件内容，一键复制到面板
- ✅ 设置页：主题（跟随系统/浅色/深色）、开机自启（HKCU Run 键，登录后静默进托盘）、配置文件夹
- ✅ 掉线通知：连不上超 3 分钟弹一次系统通知，恢复再报一次（可在设置里关、可试弹）
- ✅ 访问密码页：一眼看清每条隧道的保护状态（要密码/锁 IP/谁都能进），缺保护的当场能修
- ✅ 自绘标题栏：和界面同色、圆角窗口、拖拽移动、双击最大化（悬停弹贴靠布局面板这个便利功能没做，用 `Win+Z` / `Win+方向键` 代替）

测试（需本地靶机；`--ignored` 的用例会自建/清理测试容器）：

```bash
cargo test -p shangxian-core --test env_checks
cargo test -p shangxian-core --test guardian_docker -- --ignored
```

## 安全约定

- 清理远端残留**只按端口找持有者**，绝不按用户名全杀。
- 云端端口只绑服务器回环，公网只能走 nginx。
- 私钥不进配置文件、不进日志、不进版本库。
