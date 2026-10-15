# 上线助手 · AI Agent 文档（文件名：`AGENTS.md`）

**这是什么**：给 AI agent / 自动化脚本看的说明书。读完这份，你会知道怎么用 `cli.exe` 遥控「上线助手」桌面应用，把用户电脑上的服务发布到公网（SSH 隧道 + 云端 nginx + 访问保护）。

**解决什么问题**：不用猜命令、不用先读源码，就能查状态、建隧道、设访问保护、排查掉线、配免密、体检、备份配置；文中每一条命令和字段都能在源码或 `cli.exe` 的真实输出里核对到。

**什么时候用**：用户说「上线 / 发布到公网 / 隧道掉了 / 打不开 / 配免密 / 帮我体检一下 / 删错了想恢复」时读这份。只想了解这个软件给人怎么用，读 [使用指南.md](使用指南.md)。

**建议阅读顺序**：第 1 节「铁律」（安全红线，先读）→ 第 9 节「常用配方」（照着做就能完成整件事）→ 需要查参数 / 字段时再翻第 4 节「命令参考」和第 5 节「底层方法表」。

> 这份文档随安装包一起分发：安装目录里也有 `AGENTS.md`、`SKILL.md` 和 `使用指南.md`（给人看的使用指南，内容与仓库里的 `docs/使用指南.md` 一致）；两处不一致时，以较新的那份为准。
> 文中命令、字段的出处：`cli/src/main.rs`、`core/src/*.rs`（含 `state.rs` / `events.rs` / `doctor.rs` / `cloudcheck.rs` / `reason.rs` / `probe.rs` / `wake.rs` / `log.rs` / `logfile.rs` / `proc.rs`）、`app/src-tauri/src/ctl.rs`，或 `cli.exe` 的实际输出。
> 本版对应的内容清单（ctl 60 条方法 / 配置 v6 / 日期）见文末「文档版本」。

---

## 0. 一句话定位

**应用（托盘里的「上线助手」）是干活的；`cli.exe` 是遥控器。**

`cli.exe` 自己不建隧道、不管进程。它干的只有两件事：

1. 通过本机控制通道，把请求发给正在运行的应用（应用去执行、去看护）；
2. 应用没在跑时，只读的几个命令直接读配置文件，并明确告诉你「状态是 unknown」。

所以：**要改东西，先确认应用在跑。**

### 第一步：先找到 `cli.exe`（确认软件装没装）

下面的查找脚本以 Git Bash / bash 为例；能跑通就用找到的路径：

```bash
CLI=""
for c in "$(command -v cli 2>/dev/null)" \
         "$LOCALAPPDATA/Shangxian/cli.exe" \
         "$LOCALAPPDATA/Programs/Shangxian/cli.exe" \
         "$PROGRAMFILES/Shangxian/cli.exe" \
         "./target/release/cli.exe" \
         "./target/debug/cli.exe"; do
  if [ -f "$c" ]; then CLI="$c"; break; fi
done
echo "$CLI"
```

三种安装形态：

1. **安装版（推荐给用户）**：应用和 `cli.exe` 装在同一个文件夹。找不到时，让用户在开始菜单搜「上线助手」→ 右键 →「打开文件位置」，把那个文件夹里 `cli.exe` 的完整路径告诉你。
2. **便携版**：`cli.exe` 就在解压出来的文件夹里（和 `app.exe`、`data\` 放在一起）。让用户告诉你解压到了哪。
3. **源码运行**：如果你正待在源码仓库目录里，用 `./target/release/cli.exe`（或 `target/debug/cli.exe`）。

**找不到任何 `cli.exe`（或不确定用户装没装）就停下，直接问用户**：请他找到「上线助手」的安装位置，把 `cli.exe` 的完整路径发给你。**不要**猜路径、不要自己下载、不要改用手工 ssh 命令代替。

安装目录里同时带 `AGENTS.md`（本文档）、`SKILL.md`（给 agent 的技能文件）和 `使用指南.md`（给人看的使用指南），随安装包一起分发；各处内容不一致时，**以较新的那份为准**。

---

## 1. 铁律（先读，再动手）

| # | 规则 | 为什么 / 后果 |
|---|---|---|
| 0 | **缺信息先问用户，不要照抄示例、不要自己编**。每一项参数只有两个合法来源：用户明确说的，或命令输出里查到的。示例里的域名、端口、账号、密码、路径**全是占位**。缺以下任何一项就先停下、一次问清，拿到答复再动手：① 域名（`--url`）② 云端端口（`--remote-port`）③ 服务器地址 / 登录账号 / 配免密时要用的登录密码 ④ 访问用户名 / 访问密码 ⑤ 本机端口 ⑥ 服务的启动命令（程序 / 参数 / 工作目录）⑦ 这次要动的具体是哪条隧道 / 服务 / 服务器 | 自编参数会把用户的服务内容发到公网、占错端口、改乱配置，最后都要人收拾。问清楚是最省事的路 |
| 1 | **绝不暂停 / 重启 / 删除「用户正在用」的本机服务和它的隧道**——包括承载你与用户当前会话的通道，以及项目文档里标注「不许碰」的清单（本仓库长期环境里那条叫 `pi-web`，但重点是「谁在用」，不是名字） | 用户当前会话正走这条通道，动它 = 立刻断线。名字对不上也要按「可能正在用」处理；拿不准先问 |
| 2 | 删除本机服务 / 服务器前，先看有没有隧道引用 | 应用会拒绝并列出是哪几条。**不要绕过**（不要手改 config.json 硬删） |
| 3 | 访问密码只在「设置时」出现一次，只存加密结果（优先 bcrypt `$2y$…`；服务器 nginx 验不了时自动用兼容格式 `$apr1$…`）；加密在**本机**算好，服务器不用装任何东西；配免密的登录密码只在内存里用一次 | 密码不落盘、不进日志。不要把明文密码写进 config.json、日志、`cli call` 的参数里；传给命令行时优先走标准输入（`echo 密码 \| cli server hash-password ...`） |
| 4 | 云端只做「普通 sshd + nginx」，而且**只动站点配置** | 不替用户装任何软件（包括 fail2ban）、不改服务器 sshd 配置；**永不碰全局 `nginx.conf`**（要改 `http{}` 的东西，比如 `limit_req_zone`/`limit_req`，默认只出文案和强警告，不生成、不自动下发）。应用也永远不会自动写服务器上的 nginx 配置或自动 reload——片段一律由人粘贴 |
| 5 | 应用退出 = 所有隧道停止；本机服务保活（继续跑） | 隧道跟应用走，别人立刻访问不了。但应用启动的本地服务不会跟着退，下次打开应用会按端口认领。不要用「杀掉应用」当重启手段 |
| 6 | 隧道 `paused` = 别人访问不了 | 不确定要不要停，先问 |
| 7 | 端口写死、冲突报错，不自动漂移 | 改了云端端口必须重新贴 nginx 配置，否则 nginx 还指向老端口 |
| 8 | 只读命令可以随时跑；写命令（add/edit/start/stop/remove/setup-key）先想清楚再跑 | 写命令会立刻改用户的真实配置并落盘 |
| 9 | 删除 / 暂停类命令要确认：`tunnel stop` / `tunnel remove` / `service remove` / `server remove` / `tunnel forget-key` / `app stop-all` / `config import` | 交互终端会问 y/N；非交互（管道 / 脚本 / agent）**必须加 `--yes`**，不加直接拒绝、什么都不改（退出码 1）。**`--yes` 只是替人回答 y/N，不等于用户同意**：加 `--yes` 前必须先在对话里说清「要动哪个对象、会发生什么」，并拿到用户明确的同意（「可以 / 好 / 确认」这类），没表态就不做 |
| 10 | 输出和退出码要按约定判读 | 成功（含只读命令、`--json`）是 `{"ok":true,"result":...}`；失败是 `{"ok":false,"error":"..."}`。退出码：0 成功、1 出错、2 命令写法错、**3 = 这个命令需要应用在跑、但它没在跑**（不只是 `app status`；`app stop-all` 是唯一例外：没在跑也退 0）。`cli doctor --strict` 的 1 不是命令跑错，是「检查没过」 |
| 11 | **basic auth（访问密码）必须配 HTTPS**；只有 80 端口的站点**不算发布完成** | 在 `http://` 上输密码等于明文传密码。发布完成的验收 = 用 `https://<域名>/` 打开一次（看到业务页、401 密码框或 403 白名单拦截都算「线路通」），并且 `http://` 会跳 `https://`。`cli doctor --deep` 的「云端配置 / 公网访问 / 域名解析」三步和更全的 `cli call check_tunnel_cloud`（`tunnel check` 的升级版，多了 HTTPS/证书/端口/审计）都会检查；网址写 `https://` 但云端没开 443、或密码开着却能明文 `http://` 提交，都要让用户去面板打开证书 /「强制 HTTPS」 |
| 12 | 撤销窗口只有 10 分钟（最多 20 条） | `tunnel remove` / `service remove` / `server remove` 成功后，人话输出会提示「10 分钟内可以撤销：cli undo last」。已删的对象各存一条墓碑（`state.json` 的 `trash`）：`cli undo`（最近一次）/ `cli undo list` / `cli undo und-xxxxxxxx`。**隧道撤销后一律是暂停状态**，想继续发布要再 `cli tunnel start`；服务只恢复登记信息、不启动进程；同 id 已存在或依赖不在（隧道引用的服务/服务器被删了）会拒绝并说明 |
| 13 | 体检 / 诊断包都是只读的，诊断包已脱敏 | `cli doctor` / `cli diag` 不改任何配置；体检报告落 `<数据目录>\logs\doctor-last.json`。诊断包不含密码哈希、密钥内容、ctl 通行证，但日志里可能有域名、路径、本机用户名——发给帮忙排查的人没问题，**不要公开发布到网上** |

---

## 2. 三十秒上手

```bash
# 开发版可执行文件（本仓库）
./target/debug/cli.exe app status --json     # 应用在不在（没在跑退出码 3）
./target/debug/cli.exe status --json         # 一次看全：应用+服务+隧道+服务器
./target/debug/cli.exe doctor                # 一键体检（不连服务器；--deep 才连）
./target/debug/cli.exe guide                 # 内置速查（人话）
./target/debug/cli.exe methods --json        # 全部底层方法（60 个）
```

拿到一个陌生环境，先跑 `status --json` + `doctor`，再决定下一步。

---

## 3. 运行环境与输出约定

| 项目 | 约定 |
|---|---|
| cli 位置 | 开发版：`target\debug\cli.exe`（或 release）；分发给用户时，`cli.exe` 要和 `app.exe` 放同一个文件夹（`cli app start` 靠这个找人） |
| 名字 / 版本 | 程序名 `cli`，版本 `0.6.3`（`cli --version` 输出 `cli 0.6.3`） |
| 写操作前提 | 应用必须在跑（`cli app status --json` 看到 `result.running: true`） |
| 只读四个命令 | `status` / `services` / `tunnels` / `servers` 离线也能读配置文件，状态字段为 `"unknown"` |
| 默认输出 | 中文人话，成功以 `✓` 开头，失败以 `✗` 开头（写到 stderr） |
| `--json` | 全局参数，位置随意。agent 一律加。成功 `{"ok": true, "result": ...}`，失败 `{"ok": false, "error": "人话原因"}`，失败退出码 1 |
| 危险操作 | `tunnel stop/remove`、`service remove`、`server remove`、`tunnel forget-key`、`app stop-all`、`config import`：交互终端打印要动谁再问 y/N（回车 = 什么都不做，退出码 0、`result.cancelled = true`）；非交互**必须加 `--yes`**，不加就拒绝执行（退出码 1）；`config import --dry-run` 不写盘、不用确认 |
| 退出码 | `0` 成功（确认里选「不」也是 0）；`1` 出错（包括非交互下拒绝执行、undo 令牌过期、`doctor --strict` 有没过）；`2` 命令写法不对（拼错参数、少值）；`3` **这个命令需要应用在跑、但应用没在跑**（`stats` / `diag` / `logs` / 真撤销 / `config import` / `app update-check` / 写命令等；`app stop-all` 没在跑反而退 0） |
| 编码 | UTF-8；中文表格按显示宽度对齐 |
| 通信 | 只在本机回环（127.0.0.1）上说话，端口随机，写进 `ctl.json`，带 token |

---

## 4. 命令参考

所有命令总览（`cli --help`）：

```
status    概览：应用 + 本机服务 + 发布隧道 + 云端服务器
services  本机服务列表
tunnels   发布隧道列表
servers   云端服务器列表
ports     本机正在监听的端口（带进程名和程序路径）
doctor    体检：应用、文件夹、SSH、本机端口、服务器、隧道、云端、公网、域名
diag      打一个脱敏诊断包（发给帮你排查的人）
stats     运行统计：应用运行多久、隧道重连情况、今日重连次数
logs      读日志：事件（应用里发生的事）/ 应用日志 / 隧道日志 / 本机服务日志
undo      撤销删除（删除后 10 分钟内有效；找不到就退出码 1）
nginx     生成 nginx 配置片段（人工贴到面板上）
service   本机服务：新增/编辑/启停/删除
tunnel    发布隧道：新增/编辑/启停/删除/核对云端保护
server    云端服务器：新增/编辑/删除/测试连接/配免密
app       应用自己：启动、看状态、停全部服务、查更新
config    配置备份：导出 / 导入
methods   列出所有可调用的底层方法（给 agent 看）
call      直接调用底层方法（万能口）：cli call <方法> '{"参数":...}'
guide     给 AI agent 的速查说明
```

### 4.1 总览类命令

#### `cli status`

| 项 | 说明 |
|---|---|
| 作用 | 一次拿全：应用信息 + 本机服务 + 隧道 + 服务器 |
| 参数 | 无 |
| 前提 | 应用在跑 = 实时状态；应用没跑 = 读配置文件，状态全 `unknown` |

人话输出（应用在跑时，真实格式）：

```
应用：在跑（进程号 1283452，配置文件夹 C:\Users\你\AppData\Roaming\Shangxian）

本机服务 3 台：
  [在跑        ] pi-web（svc-8419a9）       本机端口 30141    只监控，不帮你启动，已发布到 1 条隧道

发布隧道 3 条：
  [已暂停    ] pi-web（pi-web）           pi-web           → 香港-1       · 云端 13041  · 谁都能进
        网址 https://piweb.example.com
  [正在发布  ] 靶机演示（demo-target）    靶机演示         → 本地靶机     · 云端 19103  · 谁都能进

云端服务器 2 台：
  香港-1（srv-api）    user@api.example.com:22 · 默认密钥
```

应用没跑时：

```
应用：没在跑（下面是配置文件里的内容，状态看不出来；cli app start 可以叫起来）

本机服务 1 台：
  [不知道（应用没在跑）] 示例网站（svc-demo）       本机端口 8188     启动程序 C:/Python311/python.exe，已发布到 1 条隧道
...
```

`--json`（应用在跑；最外层是 `{"ok":true,"result":...}` 信封，下面只展开 `result` 里的内容；本版快照 `snapshotVersion` = 2）：

```json
{
  "app": {
    "snapshotVersion": 2,
    "pid": 12345,
    "appVersion": "0.6.3",
    "dataDir": "C:\\Users\\你\\AppData\\Roaming\\Shangxian",
    "startedAt": 1760000000000,
    "jobGuardOk": true,
    "jobGuardError": null,
    "configTrusted": true,
    "updateInfo": { "status": "upToDate", "latest": "0.6.3", "url": null, "checkedAt": 1760000000000, "error": null },
    "settings": {
      "refreshIntervalMs": 800,
      "quietHours": { "enabled": false, "start": "22:00", "end": "08:00" },
      "checkUpdateOnStart": true
    },
    "notifyEnabled": true,
    "autostart": false
  },
  "servers": [
    { "host": "api.example.com", "id": "srv-abc123", "keyPath": null,
      "name": "香港-1", "sshPort": 22, "user": "user", "vhostDir": null }
  ],
  "services": [
    {
      "autostart": true, "id": "svc-abc123",
      "launchArgs": "main.py", "launchExe": "C:\\Python311\\python.exe",
      "launchWorkdir": "D:\\web", "localPort": 8188,
      "logPath": "C:\\Users\\你\\AppData\\Roaming\\Shangxian\\logs\\service-svc-abc123.log",
      "name": "comfyui", "note": null, "reason": null,
      "status": "running", "health": "up", "logOverflowed": false,
      "lastFailure": null,
      "tunnelCount": 1, "tunnelNames": ["comfyui 发布"], "watch": true
    }
  ],
  "tunnels": [
    {
      "access": ["password"], "accessDetail": null, "accessUser": "me",
      "enabled": true, "hasPassword": true, "id": "tun-abc123", "keyPath": null,
      "localPort": 8188, "name": "comfyui 发布",
      "reason": null, "reasonCode": null,
      "retryState": "active", "attempt": 0, "retryInSecs": null,
      "connectedSince": 1760000000000, "uptimeSecs": 42, "reconnectsToday": 0,
      "lastFailure": null,
      "protectionCheck": { "status": "unknown", "ts": null, "source": null,
        "cloud": null, "expected": { "password": true, "ip": false, "allowIps": [], "url": "comfy.example.com" },
        "driftFields": [], "error": null },
      "remotePort": 13000, "serverId": "srv-abc123",
      "serviceId": "svc-abc123", "serviceName": "comfyui",
      "serviceOk": true, "serviceStatus": "running", "status": "running",
      "url": "comfy.example.com"
    }
  ]
}
```

