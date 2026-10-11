# 上线助手 · 进度快照（供上下文压缩/新会话接手用）

更新时间：2026-10-11。详细设计见 DESIGN.md，使用说明见 README.md。

## 项目与约束

- 项目：`D:/AI/SSH_Tunnel_manager`，SSH 隧道发布管理桌面应用（Tauri v2 + Rust 核心）。
- 用户硬约束：**pi-web 绝不接管**（当前会话依赖它的隧道，动它=断线）；karryn viewer 可接管；每个服务要有访问控制；轻量/美观/直觉；速度优先；中文人话汇报。
- 云端：api.example.com（120.26.196.197），nginx 由小皮面板管理，证书走面板。

## 已完成（全部有验证）

- **核心库 shangxian-core**：`job`（作业对象，崩溃零孤儿）、`ssh`（写死 System32 ssh.exe、每服务独立连接）、`supervisor`（状态机+退避 2→30s+精确清理触发）、`config`（`%APPDATA%\Shangxian\config.json`，原子写入）、`guardian`（守护/启停/导入/服务器/日志）、`cleanup`（**按端口精确清理**残留 sshd 会话 + apr1 哈希）、`local_ports`、`import`（解析 ps1/sh 两种写法）、`nginx`（片段生成）。
- **Tauri 壳**：托盘常驻（关闭=缩托盘）、单实例锁、`--hidden` 静默启动；命令：get_snapshot、service_start/stop/restart、remove_service、list_local_ports、add_service、scan_imports、import_services、get_logs、save_server、delete_server、hash_password、get_nginx_snippet、get_autostart、set_autostart、get_data_dir、open_data_dir。
- **前端**：设计系统（深浅色）、服务列表（五状态三件套+修复带）、3 步发布向导、导入弹层、日志弹层、nginx 配置弹层、服务器页（增删改）、设置页（主题/开机自启/配置文件夹）、卡片 ⋯ 菜单。
- **验证证据**：残留清理端到端测试通过（撞端口→自动清理→重连稳定）；`env_checks` 4 测试、`guardian_docker` 2 测试全过；界面截图逐页检查过。
- **踩坑记录**：Windows 有两份 ssh（用 System32 那份）；schtasks 建任务被拒（要管理员）→ 开机自启改用 HKCU Run 注册表键；Tauri 2.12 在 Windows 无 overlay 标题栏；`core/` 改动不会触发 tauri dev 重建（需 `touch app/src-tauri/src/lib.rs`）；容器里查端口持有者要以隧道用户身份（root 缺 CAP_SYS_PTRACE）。

## 当前运行状态

- 应用：`npm run tauri dev` 看护中（日志 `.setup/tauri-dev.log`）。
- 配置：服务器 srv-target（本地靶机 127.0.0.1:2222）、srv-api（香港-1）；服务 pi-web（**已暂停**，30141→13041）、karryn viewer（已暂停，8787→18787）、demo-target（运行中，18083→19103，本地靶机）。
- 本地 Docker 靶机：容器 `sshd-test`（用户 tun，密钥 `.setup/target_key`，AllowTcpForwarding 已开，`find_port_owner` 脚本已装）。

## 常用命令

```bash
# 开发运行（cargo 不在 PATH，要补）
cd app && export PATH="$HOME/.cargo/bin:$PATH" && npm run tauri dev
# 测试
cargo test -p shangxian-core --test env_checks
cargo test -p shangxian-core --test guardian_docker -- --ignored   # 需靶机
# 截图自检
./target/debug/capture.exe 上线助手 .setup/shot.bmp
```

## 待办（按优先级）

1. （可选）接管 viewer 的实战演练；nginx 片段目前按设计"人工应用"
2. 第二台电脑（203.0.113.10）部署：导入 comfyui(18188)、409piweb(13042)

## 自绘标题栏（2026-10-11 晚完成）

