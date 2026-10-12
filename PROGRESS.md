# 上线助手 · 进度快照（供上下文压缩/新会话接手用）

更新时间：2026-10-12。详细设计见 DESIGN.md，使用说明见 README.md。

## 项目与约束

- 项目：`D:/AI/SSH_Tunnel_manager`，SSH 隧道发布管理桌面应用（Tauri v2 + Rust 核心）。
- 用户硬约束：**pi-web 绝不接管**（当前会话依赖它的隧道，动它=断线）；karryn viewer 可接管；每个服务要有访问控制；轻量/美观/直觉；速度优先；中文人话汇报。
- 云端：api.example.com（120.26.196.197），nginx 由小皮面板管理，证书走面板。

## 已完成（全部有验证）

- **核心库 shangxian-core**：`job`（作业对象，崩溃零孤儿）、`ssh`（写死 System32 ssh.exe、每服务独立连接）、`supervisor`（状态机+退避 2→30s+精确清理触发）、`config`（`config.json` 放数据目录，原子写入，版本 5）、`guardian`（守护/启停/导入/服务器/日志）、`cleanup`（**按端口精确清理**残留 sshd 会话 + **本机 bcrypt** 访问密码哈希）、`local_ports`、`import`（解析 ps1/sh 两种写法）、`nginx`（片段生成 + `vhostDir` 解析）。
- **Tauri 壳**：托盘常驻（关闭=缩托盘）、单实例锁、`--hidden` 静默启动；命令：get_snapshot、service_start/stop/restart、remove_service、list_local_ports、add_service、scan_imports、import_services、get_logs、save_server、delete_server、hash_password、get_nginx_snippet、get_autostart、set_autostart、get_data_dir、open_data_dir。
- **前端**：设计系统（深浅色）、服务列表（五状态三件套+修复带）、3 步发布向导、导入弹层、日志弹层、nginx 配置弹层、服务器页（增删改）、设置页（主题/开机自启/配置文件夹）、卡片 ⋯ 菜单。
- **验证证据**：残留清理端到端测试通过（撞端口→自动清理→重连稳定）；`env_checks` 4 测试、`guardian_docker` 2 测试全过；界面截图逐页检查过。
- **踩坑记录**：Windows 有两份 ssh（用 System32 那份）；schtasks 建任务被拒（要管理员）→ 开机自启改用 HKCU Run 注册表键；Tauri 2.12 在 Windows 无 overlay 标题栏；`core/` 改动不会触发 tauri dev 重建（需 `touch app/src-tauri/src/lib.rs`）；容器里查端口持有者要以隧道用户身份（root 缺 CAP_SYS_PTRACE）。
- **云端配置弹层精简**（2026-10-12）：说明压成 4 条短句（按密码/白名单条件追加）；删除「复制这段」按钮和独立的证书续期卡片，改为示意图里每个高亮段首行行尾的小「复制」按钮（复制该段原文含注释，1.6 秒「已复制 ✓」）；未填网址/无保护的警示行不配按钮。验证：`app` 的 `npm run build` 通过、`cargo test -p shangxian-core` 全绿。

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

1. （可选）接管 viewer 的实战演练；nginx 片段目前按设计“人工应用”
2. 第二台电脑（203.0.113.10）部署：导入 comfyui(18188)、409piweb(13042)
3. 文档已随 v0.5 全景修复更新（README / DESIGN / PROGRESS / 使用手册 / AI-AGENT / SKILL，见下节）

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
- **访问保护页**：每个服务的保护状态一览（绿=已保护、橙=谁都能进/缺密码），当场能改；页面底部提醒"改完密码要重新复制云端配置"。

---

## 前端拆分：「发布隧道」只管发布 / 「访问保护」只管门禁（2026-10-11 晚）