> 字段差异（重要）：
> - 应用在跑时，`app` 没有 `running` 字段（用 `pid` 是否为 null 判断），但多 `snapshotVersion`/`startedAt`/`jobGuardOk`/`jobGuardError`/`configTrusted`/`updateInfo`/`settings`；`services` 多 `health`（`up`/`down`/`unknown`）、`reason`、`logPath`、`logOverflowed`、`lastFailure`；`tunnels` 多 `reasonCode`、`retryState`/`attempt`/`retryInSecs`/`connectedSince`/`uptimeSecs`/`reconnectsToday`、`lastFailure`、`protectionCheck`、`serviceOk`/`serviceStatus`、`keyPath`，**没有** `serverName`。
> - 应用没跑时（`cli services/tunnels/servers` 的离线读文件路径），`app` 只有 `{pid:null, appVersion:null, dataDir, running:false}`；服务/隧道状态全为 `"unknown"`；隧道多 `serverName`，少 `health`/`reason`/`protectionCheck` 等运行字段；服务器字段不变。
> - `protectionCheck.status`：`"unknown"`（未核对）/ `"ok"` / `"drift"`（和设置对不上，看 `driftFields`）/ `"error"`（检查本身没跑成，看 `error`）；`driftFields` 取值集合：`conf` / `password` / `ip` / `allowIps` / `htpasswd`。

#### `cli services` / `cli tunnels` / `cli servers`

| 命令 | 作用 | 人话输出示例 |
|---|---|---|
| `cli services` | 本机服务列表 | `本机服务 3 台：` + 每台一行 |
| `cli tunnels` | 发布隧道列表 | `发布隧道 3 条：` + 每条一行（名字 → 服务 → 服务器 · 云端端口 · 访问方式；有网址/原因会各占一行） |
| `cli servers` | 云端服务器列表 | `云端服务器 2 台：` + `名字（id） user@host:port · 密钥 xxx / 默认密钥` |

`--json` 的 `result` 是 `status` 里对应数组的内容（服务数组 / 隧道数组 / 服务器数组），字段一致；外层同样是 `{"ok":true,"result":[...]}` 信封。

#### `cli ports`

| 项 | 说明 |
|---|---|
| 作用 | 本机正在监听的 TCP 端口（含进程名和程序路径） |
| 参数 | 无 |
| 来源 | 系统 `netstat -ano -p tcp`；只收 `LISTENING`、端口 ≥ 1024、地址为本机/全网的条目；按端口号升序 |
| try 前提 | 需要应用在跑（这是个底层方法调用） |

人话输出（节选）：

```
本机正在监听的端口 61 个：
  2222   com.docker.backend.exe       C:\Program Files\Docker\Docker\resources\com.docker.backend.exe
  8188   python.exe                   D:\AI\ComfyUI-aki-v1.7\python\python.exe
```

`--json`（外层信封 `{"ok":true,"result":[...]}`，下面只列 `result`）：

```json
[
  { "port": 8188, "pid": 12345, "process": "python.exe",
    "exe": "D:\\AI\\ComfyUI-aki-v1.7\\python\\python.exe" }
]
```

> `pid` / `process` / `exe` 都可能为 `null`（进程已退出或读不到）。

#### `cli logs [id]`

| 项 | 说明 |
|---|---|
| 作用 | 读日志，支持四种来源 + 轮转备份 |
| 参数 | `[id]`：隧道 / 服务 id（可省略 = 看全部来源）；`--source all\|tunnel\|service\|app`；`--kind tunnel`（默认）/ `service`（旧写法兼容，只有没写 `--source` 时才看它）；`--lines N`（**默认 200**，1~5000）；`--archive 0\|1\|2`（0 = 只看当前文件，1/2 = 连上一份 / 上两份轮转备份）；`--format lines\|json` |
| 默认行为 | 不写 id/source：`--kind` 默认 `tunnel`，但没 id 时实际退成 `source=all`（事件 + 应用日志 + 所有隧道 / 服务日志，每来源各取 lines 条，总量封顶 2000，超了 `truncated=true`） |
| 排错 | `--source tunnel/service` 必须带 id；`--source` 只认 all/tunnel/service/app；`--archive` 只能是 0/1/2；`--format` 只认 lines/json——写错都是退出码 1 |
| 日志文件 | 事件流 `events.jsonl`（超 2MB 轮转 `.1`/`.2`）；应用日志 `logs\app.log`（超 2MB 轮转，`app.log.1`/`app.log.2`）；隧道日志 `logs\<隧道id>-<8位代码>.ssh.log`（改名不换文件；超 5MB 轮转，留 `.1`/`.2`）；服务日志 `logs\service-<服务id>.log`（同样 5MB×2）。按名字命名的旧日志不再被读取 |
| 边界 | id 不存在时直接报错退出（退出码 1），不会输出空内容；应用没在跑也不可用（这是 ctl 方法，退出码 3） |

人话输出（默认 `--format lines`）：直接打印日志原文（events.jsonl 会按 `[时间] 级别 主体 人话` 排成人看的行）。

```
[连接] 2026-10-12 10:00:00 隧道「comfyui 发布」已连上云端
[访问] 2026-10-12 10:01:00 有 1 个 IP 被密码拦下（401）
```

```json
{ "ok": true, "result": { "text": "[连接] 2026-10-12 10:00:00 隧道「comfyui 发布」已连上云端\n..." } }
```

`--format json` 时每行一个 JSON（**JSONL，无信封**，方便管道）；`logs_query` 的完整 `result` 形状（加了 `--json` 才看到）：

```json
{
  "source": "all", "archive": 0, "lines": 200, "truncated": false,
  "sources": ["events", "app", "tunnel:tun-abc123", "service:svc-abc123"],
  "entries": [
    { "ts": 1760000000000, "source": "app", "origin": "events", "level": "info",
      "code": "tunnelConnected", "subject": { "type": "tunnel", "id": "tun-abc123", "name": "comfyui 发布" },
      "message": "已连上云端", "raw": "[10:00:00] [发布:comfyui 发布] 已连上云端" },
    { "ts": null, "source": "tunnel", "origin": "tunnel", "level": "debug", "code": null,
      "subject": null, "message": null, "raw": "debug1: remote forward success for: ..." }
  ],
  "text": "（source=all 时也会给一份拼好的文本）"
}
```

> 界面里看日志是最近 300 行；CLI `logs` 默认 200 行；底层方法 `get_logs` / `get_service_logs` 默认 200。更老的 `--kind` 写法仍然能用，但 web 页/管道推荐 `--source`。

#### `cli stats`

| 项 | 说明 |
|---|---|
| 作用 | 运行统计：应用运行多久、服务/隧道计数、今日重连、重试倒计时、更新状态 |
| 参数 | 无 |
| 前提 | 应用在跑（离线退出码 3） |

人话输出（真实格式）：

```
「上线助手」0.6.3（进程 12345）已运行 2 小时 13 分钟
本机服务：3 台，2 台在跑，1 台没在跑，0 台不知道
发布隧道：3 条（2 条开着），2 条正在发布，1 条正在重试，今日重连 4 次
  ·「comfyui 发布」第 3 次没连上，12 秒后重试
更新：已经是最新版
```

`--json`（外层信封 `{"ok":true,"result":...}`，下面只列 `result`，`schemaVersion` = 1）：

```json
{
  "schemaVersion": 1,
  "generatedAt": 1760000000000,
  "app": { "pid": 12345, "appVersion": "0.6.3", "startedAt": 1760000000000,
           "uptimeSecs": 7980, "dayKey": "2026-10-12", "jobGuardOk": true, "eventsSinceStart": 128 },
  "tunnels": { "total": 3, "enabled": 2, "running": 2, "retrying": 1, "reconnectsToday": 4,
    "items": [ { "id": "tun-abc123", "name": "comfyui 发布", "status": "disconnected",
                 "retryState": "active", "attempt": 3, "retryInSecs": 12,
                 "connectedSince": null, "uptimeSecs": 0, "reconnectsToday": 2 } ] },
  "services": { "total": 3, "up": 2, "down": 1, "unknown": 0,
    "items": [ { "id": "svc-abc123", "name": "comfyui", "status": "running", "health": "up" } ] },
  "update": { "status": "upToDate", "latest": "0.6.3", "url": null, "checkedAt": 1760000000000, "error": null }
}
```

#### `cli nginx <id>`

| 项 | 说明 |
|---|---|
| 作用 | 生成某条隧道的 nginx 配置片段（人工贴到云端面板） |
| 参数 | `<id>`：隧道 id |
| 前提 | 应用在跑（需要读隧道 + 服务器配置） |

人话输出 = 要新增的几行 + 简短示意 +（有密码时）密码文件内容 + 步骤（每条步骤以 `· ` 开头）。
只给新增内容，不生成整个站点配置——每个人的原配置不同，整段替换有覆盖风险。

真实例子（有访问密码；示意里灰色＝原配置的样子、高亮＝新增内容的位置）：

```
这几行加进站点配置里（不要整段替换）：
    # 访问密码（由「上线助手」新增）
    auth_basic "需要密码";
    auth_basic_user_file /xp/panel/vhost/demo.example.com.htpasswd;

证书自动续期用的一行（放进 location /.well-known/ 里）：
auth_basic off;
```

（界面里的简短示意，`...` 表示你原有配置的其它部分：）

```
server {
    ...
    server_name demo.example.com;
    # ① 新增：访问密码（放在 server_name 下面）
    auth_basic "需要密码";
    auth_basic_user_file /xp/panel/vhost/demo.example.com.htpasswd;
    ...
    # SSL证书验证目录
    location /.well-known/ {
        # ② 新增：证书续期用（放进这个目录里）
        auth_basic off;
        allow all;
    }
    ...
}

· 站点和证书先在面板建好；「反向代理」目标填 http://127.0.0.1:19103。（配过就跳过）
· 把下面高亮的行加进「站点 → 配置修改」：只加这几行，不替换整段；保存后重载 nginx，再从外网打开确认。
· 密码文件内容存到服务器上的 <站点目录>/demo.example.com.htpasswd（没有就新建）。
· 密码文件要存到 <站点目录>/ 下面；面板不是小皮的话，回应用里把服务器的「站点目录」改成你面板的目录（比如宝塔填 /www/server/panel/vhost），再重新生成；临时也可以先设环境变量 SHANGXIAN_HTPASSWD_DIR 再重新生成。
· 证书续期那两行（auth_basic off;）别漏——漏了证书到期不能自动续。
```

`--json`：

```json
{
  "snippet": "    # 访问密码（由「上线助手」新增）\n    auth_basic \"需要密码\";\n    auth_basic_user_file /xp/panel/vhost/demo.example.com.htpasswd;",
  "acmeExempt": "auth_basic off;",
  "preview": [
    { "text": "server {", "isNew": false },
    { "text": "    ...", "isNew": false },
    { "text": "    auth_basic \"需要密码\";", "isNew": true }
  ],
  "htpasswd": "shangxian:$2y$12$...",
  "steps": ["站点和证书先在面板建好；「反向代理」目标填 http://127.0.0.1:19103...", "..."]
}
```

> `snippet` 是「要新增的几行」（复制按钮复制的就是它）；`preview` 是简短示意（`isNew = false` 灰显＝原配置的样子，`true` 高亮＝新增内容的位置，`...` 是占位）；`acmeExempt` 在密码模式出现，是放进 `/.well-known/` 里豁免密码的一行（证书自动续期要用）。访问保护不同时：密码 = `auth_basic` 两行 + 豁免一行 + `htpasswd`（`用户名:哈希`）；IP 白名单 = `allow <ip>;` × N + `deny all;`；**两个同时开** = 密码块和 allow/deny 块一起生成，两个条件都要满足；没保护 = `snippet` 为 null，示意里给一行提醒注释。密码开着时（哪怕白名单也开着），`location /.well-known/` 里要放 `auth_basic off;`，证书续期才不会被密码拦住；只开白名单时没有这行（那里本来就没有密码）。
>
> 密码文件目录按服务器的「站点目录」解析：**服务器字段 `vhostDir` → 环境变量 `SHANGXIAN_HTPASSWD_DIR` → 默认 `/xp/panel/vhost`**；站点配置找 `<目录>/nginx/<域名>.conf`，密码文件放 `<目录>/<域名>.htpasswd`。服务器不是小皮面板时，在应用里把「站点目录」改对，或者用 `cli server add/edit --vhost-dir <目录>` 指定（edit 不写 = 保留原来的站点目录，写 `--vhost-dir ""` = 清空），再重新生成。

### 4.2 本机服务：`cli service ...`

#### `cli service add`

```
cli service add --name <名字> --port <本机端口> [--exe <程序>] [--args <参数>]
                [--workdir <目录>] [--note <备注>] [--watch] [--autostart]
```

| 参数 | 必填 | 说明 |
|---|---|---|
| `--name` | 是 | 服务名字 |
| `--port` | 是 | 本机端口（1~65535） |
| `--exe` | 否 | 启动程序；不填 = 只监控，不帮你启动 |
| `--args` | 否 | 启动参数，默认 `""` |
| `--workdir` | 否 | 工作目录 |
| `--note` | 否 | 备注 |
| `--watch` | 否 | **开关，不带值**：勾上 = 掉了自动拉起 |
| `--autostart` | 否 | **开关，不带值**：勾上 = 开机自动启动 |

没有 `--id` 参数，固定当「新增」处理。本机端口全机唯一（重复会报 `✗ 本机端口 30141 已经被另一台服务用了`）。

人话输出：

```
✓ 已登记本机服务「comfyui」（id=svc-1a2b3c）
```

`--json`：

```json
{ "ok": true, "result": { "id": "svc-1a2b3c" } }
```

#### `cli service edit`

```
cli service edit <服务id> [--name X] [--port N] [--exe X] [--args X] [--workdir X]
                [--note X] [--watch true|false] [--autostart true|false]
```

- 只写要改的字段，其余保持不变（参数有 `--id`，当编辑处理）。
- `--watch` / `--autostart` 两种写法都行：不带值 = 开（`--watch`）、`--watch false` = 关、不传 = 不动。

人话输出：`✓ 已改好本机服务「svc-1a2b3c」`
`--json`：`{ "ok": true, "result": { "id": "svc-1a2b3c" } }`

#### `cli service start / stop / restart / remove`

| 命令 | 作用 | 人话输出 | 说明 |
|---|---|---|---|
| `service start <id>` | 用登记的启动方式拉起服务 | `✓ 已让「svc-1a2b3c」启动（几秒后 cli services 看结果）` | 没配启动方式会报 `✗ 这台服务没有配置启动方式（只能看状态，不能帮你启动）` |
| `service stop <id>` | 停止。**默认只停应用自己启动的进程**；端口上不是你启动的进程时，先返回 `needsConfirm`，不动手 | `✓ 已停止「svc-1a2b3c」`；加 `--force` 真的停掉外部进程后是 `✓ 已停止「svc-1a2b3c」（外部启动的进程）` | 需要确认时会报错（退出码 1）：`✗ 这台服务不是你启动的，正在监听的是 C:\Python311\python.exe（PID 4108）。停止会真的把它关掉。确认要停就加 --force：cli service stop svc-1a2b3c --force`。**只监控（没配启动方式）的服务绝不接管外部进程**：不管加不加 `--force` 都直接人话报错（不会先给 `needsConfirm`，CLI 也不会提示 `--force`）：`✗ 这台服务没配启动方式，是你自己开的程序，我不会去动它（端口上现在是 PID 4108「…」）；想接管它，先在编辑界面点端口把启动方式补全，再来停止`；端口上本来没进程时直接返回 `stopped`。应用重启后上次留下、这次被按端口认领的进程也算「不是这次启动的」，同样先返回 `needsConfirm`，要用户点头 |
| `service restart <id>` | 先停再启 | `✓ 已重启「svc-1a2b3c」` | 端口上不是应用启动的进程时会报错，提示先 `service stop <id> --force` 再 `service start <id>`；只监控的服务直接报错（先补启动方式）。底层 ctl 方法 `service_restart` 这时返回的是 `{"kind":"needsConfirm",...}`（不是错误），CLI 包装成报错退出 |
| `service remove <id>` | 删除（要确认） | `✓ 已删除本机服务「名字」（svc-1a2b3c）` | 有隧道引用会拒绝：`✗ 还有 1 条隧道在用它（「靶机演示」），先把这些隧道删掉`。交互终端会先问 y/N（回车 = 什么都不做，退出码 0、`result.cancelled = true`）；非交互必须 `service remove <id> --yes`，不加就拒绝执行（退出码 1） |

`--json`：

- `service start / restart / remove`：`{ "ok": true, "result": { "ok": true } }`
- `service stop`：`{ "ok": true, "result": { "kind": "stopped" } }`（停外部进程成功时 `kind` = `"foreignStopped"`；要确认时命令按失败处理，输出 `{ "ok": false, "error": "..." }`、退出码 1）
- 交互确认里选「不」：`{ "ok": true, "result": { "cancelled": true } }`，退出码 0