- 窗口改为无系统边框（`decorations: false` + `shadow: true`），标题栏画在网页里：拖拽移动、双击最大化、右上角最小化/最大化/关闭三个按钮（关闭 = 缩托盘）。深浅主题都适配。
- **贴靠布局悬停弹层：没做成，已放弃**。原因：官方要求的命中测试（最大化按钮上回 HTMAXBUTTON）已经做了并且验证正确，但系统查的是“光标下那个窗口”——那是 WebView2 的渲染子窗口（属于另一个进程），它只会回“普通区域”；试过盖一层我们自己的隐形小窗（子窗口不支持全透明；顶层窗口全透明时对鼠标又是穿透的，alpha=1 才能命中），结果又撞上“SetWindowPos 报成功但窗口位置不变”的怪现象，继续投入性价比低。
- **替代办法**（系统自带，都能用）：`Win+Z` 直接开贴靠面板、`Win+←/→` 左右贴靠、把窗口拖到屏幕边缘。
- 已验：命中测试全过（最大化=9、按钮/空白=1、顶边=12、圆角=2）、点击能切换最大化、拖拽正常、深浅两套主题外观正常。

## 本轮新增（2026-10-11 晚）

- **掉线通知**：core/notify.rs 纯逻辑（连续异常 3 分钟提醒一次、恢复再报、用户暂停的不报）+ tauri-plugin-notification + 后台巡检线程（每 5 秒）；设置页可开关、可试弹（`app.exe --test-notify` 也能触发）。
- **服务编辑**：编辑弹层（改名/网址/端口/访问保护，密码留空=不换）；guardian.update_service 带端口冲突校验；改端口自动停旧隧道重连。
- **重大修正："进程活着 ≠ 连上了"**。原判定"存活 3 秒=已连接"在服务器卡死（TCP 能连、握手挂住）时会误报"运行中"。现改为看 ssh -v 输出里的 `remote forward success` 行（按本次启动的日志偏移量扫新内容）；ServerAliveInterval 30→15 秒；ssh 参数加 -v。状态词：进程活但未确认 → 显示"连接中"。
- 实测：暂停 Docker 容器 → 约 70 秒后弹"连不上了"通知 → 恢复容器 → 弹"重新发布成功"。env_checks 5 项 + guardian_docker 2 项全过。
- **访问密码页**：每个服务的保护状态一览（绿=已保护、橙=谁都能进/缺密码），当场能改；页面底部提醒"改完密码要重新复制云端配置"。

---

## 下一步大改：服务/隧道拆分 + 密码配免密（2026-10-11 设计完成，待用户过目）

- 用户要求：把「服务」（登记并监控本机服务：启动路径/参数/工作目录、开机自启、掉线重连看护）和「隧道」（SSH 隧道管理：从已登记服务里选 + 选服务器发布）拆成两个场景；另外服务器连接支持「给一次密码 → 自动配好免密登录」。
- 已拍板：界面两页分开；服务看护范围=启动+进程看护+端口探测；删服务时被隧道引用就拦住报错；密码配免密先做小实验验证。
- **小实验已跑通**（`.setup/keyboot-spike`，russh + ring 加密后端）：首次「生成密钥→密码登录→写公钥→密钥登录」全通；重复跑不重复写；密码错能区分。踩坑：russh 默认的 aws-lc 后端在 Windows 要 NASM 汇编器，换 ring 才行。
- 完整设计写进 **DESIGN.md 第 9、10 节**（v0.4）：两个对象的字段、交界规则、数据模型与 v3→v4 迁移、界面结构、实施顺序、错误分类、安全与代价。
- 实施顺序：① 模型+迁移+后端命令 → ② 本机服务页 → ③ 隧道页改造 → ④ 密码配免密。

---

## v0.4 实施进度（2026-10-11）

### 已完成