- **分工落地**：「发布隧道」页 = 新建 / 启停 / 日志 / 删除 / 改名称网址云端端口；「访问保护」页（原「访问密码」）= 网页访问密码 / IP 白名单 / 检查云端。导航与页面标题改「访问保护」（文件名 Passwords.tsx 不变），补消歧文案（服务器登录密码 = SSH 用；网页访问密码 = 访问保护用；密码文件 = htpasswd）。
- **新增 `app/src/components/ProtectionModal.tsx`**：访问保护专用弹层（密码开关 + 访问用户名 + 新密码 + IP 白名单）；密码是否开启用 `tunnel.access` 数组判断（不用 `hasPassword`，它是残留哈希的派生值、历史上会误报）；新密码留空 = 保留原密码；保存成功后由访问保护页直接打开该隧道的「云端配置」作为下一步。
- **访问保护页**：行按钮改开 ProtectionModal，删掉每行的独立「云端配置」按钮（独立入口只保留在「发布隧道」页卡片 ⋯ 菜单），保留 ProtectionStatus 行与「检查云端」。
- **编辑隧道弹层**：去掉访问保护部分，只留名称/网址/云端端口；保存时把当前 `access`/`accessUser`/`accessDetail` 原样回传、不传 `accessHash`（密码保持不变），改结构不误动保护。
- **隧道卡片**：去掉内嵌保护状态行与「检查云端」。**新建向导**：保留第 3 步，但「哈希失败可稍后补」改成明确阻断（连不上服务器直接拦住，后端创建时必须有哈希）。
- **文档同步**：docs/使用手册.md（3.2/3.4/3.6/常见问题/安全红线）、DESIGN.md（4.2 导航、4.3 侧栏示意、9.6 界面分工）、PROGRESS.md 本节。
- **验证**：`cd app && npm run build` 通过（tsc + vite，44 modules，无错误）；未运行应用、未连服务器。

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
- **访问保护可叠加 + 检查云端**（2026-10-11）：隧道的保护从「三选一」改成「要密码」和「只允许指定 IP」可同时开（同时开 = 两个条件都要满足；都不开 = 界面标红）；发布隧道页显示当前设置的保护，新增「检查云端」按钮（SSH 读云端站点配置和密码文件，列出实际生效状态，不一致提示重新贴配置）；配置 `access` 改数组、版本 4→5 自动迁移（备份 `config.json.v4.bak`）；CLI `--access` 可重复并新增 `tunnel check`，方法表 35 条（v0.5 新增 `test_server` 后为 36 条）。

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

### nginx 层访问密码：真实应用一次（2026-10-11）

给一条隧道加「第二道门」（nginx basic auth）的完整流程走通了：

1. 应用里「访问密码」页设置密码 → 只存 bcrypt 哈希（`$2y$12$…`，本机算）；点「云端配置」拿到片段（含 htpasswd 内容 + 目标路径，路径按服务器的 `vhostDir` 解析）。
2. 服务器的站点配置是 root 的，普通账号改不了、sudo 要密码——所以让用户在有终端的 SSH 会话里跑一次准备好的脚本（脚本里做了：备份 → 写 htpasswd → 插 auth_basic → `nginx -t` → reload）。
3. 一个容易漏的坑：`auth_basic` 放在 server 层会连 `/.well-known/` 一起保护，**证书自动续期会失败**——脚本里给那个 location 加了 `auth_basic off`。
4. 验证：公网不带密码 401 + `WWW-Authenticate`（浏览器弹框）；密码错仍 401；`/.well-known/` 不弹（豁免生效）；其他站点配置零改动。

另外：应用自己不会自动改服务器 nginx（设计如此），这一步始终是「生成片段 + 人工应用」。

## 自动读取启动方式 + 敏感拦截 + 停止确认（2026-10-11）

- **冻结接口落地**：Tauri `inspect_process_launch_info(port, expectedPid?)`（`core::proc_detail::LaunchInfo`）、`preview_launch_command(exe, args)`；`service_stop(id, killForeign?, expectedPid?)` 返回 `{kind: stopped|needsConfirm|foreignStopped, pid?, exe?}`。接口名/字段名未改。
- **前端（app/）**：点监听端口行先照旧填端口/程序/名字，再自动读取启动方式；读不可靠（`pidMatched=false` 或 `multipleOwners`/`processGone`/`portOwnerChanged`）就不覆盖任何字段，只在警告区说明；`sensitiveArgs` 红字 + 勾选框，不勾不能保存；自动填的字段带「自动读取 · 未验证」，一编辑就消失；工作目录 hint 改成「读到的是现在的工作目录」；启动参数改两行 textarea；参数下方显示只读的「实际会执行的命令行」预览；已登记端口标「已登记：名字」，点击跳到那台服务编辑并自动补全；停止外部进程前弹人话确认框（程序名 + PID），确认后才 `killForeign=true`。
- **CLI**：`service stop` 默认只停应用自己启动的；`needsConfirm` 时退出码 1 并提示 `--force`（`--force` 传 `killForeign=true`、省略 `expectedPid`）；`service restart` 遇 `needsConfirm` 提示先手动停。
- **ctl.rs**：方法表更新 `service_stop` 参数/返回，新增 `inspect_process_launch_info`、`preview_launch_command`（当时共 34 条；后续 v0.5 新增 `test_server`，现为 36 条）；`lib.rs` 注册新命令（inspect 走 async + spawn_blocking）。
- **文档**：AI-AGENT.md（方法表 + 输出样例 + 自动读取纪律）、使用手册.md（字段文案与交互说明）、DESIGN.md §11（实现记录与已知边界：环境变量不捕获、workdir=当前目录、参数近似还原、32/64 位与权限边界）、skills/shangxian/SKILL.md（停止语义）。
- **验证（含审查修复后复验）**：`cd app && npm run build` 通过；`cargo check --workspace --all-targets` 通过；core 单测 25 项 + `env_checks` 7 项 + 真机集成 `proc_features` 7 项 + `stop_foreign` 10 项全过（49 passed / 0 failed / 6 ignored），测试后无孤儿进程。
- **独立审查修复**：审查提出 1 P1 + 4 P2 已修复并复验 PASS ——（1）停止时先停自家进程整棵树再轮询端口，包装脚本（.bat/.ps1）起的孙进程监听不再被误判成外部、不再弹假确认框；（2）只监控服务一律拒绝停止（无论 --force 与否），CLI 不再提示必然失败的 --force；（3）编辑模式点端口取消后启动程序/参数/工作目录三个字段原样保留；（4）参数因换行/超长被丢弃时在界面上展示只读的原始命令行；另有告警条件与文案对齐、service_stop 改异步等小修。