**停止的校验规则**（`service stop --force` 真的结束外部进程前，core 会再核对一遍）：

- 服务配的是**完整路径**：拿端口上进程的当前路径跟配置路径比，一致才会结束；不一致一律拒绝（防误杀同名程序，报错会带上双方路径）。
- 服务只配了**程序名**（不含 `\` 或 `/`）：按文件名**忽略大小写**比对。这是放宽规则——机器上任何同名程序都可能被结束，配置时尽量写完整路径。
- 只监控（没配启动方式）的服务：任何情况都不接管端口上的进程，直接报错，不会走到确认流程。

### 4.3 发布隧道：`cli tunnel ...`

#### `cli tunnel add`

```
cli tunnel add --service <服务id> --server <服务器id> --remote-port <云端端口>
               [--name <隧道名字>] [--url <网址>]
               [--access none|password|ip]... [--access-user <用户名>]
               [--access-password <密码>] [--access-ips "1.2.3.4,5.6.7.8"]
```

| 参数 | 必填 | 说明 |
|---|---|---|
| `--service` | 是 | 本机服务 id（不是名字） |
| `--server` | 是 | 云端服务器 id（不是名字） |
| `--remote-port` | 是 | 云端端口（写死，不自动漂）。**值来自用户的云端规划，不要自己挑**：同一台服务器上同一个端口只能给一条隧道（重复会报「云端端口 xxx 已被这个服务器上的其他隧道占用」）；要避开 80 / 443（网页端口，nginx 在用）；用户没给或不确定 → 先问 |
| `--name` | 否 | 不填 = `<服务名> 发布` |
| `--url` | 否 | 公网要用的域名：**只要对外发布就该填**（只填纯域名，不要 `http://`、不要端口和路径）。留空、或写得不合规范时，nginx 片段会用「你的域名」占位并给警告——后续核对、检查云端都没有意义。用户没给域名 → 先问 |
| `--access` | 否 | `none`（默认）/ `password` / `ip`。**可重复，也支持逗号分隔**：`--access password --access ip` 或 `--access password,ip` = 两个条件都要满足（nginx 默认 satisfy all）。**默认不传 = none = 谁都能进，建隧道时务必显式传值**。写别的值（比如 `passwrod`）会**直接报错**，不会静默变成无保护 |
| `--access-user` | 否 | 访问用户名（选了 `password` 用）。不填时生成的 htpasswd 默认用户名是 `shangxian`——**不要默认**：应显式问用户想用什么用户名，并通过 `--access-user` 传进去 |
| `--access-password` | 否 | 访问密码。加密（优先 bcrypt `$2y$…`；服务器 nginx 验不了时自动用兼容格式 `$apr1$…`）在**本机**算好再写进配置，密码本身不落盘、不碰到服务器（不要服务器装任何东西）。但会短暂出现在本机命令行；介意就用 `hash-password` 拿哈希再 `cli call add_tunnel` |
| `--access-ips` | 否 | IP 白名单，逗号分隔（选了 `ip` 用） |

行为：新增后 `enabled: true`，应用立刻开始发布。

> 建隧道前用铁律 0 逐项检查参数来源：`--service` / `--server` 必须用 `cli services --json` / `cli servers --json` 查出来的真实 id（不是名字）；`--url`、`--remote-port`、`--access-user`、`--access-password` 必须来自用户。

人话输出：

```
✓ 已建好隧道「comfyui 发布」并开始发布（id=tun-4d5e6f）
  访问保护：密码，已生效
```
（没保护时这句是「访问保护：无（谁都能进）。想加门：--access password --access-password <密码> 或 --access ip --access-ips "1.2.3.4"」）

`--json`：

```json
{ "ok": true, "result": { "id": "tun-4d5e6f" } }
```

失败例子（云端端口冲突）：

```
✗ 云端端口 19103 已被这个服务器上的其他隧道占用
```

#### `cli tunnel edit`

```
cli tunnel edit <隧道id> [--name X] [--remote-port N] [--url X]
                [--access none|password|ip]... [--access-user X]
                [--access-password X] [--access-ips X]
```

- 只写要改的字段。**不能换本机服务、不能换服务器**（参数里没有 service/server）。
- `--access` 同样可重复/逗号分隔，写什么就是什么（整体替换）：不传 `--access` = 保持原来的保护不变；`--access none` = 改成无保护。
- `--access-password` 不填 = 密码保持不变。选了 `password` 但从来没有密码哈希，会报：`✗ 这条隧道选了「要密码才能进」，请先设置访问密码`。
- `--access-password` 的加密也是本机算（优先 bcrypt，服务器 nginx 不支持时自动用兼容格式），服务器不用装东西；`--access-ips` 必须配 `--access ip`、`--access-password` 必须配 `--access password`，配不上会直接报错（不会静默丢掉）。
- 改了云端端口：应用会停掉旧隧道，按新端口重连。**记得重新贴 nginx 配置。**
- 改了服务器地址 / SSH 端口 / 账号 / 密钥，或改了本机服务的端口：应用会自动停掉旧隧道、按新参数重建（不用手删重建）。

人话输出：`✓ 已改好隧道「comfyui 发布」（tun-4d5e6f）`，下一行还会回显最终保护状态（会断一下再自动连上）
`--json`：`{ "ok": true, "result": { "ok": true } }`

#### `cli tunnel check <隧道id>`

| 项 | 说明 |
|---|---|
| 作用 | SSH 到云端，核对**实际生效**的访问保护（和界面「检查云端」同一套判定；有人工漏贴过云端配置时，这里看得到） |
| 读什么 | `<站点目录>/nginx/<域名>.conf`（站点配置）和 `<站点目录>/<域名>.htpasswd`（密码文件）；站点目录按服务器的 `vhostDir` 解析 |
| 参数 | `<id>`：隧道 id |
| 前提 | 应用在跑；能 SSH 上服务器 |
| 耗时 | 和 `check_tunnel_cloud` 的 `full` 模式同一条流水线，SSH 最长约 **30 秒**（脚本内部各步还有更短的封顶） |
| 只读 | 只读云端文件，不改任何东西；结果会存进运行记录（`state.json` 的 `tunnels[id].protectionCheck`，`source` = `"manual"`） |

> 想要更全的检查（HTTPS/证书/端口回环/访问审计/环境识别），用底层方法：`cli call check_tunnel_cloud '{"id":"<隧道id>","mode":"full"}' --json`。`mode` 取 `full`（默认）/ `drift` / `audit` / `env`；HTTPS、证书天数、80 跳转、端口是不是只绑回环、最近 1000 行的 401/403 统计都在返回里（详见第 5 节）。`cli tunnel check` 是它的兼容版：**执行和 `full` 同一条 `run_cloud_check`**（同一份云端输出、同一套漂移判定），只是把返回裁剪成老的 `AccessCheck` 字段（只看访问保护，字段固定）。

人话输出：逐项列出云端实际状态（密码开/关、白名单开/关及 IP 清单、密码文件在不在、站点配置找没找到）。CLI 只报云端实情，不会替你对比；要和设置核对，自己拿 `cli tunnels --json` 里的 `access` / `accessDetail` 对（应用界面里由应用直接给出不一致警告）。

`--json`：

```json
{ "confFound": true, "passwordOn": true, "ipOn": false, "allowIps": [], "htpasswdExists": true }
```

> 字段含义：`confFound` = 云端站点配置找到了；`passwordOn` = 站点顶层有 `auth_basic` 且值不是 `off`（要密码）；`ipOn` = 站点顶层有 `deny all;`（IP 白名单开着）；`allowIps` = 实际读到的白名单 IP；`htpasswdExists` = 密码文件在。**读的是云端实际状态，不是应用里的设置**——不一致就按「重新贴云端配置」处理。

#### `cli tunnel start / stop / remove`

| 命令 | 作用 | 人话输出 |
|---|---|---|
| `tunnel start <id>` | 开始发布 | `✓ 已开始发布「tun-4d5e6f」` |
| `tunnel stop <id>` | 暂停发布（别人立刻访问不了；**要确认**） | `✓ 已暂停「comfyui 发布」（tun-4d5e6f）`；非交互必须加 `--yes`，否则拒绝（退出码 1） |
| `tunnel remove <id>` | 删除隧道（配置一起删；**要确认**） | `✓ 已删除隧道「comfyui 发布」（tun-4d5e6f）`；非交互必须加 `--yes` |

三个命令的 `--json` 都是：`{ "ok": true, "result": { "ok": true } }`（暂停/删除在交互终端里选「不」时是 `{ "ok": true, "result": { "cancelled": true } }`、退出码 0）。

### 4.4 云端服务器：`cli server ...`

#### `cli server add`

```
cli server add --name <名字> --host <地址> --user <登录账号>
               [--port <SSH端口>] [--key <密钥文件>]
```

| 参数 | 必填 | 说明 |
|---|---|---|
| `--name` | 是 | 名字 |
| `--host` | 是 | IP 或域名 |
| `--user` | 是 | SSH 用户名 |
| `--port` | 否 | SSH 端口，默认 22 |
| `--key` | 否 | 密钥文件；不填 = 用系统默认密钥（`~/.ssh`） |

人话输出：

```
✓ 已添加服务器「香港-1」（id=srv-7a8b9c）
  没配密钥的话：cli server setup-key srv-7a8b9c
```

`--json`：`{ "ok": true, "result": { "id": "srv-7a8b9c" } }`

#### `cli server edit`

```
cli server edit <服务器id> [--name X] [--host X] [--user X] [--port N] [--key X]
```

人话输出：`✓ 已改好服务器「srv-7a8b9c」`
`--json`：`{ "ok": true, "result": { "id": "srv-7a8b9c" } }`

#### `cli server remove`

人话输出：`✓ 已删除服务器「名字」（srv-7a8b9c）`
有隧道引用会拒绝：`✗ 还有隧道挂在这台服务器上，先处理它们`。**要确认**：交互终端问 y/N，非交互必须 `server remove <id> --yes`。
`--json`：`{ "ok": true, "result": { "ok": true } }`

#### `cli server test <服务器id>`

先敲门再进屋：真连一次 SSH，确认这台服务器现在到底能不能连上。

| 项 | 说明 |
|---|---|
| 作用 | 真连一次这台服务器的 SSH（在服务器上跑一句 `echo` 看能不能回话） |
| 参数 | `<id>`：服务器 id |
| 前提 | 应用在跑（没在跑退出码 **3**） |
| 耗时 | 最长 **15 秒**；连不上也会在 15 秒内返回，不会一直等 |
| 只读 | 只跑一句 `echo`，不改服务器上任何东西 |

人话输出（成功）：

```
连接成功，用时 0.8 秒
```

`--json`：`{ "ok": true, "result": { "message": "连接成功，用时 0.8 秒" } }`（成功看 `result.message`，里面是用时）。

失败时是 `{"ok":false,"error":"..."}`、退出码 1，人话形如：`✗ 无法连接 api.example.com（端口 22）：登录被服务器拒绝了：账号或密钥不正确，也可能服务器已禁用这种登录方式。原始报错：Permission denied (publickey).`——原因翻译和隧道掉线同一套，照着人话处理，不要重试硬刷。

> 底层等价方法仍可用：`cli call test_server '{"serverId":"<服务器id>"}' --json`（返回 `{"message":"..."}`）。`cli server test` 就是它的包一层。
> **配免密前先跑这一步**：先确认连得上，再 `cli server setup-key`；连都连不上时配免密只会白费一次密码。

#### `cli server setup-key <服务器id>`

| 项 | 说明 |
|---|---|
| 作用 | 用密码登录一次，自动配好免密（生成密钥 → 写公钥 → 用密钥验证） |
| 建议先做 | 先 `cli server test <服务器id>` 确认能连上，再配免密；配完可以再 test 一次，确认换成密钥后也连得上 |
| 密码输入 | **从标准输入读一行**（不进命令行、不进历史）。例：`echo 密码 \| cli server setup-key srv-7a8b9c` |
| 前提 | 应用在跑；服务器允许密码登录；服务器上 `~/.ssh` 权限正常 |
| 副作用 | 生成 `<配置文件夹>\keys\<服务器id>_ed25519`（+ `.pub`），并把服务器的 `keyPath` 指向它 |

人话输出（`message` 原文）：

```
配好了！以后连这台服务器都走密钥，不用再输密码。密钥放在：C:\Users\你\AppData\Roaming\Shangxian\keys\srv-7a8b9c_ed25519
```

（实际目录看配置文件夹；老安装 `%APPDATA%`、新安装 `%LOCALAPPDATA%`）

`--json`：

```json
{
  "ok": true,
  "result": {
    "keyPath": "C:\\Users\\你\\AppData\\Roaming\\Shangxian\\keys\\srv-7a8b9c_ed25519",
    "message": "配好了！以后连这台服务器都走密钥，不用再输密码。密钥放在：..."
  }
}
```

常见失败（源码里的原话）：`密码不对，再试一次（或者检查一下登录账号）`、`这台服务器不允许用密码登录（它接受的方式：...）`、`公钥写进去了，但用密钥登录还是没成功。请检查服务器上 ~/.ssh 的权限（目录 700、authorized_keys 文件 600）`。

> 顺序建议：`cli server test <id>`（能连）→ `cli server setup-key <id>`（配免密）→ 再 `cli server test <id>` 一次（确认换成密钥后也能连）。

#### `cli server hash-password <服务器id>`

| 项 | 说明 |
|---|---|
| 作用 | 生成 nginx 用的访问密码哈希（优先 bcrypt `$2y$12$…`；服务器 nginx 验不了时自动用兼容格式 `$apr1$…`） |
| 密码输入 | **从标准输入读一行**（`echo 密码 \| cli server hash-password <id>`）；交互终端也不回显 |
| 实现 | **在本机算**（Rust bcrypt / apr1）；会先只读探测一次服务器支不支持 bcrypt，不需要服务器装任何东西 |
| 限制 | 用 bcrypt 时密码超过 72 字节会被拒绝（bcrypt 的上限）；用兼容格式兜底时没这个限制 |
| 副作用 | 只读探测一次服务器（看 nginx 真正加载的加密库支不支持 bcrypt）；连不上或探不出就按兼容格式算。服务器 id 必须存在 |

人话输出（真实形状）：

```
$2y$12$xxxxxxxxxxxxxxxxxxxxxxyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy
```

`--json`：`{ "ok": true, "result": { "hash": "$2y$12$...", "format": "bcrypt" } }`；用了兼容格式时 `format` 是 `"apr1"`，并多一个 `note` 说明原因。

失败例子：`✗ 访问密码太长了：bcrypt 只认前 72 个字节，请换短一点的密码`。

### 4.5 应用命令

#### `cli app status`

人话输出：

```
✓ 应用在跑（进程号 12345，控制端口 58601）
✗ 应用没在跑（offline）。要叫起来：cli app start
```

`--json`：

```json
{ "ok": true, "result": { "running": true, "offline": false, "port": 58601, "pid": 12345, "appVersion": "0.6.3" } }
{ "ok": true, "result": { "running": false, "offline": true, "hint": "要叫起来：cli app start" } }
```

**退出码**：应用在跑 = 0；没在跑 = **3**（脚本可以直接 `cli app status || cli app start`）。

#### `cli app start`

| 项 | 说明 |
|---|---|
| 作用 | 把应用叫起来（静默进托盘） |
| 找程序 | 依次找：`cli.exe` **同一个文件夹**里的 `app.exe` / `Shangxian.exe` / `上线助手.exe` → 注册表 HKCU Run 项 `Shangxian` 里记的路径 → `LOCALAPPDATA\Programs\Shangxian`、`PROGRAMFILES\Shangxian` 等常见安装位置 |
| 等待 | 最多 30 秒等控制通道就绪；已在跑则直接返回 |
| 失败 | 找不到程序：`✗ 没找到『上线助手』主程序。\n  办法：手动点图标把应用打开，命令行照样能连上（cli app status 确认）。`；起来了但通道没通：`✗ 应用起来了但控制通道没通（看托盘图标，或手动打开应用）` |

人话输出：`✓ 应用已启动（在托盘里，进程号 12345）` 或 `✓ 应用本来就在跑（进程号 12345）`
`--json`：`{ "ok": true, "result": { "running": true, "offline": false, "pid": 12345 } }`

#### `cli app stop-all`

| 项 | 说明 |
|---|---|
| 作用 | 停掉全部**开启中**的发布隧道 + 应用拉起的本机服务（升级 / 卸载前用；安装器会调它） |
| 参数 | `--yes`（非交互必填；交互会问 y/N） |
| 退出码特例 | **应用没在跑也退 0**（打印「没什么要停的」）——安装器不会卡 |
| 改不改设置 | **只停运行、不改配置**：`enabled` 原样保留、不写 `config.json`；覆盖升级后原来开着的隧道还是「开着发布」，应用起来后会按原设置重建连接，不用重新手动开始发布 |
| 时限 | 15 秒硬上限：超时就先返回（打印「先让安装/卸载继续，应用会把剩下的停完」），退出码 0 |