- **core 数据模型拆分**（core/src/config.rs）：`LocalService`（名字/本机端口/启动程序+参数+工作目录/看护/自启/备注）+ `Tunnel`（service_id + server_id + 云端端口/网址/访问保护/密钥覆盖/enabled）；`AppConfig.version = 4`。
- **v3→v4 自动迁移**：老配置读进来先备份成 `config.json.v3.bak`，再拆成「本机服务 + 隧道」并写回新格式；同一个本机端口的多条老服务会合成一台本机服务；**隧道沿用老 id**（界面标识稳定）。单测 `v3_config_migrates_without_losing_data` 覆盖。
  - 真实配置已验证：3 个老服务 → 3 台本机服务（pi-web 30141 / karryn viewer 8787 / 靶机演示 18083）+ 3 条隧道（id 不变），备份文件已生成。
- **本机服务运行时**（core/src/localservice.rs，新）：启动（按 .exe/.bat/.ps1/.sh 选启动方式，进程进作业对象防孤儿）、停止（只停我们自己启动的那个进程，握句柄不存在误杀）、看护（退避 2→30 秒 + 抖动，连续失败 5 次停下报信）、端口探测（每 2 秒 TCP 连一次，通了才算「在跑」）、状态词（stopped/starting/running/not_listening/failed）。
- **guardian 重写**：本机服务与隧道两套管理；新增规则——本机端口全局唯一、删服务被隧道引用就拦住报错（`还有 N 条隧道在用它（…），先把这些隧道删掉`）、删服务器被隧道引用也拦住、服务停了隧道照跑但界面标出来。
- **Tauri 命令层**：`get_snapshot`（含 services/tunnels/servers）、`save_service`/`remove_service`/`service_start`/`service_stop`/`set_service_watch`/`set_service_autostart`/`get_service_logs`、`add_tunnel`/`update_tunnel`/`remove_tunnel`/`tunnel_start`/`tunnel_stop`/`tunnel_restart`、`list_local_ports`（现在带进程名和程序路径）、`setup_passwordless`。
- **密码配免密**（core/src/keyboot.rs，新）：russh（ring 加密后端，默认的 aws-lc 在 Windows 要 NASM 不能用）+ 系统 ssh-keygen/ssh；流程=生成密钥 → 密码登录 → 幂等写 authorized_keys → 密钥登录验证；错误分类（密码错 vs 服务器不允许密码登录）。靶机实测通过（`keyboot_passwordless_flow`）。
- **测试**：env_checks 6 项全过（含新的 `local_service_start_stop_and_watch`：启动→端口通→外部杀进程→看护拉起→用户停止不再拉起）；guardian_docker 3 项（含 keyboot）。

### 进行中 / 待办

- 前端两页改造（本机服务页 + 发布隧道页 + 弹层/向导/日志/密码页）：已交给分身 w5 做，做完由我验收（tsc 零错误 + 截图 + 实机操作）。
- 服务器页加「用密码登录并配好免密」入口（后端 `setup_passwordless` 已就绪）。
- README 更新（讲清「本机服务」和「发布隧道」两个概念）。

### v0.4 完成与验收（2026-10-11 晚）

前端两页改造由分身 w5 完成后，我逐项验收（用 CDP 驱动真实窗口，不注入鼠标、不打扰前台）：

- ✅ `tsc --noEmit` 零错误；5 个页面 + 弹层全部渲染正常（截图：`.setup/v04-*.png`）
- ✅ 真实数据：迁移后的 3 台本机服务 / 3 条隧道 / 2 台服务器全部正确显示
- ✅ 添加本机服务：端口列表点一下自动填好（名字/端口/程序路径），保存成功
- ✅ 发布新隧道向导三步走通，**端到端验证**：容器里 `curl 127.0.0.1:13000` → 200（直达本机 8188）
- ✅ 删除拦截：还有隧道在用时报「⚠ 还有 1 条隧道在用它（「…」），先把这些隧道删掉」，卡片不被删
- ✅ 看护/开机自启开关写进配置并回读正确
- ✅ 用密码配免密（界面走通）：靶机生成密钥 → 写入服务器 → 密钥登录验证 → 服务器 keyPath 自动指向新密钥 → 隧道用新密钥重启后照常连通（容器访问 19103 → 200）
- ✅ 中途一次误点把 pi-web 的发布启动了几秒：远端 13041 被旧脚本占着 → 应用日志记录 `remote port forwarding failed` + 清理跑了一次但「找不到持有进程」没杀任何东西 → pi-web 全程可达（307）。**清理只杀该杀的，这条红线在真实场景又验证了一次**。