## 本地服务保活 + 退出停全部（2026-10-11，已完成）

应用退出/重启/崩溃不再带走本地服务，下次启动按端口认领；SSH 隧道行为不变（跟应用走，应用一退即断）。

- **core（已落地）**：`Guardian::stop_all_for_exit() -> (usize, usize)`（已停止 / 没能停）：先停应用自己本次启动的进程整棵树，再对仍在监听的认领进程按程序路径核对归属，核对通过才结束；核对不过的（如配置是包装脚本、实际监听者是子进程）不硬杀、计入没能停。本地服务改用保活作业对象（不设 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）；首次 `poll` 按端口探测认领（**不做归属核对**），认领后 `user_stopped=false`，看护（watch）对认领进程照常拉起。`ssh.exe` 仍是 kill-on-close 作业。
- **app**：托盘新增「退出并停止全部本地服务」（id `quit_stop_services`）：`guardian.lock().stop_all_for_exit()` 后先弹结果通知（如「已停止 N 个本地服务；M 个没能停（可能是脚本启动的服务，需要手动处理）」；通知发不出去则写应用日志 `logs/app.log`），再 `app.exit(0)`；普通「退出」保持只退应用、不杀服务。编辑运行中的服务、且启动方式（launchExe/launchArgs/launchWorkdir）有变化时，卡片提示「启动方式已改；正在跑的程序没有动，改动会在下次重启服务时生效」（点「启动」后清除）；认领来的进程停止时先弹确认（复用 `needsConfirm` 流程）。
- **CLI**：检查无退出应用命令（只有 `app start` / `app status`），无需改语义；停止认领进程与外部进程同路径（`needsConfirm` → `--force`）。
- **文档**：使用手册新增「退出和重启应用时，本地服务会继续跑」+ 托盘菜单三行；DESIGN.md §11.4（认领不做归属核对、退出停全部语义、包装脚本/首轮认领/只停监听者三条边界）；AI-AGENT.md 铁律 5 / service stop / 离线行为 / 已知坑 11；SKILL.md 铁律 5、8。
- **验证**：`cd app && npm run build` 通过；`cargo check -p app -p shangxian-cli` 通过，`cargo check --workspace --all-targets` 通过。core 测试 81 passed / 0 failed / 8 ignored（lib 49、env_checks 9、keep_alive 6、proc_features 7、stop_foreign 10；8 项 ignored 均按设计跳过：Docker 靶机 3、真机探针 1、真机对照 1、辅助进程在各测试二进制中共 3），测试后无孤儿进程。

## v0.5 全景修复 + 文档同步（2026-10-12）

52 条审查发现全部修完，行为定稿。详细梳理见 DESIGN.md §11.5；文档（README / DESIGN / PROGRESS / docs/使用手册.md / docs/AI-AGENT.md / skills/shangxian/SKILL.md）以这批行为为准重排：