人话输出：`✓ 已停止 3 条发布隧道、2 个本机服务。` 有停不掉的会补一句（脚本启动的/别的程序占用的）。`--json` 的 `result`：`{ "tunnelsStopped": 3, "tunnelsFailed": 0, "servicesStopped": 2, "servicesFailed": 1 }`。

#### `cli app update-check`

| 项 | 说明 |
|---|---|
| 作用 | 查一下有没有新版本（只查，不下载、不安装） |
| 数据源 | GitHub Releases 最新版（`iranolly/shangxian`） |
| 前提 | 应用在跑（离线退出码 3） |
| 输出 | `status` = `upToDate` / `available` / `error` / `unknown`；有新版额外一行下载地址 |

`--json` 的 `result`：`{ "status":"available", "latest":"0.7.0", "url":"https://github.com/...", "checkedAt":1760000000000, "error":null }`（结果同步写进 `state.json` 的 `updateInfo` 并发一条事件）。

### 4.6 元命令

#### `cli methods`

打印方法表和参数格式。人话输出格式：

```
能调的方法（cli call <方法> '<参数JSON>'）：

  ping                   看应用在不在（会校验通行证）
                         参数：{}

  status                 一次拿全：应用信息 + 本机服务 + 隧道 + 服务器
                         参数：{}
  ...
```

`--json`：数组，元素为 `{ "method": ..., "desc": ..., "params": ... }`（内容见第 5 节），外层包 `{ "ok": true, "result": [...] }`。
**注意**：`methods` 本身也要经过应用（是底层方法），应用没跑时会失败。

#### `cli call <方法> ['<参数JSON>']`

| 项 | 说明 |
|---|---|
| 作用 | 万能口：直接调底层方法 |
| 参数 | `<方法>` 必填；`<参数JSON>` 可省略，默认 `{}` |
| 输出 | 加 `--json`：`{"ok": true, "result": <结果>}`；不加：把结果按人话打印（字符串直接打，对象一行一个字段） |
| 失败 | 无 `--json`：`✗ 参数不是合法 JSON：...`（退出码 1）；有 `--json`：`{"ok": false, "error": "..."}` |

示例（都加 `--json`，看到的就是信封后的 `result`）：

```bash
cli call ping '{}' --json                # → {"ok":true,"result":{"pong":true}}
cli call get_data_dir '{}' --json        # → {"ok":true,"result":{"path":"C:\\Users\\你\\AppData\\Roaming\\Shangxian"}}
cli call get_notify_enabled '{}' --json  # → {"ok":true,"result":{"enabled":true}}
cli call get_autostart '{}' --json       # → {"ok":true,"result":{"enabled":false}}
cli call get_logs '{"id":"tun-4d5e6f","lines":100}' --json   # → {"ok":true,"result":{"text":"..."}}
cli call save_service '{"name":"x","localPort":9000,"watch":false,"autostart":false}' --json
                                  # → {"ok":true,"result":{"id":"svc-..."}}
cli call set_notify_enabled '{"enabled":false}' --json       # → {"ok":true,"result":{"ok":true}}
```

#### `cli guide`

打印内置的给 agent 的速查（一段固定文本，见附录）。**纯本地文本，应用没跑也能用。**

---

### 4.7 体检、诊断、撤销、备份

#### `cli doctor`

| 项 | 说明 |
|---|---|
| 作用 | 一键体检：固定 9 步（app → dirs → ssh → ports → servers → tunnels → cloud → web → dns） |
| 参数 | `--deep`：会连服务器 + 查公网/域名（单步 ≤ 20 秒、整次 ≤ 60 秒）；不写 = 快速检查（只跑本机，cloud/web/dns 标 `skip`）；`--strict`：只要有 `fail` 就退出码 1 |
| 前提 | **应用在跑时由应用体检（能看到实时状态）；应用没在跑也能跑**：本地真跑 app/dirs/ssh/servers/dns（`--deep` 时 dns），ports/tunnels/cloud/web 标 skip |
| 状态词 | 每步 `pass` / `warn` / `fail` / `skip`；`summary` 统计四类数量 |
| 落盘 | 应用跑时报告写到 `<数据目录>\logs\doctor-last.json`，并发一条 `doctorRan` 事件 |
| 只读 | 体检不会改任何配置；配置读坏/版本过高会直接报在 `app` 步里 |

人话输出（示例；下面是应用没在跑时的离线体检，格式与真实输出一致）：

```
「上线助手」体检（快速检查）
✗ 程序状态     应用没在跑（现在看不到实时状态）
    怎么办：运行 cli app start（或双击「上线助手」）把应用打开，再跑一次 cli doctor。
✓ 文件夹       数据目录在，能读能写（C:\Users\你\AppData\Roaming\Shangxian）
✓ SSH 程序     找到系统自带 ssh：C:\Windows\System32\OpenSSH\ssh.exe
· 本机端口     应用没在跑，不猜本机服务的状态
✓ 云端服务器   1 台服务器都正常（1 条开启隧道引用）
· 发布隧道     应用没在跑，看不到实时状态
· 云端配置     快速检查不连服务器；要看云端加 --deep
· 公网访问     快速检查不连服务器；要看公网加 --deep
· 域名解析     快速检查不连网；要查域名加 --deep
✗ 自检有 1 项没过，4 项跳过
```

`--json` 的 `result` 就是 `DoctorReport`（外层 `{"ok":true,"result":...}`，`schemaVersion` = 1；下面是截选）：

```json
{
  "schemaVersion": 1,
  "checkedAt": 1760000000000,
  "appVersion": null,
  "appRunning": false,
  "mode": "quick",
  "steps": [
    { "id": "app", "title": "程序状态", "status": "fail", "detail": "应用没在跑（现在看不到实时状态）", "advice": "运行 cli app start（或双击「上线助手」）把应用打开，再跑一次 cli doctor。", "durationMs": 0 },
    { "id": "dirs", "title": "文件夹", "status": "pass", "detail": "数据目录在，能读能写", "advice": "" },
    { "id": "cloud", "title": "云端配置", "status": "skip", "detail": "快速检查不连服务器；要看云端加 --deep", "advice": "" }
  ],
  "summary": { "pass": 4, "warn": 0, "fail": 1, "skip": 4 }
}
```

> `fail` 的步骤可能带 `fix: { label, text }`（前端按钮文案 + 怎么修）和 `children`（cloud/web/dns 的子项）。**`cli doctor --strict` 退出码 1 不是命令跑错，是「检查没过」**——脚本判断：`cli doctor --strict || echo "有项没过"`。

#### `cli diag`