验收中我自己修掉的三个问题：

1. **只监控的服务状态永远显示"没启动"**（即使端口其实是通的）→ localservice 里对「没配启动方式」的服务改为持续探测端口：通=在跑、不通=没启动；隧道卡片上的「本机服务没在跑」黄条因此才准确（新增单测 `monitor_only_service_reports_port_state`）。
2. 只监控的服务不该显示「启动/停止」按钮（那是用户自己开的程序）→ 前端隐藏按钮 + 后端 `stop_service` 返回人话错误「这台服务没配启动方式，是你自己开的程序，我不会去动它」。
3. 服务器页补上「用密码登录一次，帮我配好免密」入口（后端 `setup_passwordless` 已就绪）。

测试现状：env_checks 7 项全过、guardian_docker 3 项（含 keyboot）、tsc 零错误。

验收工具（新）：`tools/cdp.mjs`（eval / shot / call）——用 CDP 驱动运行中的应用；`tools/capture/src/bin/tap.rs`（注入点击，注意会点到前台窗口，前台有游戏时别用）。

### 真实接管演练（2026-10-11 晚）

把一条真实隧道从旧脚本接管到「上线助手」，全程没有中断本机服务：

1. 先确认旧脚本管着哪条隧道（脚本会把自己的 ssh 进程号写在临时状态文件里），只停那条 ssh——**本机服务是脚本的子进程，杀脚本进程不会带走它**，所以服务一直没断（端口探测全程在听）。
2. 停掉脚本的重连循环（注意：Git Bash 的进程号和系统进程号不是一回事，要用系统号停）。
3. 应用里点「开始发布」→ 隧道日志出现 `remote forward success for: listen 127.0.0.1:<远端端口>`，界面变「正在发布」。
4. 端到端验证：服务器上 `curl 127.0.0.1:<远端端口>` 拿到业务响应；从公网访问网址打开的是业务页面（登录保护正常拦截）；远端端口只有一条监听（没有旧脚本残留）；观察 20 秒无抢占。

结论：接管流程可用，清理与看护都按设计工作。旧脚本今后不要再跑（否则会和应用的隧道抢同一个远端端口）。

### 看护/自启配置 + 发现并修掉一个重复启动的 bug（2026-10-11 深夜）

给两台本机服务登记了启动方式，打开看护与开机自启（两台都跑起来后：应用启动 → 自动拉起服务 → 自动发布带 enabled 的隧道）：

| 服务 | 启动方式 | 说明 |
|---|---|---|
| pi-web | `pi-web.cmd --hostname 127.0.0.1 --port 30141 --no-open` | 正常模式 |
| karryn viewer | `python tools/viewer_serve.py --no-open --port 8787 --secure-cookie --dev` | **dev 模式**（改前端/数据自动刷新；改服务端文件退出码 75，由看护接手重启） |

真实场景里发现的问题：应用启动时会给「开了自启且有启动方式」的服务直接拉起进程，**没有先看端口**——服务其实已经在跑（用户自己开的）时，会多起一个进程去抢同一个端口。修法：拉起前先探测端口，已经在听就认定「它已经在跑」，不重复启动并写一行日志。单测 7 项全过。

另外把应用自己的开机自启指向了正式编译版（`target/release/app.exe`），不再指向开发版（开发版依赖本地前端 dev server，重启电脑后打不开界面）。