- **密码**：访问密码改**本机 bcrypt**（`$2y$12$…`），服务器不用装 openssl；明文不进命令行、不落盘。
- **站点目录**：服务器 `vhostDir` → `SHANGXIAN_HTPASSWD_DIR` → 默认 `/xp/panel/vhost`；配置 `<目录>/nginx/<域名>.conf`、密码文件 `<目录>/<域名>.htpasswd`。
- **配置读坏**：留底 `config.json.bad`、界面红条、读坏期间禁止自动保存；不再静默清空也不拿空配置覆盖。
- **隧道重建**：改服务器地址/端口/账号/密钥或本机端口，跑着的隧道自动按新参数重建；失败原因（认证、解析、端口占用等）写进 `reason` 显示在卡片上。
- **看护与自启**：看护/开机自启必须先填启动程序；自启先认领已在跑的端口（约 13 秒宽限）；假死（进程在但端口不通）开看护自动重启、没开只报警。
- **日志**：按隧道/服务 id 命名（改名不换文件），隧道日志 >5MB 轮转保留 2 份；旧命名日志不再被界面/CLI 读取。
- **CLI**：`--access` 拼错报错；回显最终保护状态；破坏性命令（tunnel stop/remove、service/server remove）要确认（交互 y/N，非交互 `--yes`）；JSON 统一 `{"ok":true,"result":...}`；退出码 0/1/2/3；`logs --kind` 只认 tunnel/service；输入密码不回显。
- **应用**：托盘「退出（会停止发布）」带确认；关窗首次提示缩托盘；通知「发布掉线/恢复：名字」；托盘 tooltip「X 条发布中 · Y 条掉线」；向导成功屏「还差一步：贴云端配置」+ 直达按钮；无保护发布要勾选；保护绿色只在「检查云端」通过后；服务器页「测试连接」+「站点目录」；空页三步引导；ctl 新增 `test_server`（共 36 个方法）。

> 文档阶段验证：只改文档，不跑应用、不做 git 操作；所有行为逐条对照现有代码（config.rs / nginx.rs / cleanup.rs / guardian.rs / supervisor.rs / localservice.rs / cli/main.rs / ctl.rs / 前端）。

## v0.6 可靠性 / 生命周期改造 + 文档同步（2026-10-12）

本轮落地约 60 项改造（版本号 `0.6.0`，即 `cli 0.6.0`；设计底稿 `.setup/design-v2/D0-arbitration.md` ~ `D5-lifecycle.md`，D0 裁决优先，行为以代码为准。架构与数据模型定稿见 **DESIGN.md §12**，Agent 文档见 **docs/AI-AGENT.md（本版对应 ctl 61 条方法 / 配置 v6）**，助手技能见 **skills/shangxian/SKILL.md**（并同步到本机 `C:/Users/cgt/.pi/agent/skills/shangxian/SKILL.md`）。

### 本轮改造要点（按模块）

- **配置 v6**：`config.json` 新增 `settings`（`refreshIntervalMs` 500..=10000、`quietHours`、`checkUpdateOnStart`）与隧道 `preset`（web/ws/upload）、服务 `probeKind`（tcp/http）；`normalize_config` 读时归一化（越界夹回、非法值换默认、发 `configNormalized` 事件）；版本高于程序「降级拦截」只读不写、不产生 `.bad`。
- **运行状态与事件**：新增 `state.json` v1（当日重连、失败记忆、`protectionCheck`、墓碑、环境缓存；去抖落盘 + 退出 flush）与 `events.jsonl` v1（结构化事件、10 秒去重、每秒 ≤10 条、2MB×2 轮转）。
- **原因码与探测**：`core/src/reason.rs` 42 个原因码（`reasonCode` 写进快照/事件）；`core/src/probe.rs` 每服务独立探测线程（TCP 100ms / HTTP 500ms，连续 3 次失败才判 down）；`core/src/wake.rs` 睡眠唤醒 / 网络恢复 / 时钟跳变检测。
- **体检与云端核对**：`core/src/doctor.rs` 九步体检（quick/deep，单步 20s / 整次 60s，报告落 `logs/doctor-last.json`）；`core/src/cloudcheck.rs` 云端 V2 只读核对（访问保护 + HTTPS/证书 + 端口回环 + 环境识别 + 访问审计 + 公网 IP + 漂移判定）。
- **删除撤销 / 备份 / 诊断包**：删除（隧道/服务/服务器）返回撤销令牌，`state.json` 墓碑 10 分钟 / 最多 20 条；配置导出（默认脱敏）与导入（merge/replace、dry-run、原子写 + 自动备份）；`cli diag` 生成脱敏诊断包（不含哈希/密钥/token）。
- **CLI**：新增 `stats` / `doctor` / `diag` / `undo` / `config export|import` / `app stop-all|update-check` / `tunnel forget-key`；`logs` 支持 `--source all|tunnel|service|app` + `--archive 0|1|2` + `--format lines|json`；`tunnel add/edit --preset`；退出码 3 扩展到「所有需要应用但应用没在跑」的命令；`doctor --strict` 的 1 是「检查没过」不是命令错误。
- **安全**：隧道 SSH `StrictHostKeyChecking=accept-new`（TOFU，指纹变更拒连，`forget-key` 人工重信任）；访问密码继续本机 bcrypt（`$2y$12$…`）；`basic auth 必须配 HTTPS、只有 80 端口不算发布完成`；`limit_req` / fail2ban / 全局 nginx.conf 一律不碰（只出提示与强警告文案）。
- **生命周期**：崩溃自恢复（`RegisterApplicationRestart` + 10 分钟窗口第 3 次暂停 + 可关）；延迟建窗（`--hidden`/`--recovered` 只进托盘，关窗销毁窗口）；单实例升级为跨会话全局互斥量 `Global\Shangxian.App.v1`（+`app.lock` 回落）。
- **更新检查**：`check_update` / `app update-check`，GitHub Releases 数据源，启动后约 30 秒静默一次（≥6h 限频、`checkUpdateOnStart` 可关），只提示不自动下载/安装。