| 项 | 说明 |
|---|---|
| 作用 | 打一个**脱敏**诊断包（zip），发给帮忙排查的人 |
| 参数 | `--out <文件>`（不写 = 存到 `<数据目录>\diag\` 下按时间命名）；`--deep` 传给体检（包里的 doctor.json 用 deep 报告） |
| 包含 | `config.redacted.json`（没有密码哈希、没有密钥路径，只有 `hasPassword`/`keyPathPresent`）、`state.redacted.json`、`snapshot.json`、`stats.json`、`doctor.json`、`events.tail.jsonl`、`logs/app.tail.txt`、各隧道/服务日志尾巴、`README.txt` |
| 脱敏 | 密码/密码哈希/token/密钥内容、`C:\Users\<名字>` 里的用户名都会替换成占位；`launch.args` 在诊断包里只留长度（`<已省略>`） |
| 安全边界 | 包里**不会有** ctl 通行证、密钥文件内容、密码哈希（bcrypt/apr1）；但有域名/路径等信息，不要公开贴到网上 |
| 前提 | 应用在跑（离线退出码 3） |

人话输出：`✓ 诊断包已生成：C:\...\diag\diag-20261012-101500.zip（120.5 KB，已脱敏）`；`--json` 的 `result`：`{ "path":"...", "bytes":123456, "redacted":true, "deep":false, "truncated":false }`。

#### `cli undo`

| 参数 | 作用 |
|---|---|
| 不写 / `last` | 撤销**最近一次**删除 |
| `list` | 列出还能撤销的（10 分钟内、最多 20 条），离线也能看（读 `state.json`） |
| `und-xxxxxxxx` | 按令牌撤销指定的一条 |

- 可撤销的是 `tunnel remove` / `service remove` / `server remove` 三种；每个成功的人话输出都会提示「（10 分钟内可以撤销：cli undo last）」。
- **真撤销要应用在跑**（要改配置，离线退出码 3）；隧道恢复后是**暂停**状态，要 `cli tunnel start`；服务只恢复登记、不启动；服务器原样恢复。
- 过期/找不到：`✗ 这条撤销已经过期了（删除超过 10 分钟）；要恢复只能重新建一个`（退出码 1）。同 id 已存在、依赖不在时也是人话拒绝。
- `--json` 的 `result`：`{ "restored": { "type":"tunnel", "id":"tun-...", "name":"comfyui 发布" }, "enabled": false }`。

#### `cli config export`

| 参数 | 作用 |
|---|---|
| `--out <文件>` | 存到哪；不写 = 把 JSON 打印出来（自己存成 `.json` 就是备份） |
| `--with-paths` | 连真实密钥路径 `keyPath` 一起导出（**仍然不含密码哈希**） |

- 导出是**纯读**：应用没在跑也能导（直接读 `config.json`）。
- 默认脱敏：`accessHash` → `hasPassword`（true/false）、`keyPath` → `keyPathPresent`（true/false）；版本比程序新时也不拒绝，按已知键脱敏后照导（自救路径）。
- `--json` 的 `result`：`{ "redacted": true, "config": {...}, "path": "C:\\..." }`（`path` 只在写了 `--out` 时出现）。

#### `cli config import <文件>`

| 参数 | 作用 |
|---|---|
| `--mode merge` | 默认：按 id 合并（同名 id 更新，没写的字段保持现状） |
| `--mode replace` | 整份替换（导入前自动备份成 `config.json.bak-<时间戳>`） |
| `--dry-run` | 只检查、不写盘、不用确认（输出和真导入一样的「新增/更新/没变化」摘要） |
| `--yes` | 跳过 y/N（非交互必填，否则退出码 1） |

- 导入文件最大 1MB，必须是 `config export` 导出的格式或同形状的 JSON。
- **用脱敏文件导回来不会把密码/密钥清空**：`hasPassword=true` 保留原哈希、`keyPathPresent=true` 保留原密钥路径；`merge` 模式尤其安全。
- 导入是原子操作：先校验、再备份、再写盘、最后换进内存；失败什么都不改。应用没在跑不能导入（退出码 3）。
- `--json` 的 `result`：`{ "mode":"merge", "added":{"servers":0,"services":1,"tunnels":0}, "updated":{...}, "unchanged":{...}, "warnings":[], "backupPath":"..." }`（dry-run 时没有 `backupPath`）。

---

## 5. 底层方法表（`cli methods` 的真实内容）

`cli call <方法> '<参数JSON>'` 能调的全部方法，**共 60 条**（源码 `app/src-tauri/src/ctl.rs` 的 `METHODS` 常量；`cli methods` 打印的就是这张表）。

> 数量说明：设计稿（`.setup/design-v2/D2-contracts.md` §15.2）冻结的是「36 旧 + 12 新 + 4 云端 = 52」，实现时又追加了生命周期与管理类（`stop_all`、恢复状态三件套、免打扰两件套、`restart_service_rotate_logs`、`forget_host_key` 等），所以以源码的 60 条为准（旧的 61 条含已被移除的 `tunnel_restart`）。

### 5.1 数据与日志

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `ping` | 看应用在不在（会校验通行证） | `{}` |
| `status` | 一次拿全：应用信息 + 本机服务 + 隧道 + 服务器 | `{}` |
| `get_snapshot` | 只拿三张清单（不含应用信息） | `{}` |
| `methods` | 列出所有能调的方法（就是这张表） | `{}` |
| `stats` | 运行统计：时长、今日重连、重试倒计时 | `{}` |
| `logs_query` | 按来源读日志（事件/应用/隧道/服务，可含轮转备份） | `{source?, id?, lines?, archive?, format?}` |

### 5.2 体检与诊断

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `doctor` | 自检（app→dirs→ssh→ports→servers→tunnels→cloud→web→dns） | `{mode?}`：`quick`（默认）/ `deep` |
| `run_doctor` | 自检（doctor 的旧名字，等价） | `{mode?}` |
| `build_diag` | 打一个脱敏诊断包 | `{out?, deep?}` |

### 5.3 删除撤销、配置备份、设置、更新、停全部

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `stop_all` | 停止全部发布与本机服务（卸载/升级前用；**只停运行、不改设置**，`enabled` 不动、不写盘） | `{}` |
| `list_deleted` | 最近删除（10 分钟内可撤销） | `{}` |
| `restore_deleted` | 撤销删除（隧道恢复为暂停） | `{token}` |
| `purge_deleted` | 放弃撤销 | `{token}` |
| `export_config` | 导出配置（默认不含密码哈希/密钥路径） | `{out?, redacted?}` |
| `import_config` | 导入配置（默认合并） | `{doc?/path?, mode?, dryRun?}`；`mode` = `merge`（默认）/ `replace` |
| `check_update` | 检查有没有新版本 | `{}` |
| `get_settings` | 读设置项 | `{}` |
| `set_settings` | 整份保存设置项 | `{refreshIntervalMs, quietHours, checkUpdateOnStart}` |

### 5.4 本机服务

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `list_local_ports` | 本机正在监听的端口（带进程名和程序路径） | `{}` |
| `scan_imports` | 扫描现有启动脚本，返回可导入的建议 | `{}` |
| `import_services` | 把导入建议变成服务（默认暂停） | `{items: [...]}` |
| `save_service` | 新增/编辑本机服务（不填 id 就是新增） | `{id?, name, localPort, launchExe?, launchArgs?, launchWorkdir?, watch, autostart, note?}` |
| `remove_service` | 删除本机服务（有隧道在用会拒绝；返回撤销令牌） | `{id}` |
| `service_start` | 启动本机服务（用登记过的启动方式） | `{id}` |
| `service_stop` | 停止本机服务（默认只停应用自己启动的；外部进程要先拿 `needsConfirm` 让用户确认） | `{id, killForeign?, expectedPid?}` |
| `service_restart` | 重启本机服务（先停再启；外部进程在听时不碰，返回 `needsConfirm`） | `{id}` |
| `inspect_process_launch_info` | 自动读取某端口上进程的启动方式（程序/参数/工作目录 + 警告） | `{port, expectedPid?}` |
| `preview_launch_command` | 给人看的实际命令行预览（走和启动一样的分支，不参与启动） | `{exe, args}` |
| `set_service_watch` | 开关看护（掉了自动拉起） | `{id, watch}` |
| `set_service_autostart` | 开关开机自启 | `{id, autostart}` |
| `get_service_logs` | 读本机服务的日志尾部 | `{id, lines?}`（默认 200） |
| `restart_service_rotate_logs` | 重启本机服务并轮转日志（日志太大时用） | `{id}` |

### 5.5 发布隧道

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `add_tunnel` | 新增发布隧道 | `{name, serviceId, serverId, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}` |
| `update_tunnel` | 编辑发布隧道（`accessHash` 传 null 表示不改密码） | `{id, name, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}` |
| `remove_tunnel` | 删除发布隧道（返回撤销令牌） | `{id}` |
| `tunnel_start` | 开始发布 | `{id}` |
| `tunnel_stop` | 暂停发布 | `{id}` |
| `forget_host_key` | 清掉这台服务器的 SSH 指纹记录（服务器重装过时用） | `{id}`（隧道 id） |
| `get_logs` | 读隧道的日志尾部 | `{id, lines?}`（默认 200） |

### 5.6 云端服务器与云端检查

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `save_server` | 新增/编辑云端服务器（不填 id 就是新增） | `{id?, name, host, sshPort, user, keyPath?, vhostDir?}` |
| `delete_server` | 删除云端服务器（有隧道在用会拒绝；返回撤销令牌） | `{id}` |
| `setup_passwordless` | 用密码登录一次，自动配好免密（密码不落盘） | `{serverId, password}` |
| `test_server` | 测试连接：真连一次这台服务器的 SSH（最长 15 秒） | `{serverId}`（也认 `id`） |
| `hash_password` | 生成访问密码的哈希（优先 bcrypt；服务器 nginx 验不了时用兼容的 apr1；本机算好、给 nginx 用） | `{serverId, password}` |
| `get_nginx_snippet` | 生成某条隧道的 nginx 配置片段 | `{id}` |
| `check_tunnel_access` | 核对云端 nginx 上这条隧道的实际访问保护（和 `check_tunnel_cloud` 的 `full` 同路，SSH 最长约 30 秒；结果会存进运行记录） | `{id}` |
| `check_tunnel_cloud` | 云端完整检查（full/drift/audit/env；含 HTTPS/证书/端口/审计） | `{id, mode?}` |
| `scan_server_env` | 识别服务器面板与网站服务（结果缓存到运行记录） | `{serverId}` |
| `run_access_audit` | 统计最近 1000 行的 401/403 访问情况 | `{id}` |
| `get_public_ip` | 看服务器眼里你的上网 IP | `{}` |

### 5.7 通知、自启与生命周期

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `get_notify_enabled` | 掉线通知开着吗 | `{}` |
| `set_notify_enabled` | 开关掉线通知 | `{enabled}` |
| `get_notify_quiet` | 免打扰时段设置 | `{}` |
| `set_notify_quiet` | 设置免打扰时段 | `{enabled, start, end}`（`HH:MM`） |
| `get_autostart` | 开机自启开着吗 | `{}` |
| `set_autostart` | 开关开机自启 | `{enabled}` |
| `get_data_dir` | 配置文件夹在哪 | `{}` |
| `get_recovery_state` | 崩溃自动恢复的状态 | `{}` |
| `clear_recovery_state` | 崩溃自动恢复计数清零、重新启用 | `{}` |
| `set_crash_recovery` | 开关崩溃自动恢复 | `{enabled}` |

### 5.8 成功返回形状（补充）

| 方法 | 成功返回 |
|---|---|
| `ping` | `{"pong": true}` |
| `stats` | 见 4.1 的 `cli stats --json` 示例（`schemaVersion` = 1） |
| `logs_query` | 见 4.1 的 `cli logs` 示例（`{source, archive, lines, truncated, sources, entries, text?}`） |
| `doctor` / `run_doctor` | `DoctorReport`：`{schemaVersion, checkedAt, appVersion, appRunning, mode, steps:[{id,title,status,detail,advice,children?,durationMs?,fix?}], summary:{pass,warn,fail,skip}}` |
| `build_diag` | `{"path": "...", "bytes": 123456, "redacted": true, "deep": false, "truncated": false}` |
| `stop_all` | `{"tunnelsStopped": 3, "tunnelsFailed": 0, "servicesStopped": 2, "servicesFailed": 0}` |
| `list_deleted` | `{"items": [{"token":"und-3f9a1c2d","type":"tunnel","id":"tun-...","name":"...","deletedAt":1760000000000,"expiresAt":1760000600000}]}` |
| `restore_deleted` | `{"restored": {"type":"tunnel","id":"tun-...","name":"..."}, "enabled": false}` |
| `purge_deleted` | `{"ok": true}` |
| `export_config` | `{"redacted": true, "config": {...}, "path": "C:\\...（写了 out 才有）"}` |
| `import_config` | `{"mode":"merge","added":{"servers":0,"services":1,"tunnels":0},"updated":{...},"unchanged":{...},"warnings":[],"backupPath":"..."}`（dryRun 无 `backupPath`） |
| `check_update` | `{"status":"upToDate"\|"available"\|"error"\|"unknown","latest":null,"url":null,"checkedAt":null,"error":null}` |
| `get_settings` / `set_settings` | `{"refreshIntervalMs":800,"quietHours":{"enabled":false,"start":"22:00","end":"08:00"},"checkUpdateOnStart":true}` |
| `save_service` / `add_tunnel` / `save_server` | `{"id": "..."}` |
| `remove_service` / `remove_tunnel` / `delete_server` | `{"ok":true,"deleted":{"type":"service","id":"...","name":"..."},"undoToken":"und-...","undoExpiresAt":1760000600000}` |
| `setup_passwordless` | `{"keyPath": "...", "message": "..."}` |
| `test_server` | `{"message": "连接成功，用时 0.4 秒"}`；失败是人话错误（超时 / 登录被拒 / 地址解析不了 / 密钥文件用不了等） |
| `hash_password` | `{"hash": "$2y$12$..."}` |
| `get_nginx_snippet` | `{"snippet": "...", "acmeExempt": "..."/null, "preview": [...], "htpasswd": .../"...", "steps": [...]}` |
| `check_tunnel_access` | `{"confFound": true, "passwordOn": true, "ipOn": false, "allowIps": [], "htpasswdExists": true}` |
| `check_tunnel_cloud` / `run_access_audit` / `scan_server_env` / `get_public_ip` | 见 5.9 |
| `forget_host_key` | `{"message": "已忘掉旧指纹，下次连接会重新记下来"}` |
| `get_service_logs` / `get_logs` | `{"text": "..."}` |
| `service_stop` | `{"kind": "stopped"}`；外部进程要确认时 `{"kind": "needsConfirm", "pid": 4108, "exe": "C:\\Python311\\python.exe"}`；确认后停掉是 `{"kind": "foreignStopped", ...}` |
| `service_restart` | 成功 `{"ok": true}`；端口上是外部进程时 `{"kind": "needsConfirm", "pid": 4108, "exe": "..."}`（**不是错误**，调用方要自己决定怎么办） |
| `inspect_process_launch_info` | `{"port": 8080, "pid": 4108, "pidMatched": true, "exe": "C:\\Python311\\python.exe", "args": "-m http.server 8080", "workdir": "D:\\code\\demo", "rawCommandLine": "...", "warnings": []}`（读不可靠时 `pidMatched=false` 或 `warnings` 带 `multipleOwners`/`processGone`/`portOwnerChanged`；参数疑似密钥时带 `sensitiveArgs`） |
| `preview_launch_command` | `{"command": "..."}` |
| `get_notify_enabled` | `{"enabled": true/false}` |
| `get_notify_quiet` / `set_notify_quiet` | `{"enabled": false, "start": "22:00", "end": "08:00"}` |
| `get_autostart` | `{"enabled": true/false}` |
| `get_data_dir` | `{"path": "..."}` |
| `get_recovery_state` | `{"restartsInWindow": 0, "paused": false}` |
| `clear_recovery_state` | `{"ok": true, "enabled": true}` |
| `restart_service_rotate_logs` / `tunnel_*` / `set_*` / 其余写操作 | `{"ok": true}` |

> 这两个只是界面用的 Tauri 命令，**不在方法表里**：`open_data_dir`（打开配置文件夹）、`test_notification`（试弹通知）；D0 还批准了 Tauri 专用的 `get_data_dir_info`、`save_text_file`、`open_log_folder`、`get_log_archives`、`query_logs`（ctl 里叫 `logs_query`）。
> 上面这些返回值是 ctl 层原始结果；`cli call --json` 会在外面套一层 `{"ok": true, "result": ...}`（失败前者是 `{"ok": false, "error": "..."}`）。
> `get_service_logs` / `get_logs` 的 `lines` 不传时默认 200。
> `add_tunnel` / `update_tunnel` 的 `access` 是**数组**：可同时放 `"password"` 和 `"ip"`（两个都放 = 两个条件都要满足），空数组 `[]` = 无保护。

### 5.9 云端完整检查（`check_tunnel_cloud` / `scan_server_env` / `run_access_audit` / `get_public_ip`）

```bash
# 完整核对（默认 full）：访问保护 + HTTPS/证书 + 端口回环 + 文件定位 + 环境
cli call check_tunnel_cloud '{"id":"tun-4d5e6f","mode":"full"}' --json
# 只核对漂移（轻量：CONF/HTPASSWD/HTTPS/PORT）
cli call check_tunnel_cloud '{"id":"tun-4d5e6f","mode":"drift"}' --json
# 访问审计：统计最近 1000 行的 401/403
cli call run_access_audit '{"id":"tun-4d5e6f"}' --json
# 识别服务器面板/网站服务（结果缓存进 state.json 的 servers[id]）
cli call scan_server_env '{"serverId":"srv-7a8b9c"}' --json
# 服务器眼里你的上网 IP（加白名单前用它核对）
cli call get_public_ip '{}' --json
```

`check_tunnel_cloud` 的 `result`（外层信封后，含 `full` 模式的所有节）大致形状：

```json
{
  "check": {
    "status": "drift", "ts": 1760000000000, "source": "manual",
    "cloud": { "confFound": true, "passwordOn": true, "ipOn": false, "allowIps": [], "htpasswdExists": true },
    "expected": { "password": true, "ip": false, "allowIps": [], "url": "comfy.example.com" },
    "driftFields": ["password"], "error": null,
    "extra": {
      "https": { "enabled": true, "certDays": 67, "redirect80": true, "plainOpen": false,
                 "temporaryRedirect": false, "serverMatch": "matched", "warnings": [] },
      "port": { "bound": "loopback", "tool": "ss", "port": 13000, "evidence": [], "warnings": [] },
      "files": ["/xp/panel/vhost/nginx/comfy.example.com.conf"],
      "env": { "panel": "xp", "web": "nginx", "panelConfidence": "high", "webConfidence": "high",
               "nginxBin": "/usr/sbin/nginx", "vhostDir": "/xp/panel/vhost", "vhostDirExists": true, "wsMap": true },
      "clientIp": "203.0.113.10",
      "audit": { "available": true, "path": "/www/wwwlogs/comfy.example.com.log", "window": 1000, "count401": 3, "count403": 0, "topIps": [{"ip": "203.0.113.10", "count": 2}], "entries": [], "truncated": false, "warnings": [] },
      "warnings": ["临时跳转，建议改用 301"]
    }
  },
  "actual": { "confFound": true, "passwordOn": true, "ipOn": false, "allowIps": [], "htpasswdExists": true },
  "port": { "bound": "loopback", "tool": "ss", "port": 13000, "evidence": [], "warnings": [] },
  "files": ["/xp/panel/vhost/nginx/comfy.example.com.conf"],
  "env": null,
  "clientIp": "203.0.113.10",
  "audit": null
}
```

> - `check.status`：`ok` / `drift` / `error` / `unknown`；`drift` 时看 `driftFields`（`conf` / `password` / `ip` / `allowIps` / `htpasswd`）。
> - `https` 里 `enabled` = 云端有没有 443 + ssl；`certDays` = 证书剩余天数；`redirect80` = 80 是否跳 https（302/307 会带警告「临时跳转，建议改用 301」）；`serverMatch` = `matched` / `single` / `none`。
> - `port.bound`：`loopback`（只绑回环，安全）/ `exposed`（对着公网，危险）/ `missing` / `unknown`。
> - `env.panel`：`bt`（宝塔）/ `xp`（小皮）/ `none` / `unknown`；`env.web`：`nginx` / `apache` / `both` / `none` / `unknown`。
> - `audit`：`available`（有没有读到访问日志）、`path`、`window`（默认 1000 行）、`count401`/`count403`、`topIps`（最多前 5 个）、`entries`（最多 30 条样本：`{ip,time,method,path,status,bytes,raw}`）、`truncated`、`skipReason`；计数是「最近 N 行」的近似值。
> - `get_public_ip`：`{"ip":"203.0.113.10","source":"sshConnection","at":...,"server":"香港-1","message":"..."}`。
> - `scan_server_env`：直接返回 `CloudEnv`（同 `env` 字段）。
> - `run_access_audit`：返回和 `check_tunnel_cloud` 同形状（带 `check`/`actual`/`audit`）。
> - 这些方法**只读云端**，不会写服务器；手动调一次就把结果存进 `state.json` 对应隧道的 `protectionCheck`。
>
> **自动漂移复检已下线**：云端核对现在只有手动「检查云端」。

### 5.10 自动读取启动方式（给 agent 的专用方法）

界面上「点一下监听端口就自动填启动方式」用的是这两个方法，agent 也能直接调：

```bash
cli call inspect_process_launch_info '{"port":8080,"expectedPid":4108}' --json
cli call preview_launch_command '{"exe":"C:\\Python311\\python.exe","args":"-m http.server 8080"}' --json
```

`inspect_process_launch_info` 返回：

```json
{
  "port": 8080,
  "pid": 4108,
  "pidMatched": true,
  "exe": "C:\\Python311\\python.exe",
  "args": "-m http.server 8080",
  "workdir": "D:\\code\\demo",
  "rawCommandLine": "C:\\Python311\\python.exe -m http.server 8080",
  "warnings": []
}
```

读结果的纪律（界面就是这个逻辑，agent 照抄）：

- `expectedPid` 传「你刚才在 `list_local_ports` 里看到的 pid」，用来核对占用者有没有变。
- **`pidMatched=false` 或 `warnings` 里出现 `multipleOwners` / `processGone` / `portOwnerChanged`：这份信息不可靠，不要拿它去改配置**，只把 `warnings[].text` 转告用户。
- `warnings` 里有 `sensitiveArgs`：参数里疑似有密钥（`--token`、`password=` 这类）。这是提醒不是阻断；保存前要让用户明确确认。**这个拦截只在界面生效**：`cli service add/edit`、`cli call save_service` 直接写配置，不做敏感词拦截也不过滤参数。
- `workdir` 读到的是进程**现在**的工作目录，不一定等于启动目录（程序自己 `chdir` 过就变了）。
- 环境变量不捕获；`args` 是从原始命令行剥掉程序路径后的近似还原，不保证逐字一致。32 位（WOW64）进程会走 32 位 PEB 读取，拿不到就返回空字段 + 警告。

`preview_launch_command` 返回 `{"command": "..."}`，是给人看的最终命令行（含 `.bat` 走 `cmd /C`、`.ps1` 走 `powershell -File` 这类包装），**不能拿来当 shell 命令直接执行**。

## 6. 离线行为（应用没在跑时）

判断方法：`cli app status --json` → `{"ok":true,"result":{"running":false,"offline":true,...}}`，退出码 3。

| 命令 | 能用吗 | 结果 |
|---|---|---|
| `status` / `services` / `tunnels` / `servers` | ✅ | 直接读配置文件夹里的 `config.json`；所有状态字段 = `"unknown"`；`result.app.running = false`、`pid = null`；退出码 0 |
| `app status` | ✅ | 读 `ctl.json` 并试连一次，输出 `{"ok":true,"result":{"running":false,"offline":true,...}}`；**退出码 3** |
| `doctor`（含 `--deep`） | ✅ | 离线体检：本地真跑 app/dirs/ssh/servers/dns（`--deep` 才查 dns），ports/tunnels/cloud/web 标 `skip`；退出码 0（`--strict` 时有 fail 才 1） |
| `undo list` | ✅ | 只读 `state.json` 里的墓碑（能看，但真撤销仍要应用在跑） |
| `config export` | ✅ | 离线直接读 `config.json` 导出（默认脱敏）；退出码 0 |
| `guide` / `--help` / `--version` | ✅ | 纯本地文本 |
| `app stop-all` | ✅（特例） | 应用没在跑 = 没什么要停的，打印一句、**退出码 0**（安装器不会卡） |
| `app start` | ⚠️ | 尝试拉起应用；找不到 exe 或 30 秒通道不通就失败 |
| `ports` / `stats` / `logs` / `nginx` / `methods` / `call` / `diag` | ❌ | 报错，退出码 3（需要应用在跑） |
| `undo`（真撤销）/ `config import` / `app update-check` | ❌ | 报错，退出码 3 |
| `service *` / `tunnel *` / `server *` | ❌ | 报错，退出码 3 |

报错原文（实测）：

```
✗ 『上线助手』没在运行（连不上它的控制通道）。先打开应用，或者运行：cli app start（找不到 ctl.json）
```

加 `--json` 时同一条错误变成：

```json
{
  "error": "『上线助手』没在运行（连不上它的控制通道）。先打开应用，或者运行：cli app start（找不到 ctl.json）",
  "ok": false
}
```

离线快照的字段差异见 4.1 的说明（`unknown` 状态、缺 `reason`/`logPath`/`serviceOk`/`protectionCheck` 等）。

> 应用没在跑时，读配置的命令不会静默拿空配置：配置读坏或版本比程序新会**直接报错**（退出码 1），人话里说清是读坏了（看 `config.json.bad`）还是「这份设置是新版存的，先升级」——不会拿空配置覆盖原文件。

> **应用没在跑 ≠ 本地服务没在跑**：本地服务是保活的，应用退出、重启、崩溃后被强杀都不会带走它们。`services` 里的状态是 `unknown` 只是说 cli 没去探端口，程序本身可能正跑着；想停掉要么重新打开应用（下次启动会按端口认领，然后在界面上停），要么用任务管理器。应用退出时只有 SSH 隧道会断。

---

## 7. 控制协议与 ctl.json

### 工作方式（源码：`core/src/ctl.rs`）

1. 应用启动时在 `127.0.0.1` 上绑一个**随机端口**（只有本机能连）。
2. 把端口号、通行证、进程号写进 `<数据目录>\ctl.json`。
3. `cli.exe` 读 `ctl.json` → 连上去 → 发一行 JSON → 收一行 JSON。
4. 应用重启后端口和通行证都会换；应用退出时会删掉 `ctl.json`（删不掉也没事，连不上就是没运行）。

### ctl.json 字段（真实文件）

```json
{
  "port": 58601,
  "token": "8222253e...b180",
  "pid": 1283452,
  "appVersion": "0.6.3"
}
```

| 字段 | 含义 |
|---|---|
| `port` | 应用监听的端口（每次启动都换，仅 127.0.0.1） |
| `token` | 通行证，64 位十六进制；请求必须带上 |
| `pid` | 应用进程号 |
| `appVersion` | 应用版本 |

### 请求 / 回话格式

请求（一行，`\n` 结尾）：

```json
{"token":"<ctl.json 里的 token>","id":1,"method":"status","params":{}}
```

成功：

```json
{"id":1,"ok":true,"result":{ ... }}
```

失败：

```json
{"id":1,"ok":false,"error":"人话原因"}
```

| 细节 | 值 |
|---|---|
| 连接超时 | 3 秒 |
| 读超时 | 120 秒（配免密、连服务器这类操作要等十几秒） |
| 一条连接 | 只处理一条请求 |
| 通行证不对 | `{"ok":false,"error":"通行证不对（ctl.json 可能是旧的，重启应用即可）"}` |
| 方法不存在 | `没有这个方法：xxx（能用的都在这儿：cli methods）` |

安全说明：`token` 就是门钥匙，`ctl.json` 同机器的程序能读到；不要把它贴给别人、不要把它转发到外网。控制端口只在回环上，公网连不到。

---

## 8. 配置文件与运行状态字段表

### 8.1 位置（都在数据目录下）

| 内容 | 路径 | 说明 |
|---|---|---|
| 数据目录 | 解析顺序：环境变量 `SHANGXIAN_DATA_DIR`（必须是完整路径）→ 便携版 `<exe 目录>\data`（exe 旁边放一个空文件 `shangxian.portable`）→ 老安装 `%APPDATA%\Shangxian`（存在就继续用）→ 新安装 `%LOCALAPPDATA%\Shangxian`；两个环境变量都没有且不是便携版/指定目录时直接报错（不会回退到相对目录） | 实际路径以 `cli call get_data_dir` / 设置页「打开」为准 |
| 配置 | `<数据目录>\config.json`（原子写入：临时文件 + 改名） | 唯一事实来源 |
| 读坏留底 | `<数据目录>\config.json.bad`；读坏期间应用不自动保存（防拿空配置覆盖）；原文件会从 `config.json` 移走 | 修好再重开应用 |
| 老版本备份 | `config.json.v3.bak`、`config.json.v4.bak`（迁移时自动生成，只生成一次）；导入前的手工备份是 `config.json.bak-<时间戳>` | |
| 运行状态 | `<数据目录>\state.json`（本机私有，删掉 = 回到初次运行，无副作用；损坏改名 `state.json.bad`，版本过高改名 `state.json.unsupported`，都用默认状态继续） | 只有运行中的应用写；去抖落盘（≥1 秒、最晚 5 秒）+ 退出前 `flush` |
| 事件流 | `<数据目录>\events.jsonl`（一行一个 JSON；超 2MB 轮转 `.1`/`.2`） | 应用里发生的事，`cli logs --source app` 读 |
| 应用日志 | `<数据目录>\logs\app.log`（降级/普通日志；超 2MB 轮转，留 `app.log.1`/`.2`） | |
| 窗口记忆 | `<数据目录>\window-state.json`（位置/大小/最大化；坏文件按默认值） | 无窗口托盘态时不占内存 |
| 崩溃恢复 | `<数据目录>\recovery.json`（10 分钟窗口内自动恢复次数；第 3 次起暂停）；`<数据目录>\crash-recovery.off` 存在 = 用户手动关了崩溃自动恢复 | |
| 诊断包 | `<数据目录>\diag\`（`cli diag` 默认落这里） | |
| 体检报告 | `<数据目录>\logs\doctor-last.json` | 最近一次体检 |
| 密钥 | `<数据目录>\keys\<服务器id>_ed25519`（+ `.pub`）；主机指纹记录在密钥旁边 `shangxian_known_hosts`（TOFU，见 8.5） | |
| 日志 | `<数据目录>\logs\`：隧道 `<隧道id>-<8位代码>.ssh.log`（超 5MB 轮转，留 `.1`/`.2`），本机服务 `service-<服务id>.log`（同样 5MB×2）；按名字命名的旧日志不再被读取 | |
| 通行证 | `<数据目录>\ctl.json`（应用运行时才有） | |

### 8.2 config.json（`core/src/config.rs`，JSON 用 camelCase，当前版本 **6**）

顶层：

| 字段 | 类型 | 说明 |
|---|---|---|
| `version` | number | 当前 = **6** |
| `servers` | 数组 | 云端服务器 |
| `services` | 数组 | 本机服务 |
| `tunnels` | 数组 | 发布隧道 |
| `notifyEnabled` | bool | 掉线通知，默认 `true`（留在根字段，不迁进 `settings`） |
| `settings` | object | v6 新增的全局设置，见下 |

`settings`（Settings）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `refreshIntervalMs` | number | 界面刷新间隔，范围 **500..=10000**，默认 `800`；越界读时夹回边界、发 `configNormalized` 事件 |
| `quietHours` | object | 免打扰时段（只影响系统通知，事件照记） |
| `quietHours.enabled` | bool | 默认 `false` |
| `quietHours.start` / `.end` | string | `"HH:MM"` 24 小时制，允许跨午夜；默认 `"22:00"` / `"08:00"` |
| `checkUpdateOnStart` | bool | 启动后约 30 秒静默查一次更新（≥6 小时限频），默认 `true` |

`servers[]`（ServerConfig）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | `srv-` + 6 位十六进制 |
| `name` | string | 名字 |
| `host` | string | 主机名或 IP |
| `sshPort` | number | SSH 端口，缺省 22 |
| `user` | string | 登录用户名 |
| `keyPath` | string / null | 私钥路径；`null` = 用系统默认密钥 |
| `vhostDir` | string / null | 面板放站点的目录；`null`/空 = 继续往下找。站点目录解析顺序：**服务器 `vhostDir` → 自动识别结果（`state.json` 的 `servers[id].vhostDir`）→ 环境变量 `SHANGXIAN_HTPASSWD_DIR` → 默认 `/xp/panel/vhost`**。站点配置在 `<目录>/nginx/<域名>.conf`，密码文件在 `<目录>/<域名>.htpasswd` |

`services[]`（LocalService）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | `svc-` + 6 位十六进制 |
| `name` | string | 名字 |
| `localPort` | number | 本机端口 |
| `probeKind` | string | 探测方式：`"tcp"`（默认，只看能不能连上）或 `"http"`（要一个 HTTP 响应，401/403/404/500 都算活）；非法值读时归一成 `tcp`。**CLI / 界面暂未提供修改入口**，手改配置或导入时生效 |
| `launch` | object / null | 启动方式；`null` = 只监控不启动 |
| `launch.exe` | string | 程序或脚本路径（`.bat`/`.cmd` 走 cmd，`.ps1` 走 powershell，`.sh` 走 bash） |
| `launch.args` | string | 启动参数（一行，支持双引号包住带空格的参数），默认 `""` |
| `launch.workdir` | string / null | 工作目录 |
| `watch` | bool | 看护：进程掉了自动拉起 |
| `autostart` | bool | 开机自启：应用启动后拉起 |
| `note` | string / null | 备注 |

`tunnels[]`（Tunnel）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | `tun-` + 6 位十六进制（v3 老配置迁移时沿用旧 id） |
| `name` | string | 名字 |
| `serviceId` | string | 指向 `services[].id` |
| `serverId` | string | 指向 `servers[].id` |
| `remotePort` | number | 云端端口（写死，永不自动漂移） |
| `url` | string / null | 公网网址 |
| `keyPath` | string / null | 覆盖服务器的密钥；`null` = 继承服务器 |
| `access` | array | 可同时含 `"password"` 和 `"ip"`（两个都含 = 两个条件都要满足）；`[]` = 无保护 |
| `accessUser` | string / null | 访问用户名 |
| `accessHash` | string / null | 访问密码的哈希（优先 `$2y$12$…`，服务器不支持 bcrypt 时是兼容格式 `$apr1$…`；本机算）；明文密码只显示一次，不落盘 |
| `accessDetail` | string / null | 访问保护细节（IP 白名单，逗号分隔） |
| `enabled` | bool | 是否正在发布；`false` = 已暂停 |
| `preset` | string | 历史字段（v6 起存在）：新版本界面/CLI 不再设置；老隧道可能保留 `ws`/`upload`，决定云端片段里哪组行是激活的；非法值读时归一成 `web` |

### 8.3 迁移与降级拦截

- **v3 → v4**：老配置先备份成 `config.json.v3.bak`，再拆成「本机服务 + 隧道」并写回。老 service 的 id 保留给隧道（界面标识稳定）；同一个本机端口的多条老服务合成一台本机服务；老数据没有看护/自启信息，迁移后默认关。
- **v4 → v5**：`access` 从字符串变成数组，备份 `config.json.v4.bak`。`"password"` → `["password"]`、`"ip"` → `["ip"]`、`"none"`/空 → `[]`；`accessUser`/`accessHash`/`accessDetail` 原样保留。只改形状，云端 nginx 不用动。
- **v5 → v6**：只增不改——新增 `settings` 与隧道 `preset`；旧版二进制读 v6 文本不会报错。读进来后 `normalize_config` 会把 `refreshIntervalMs` 夹到 500..=10000、时间格式错的静音时段换默认值、不认识的 `preset` 换 `"web"`，被改过的字段名发一条 `configNormalized` 事件（归一化不写盘，下次保存自然落盘）。
- **降级拦截（版本比程序新）**：`version > 6` 时应用**只读不写**、原文件一字不动、**不产生 `.bad`**；人话：`这份设置是新版「上线助手」存的（版本 N），当前版本只认到 6。请先升级到最新版再打开；你的设置文件没有被改过。` 发 `configUnsupported` 事件。`cli config export` 仍能导（按已知键脱敏后原文照导，是自救路径）。

### 8.4 state.json（`core/src/state.rs`，`stateVersion` = 1）

| 字段 | 类型 | 说明 |
|---|---|---|
| `stateVersion` | number | 恒为 1 |
| `dayKey` | string | 本机自然日 `"YYYY-MM-DD"`；变化时重置当日计数 |
| `lastSavedAt` | number | 最近一次落盘时刻（epoch ms） |
| `updateInfo` | object | 最近一次更新检查：`{status, latest, url, checkedAt, error}`，`status` = `unknown`/`upToDate`/`available`/`error` |
| `tunnels` | object | key = 隧道 id：`{lastFailure, protectionCheck, stats}` |
| `tunnels[id].lastFailure` | object / null | `{ts, reason, code}`；连上后清空，重启不失忆 |
| `tunnels[id].stats` | object | `{reconnectsToday, connectedToday, lastConnectedAt}` |
| `tunnels[id].protectionCheck` | object | 最近一次云端保护核对，见下 |
| `services` | object | key = 服务 id：`{lastFailure}`；health 变 up 时清空 |
| `trash` | array | 删除撤销墓碑，最多 **20** 条、存活 **10 分钟**：`{token, kind, deletedAt, expiresAt, item, dependencies}`；`kind` = `service`/`tunnel`/`server` |
| `servers` | object | key = 服务器 id：云端环境识别缓存（`scan_server_env` / `check_tunnel_cloud` 的 `env` 结果） |

`protectionCheck`（冻结结构）：

```json
{
  "status": "unknown | ok | drift | error",
  "ts": 1760000000000,
  "source": "manual | auto | null",
  "cloud": { "confFound": true, "passwordOn": true, "ipOn": false, "allowIps": [], "htpasswdExists": true },
  "expected": { "password": true, "ip": false, "allowIps": [], "url": "comfy.example.com" },
  "driftFields": ["conf", "password", "ip", "allowIps", "htpasswd"],
  "error": null,
  "extra": { "https": {}, "port": {}, "files": [], "env": null, "clientIp": null, "audit": null, "warnings": [] }
}
```

- `status` = `unknown`（还没核对）/ `ok`（和设置一致）/ `drift`（看 `driftFields` 是哪几项对不上）/ `error`（检查本身没跑成，看 `error` 人话）。
- `source` = `"manual"`（界面「检查云端」/`check_tunnel_access`/`check_tunnel_cloud`）/ `"auto"`（后台自动漂移复检，见 5.9）/ `null`（老记录没写来源）。
- `extra` 只透传、不参与漂移判定；**不允许**放密码哈希/token/密钥内容。

### 8.5 events.jsonl（`core/src/events.rs`，行版本 `v` = 1）

一行一个紧凑 JSON：

```json
{"v":1,"ts":1760000000000,"kind":"protection","level":"warn","code":"protectionDrift",
 "subject":{"type":"tunnel","id":"tun-abc123","name":"comfyui 发布"},
 "message":"云端配置和设置对不上（密码）","data":{"driftFields":["password"]}}
