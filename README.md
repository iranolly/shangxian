# 上线助手 v0.6.3

把你电脑上的服务发布到公网：自动管好 SSH 隧道、云端 nginx、域名和访问保护。
程序掉了自动拉起来，电脑睡醒、网络恢复、甚至应用自己崩了都能自己接上。
完整设计见 [DESIGN.md](DESIGN.md)；本次更新见 [Releases 发布说明](https://github.com/iranolly/shangxian/releases)。

- **下载安装包**：[Releases](https://github.com/iranolly/shangxian/releases)（安装版，中文安装向导；不想装就用便携包）
- 给人看的使用指南：[docs/使用指南.md](docs/使用指南.md)（安装包/便携包里也带一份）
- 想自己编译、改代码：[AGENTS.md](AGENTS.md)（开发指南：环境、编译、测试、打包）
- 给 AI agent 看的：[docs/AGENTS.md](docs/AGENTS.md)（配 `cli.exe` 用；安装后程序旁边也带一份）
- 给 AI agent 用的 skill：[skills/shangxian/SKILL.md](skills/shangxian/SKILL.md)（Pi 用户一条命令装：`pi install git:github.com/iranolly/shangxian`；其他工具把文件夹拷进 skills 目录）

## 两个概念

- **本机服务**：你电脑上的一个东西（比如 comfyui 在 8188）。可以填好启动程序/参数/工作目录，让助手帮你启动、看着它（掉了自动拉起来）、开机自启。**不发布也能用**——只想让它帮你看着本机服务也行。
- **发布隧道**：把某台本机服务接到某台云服务器上，让外网能访问。一条服务可以发到多台服务器；一台服务器上可以挂多条隧道。

删本机服务时如果还有隧道在用它，助手会拦住并告诉你先删哪几条（不自动连带删）。删除后 10 分钟内可以在「设置 → 最近删除」里恢复。

## 功能一览

**发布与管理**

- 本机服务与发布隧道分开管：登记、启动、停止、看护、开机自启；隧道从已登记的服务里选，一键发布/暂停。
- 新建向导 3 步走完；支持**发布预设**（普通网站 / WebSocket / 大文件上传）、**克隆隧道**、**复制公网网址**、**手机扫码打开**（二维码离线生成）。
- 列表有搜索、状态筛选、排序、按服务器分组；隧道和本机服务都支持**批量管理**（隧道：开始发布 / 暂停 / 删除；服务：启动 / 停止 / 删除）。
- 访问保护可以同时要密码和锁 IP；密码在本地算成哈希（优先 bcrypt `$2y$…`；服务器 nginx 自带旧加密库、验不了 bcrypt 时自动用兼容格式 `$apr1$…`），服务器不用装 openssl，明文不落盘、不进命令行。
- 「检查云端」SSH 读回服务器上的站点配置和密码文件，核对实际生效的保护；对不上时卡片会弹漂移条，改了保护相关设置后核对结果会自动清成「待核对」。
- HTTPS 提醒：网址写了 https 但没开、证书快到期或已过期、密码能用 http 明文提交，都会红黄字提示。
- 访问审计：按需读云端最近 1000 行访问日志，统计被挡了多少次、哪些 IP 试得最多（只在本地显示）。

**稳定与自愈**

- 掉线自动重试（退避 2→30 秒）、远端残留按端口精确清理、日志按 id 命名并自动轮转。
- 电脑**睡眠唤醒 / 网络恢复**后自动重建隧道，不用手动点。
- 应用**崩溃自动恢复**（Windows 拉起、静默回托盘）；10 分钟内连崩 3 次自动暂停恢复防死循环。
- 本机服务保活：应用退出/崩溃不带走本地程序；下次启动按端口认领，不重复启动。
- 掉线/恢复系统通知；多条自动合并；**免打扰时段**只压掉线提醒，恢复照报。
- 配置读坏不静默清空（留底 + 红条 + 暂停自动保存）；新版存的配置被旧版打开时只读不改，升级后自动恢复。

**排查工具**

- **体检**：9 步检查（程序、文件夹、SSH、本机端口、服务器、隧道、云端配置、公网访问、域名解析），快速/深度两档，哪一步没过给「怎么办」。
- **诊断包**：一键导出脱敏 zip；不含登录密码、密码哈希、私钥、控制令牌、启动参数原文。
- **日志查看器**：搜索、只看错误、行数 200~2000、归档切换、全部日志按来源分组、复制/导出、显示路径并打开文件夹。
- **运行统计**：应用跑了多久、今日重连几次、下轮重试还有多久。

**省心功能**

- **撤销删除**：删错了 5 秒内点「撤销」，10 分钟内还能在「最近删除」恢复（隧道恢复为暂停，服务不自动启动）。
- **导入导出**：设置可导出备份（命令行），也能从文件合并/整体替换导回（界面或命令行），导入前可先干跑检查，替换前自动备份。
- **更新检查**：启动约 30 秒后静默查一次（6 小时内不重复），发现新版本只提醒一次、只跳发布页，绝不自动下载安装；设置页可关。
- **通知中心**：标题栏铃铛，掉线、恢复、体检失败等事件都能回看和标已读。
- 设置页：外观、开机自启、掉线提醒、刷新频率（默认 800 毫秒，不看窗口时停刷）、启动检查更新、免打扰时段、数据目录、最近删除、体检、关于。
- 窗口位置和大小会记住；关窗缩托盘、托盘悬停显示「X 条发布中 · Y 条掉线」。

**安装与发布物料**

- 中文 NSIS 安装器：程序和快捷方式都叫「上线助手」，装在自己账号目录、**不需要管理员**；卸载时有「同时删除配置和密钥」勾选框（**默认不勾**，配置默认保留）。
- 安装/卸载前先调 `cli app stop-all --yes`，把正在发布的隧道和应用拉起的本机服务停掉再动程序文件。
- 安装包：`Shangxian_<版本>_x64-setup.exe`；便携包：`shangxian-v<版本>-portable.zip`（数据放解压目录的 `data\`，整个文件夹拷走即搬家）；另出 `SHA256SUMS` 校验和。
- 安装包和便携包都带三份文档，就在 `cli.exe` 旁边，装完/解压后可以直接读：给 AI 看的 `AGENTS.md`（命令参考 / 配方 / 安全红线）和 `SKILL.md`（agent 技能文件），以及给人看的 `使用指南.md`。

## 命令行（给 AI agent 和脚本用）

应用是真正干活的那个（常驻托盘），`cli.exe` 是遥控器：查状态、改配置、起停服务与隧道。

```bash
# 查看
cli status                 # 概览：应用 + 本机服务 + 隧道 + 服务器
cli services --json        # 本机服务清单（id、端口、状态、启动方式）
cli tunnels --json         # 隧道清单（服务 → 服务器、云端端口、状态）
cli servers --json         # 云端服务器清单
cli ports --json           # 本机正在监听的端口（带进程名和程序路径）

# 操作
cli tunnel start <id>      # 开始发布；stop 暂停（要确认）
cli tunnel check <id>      # 去云端核对实际生效的访问保护（密码 / IP 白名单）
cli service start <id>     # 拉起本机服务；stop 停止；edit 改配置
cli nginx <隧道id>         # 生成要贴到云端面板的 nginx 片段

# 体检 / 排查
cli doctor [--deep] [--strict]   # 9 步体检；--deep 连服务器和公网；--strict 有没过就退出码 1
cli diag [--out 文件] [--deep]   # 打一个脱敏诊断包
cli stats                        # 运行统计：时长、今日重连、重试倒计时
cli logs <隧道id> --lines 80     # 看日志；--kind service 看服务日志；--archive 1/2 看轮转备份
cli logs --source all --format json --json   # 事件 + 应用日志 + 所有隧道/服务日志

# 撤销 / 备份 / 停一切 / 查更新
cli undo [last|list|令牌]   # 撤销删除（删除后 10 分钟内、最多 20 条）
cli config export --out 备份.json [--with-paths]   # 导出设置（默认脱敏）
cli config import 备份.json [--dry-run] [--yes] [--mode replace]   # 导入设置
cli app stop-all --yes      # 升级/卸载前停全部隧道和应用拉起的本机服务
cli app update-check        # 查有没有新版本（只查，不下载、不安装）

cli methods                # 列出所有能调的方法（自描述）
cli guide                  # 给 AI agent 的速查说明
```

- 默认输出中文人话；加 `--json` 输出机器格式（agent 一律加）：成功是 `{"ok":true,"result":...}`，失败是 `{"ok":false,"error":"人话原因"}`。
- 退出码：`0` 成功、`1` 出错、`2` 命令写法不对、`3` 应用没在跑（`cli app status`）。
- 危险操作（暂停/删除隧道、删除服务/服务器、停全部、重置指纹、导入配置）会确认：交互终端问 `y/N`，非交互（脚本/agent）必须加 `--yes`，不加就先拒绝不动手。
- `--access` 只认 `password` / `ip` / `none`，拼错直接报错（不会悄悄变成「无保护」）；访问密码在你电脑上算好（优先 bcrypt，服务器不支持时自动用兼容格式），密码本身不碰服务器；不想让密码出现在命令行里就用 `cli server hash-password`（密码走标准输入，不回显）。
- 应用没在跑时，只读命令会退回直接读配置文件（状态标 `unknown`）；要改东西先 `cli app start`。`cli doctor` 离线也能跑，`cli app stop-all` 没在跑时直接退出码 0。
- 它是通过本机回环端口 + 一张通行证（数据目录里的 `ctl.json`）跟应用说话的，外面连不进来。

## 目录结构

```
上线助手/
├─ AGENTS.md            开发指南（环境 / 编译 / 测试 / 打包）
├─ DESIGN.md            设计方案（功能 / 架构 / 界面 / 路线）
├─ docs/                使用指南（给人）+ AGENTS.md（给 agent）
├─ packaging/说明.txt    便携包里给用户看的说明
├─ Cargo.toml           工作区（core + app/src-tauri + cli + tools/capture）
├─ core/                Rust 核心库（不依赖界面）
│  ├─ src/config.rs     配置与 v3→v4→v5→v6 自动迁移（能手改、能备份；读坏留底 config.json.bad；便携模式）
│  ├─ src/state.rs      运行状态：重连统计、保护核对、撤销墓碑（state.json）
│  ├─ src/events.rs     结构化事件流（events.jsonl，自动轮转）
│  ├─ src/job.rs        作业对象：主程序崩溃也不留孤儿进程（本机服务保活）
│  ├─ src/ssh.rs        ssh.exe 参数构造（写死系统路径，每条隧道独立连接）
│  ├─ src/supervisor.rs 单隧道看护状态机（退避重连、日志按 id 命名 + 轮转）
│  ├─ src/localservice.rs 本机服务：启动/停止/看护/端口探测/假死处理/日志封顶
│  ├─ src/guardian.rs   总看护：服务与隧道对齐、参数变自动重建、检查云端、停全部
│  ├─ src/keyboot.rs    密码登录一次 → 自动配好免密
│  ├─ src/cleanup.rs    按端口精确清理残留会话 + 本机 bcrypt 访问密码哈希
│  ├─ src/doctor.rs     体检的 9 步纯逻辑（快速 / 深度）
│  ├─ src/wake.rs       睡眠唤醒 / 网络恢复 / 时钟跳变检测
│  ├─ src/protection.rs 云端访问保护解析（密码 / IP 白名单）
│  ├─ src/cloudcheck.rs 云端检查脚本与结果解析（环境 / HTTPS / 端口 / 审计）
│  ├─ src/log.rs        应用日志（app.log）与时间戳 / 轮转
│  ├─ src/probe.rs      本机端口探测（TCP / HTTP，防抖、锁外）
│  ├─ src/ctl.rs        控制通道协议与客户端（命令行从这儿连应用）
│  └─ src/bin/spike.rs  第 0 步验证程序（对本地 sshd 靶机跑真实隧道）
├─ cli/                 命令行（cli.exe）：给 AI agent 和脚本用
├─ app/                 Tauri 桌面应用
│  ├─ src/              Web 界面（React + TypeScript）
│  │  ├─ components/    卡片、列表工具条、弹层、体检、通知中心、向导……
│  │  ├─ pages/         本机服务 / 发布隧道 / 云端服务器 / 设置
│  │  └─ store/         快照刷新、乐观合并、toast、通知、筛选偏好
│  └─ src-tauri/        Tauri 壳（Rust）
│     ├─ nsis/          中文安装器文案 + 安装/卸载钩子
│     ├─ src/lib.rs     命令注册、启动矩阵、托盘
│     ├─ src/ctl.rs     控制通道分发表（方法与中文说明，cli methods 就是它）
│     ├─ src/notify.rs  掉线/恢复通知线程（免打扰、聚合、点击打开）
│     ├─ src/recovery.rs 崩溃自动恢复（RegisterApplicationRestart + 防死循环）
│     ├─ src/power.rs   睡眠唤醒 / 网卡变化回调
│     └─ src/mainwindow.rs / singleton.rs / titlebar.rs / logging.rs
└─ .setup/              本地测试环境与设计笔记（不进版本库）
```

## 构建与运行

发布（一键出安装包 + 便携包 + 校验和）：

```bash
bash build-release.sh          # Windows 上也可以双击 build-release.bat
```

出来的东西都在 `target/release/bundle/nsis/`：

- `Shangxian_<版本>_x64-setup.exe` — NSIS 安装包（中文界面，装好后 `app.exe` + `cli.exe` 在一起）
- `shangxian-v<版本>-portable.zip` — 免安装便携包（解压即用；里面有 `shangxian.portable` 标记，数据存到解压目录的 `data\`）
- `SHA256SUMS` — 上面两个文件的 SHA256 校验和（Git Bash 里 `sha256sum -c SHA256SUMS` 可验证）

前置：Rust（`~/.cargo/bin`）、Node.js、系统自带 OpenSSH。

```bash
# 核心库 + 验证程序（快）
cargo build -p shangxian-core --bins

# 命令行（target/debug/cli.exe）
cargo build -p shangxian-cli

# 桌面应用（开发模式，自动起前端 dev server 并打开窗口）
cd app && npm install && npm run tauri dev

# 桌面应用（打安装包）：不要在 app 目录单独 build —— 少了 cli.exe 会失败。
# 在项目根目录跑一键发布（先编译 cli 放到 sidecar 位置，再打中文 NSIS 安装包，
# 最后出便携包和 SHA256SUMS；只出 .exe/zip，不出 .msi）
bash build-release.sh
```

## 第 0 步验证结果（2026-10-11，本地 Docker sshd 靶机）

| 验证项 | 结果 |
|---|---|
| 双隧道并存 | 19101/19102 两条隧道同时工作，穿透访问返回本机服务内容 ✓ |
| 按端口精确清理 | 只杀 19101 的持有者：A 自动重连（2s 退避），B 全程未受影响 ✓ |
| 断线自动重连 | 退出检测 ~0.2s，退避 2→4→8s 递增、稳定后重置 ✓ |
| 主程序崩溃零残留 | 强杀主程序后，两条 ssh 子进程被作业对象带走，云端端口立即释放 ✓ |
| 服务端能力探测的必要性 | linuxserver 镜像默认 `AllowTcpForwarding no`，不探测就会一直失败 ✓（真实服务器已允许转发） |

## 当前进度（v0.6.3）

- ✅ 第 0 步验证（见上表）
- ✅ 核心库：配置 v6、运行状态、事件流、日志、体检、唤醒检测、云端检查、便携模式；本机服务与隧道全套看护
- ✅ 本机服务与发布隧道拆分：登记/启动/停止/看护/开机自启；老配置自动迁移（v3→v4→v5→v6，逐级备份）
- ✅ 密码登录一次 → 自动配好免密；访问保护（本地算哈希：优先 bcrypt，服务器不支持时自动用兼容的 apr1；nginx 片段 + 检查云端 + 漂移提示）
- ✅ 界面：五状态卡片、列表搜索/筛选/排序/分组、批量管理、3 步向导、克隆、预设、二维码、日志查看器、体检、诊断包、通知中心、最近删除、设置页全项
- ✅ 可靠性：睡眠唤醒自愈、崩溃自动恢复、服务保活与认领、假死处理、日志轮转、配置读坏保护、降级拦截
- ✅ 中文安装器 + 便携包 + SHA256SUMS；安装/卸载前停干净，卸载可选删数据
- ✅ CLI：体检 / 诊断包 / 统计 / 日志（含归档）/ 撤销 / 配置导入导出 / 停全部 / 查更新，JSON 信封与退出码统一
- ✅ 更新检查：GitHub 发布页，启动 30 秒后静默查、6 小时限频、发现新版只提醒一次、绝不自动安装

测试（需本地靶机；`--ignored` 的用例会自建/清理测试容器）：

```bash
cargo test -p shangxian-core --test env_checks
cargo test -p shangxian-core --test guardian_docker -- --ignored
```

## 安全约定

- 清理远端残留**只按端口找持有者**，绝不按用户名全杀。
- 云端端口只绑服务器回环，公网只能走 nginx。
- 私钥不进配置文件、不进日志、不进版本库；诊断包和配置导出默认脱敏（不含密码哈希、私钥内容、控制令牌、启动参数原文）。
- 访问密码只存哈希（优先 bcrypt；服务器不支持时用兼容的 apr1），明文不落盘、不进命令行；配免密用的登录密码只在内存里用一次。
- 更新只提示、不自动下载安装；安装包没有代码签名，只从本仓库 Releases 下载并核对校验和。