### 测试口径（本轮定稿）

- `cargo test -p shangxian-core`：**308 项通过**（lib 单测）。
- 集成测试：**43 项**（`env_checks` / `guardian_docker` / `keep_alive` / `proc_features` / `proc_probe` / `stop_foreign` 等；部分需靶机的按设计 `#[ignore]`）。
- `cargo test -p shangxian-cli`：**17 项通过**。
- `app` 前端：`npx tsc --noEmit` **0 错误**（`npm run build` 通过）。
- 说明：以上为本轮验收时的口径；本次文档同步（AI-AGENT / SKILL / DESIGN / PROGRESS）只改文档、不跑应用、不跑 git，数字沿用本轮测试记录。

### 文档同步记录（本次）

- `docs/AI-AGENT.md`：方法表补齐到实现里的 61 条；新增 `stats`/`doctor`/`diag`/`undo`/`config export|import`/`app stop-all|update-check`/`tunnel forget-key`/`--preset` 用法；铁律新增「basic auth 必须配 HTTPS」「云端配置由人粘贴、不装软件、不动全局 nginx.conf」「撤销窗口」「体检/诊断包只读」；配方 1/3/5 补验收（https 打开一次、`cli doctor --deep`、`tunnel check`）；配置章节升级为 v6 + `state.json` + `events.jsonl` + 原因码；附录换成 `cli guide` 原文。
- `skills/shangxian/SKILL.md`：同步铁律、命令表、验收与排查配方，保持自包含；改完原样拷贝到 `C:/Users/cgt/.pi/agent/skills/shangxian/SKILL.md`，`diff` 验证一致。
- `DESIGN.md`：HEADER 增加 v0.6 变更；修正 SQLite 等过时表述；新增 **§12 架构与数据模型**（模块清单、数据文件、安全模型、生命周期、明确不做）。
- `PROGRESS.md`：本节。

## 界面合并：「访问保护」并入「发布隧道」（2026-10-12）

按用户要求两页不再重复，拍板合并：删掉独立「访问保护」页，门禁跟着隧道卡片走。

- **导航**：本机服务 / 发布隧道 / 云端服务器 / 设置（4 项）。
- **卡片**：删掉原来两份重复的显示（保护徽标 + 云端检查条），改由 `ProtectionStatus` 一处承担：保护小牌 + 「检查云端」 + 漂移条 + HTTPS 提醒；小牌可点，直接开 `ProtectionModal` 改保护。
- **列表工具条**：新增「没设保护」「待核对」两个筛选（带条数），替代旧保护页顶部的「一览」卡片。
- **⋯ 菜单**：新增「访问记录」，点开弹窗查看（新组件 `AuditModal`；2026-10-12 晚由卡片内联小框改成弹窗）；读取逻辑从旧保护页搬来，行为不变。
- **保存流程不变**：`ProtectionModal` 保存成功 → 自动打开该隧道的「云端配置」。
- **发布向导第 3 步 / 克隆弹层保留**：新建时设一次；相关文案（向导成功屏、编辑隧道弹层提示）改指向「卡片上的保护小牌」。
- **文件**：删 `pages/Passwords.tsx`、`components/card/ProtectionBadge.tsx`、`components/card/CloudDriftBar.tsx`；新增 `components/AuditModal.tsx`；改 `App` / `Sidebar` / `Tunnels` / `TunnelCard` / `CardMenu` / `ProtectionStatus` / `ListToolbar` / `listPrefs` / `selectors`。
- **验证**：`npx tsc --noEmit` 0 错误；`npm run build` 通过。
- **文档**：DESIGN.md §4.2 / §9.6、docs/使用手册.md（3.2 / 3.4 / 3.7 弹层表 / 排查表）、README.md 文件树同步。