```

| 字段 | 取值 |
|---|---|
| `kind` | `app` / `tunnel` / `service` / `protection` / `update` / `doctor` |
| `level` | `info` / `warn` / `error` |
| `code` | 第 8.7 节的原因码；没有就是 null |
| `subject` | `{type, id, name}`；应用级是 `{"type":"app","id":"app","name":"上线助手"}` |
| `message` | 大白话中文短句，≤200 字符 |
| `data` | 可选附加数据；**禁止**放密码哈希、token、密钥内容、明文密码 |

写入纪律：同样的事件 10 秒内只写一条（去重）；单会话每秒最多 10 条，超了丢弃并写一条「事件太密集，后面 10 秒不记了」的 warn（最多一次），随后静默 10 秒；写失败绝不 panic、绝不阻塞。`sys-notice` 这类一次性提示**不进** events.jsonl。

### 8.6 状态词与原始值对照

| 原始 `status` | 界面中文 | CLI 中文 |
|---|---|---|
| `stopped` | 没启动 | 没启动 |
| `starting` | 正在启动 | 正在启动 |
| `running`（服务） | 在跑 | 在跑 |
| `not_listening` | 进程在但端口不通 | 进程在但端口不通 |
| `failed` | 起不来（已停止自动重试） | 起不来 |
| `running`（隧道） | 正在发布 | 正在发布 |
| `connecting` | 连接中 | 连接中 |
| `disconnected` | 掉线重连中 | 掉线重连中 |
| `error` | 出错 | 出错 |
| `paused` | 已暂停 | 已暂停 |
| `unknown` | （离线读配置时） | 不知道（应用没在跑） |

其他字段语义：

- `health`（服务）：`up`（端口在听）/ `down`（探测过，不通/已停/失败）/ `unknown`（还没探过）。探测每 2 秒一次，**连续 3 次失败**才判 down（约 6 秒），任何一次成功立即清零。HTTP 模式（`probeKind: "http"`）收到任何字节都算活（401/403/404/500 都算），连接被拒/超时才算死。
- `retryState`（隧道）：`active`（会重试）/ `stopped`（不重试了，比如用户暂停、连续失败已停）；`attempt` 第几次重试；`retryInSecs` 还有几秒重试（不重试时 null）；退避 2→30 秒（逐次翻倍 + 抖动，稳定 60 秒重置）。
- `serviceOk`：本机服务标的是否「在跑」（端口真的通）；`false` 时界面标「本机服务没在跑（访问会失败）」。
- `serviceStatus`：本机服务的原始状态词。
- `reason` / `reasonCode`：最近一次出错的人话原因 + 原因码（见 8.7），只有坏状态才有；连上后自动清掉。
- `hasPassword`：`accessHash` 非空就是 `true`。
- `logPath`：本机服务日志的完整路径；`logOverflowed`：运行中日志已超 5MB、需要手动轮转（界面上给入口 `restart_service_rotate_logs`）。
- `jobGuardOk`：子进程保底清理（作业对象）是否装配成功；`false` 时有残留进程风险，看 `jobGuardError` 人话。
- `configTrusted`：配置是否可信；`false` = 读坏/降级拦截，写操作会被拒。

### 8.7 原因码（`reason` / `reasonCode` / 事件的 `code`）

`core/src/reason.rs` 的 42 个码（消息是人话，这里只列码与含义）：

| 码 | 含义 |
|---|---|
| `sshMissing` | 找不到 ssh 命令 |
| `netTimeout` / `connRefused` / `connReset` / `dnsFail` | 网络：超时 / 被拒 / 被掐断 / 解析不了 |
| `authDenied` / `keyUnusable` | 登录被拒 / 密钥文件用不了 |
| `hostKeyChanged` | 服务器指纹变了（TOFU 拒绝，要人去核对） |
| `remotePortBusy` | 云端端口被占 |
| `spawnFail` / `exitUnknown` / `signalExit` / `retryStopped` | 进程起不来 / 退出原因不明 / 被信号结束 / 不再重试 |
| `serverMissing` / `serviceMissing` | 引用的服务器 / 服务不在了 |
| `configUntrusted` / `configUnsupported` / `configCorrupt` / `configNormalized` / `configMigrated` / `configSaved` | 配置读坏 / 版本过高 / 损坏留底 / 归一化 / 迁移 / 已保存 |
| `cleanupFail` | 残留清理失败（没动别人的进程） |
| `startFail` / `crashLoop` / `hung` / `exited` / `autostartFail` | 本机服务：启动失败 / 连续崩溃停重试 / 假死 / 已退出 / 自启失败 |
| `appStarted` / `appExited` / `stateReset` | 应用启动 / 退出 / 运行状态重置 |
| `deleted` / `restored` | 删除 / 撤销 |
| `updateChecked` / `updateAvailable` | 更新检查 / 发现新版 |
| `protectionChecked` / `protectionDrift` / `protectionError` | 云端核对 / 漂移 / 检查失败 |
| `doctorRan` | 体检跑过 |
| `tunnelConnected` / `tunnelDown` / `serviceUp` / `serviceDown` | 状态变化事件 |

---

## 9. 常用配方

> 下面例子里 `<...>`、域名、端口、账号、密码、IP、路径**全是占位**：每一项都必须来自用户或命令输出（见铁律 0），**禁止照抄示例、自己编**。id 用 `cli services --json` / `cli servers --json` / `cli tunnels --json` 查真值。

### 配方 1：把本机 8188 发布到公网（从零开始）

> 本配方里的 `comfy.example.com`、`13000`、`me`、`你的访问密码`、`D:\AI\ComfyUI-aki-v1.7\...`、`srv-7a8b9c` **全是占位**；缺域名 / 云端端口 / 服务器地址账号 / 访问用户名密码 / 启动命令时，先按铁律 0 问用户，别照抄。

```bash
# 0. 确认应用在跑（退出码 3 = 没在跑；先 cli app start，或请用户手动打开应用）
cli app status --json

# 1. 看本机在听哪个端口、是哪个进程在听（记下端口和 pid）
cli ports --json
# → [{"port":8188,"pid":4108,"process":"python.exe","exe":"D:\\AI\\ComfyUI-aki-v1.7\\python\\python.exe"}]

# 2. 让应用读出这个进程的启动方式（专用方法，详见 5.10）
cli call inspect_process_launch_info '{"port":8188,"expectedPid":4108}' --json
# → 拿到 exe / args / workdir；先看 warnings：
#   · warnings 非空（multipleOwners / processGone / portOwnerChanged）或 pidMatched=false
#     → 这份信息不可靠：**停下来，把 warnings 转告用户并问清楚启动命令**，不要拿它登记服务
#   · 读不到启动方式 → 只登记端口（不填 --exe，纯监控），并告诉用户「只能看状态、不能帮你启动」
#   · warnings 里有 sensitiveArgs（参数疑似带密钥）→ 保存前必须让用户明确确认

# 3. 登记本机服务（已登记过就跳过；纯监控就不填 --exe/--args/--workdir）
cli service add --name comfyui --port 8188 \
  --exe "<步骤 2 读到的程序路径>" \
  --args "<步骤 2 读到的参数>" --workdir "<步骤 2 读到的工作目录>" \
  --watch --json
# → {"ok":true,"result":{"id":"svc-1a2b3c"}}

# 4. 找服务器 id；没有就先新增（host / user 必须由用户提供，别猜）
cli servers --json
cli server add --name 香港-1 --host <服务器地址> --user <登录账号> --json

# 5. 先确认服务器连得上，再配免密
cli server test <服务器id> --json
# → {"ok":true,"result":{"message":"连接成功，用时 0.8 秒"}}
echo '服务器登录密码' | cli server setup-key <服务器id>   # 密码走 stdin，不进命令行、不进历史
cli server test <服务器id> --json                        # 确认换成密钥后也能连

# 6. 建隧道并开始发布（显式要求密码，别用默认的 none；url / remote-port / access-user 都来自用户）
cli tunnel add --service svc-1a2b3c --server srv-7a8b9c \
  --remote-port <用户规划的云端端口，避开 80/443> \
  --url <用户给的域名，只填纯域名> \
  --access password --access-user <用户给的用户名> --access-password '用户的访问密码' --json
# 两个门都要（密码 + 白名单）就再加：--access ip --access-ips "1.2.3.4"
# → {"ok":true,"result":{"id":"tun-4d5e6f"}}

# 7. 生成云端 nginx 配置（把片段交给用户，让他自己贴：面板 → 站点 → 配置修改 → 重载）
cli nginx tun-4d5e6f

# 8. 用户贴完并重载后，请他用 https:// 打开一次（验收必须做这一步，agent 不能代宣告完成）
#    看到业务页 / 401 密码框 / 403 白名单拦截都算线路通；
#    网址写的是 https 但打不开 → 回面板把证书和「强制 HTTPS」开上。

# 9. 核对与体检（三项都过才算发布完成）
cli tunnels --json                    # status 是不是 "running"
cli logs tun-4d5e6f --lines 20        # 看有没有 "remote forward success"
cli tunnel check tun-4d5e6f --json    # 云端实际生效的保护和设置对不对得上
cli doctor --deep                     # 云端配置 / 公网访问（https）/ 域名解析 三步都要 pass
```

**发布完成的定义**：`tunnels --json` 里 `status = "running"`、`tunnel check` 不报不一致、`doctor --deep` 的「云端配置 / 公网访问 / 域名解析」不报 `fail`、**用 `https://` 打开过一次**。只有 80 端口的站点不算发布完成（**basic auth 必须配 HTTPS**）。

### 配方 2：看某条隧道为什么掉线

> `<隧道id>` / `<服务id>` 用 `cli tunnels --json` / `cli services --json` 查真值；示例里的域名、端口、IP、密码都是占位，**值来自用户，禁止照抄**。诊断命令都是只读的，只能看不能改。

```bash
cli tunnels                    # 人话列表里坏状态会带 "↳ 原因"
cli tunnels --json             # 结构化：status / reason / reasonCode / retryState / attempt / serviceOk
cli logs <隧道id> --lines 200  # 隧道侧日志（ssh 的 debug 输出）
cli logs <服务id> --source service --lines 200   # 本机服务侧日志（老写法 --kind service 也行）
cli logs --source app --lines 100      # 事件 + 应用日志（"为什么断的"往往在这里）
cli ports --json               # 本机端口到底通不通、是哪个进程
cli doctor                     # 一步到位看九项；--deep 才连云端和域名
```

判读：

| 看到什么 | 说明 |
|---|---|
| `status: "disconnected"` | 云端联系不上，会自动重试（2s 起、最长 30s 间隔） |
| `status: "connecting"` 且日志一直失败 | 看日志里的 `Permission denied` / `Connection refused` / `remote port forwarding failed` |
| `remote port forwarding failed` | 云端端口被占；应用会尝试精确清理残留 sshd 会话后重连 |
| `serviceOk: false` | 问题在本机：服务没在跑（先解决它，隧道没错） |
| `reason` 有值 | 直接读这句人话；旁边 `reasonCode` 是机器码（见 8.7） |
| `retryInSecs` 有值 | 正在退避重试，别干扰；等它自己连，实在不想等就先停后启（`tunnel stop <id> --yes` → `tunnel start <id>`） |

### 配方 3：给隧道加 / 换访问密码（也可以加 IP 白名单，两个能一起开）

> 下面 `me`、`新密码`、`1.2.3.4`、`<隧道id>` 全是占位：用户名 / 密码必须由用户给，白名单 IP 由用户给（可以先用「公网 IP 助手」查到真实出口 IP），隧道 id 用 `cli tunnels --json` 查。**禁止照抄示例值**。

方式 A（简单，密码会短暂出现在本机命令行里）：

```bash
cli tunnel edit <隧道id> --access password --access-user me --access-password '新密码'
# 再加 IP 白名单（两个条件都要满足）：
cli tunnel edit <隧道id> --access password --access ip --access-ips "1.2.3.4,5.6.7.8"
cli nginx <隧道id>     # 重新复制片段 + 密码文件内容，贴回面板
cli tunnel check <隧道id> --json   # 核对云端实际生效的保护
```

方式 B（推荐给 agent / 不想让密码进命令行）：

```bash
# 1. 在本机算哈希（密码走 stdin，不进命令行；会只读探测一次服务器支持的格式）
echo '新密码' | cli server hash-password <服务器id>
# → $2y$12$xxxxxxxxxxxxxxxxxxxxxxyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy

# 2. 用 cli call 直接把哈希写进隧道（先取当前 name/remotePort）
cli tunnels --json
cli call update_tunnel '{"id":"<隧道id>","name":"comfyui 发布","remotePort":13000,"access":["password"],"accessUser":"me","accessHash":"$2y$12$xxxxxxxxxxxxxxxxxxxxxxyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy"}' --json
# → {"ok": true, "result": {"ok": true}}
# 密码 + 白名单一起要："access":["password","ip"]，再带 "accessDetail":"1.2.3.4,5.6.7.8"

# 3. 云端配置重新贴一遍（关键：不重贴，旧密码还有效）
cli nginx <隧道id>

# 4. 核对云端实际生效的保护，和设置对不上就重新贴
cli tunnel check <隧道id> --json
```

只要白名单：`--access ip --access-ips "1.2.3.4,5.6.7.8"`；要密码 + 白名单：`--access password --access ip --access-ips "..."`（两个条件都要满足）；换回无保护：`--access none`（不推荐）。

**【铁律】密码开着就必须让用户用 https:// 打开**：先确认云端有 443 证书（`cli tunnel check` 看不到 HTTPS，用 `cli call check_tunnel_cloud '{"id":"<隧道id>"}' --json` 看 `extra.https.enabled` / `certDays` / `redirect80`），再看 80 端口是否强制跳 https；如果 `plainOpen=true` 或 `redirect80=false`，告诉用户去面板打开「强制 HTTPS」，否则不要宣告发布完成。

### 配方 4：配免密登录

```bash
cli servers --json        # 找到 id，确认 keyPath 是 null（或密钥失效）
echo '服务器登录密码' | cli server setup-key <服务器id>
cli servers --json        # keyPath 应变成 ...\keys\<服务器id>_ed25519
```

注意：

- 密码从 stdin 进，**不要**写成参数。
- 服务器不允许密码登录时会返回人话原因；不要重复硬试，让用户手动配密钥。
- 配好后，新密钥对**之后启动**的隧道生效；已经在跑的隧道不会自动换密钥，需要先停再开才会用上新密钥（界面：点卡片上的「暂停」，再点「开始发布」；命令行：`cli tunnel stop <隧道id> --yes`，再 `cli tunnel start <隧道id>`）。

### 配方 5：换服务器

`cli tunnel edit` **不能换服务器**（源码限制，参数里没有 server）。做法：

```bash
# 1. 记录老隧道的配置
cli tunnels --json

# 2. 在新服务器上建一条新隧道（新云端端口必须是新服务器上没被占用的）
cli tunnel add --service <服务id> --server <新服务器id> --remote-port 13001 \
  --url <同一个或新的网址> --access password --access-user me --access-password '密码' --json

# 3. 等新隧道 status 变成 running，再从外网验证一次
cli tunnels --json
cli nginx <新隧道id>      # 新服务器上的 nginx 也要贴
````

```bash
# 4. 把 nginx 贴好、用 https:// 打开一次、跑一次体检
cli tunnel check <新隧道id> --json
cli doctor --deep

# 5. 确认没问题后，删掉老隧道（老服务器上占的端口随连接断开释放；删除要确认）
cli tunnel remove <老隧道id> --yes
```

⚠️ 涉及「用户正在用」的隧道（本仓库环境里那条叫 pi-web）**任何情况下都不要动**。

### 配方 6：应用没在跑

```bash
cli app status --json          # → {"ok":true,"result":{"running":false,"offline":true,...}}，退出码 3
cli app start                  # 在 cli.exe 同目录找 app.exe / 上线助手.exe 拉起来
cli app status --json          # 等它 ready（app start 内部最多等 30 秒）
cli status --json              # 现在能拿到实时状态了
```

如果 `cli app start` 找不到程序：说明主程序不在 `cli.exe` 同一个文件夹里，或者名字不在 `app.exe` / `Shangxian.exe` / `上线助手.exe` 这三个里——请让用户手动打开应用（命令行照样能连上）。

### 配方 7：体检 / 打诊断包（用户说「不知道哪出了问题」时）

```bash
cli doctor                    # 先快速体检（不连服务器，秒出）
cli doctor --deep --json      # 要连云端/域名时用；单步 20 秒、整次 60 秒上限
# 有没过就直接让脚本退出码 1（不算命令错误）：
cli doctor --strict || echo "有项没过，按上面的「怎么办」逐条处理"

cli diag --out 诊断包.zip     # 还查不出来就打个脱敏诊断包，让用户发给帮忙排查的人
cli stats                     # 附一份运行统计：多久、重连几次、是不是在退避重试
```

纪律：诊断包**不要公开贴到网上**；`doctor` / `diag` 都不会改配置，可以放心跑。

### 配方 8：误删了怎么恢复

```bash
cli undo list        # 看还能撤销哪些（删除后 10 分钟内，最多 20 条）
cli undo             # 撤销最近一次（= cli undo last）
cli undo und-3f9a1c2d
```

- 隧道恢复后是**暂停**状态：要 `cli tunnel start <id>` 才会重新发布。
- 服务只恢复登记信息，不会自动启动。
- 超过 10 分钟找不回，只能重新建。

### 配方 9：备份 / 迁移配置到另一台电脑

```bash
# 旧机器：导出（默认脱敏：不含密码哈希、不含密钥路径）
cli config export --out 备份.json
# 不想要密钥路径也行（默认就没有）；要带上就加 --with-paths（仍不含密码哈希）

# 新机器：先看一眼会改什么，再导入
cli app start
cli config import 备份.json --dry-run
cli config import 备份.json --yes                    # 按 id 合并（推荐）
cli config import 备份.json --mode replace --yes     # 整份替换（先自动备份）
```

- 用脱敏文件导回来**不会把密码和密钥清空**：`hasPassword=true` 保留原哈希，`keyPathPresent=true` 保留原密钥路径。
- 导入会改设置：交互终端问 y/N；管道 / 脚本必须在 `--yes`（否则退出码 1）。

---

## 10. 输出与退出码约定

| 情况 | 人类输出 | `--json` 输出 | 退出码 |
|---|---|---|---|
| 成功（只读：status/services/tunnels/servers/ports/logs/nginx/stats/doctor/diag/methods/guide） | 中文表格 / 原文 | `{"ok": true, "result": <结果>}` | 0 |
| 成功（写操作） | `✓ ...` | `{"ok": true, "result": <结果>}` | 0 |
| 成功（`cli call`） | 结果按人话打印 | `{"ok": true, "result": <结果>}` | 0 |
| 成功（`server hash-password`） | 哈希 | `{"ok": true, "result": {"hash": "..."}}` | 0 |
| 成功（`logs --format json` 不带 `--json`） | JSONL：一行一条，无信封 | — | 0 |
| 交互确认里选「不」 | `已取消，什么都没做。` | `{"ok": true, "result": {"cancelled": true}}` | 0 |
| `app status` 发现应用没在跑 | `✗ 应用没在跑（offline）...` | `{"ok": true, "result": {"running": false, "offline": true}}` | **3** |
| 命令需要应用、但应用没在跑（stats/diag/logs/真撤销/config import/写操作…） | `✗ 『上线助手』没在运行...`（stderr） | `{"ok": false, "error": "..."}` | **3** |
| 体检有没过（`doctor --strict`） | `✗ 自检有 N 项没过` | `{"ok": true, "result": <报告>}` | **1**（不算命令错误） |
| 业务失败（方法不存在、拒绝删除、端口冲突、需要确认但非交互…） | `✗ 人话`（stderr） | `{"ok": false, "error": "人话"}` | 1 |
| 参数 JSON 不是合法 JSON | `✗ 参数不是合法 JSON：...` | `{"ok": false, "error": "..."}` | 1 |
| 命令拼错 / 缺必填参数 | clap 错误 + 一句中文下一步提示 | — | 2 |
| `--help` / `--version` | 帮助 / 版本 | — | 0 |

失败时 JSON 的键顺序是字母序：`error` 在前、`ok` 在后（`serde_json` 默认行为）。

危险操作的确认策略（`tunnel stop/remove` / `service remove` / `server remove` / `tunnel forget-key` / `app stop-all` / `config import`）：

- 有 `--yes`：直接做；
- 交互终端（能敲键盘）：先把要动哪个对象写清楚，再问 `y/N`（回车 = 什么都不做）；
- 非交互（管道 / 脚本 / agent）：直接拒绝、报错退出码 1，并提示加 `--yes`。
- 例外：`config import --dry-run` 不写盘、不用确认；`app stop-all` 在应用没在跑时退 0。

---

## 11. 已知坑与注意事项

| # | 事项 | 说明 |
|---|---|---|
| 1 | **不动用户正在用的东西** | 本仓库环境里用户正在用的是 pi-web 服务 + 它的隧道；其它环境按「谁在用」判断（用户当前会话依赖的通道、网页入口这类）。任何时候不暂停、不重启、不删除，拿不准先问 |
| 2 | `tunnel add --access` 默认 `none` | 界面向导默认 `password`，命令行默认**无保护**。agent 建隧道必须显式传 `--access password` 或 `--access ip`（可重复/逗号分隔，两个都传 = 两个条件都要满足） |
| 3 | `hash-password` 在本机算 | 优先 bcrypt（`$2y$12$…`），服务器 nginx 验不了时自动用兼容格式（`$apr1$…`）；**不需要服务器装任何东西**，但会只读探测一次服务器；只要服务器 id 存在于配置里就能算（探测失败按兼容格式兜底）。用 bcrypt 时密码超过 72 字节会被拒绝 |
| 4 | `tunnel add/edit --access-password` 也是本机算哈希 | 不依赖服务器；但密码会短暂出现在本机命令行，介意就用配方 3 的方式 B。`--access` 写错、`--access-password` 配不上 `--access password` 都会直接报错（不会静默丢掉保护） |
| 5 | `tunnel edit` 不能换服务/服务器 | 想换服务器用配方 5：先 add 新、验证、再 remove 旧 |
| 6 | `logs <不存在的id>` | 会直接报错「没找到隧道/本机服务「xxx」」并退出码 1（2026-10-11 已修） |
| 7 | `logs --kind` 只认 `tunnel` / `service` | 写别的值直接报错（退出码 1），不会当隧道日志处理 |
| 8 | `watch`/`autostart` 两种写法都行 | `service add` 是开关（不带值）；`service edit` 不带值 = 开、`--watch false` = 关、不传 = 不动 |
| 9 | 密码不要进命令行 | `server setup-key`、`server hash-password` 走 stdin（交互输也不回显）；`tunnel add/edit --access-password` 会短暂出现在本机命令行，agent 优先用配方 3 的方式 B |
| 10 | 写命令立刻落盘 | 应用会把 config.json 原子替换；界面和 CLI 看到的是同一份状态（同一个 Guardian） |
| 11 | 应用退出 = 隧道全停；本机服务保活 | 不要把「应用不在跑」当成「隧道照跑」。离线时状态是 `unknown`，不是 `running`。本机服务相反：应用退出/崩溃/强杀后它们继续跑，下次打开应用按端口认领（只按端口探测，不做归属核对）；想一并停掉用托盘「退出并停止全部本地服务」（停应用自己这次启动的整棵进程树 + 能按程序路径核对出归属的认领进程；核对不过的，如启动程序填成包装脚本、实际监听者是子进程，会保留并弹通知告知几个没停） |
| 12 | 单实例 | 再开一个 app 实例只会把已有窗口显示出来，不会起第二份看护；`cli app start` 已运行时会立刻返回 |
| 13 | debug 版 app.exe 不能单独分发 | 现在 `target/debug/app.exe` 是 dev 构建，会去连 `http://localhost:1420`。给别人装要在项目根目录跑 `bash build-release.sh`（先编 cli 再打包，只出 NSIS `.exe`，不出 `.msi`）；不要在 `app` 目录单独 `npm run tauri build`（缺 sidecar 会失败） |
| 14 | 安装版主程序名 | `tauri.conf.json` 的 `productName` 是 `Shangxian`，安装后 exe 可能叫 `Shangxian.exe`；`cli app start` 已同时找 `app.exe` / `Shangxian.exe` / `上线助手.exe`（2026-10-11 已加），三个都不在就请用户手动打开 |
| 15 | 云端端口两台电脑共享 | 应用只能发现本配置内的端口冲突；另一台电脑占用的云端端口会以 `remote port forwarding failed` 形式出现，不会提前拦。跨电脑别用同一端口 |
| 16 | nginx 是人工步骤 | 应用只生成片段，不自动下发、不自动 reload。改端口/改访问保护后必须提醒用户重新贴 |
| 17 | 掉线通知 | 隧道持续异常超过 3 分钟才弹一次，恢复再弹一次；用户暂停的不报。可在设置页关（`set_notify_enabled`） |
| 18 | `tunnel check` 读的是云端实际状态 | 应用里的设置不代表云端已生效。改完保护/贴完云端配置后用 `cli tunnel check <隧道id> --json` 核对（`confFound` / `passwordOn` / `ipOn` / `allowIps` / `htpasswdExists`），对不上就重新贴配置。它和界面「检查云端」同一条判定流水线（最长约 30 秒） |
| 19 | 站点目录 `vhostDir` | 密码文件/站点配置目录的解析顺序：服务器 `vhostDir` → 环境变量 `SHANGXIAN_HTPASSWD_DIR` → 默认 `/xp/panel/vhost`。站点配置在 `<目录>/nginx/<域名>.conf`，密码文件在 `<目录>/<域名>.htpasswd`；「检查云端」也按这个目录找 |
| 20 | CLI 改站点目录（原坑已修复） | 直接用 `cli server add/edit --vhost-dir <目录>`：edit 不写 = 保留原来的站点目录，写 `--vhost-dir ""` = 清空；不用再 `cli call save_server` 带全字段 |
| 21 | 配置读坏不留空转 | 应用读坏时把原文件留底成 `config.json.bad`、弹红条，并**禁止自动保存**（防空配置覆盖）；恢复好再重开应用 |
| 22 | 隧道参数变会自动重建 | 改服务器地址/SSH 端口/账号/密钥、改本机端口、改云端端口，应用都会停掉旧连接按新参数重建；连接失败的人话原因会写进 `reason` 并显示在卡片上 |
| 23 | 看护/开机自启必须先填启动程序 | 只监控的服务（`launchExe` 为空）开不了看护/自启，应用会拒绝。开机自启先认领已在跑的端口（约 13 秒宽限），不重复启动 |
| 24 | 日志与旧文件 | 隧道日志按 id 命名且 5MB 轮转保留 2 份；服务日志按 id 命名（5MB×2）；`app.log` 2MB×2；`events.jsonl` 2MB×2。按名字命名的旧日志、旧轮转文件不再被界面/`cli logs` 读取 |
| 25 | **basic auth = 必须 HTTPS** | 密码开着但只有 80 口的站点不算发布完成。用 `cli call check_tunnel_cloud '{"id":"..."}' --json` 看 `extra.https`（`enabled` / `redirect80` / `plainOpen`），对不上让用户去面板开证书 /「强制 HTTPS」 |
| 26 | **不要自动写服务器配置 / 不要装 fail2ban / 不要动全局 nginx.conf** | 应用只生成片段和文案；`nginx -t` + reload 由用户自己来。防爆破先推白名单；`limit_req` 要改 `http{}`，默认绝不生成，只给人话强警告。agent 不要把这类操作“顺手”做了 |
| 27 | TOFU 主机指纹 | ssh 连接用 `StrictHostKeyChecking=accept-new`：首连信任、指纹变了拒连。服务器重装/换机器后用 `cli tunnel forget-key <隧道id> --yes` 重信任（会重新连）；平时**不要**随便重置指纹 |
| 28 | 撤销窗口 | 删除后 **10 分钟**内、最多 **20** 条可 `cli undo`；隧道恢复为**暂停**状态、服务不自动启动；过期就找不回 |
| 29 | 单实例是「全局」的 | 互斥量名 `Global\Shangxian.App.v1`，远程桌面另开账号会话也不能起第二个实例（防两边抢云端端口）；同一个会话里再启动会直接唤起已有窗口，跨会话重复启动才弹提示并退出，两种都不会双开 |
| 30 | 崩溃自恢复有限次 | 用 Windows `RegisterApplicationRestart` 登记的自动重启（`--hidden --recovered`，不建计划任务）；10 分钟窗口内第 3 次就暂停自动恢复，避免崩溃循环。设置页可关（`set_crash_recovery`），关掉后标记文件 `crash-recovery.off` |
| 31 | 窗口是延迟建的 | `--hidden` / 崩溃恢复启动时只进托盘不建窗；点托盘/再启动实例才建窗（省内存，也避免开发环境干扰）。关窗 = 销毁窗口，不是退出应用 |
| 32 | 事件日志有限流 | 同样的事件 10 秒内只写一条；每秒最多 10 条，超了丢弃并静默 10 秒；`sys-notice` 一次性提示不进事件流 |
| 33 | 体检/诊断包 | `doctor` 报告落 `logs/doctor-last.json`（离线也落）；`diag` 包里是脱敏后的配置/状态/日志，不含哈希和密钥，但含域名/路径，**别公开贴** |

---

## 附录：`cli guide` 原文

```
# 上线助手命令行速查（给 AI agent）

这个命令行是「上线助手」桌面应用的遥控器。应用才是真正看护隧道的那个，
它常驻在系统托盘里；命令行只负责查询和改配置。

## 三条铁律（务必遵守）
1. **严禁操作 pi-web**：本机 pi-web 服务及其隧道是用户正在使用的通道，暂停/重启会直接中断会话。
2. 隧道暂停后外部将无法访问；不确定是否要停止时，请先询问。
3. 删除本机服务/服务器前，请先查看它是否被隧道引用（存在引用时应用会拒绝并给出说明）。

## 危险操作（删除 / 暂停 / 重置指纹 / 导入配置）需要确认
- 交互终端（人工输入命令）：先打印将要操作的对象，再询问 y/N；回车 = 不执行任何操作。
- 非交互（管道 / 脚本 / AI agent）：必须显式加 --yes，否则拒绝执行并报错。
      cli tunnel stop <隧道id> --yes
      cli tunnel remove <隧道id> --yes
      cli service remove <服务id> --yes
      cli server remove <服务器id> --yes
      cli tunnel forget-key <隧道id> --yes   # 仅在服务器重装过、指纹变化时使用
      cli app stop-all --yes                  # 升级 / 卸载前停止全部隧道和服务（安装器会调用它）
      cli config import 备份.json --yes       # 导入配置；如需先查看将修改的内容，请加 --dry-run

## 最常用的命令
    cli status --json                       # 一次查看全部：应用 + 服务 + 隧道 + 服务器
    cli services --json                     # 本机服务清单（id、端口、状态、启动方式）
    cli tunnels --json                      # 发布隧道清单（服务→服务器、云端端口、状态）
    cli servers --json                      # 云端服务器清单
    cli ports --json                        # 本机正在监听的端口（带进程名和程序路径）
    cli tunnel start <隧道id>               # 开始发布
    cli tunnel stop <隧道id>                # 暂停发布（需要确认；非交互加 --yes）
    cli service start <服务id>              # 启动本机服务（使用已登记的启动方式）
    cli service stop <服务id>               # 停止（非应用启动的进程会先拦截；已配置启动方式时加 --force 可实际停止，仅监控的服务会被拒绝）
    cli logs <隧道id> --lines 80            # 查看隧道日志（--kind service 查看服务日志，其它值会报错）
    cli logs --source all --format json --json  # 事件 + 应用日志 + 所有隧道/服务日志（合并读取）
    cli logs <隧道id> --archive 1           # 查看上一份轮转备份（0/1/2）
    cli nginx <隧道id>                      # 生成需要粘贴到云端面板的 nginx 配置片段
    cli tunnel check <隧道id>               # 到云端查看实际启用的访问保护（只读，不修改服务器）

## 体检 / 排查
    cli doctor                              # 体检：应用、文件夹、SSH、端口、服务器、隧道、云端、公网、域名
    cli doctor --deep                       # 深度检查（会连接服务器和域名，耗时较长）
    cli doctor --strict                     # 有未通过项就直接退出码 1（这不算命令错误，是「检查未通过」）
    cli stats                               # 运行时长、隧道重连情况、今日重连次数、更新状态
    cli diag --out 诊断包.zip               # 生成脱敏诊断包（发给排查人员）
    cli server test <服务器id>               # 测试连接：真连一次 SSH（最长 15 秒）
    cli app stop-all --yes                  # 升级 / 卸载前停止全部隧道和服务（安装器会调用它）
    cli app update-check                    # 检查是否有新版本（仅检查，不下载、不安装）

## 撤销删除
    cli undo                                # 撤销最近一次删除（= cli undo last）
    cli undo list                           # 查看可撤销的记录（删除后 10 分钟内、最多 20 条）
    cli undo und-3f9a1c2d                   # 按令牌撤销指定的一条
- 隧道撤销后一律是暂停状态，如需继续发布请执行 cli tunnel start <id>。
- 删除命令成功后，文本输出会提示「（10 分钟内可以撤销：cli undo last）」。

## 配置类
    cli service add --name 我的网站 --port 8188 --exe C:\path\python.exe --args "main.py" --watch
    cli service edit <服务id> --port 8189 --watch true
    cli service edit <服务id> --watch false              # --watch / --autostart 也可以不带值（= 开启）
    cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000 --url 我的域名.com
    cli tunnel add ... --access password --access-user 用户名 --access-password 密码
    cli tunnel add ... --access password --access ip --access-ips "1.2.3.4,5.6.7.8"   # 密码 + 白名单两种保护同时启用
    cli tunnel edit <隧道id> --access password --access-password 新密码   # 更换密码
    cli tunnel edit <隧道id> --access none   # 取消访问保护（不传 --access = 保持原样）
    cli server add --name 香港-1 --host api.example.com --user user
    cli server add --name 宝塔机 --host api.example.com --user root --vhost-dir /www/server/panel/vhost   # 站点目录：密码文件与站点配置的存放位置
    cli server edit <服务器id> --vhost-dir /www/server/panel/vhost   # 不写 = 保留原有站点目录，不会被清除
    echo 服务器密码 | cli server setup-key <服务器id>     # 使用密码登录一次，自动配置免密登录
- --access 仅支持 password / ip / none，拼写错误会直接报错（不会静默变成「无保护」）；--access= 这种空值也会直接报错，如需取消保护请写 --access none。
- --access-password 必须配 --access password，--access-ips 必须配 --access ip，不匹配会报错。
- 每次修改后都会回显最终保护状态，例如「访问保护：密码 + 白名单（只允许指定 IP），已生效」。

## 配置备份（导出 / 导入）
    cli config export --out 备份.json     # 导出（默认脱敏：不含密码哈希、不含密钥路径）
    cli config export --with-paths        # 同时导出密钥路径（仍然不含密码哈希）
    cli config import 备份.json --dry-run # 先查看将修改的内容（不写盘、无需确认）
    cli config import 备份.json --yes     # 合并导入（按 id 合并，未写的字段保持原值）
    cli config import 备份.json --mode replace --yes   # 整份替换（导入前自动备份）
- 导入会修改设置：交互终端会询问 y/N；管道 / 脚本 / agent 必须加 --yes，否则退出码 1。
- 使用导出的脱敏文件导回时，密码哈希和密钥路径会保留（不会被清空）。

## 应用未运行时怎么办
    cli app status        # 查看是否在运行；未运行时退出码 3，JSON 中 offline: true
    cli app start         # 启动应用（静默进入托盘）
    cli app stop-all --yes # 停止全部发布隧道和应用启动的本机服务（升级 / 卸载前使用）
- `cli app stop-all` 是特例：应用未运行时直接输出「没有需要停止的内容」并退出码 0（安装器不会卡住）。
- 离线也能用的：status / services / tunnels / servers（直接读取配置文件，状态字段为 unknown）、
  cli doctor（本地可执行的那几步）、cli undo list（读取运行记录）、cli config export（读取配置文件）。
- 需要应用运行的（离线时退出码 3）：stats / diag / logs / undo（实际撤销）/ config import / app update-check / server test。
- 修改设置、启停任何内容，都必须先执行 cli app start。

## 需要调用更底层接口时
    cli methods                       # 列出所有可调用的方法 + 参数格式
    cli call <方法> '{"参数":...}'    # 直接调用（通用入口）
- 老版本（没有子命令时）可以直接调底层方法：cli call test_server '{"serverId":"..."}'（等价于 cli server test <服务器id>）。

## 完整文档在哪
- 安装目录里（和 cli.exe 同一层）有一份 AGENTS.md：命令全表、配方、已知坑、安全红线，给 AI 助手看的完整说明；旁边还有 SKILL.md（速查技能）和 使用指南.md（给人看的使用指南）。
- 在线版：https://github.com/iranolly/shangxian/blob/main/docs/AGENTS.md

## 输出约定
- 默认输出中文文本；加 `--json` 输出机器格式（agent 一律加）。读命令、写命令都是同一个信封。
- 成功（status / services / tunnels / servers / ports / logs / nginx / stats / doctor / diag / methods / call / guide / undo / config / 各种改配置）：
  {"ok": true, "result": ...}
- 失败：{"ok": false, "error": "原因说明"}
- 退出码：0 = 成功（确认里选「不」也是 0）；1 = 出错（包括非交互下拒绝执行、undo 令牌过期、doctor --strict 有未通过项）；
  2 = 命令写法不对（参数名拼错、少值）；3 = 这个命令需要应用运行，但应用未运行（JSON 里 offline: true）。
- `cli logs --format json` 不带 --json 时输出 JSONL（一行一条，无信封，供管道使用）；
  带 --json 时才给上面的信封（`--format lines --json` 保持旧形状，result 里有 text）。
- `cli call` 不带 --json 时按文本方式打印返回值，加了才给上面的 JSON 信封。
- 输入密码（cli server setup-key）时屏幕上不回显，通过管道传入同样安全。
- `cli doctor --strict` 退出码 1 不是命令执行错误，而是「检查未通过」；文本输出会写「✗ 体检有 N 项未通过」。
```

---

## 文档版本

- 本版对应 2026-10-12 的全景修复 + 可靠性 / 生命周期改造；2026-10-13 完成文档改名（`AI-AGENT.md` → `AGENTS.md`）与可读性完善（补「先找到 cli.exe」「缺信息先问」两节、重排配方 1）；2026-10-15 使用手册改名 `使用指南.md`，并随发布包一起发（安装包 / 便携包里和 `AGENTS.md`、`SKILL.md` 同层）。
- ctl 底层方法 **60 条**（见第 5 节）；CLI：`server test` 在内的常用命令见第 4 节；`cli methods --json` 可列出全部方法。
- 配置文件 **v6**（新增 `settings` 与隧道 `preset`）；运行状态存 `state.json`，事件流存 `events.jsonl`。
- 本版新增 / 变更的 CLI：`stats` / `doctor` / `diag` / `undo` / `config export|import` / `app stop-all|update-check` / `tunnel forget-key` / `server test`。
- 任何地方对不上时，以代码和 `cli.exe` 的实际输出为准：`cli/src/main.rs`、`core/src/*.rs`、`app/src-tauri/src/ctl.rs`。
