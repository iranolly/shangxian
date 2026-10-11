# 上线助手 · AI Agent 文档

> 写给 AI agent / 自动化脚本。文中每一条命令、每一个字段，都能在 `cli/src/main.rs`、`core/src/*.rs`、`app/src-tauri/src/ctl.rs` 的源码或 `cli.exe` 的实际输出里核对到。
> 人类用户请读 [使用手册.md](使用手册.md)。

---

## 0. 一句话定位

**应用（托盘里的「上线助手」）是干活的；`cli.exe` 是遥控器。**

`cli.exe` 自己不建隧道、不管进程。它干的只有两件事：

1. 通过本机控制通道，把请求发给正在运行的应用（应用去执行、去看护）；
2. 应用没在跑时，只读的几个命令直接读配置文件，并明确告诉你「状态是 unknown」。

所以：**要改东西，先确认应用在跑。**

---

## 1. 铁律（先读，再动手）

| # | 规则 | 为什么 / 后果 |
|---|---|---|
| 1 | **绝不暂停 / 重启 / 删除 pi-web 本机服务和它的隧道**（以及任何标注为「用户正在用」的东西） | 那是用户当前会话正在走的通道，动它 = 立刻断掉用户会话 |
| 2 | 删除本机服务 / 服务器前，先看有没有隧道引用 | 应用会拒绝并列出是哪几条。**不要绕过**（不要手改 config.json 硬删） |
| 3 | 访问密码只在「设置时」出现一次，只存 apr1 哈希；配免密的登录密码只在内存里用一次 | 密码不落盘、不进日志。不要把密码写进 config.json、日志、`cli call` 的参数里 |
| 4 | 云端只做「普通 sshd + nginx」 | 不替用户装常驻程序、不改服务器 sshd 配置 |
| 5 | 应用退出 = 所有隧道停止 | 别人立刻访问不了。不要用「杀掉应用」当重启手段 |
| 6 | 隧道 `paused` = 别人访问不了 | 不确定要不要停，先问 |
| 7 | 端口写死、冲突报错，不自动漂移 | 改了云端端口必须重新贴 nginx 配置，否则 nginx 还指向老端口 |
| 8 | 只读命令可以随时跑；写命令（add/edit/start/stop/remove/setup-key）先想清楚再跑 | 写命令会立刻改用户的真实配置并落盘 |

---

## 2. 三十秒上手

```bash
# 开发版可执行文件（本仓库）
./target/debug/cli.exe app status --json     # 应用在不在
./target/debug/cli.exe status --json         # 一次看全：应用+服务+隧道+服务器
./target/debug/cli.exe guide                 # 内置速查（人话）
./target/debug/cli.exe methods --json        # 全部底层方法（32 个）
```

拿到一个陌生环境，先跑 `status --json`，再决定下一步。

---

## 3. 运行环境与输出约定

| 项目 | 约定 |
|---|---|
| cli 位置 | 开发版：`target\debug\cli.exe`（或 release）；分发给用户时，`cli.exe` 要和 `app.exe` 放同一个文件夹（`cli app start` 靠这个找人） |
| 名字 / 版本 | 程序名 `cli`，版本 `0.1.0`（`cli --version` 输出 `cli 0.1.0`） |
| 写操作前提 | 应用必须在跑（`cli app status --json` 看到 `"running": true`） |
| 只读四个命令 | `status` / `services` / `tunnels` / `servers` 离线也能读配置文件，状态字段为 `"unknown"` |
| 默认输出 | 中文人话，成功以 `✓` 开头，失败以 `✗` 开头（写到 stderr） |
| `--json` | 全局参数，位置随意。agent 一律加。失败输出 `{"ok": false, "error": "人话原因"}`，退出码 1 |
| 用法错误 | 命令拼错、缺必填参数由 clap 处理，退出码 **2** |
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
logs      读日志：隧道日志或本机服务日志
nginx     生成 nginx 配置片段（人工贴到面板上）
service   本机服务：新增/编辑/启停/删除
tunnel    发布隧道：新增/编辑/启停/删除
server    云端服务器：新增/编辑/删除/配免密
app       应用自己：启动、看状态
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

`--json`（应用在跑）：

```json
{
  "app": {
    "appVersion": "0.1.0",
    "autostart": false,
    "dataDir": "C:\\Users\\你\\AppData\\Roaming\\Shangxian",
    "notifyEnabled": true,
    "pid": 12345
  },
  "servers": [
    { "host": "api.example.com", "id": "srv-abc123", "keyPath": null,
      "name": "香港-1", "sshPort": 22, "user": "user" }
  ],
  "services": [
    {
      "autostart": true, "id": "svc-abc123",
      "launchArgs": "main.py", "launchExe": "C:\\Python311\\python.exe",
      "launchWorkdir": "D:\\web", "localPort": 8188,
      "logPath": "C:\\Users\\你\\AppData\\Roaming\\Shangxian\\logs\\service-comfyui.log",
      "name": "comfyui", "note": null, "reason": null, "status": "running",
      "tunnelCount": 1, "tunnelNames": ["comfyui 发布"], "watch": true
    }
  ],
  "tunnels": [
    {
      "access": "password", "accessDetail": null, "accessUser": "me",
      "enabled": true, "hasPassword": true, "id": "tun-abc123", "keyPath": null,
      "localPort": 8188, "name": "comfyui 发布", "reason": null,
      "remotePort": 13000, "serverId": "srv-abc123",
      "serviceId": "svc-abc123", "serviceName": "comfyui",
      "serviceOk": true, "serviceStatus": "running", "status": "running",
      "url": "comfy.example.com"
    }
  ]
}
```

> 字段差异（重要）：
> - 应用在跑时，`app` 没有 `running` 字段（用 `pid` 是否为 null 判断）；`services` 多 `reason`、`logPath`；`tunnels` 多 `reason`、`accessDetail`、`serviceOk`、`serviceStatus`、`keyPath`，**没有** `serverName`。
> - 应用没跑时，`app` 有 `running: false`、`pid: null`、`appVersion: null`，没有 `autostart`/`notifyEnabled`；`tunnels` 有 `serverName`，状态全为 `"unknown"`。

#### `cli services` / `cli tunnels` / `cli servers`

| 命令 | 作用 | 人话输出示例 |
|---|---|---|
| `cli services` | 本机服务列表 | `本机服务 3 台：` + 每台一行 |
| `cli tunnels` | 发布隧道列表 | `发布隧道 3 条：` + 每条一行（名字 → 服务 → 服务器 · 云端端口 · 访问方式；有网址/原因会各占一行） |
| `cli servers` | 云端服务器列表 | `云端服务器 2 台：` + `名字（id） user@host:port · 密钥 xxx / 默认密钥` |

`--json` 是 `status` 里对应数组的内容（服务数组 / 隧道数组 / 服务器数组），字段一致。

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

`--json`：

```json
[
  { "port": 8188, "pid": 12345, "process": "python.exe",
    "exe": "D:\\AI\\ComfyUI-aki-v1.7\\python\\python.exe" }
]
```

> `pid` / `process` / `exe` 都可能为 `null`（进程已退出或读不到）。

#### `cli logs <id>`

| 项 | 说明 |
|---|---|
| 作用 | 读日志尾部 |
| 参数 | `<id>`：隧道 id（默认）或本机服务 id；`--kind tunnel`（默认）/ `--kind service`；`--lines N`（默认 60） |
| 注意 | `kind` 只认字符串 `"service"`；传别的值都按隧道日志处理 |
| 边界 | id 不存在时直接报错退出（退出码 1），不会输出空内容 |

人话输出：直接打印日志原文。

```
debug1: remote forward success for: listen 127.0.0.1:19103, connect 127.0.0.1:18083
debug1: forwarding_success: all expected forwarding replies received
```

`--json`：

```json
{ "text": "debug1: remote forward success for: ...\n..." }
```

> 界面里看日志是最近 300 行；CLI 默认 60 行；底层方法 `get_logs` / `get_service_logs` 默认 200 行。

#### `cli nginx <id>`

| 项 | 说明 |
|---|---|
| 作用 | 生成某条隧道的 nginx 配置片段（人工贴到云端面板） |
| 参数 | `<id>`：隧道 id |
| 前提 | 应用在跑（需要读隧道 + 服务器配置） |

人话输出 = 片段 + 可选密码文件内容 + 步骤（每条步骤以 `· ` 开头）。真实例子（隧道没填网址、没有访问保护）：

```
# 由「上线助手」生成 · 服务：靶机演示
# 目标：本机 18083 → 云端回环 19103
server {
    listen 443 ssl;
    server_name 你的域名（服务「靶机演示」还没填网址）;
    # 证书沿用面板里这个域名的配置（面板保存后会自动带上）

    location / {
        proxy_pass http://127.0.0.1:19103;
        ...
        client_max_body_size 100m;
        # ⚠ 没有访问保护：任何人知道网址就能访问
    }
}

· 把下面的片段放进这台服务器上该域名的站点配置里（小皮面板 → 对应站点 → 配置修改）。
· 保存后重载 nginx，然后从外网打开一次网址确认。
· ⚠ 这个服务目前没有访问保护，建议在应用里给它设一个访问密码。
```

`--json`：

```json
{
  "htpasswd": null,
  "snippet": "# 由「上线助手」生成 · 服务：靶机演示\n...\n",
  "steps": ["把下面的片段放进...", "保存后重载 nginx...", "⚠ 这个服务目前没有访问保护..."]
}
```

> 访问保护不同时片段会带：`auth_basic "需要密码";` + `auth_basic_user_file /xp/panel/vhost/<域名>.htpasswd;`（`htpasswd` 字段是一行 `用户名:哈希`）；或 `allow <ip>;` × N + `deny all;`。

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
- 注意：add 的 `--watch` 是开关；edit 的 `--watch` **要写 true/false**。

人话输出：`✓ 已改好本机服务「svc-1a2b3c」`
`--json`：`{ "ok": true, "result": { "id": "svc-1a2b3c" } }`

#### `cli service start / stop / restart / remove`

| 命令 | 作用 | 人话输出 | 说明 |
|---|---|---|---|
| `service start <id>` | 用登记的启动方式拉起服务 | `✓ 已让「svc-1a2b3c」启动（几秒后 cli services 看结果）` | 没配启动方式会报 `✗ 这台服务没有配置启动方式（只能看状态，不能帮你启动）` |
| `service stop <id>` | 停止（只停应用自己启动的进程） | `✓ 已停止「svc-1a2b3c」` | 没配启动方式会报 `✗ 这台服务没配启动方式，是你自己开的程序，我不会去动它` |
| `service restart <id>` | 先停再启 | `✓ 已重启「svc-1a2b3c」` | |
| `service remove <id>` | 删除 | `✓ 已删除本机服务「svc-1a2b3c」` | 有隧道引用会拒绝：`✗ 还有 1 条隧道在用它（「靶机演示」），先把这些隧道删掉` |

四个命令的 `--json` 都是：

```json
{ "ok": true, "result": { "ok": true } }
```

### 4.3 发布隧道：`cli tunnel ...`

#### `cli tunnel add`

```
cli tunnel add --service <服务id> --server <服务器id> --remote-port <云端端口>
               [--name <隧道名字>] [--url <网址>]
               [--access none|password|ip] [--access-user <用户名>]
               [--access-password <密码>] [--access-ips "1.2.3.4,5.6.7.8"]
```

| 参数 | 必填 | 说明 |
|---|---|---|
| `--service` | 是 | 本机服务 id（不是名字） |
| `--server` | 是 | 云端服务器 id（不是名字） |
| `--remote-port` | 是 | 云端端口（写死，不自动漂） |
| `--name` | 否 | 不填 = `<服务名> 发布` |
| `--url` | 否 | 公网网址，可空 |
| `--access` | 否 | `none`（默认）/ `password` / `ip`。**默认 none = 谁都能进，建隧道时务必显式传值** |
| `--access-user` | 否 | 访问用户名（`access=password` 用） |
| `--access-password` | 否 | 访问密码。会先 ssh 到服务器算 apr1 哈希再存，**密码本身不落盘**；服务器连不上/没 openssl 会**整条命令失败** |
| `--access-ips` | 否 | IP 白名单，逗号分隔（`access=ip` 用） |

行为：新增后 `enabled: true`，应用立刻开始发布。

人话输出：

```
✓ 已建好隧道「comfyui 发布」并开始发布（id=tun-4d5e6f）
```

`--json`：

```json
{ "ok": true, "result": { "id": "tun-4d5e6f" } }
```

失败例子（云端端口冲突，实测）：

```
✗ 云端端口 19103 已经被这个服务器上的其他隧道用了
```

#### `cli tunnel edit`

```
cli tunnel edit <隧道id> [--name X] [--remote-port N] [--url X]
                [--access none|password|ip] [--access-user X]
                [--access-password X] [--access-ips X]
```

- 只写要改的字段。**不能换本机服务、不能换服务器**（参数里没有 service/server）。
- `--access-password` 不填 = 密码保持不变。选了 `password` 但从来没有密码哈希，会报：`✗ 这条隧道选了「要密码才能进」，请先设置访问密码`。
- 改了云端端口：应用会停掉旧隧道，按新端口重连。**记得重新贴 nginx 配置。**

人话输出：`✓ 已改好隧道「tun-4d5e6f」（会断一下再自动连上）`
`--json`：`{ "ok": true, "result": { "ok": true } }`

#### `cli tunnel start / stop / restart / remove`

| 命令 | 作用 | 人话输出 |
|---|---|---|
| `tunnel start <id>` | 开始发布 | `✓ 已开始发布「tun-4d5e6f」` |
| `tunnel stop <id>` | 暂停发布（别人立刻访问不了） | `✓ 已暂停「tun-4d5e6f」` |
| `tunnel restart <id>` | 立刻重连（顺带恢复发布） | `✓ 已让「tun-4d5e6f」重连` |
| `tunnel remove <id>` | 删除隧道（配置一起删） | `✓ 已删除隧道「tun-4d5e6f」` |

四个命令的 `--json` 都是：`{ "ok": true, "result": { "ok": true } }`

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

人话输出：`✓ 已删除服务器「srv-7a8b9c」`
有隧道引用会拒绝：`✗ 还有隧道挂在这台服务器上，先处理它们`
`--json`：`{ "ok": true, "result": { "ok": true } }`

#### `cli server setup-key <服务器id>`

| 项 | 说明 |
|---|---|
| 作用 | 用密码登录一次，自动配好免密（生成密钥 → 写公钥 → 用密钥验证） |
| 密码输入 | **从标准输入读一行**（不进命令行、不进历史）。例：`echo 密码 \| cli server setup-key srv-7a8b9c` |
| 前提 | 应用在跑；服务器允许密码登录；服务器上 `~/.ssh` 权限正常 |
| 副作用 | 生成 `%APPDATA%\Shangxian\keys\<服务器id>_ed25519`（+ `.pub`），并把服务器的 `keyPath` 指向它 |

人话输出（`message` 原文）：

```
配好了！以后连这台服务器都走密钥，不用再输密码。密钥放在：C:\Users\你\AppData\Roaming\Shangxian\keys\srv-7a8b9c_ed25519
```

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

#### `cli server hash-password <服务器id>`

| 项 | 说明 |
|---|---|
| 作用 | 在服务器上生成 nginx 用的 apr1 密码哈希 |
| 密码输入 | **从标准输入读一行**（`echo 密码 \| cli server hash-password <id>`） |
| 实现 | ssh 到服务器执行 `openssl passwd -apr1 -stdin` |
| 副作用 | 无（只是算一个哈希并打印） |

人话输出（真实形状）：

```
$apr1$xxxxxxxx$yyyyyyyyyyyyyyyyyyyyyy
```

`--json`（注意：**没有 `{ok,result}` 包装**，直接是结果）：

```json
{ "hash": "$apr1$xxxxxxxx$yyyyyyyyyyyyyyyyyyyyyy" }
```

失败例子（靶机容器里没有 openssl，实测）：`✗ ssh 退出码 Some(127)：bash: line 1: openssl: command not found`

### 4.5 应用与元命令

#### `cli app status`

人话输出：

```
✓ 应用在跑（进程号 12345，控制端口 58601）
✗ 应用没在跑。要叫起来：cli app start
```

`--json`：

```json
{ "appVersion": "0.1.0", "pid": 12345, "port": 58601, "running": true }
{ "running": false }
```

#### `cli app start`

| 项 | 说明 |
|---|---|
| 作用 | 把应用叫起来（静默进托盘） |
| 找程序 | 在 `cli.exe` **同一个文件夹**里找 `app.exe`、`Shangxian.exe` 或 `上线助手.exe`（按这个顺序） |
| 等待 | 最多 30 秒等控制通道就绪；已在跑则直接返回 |
| 失败 | 找不到程序：`✗ 没找到 app.exe（应该在 <目录> 里；开发时用 target\debug\cli.exe 跑就行）`；起来了但通道没通：`✗ 应用起来了但控制通道没通（看托盘图标，或手动打开应用）` |

人话输出：`✓ 应用已启动（在托盘里，进程号 12345）` 或 `✓ 应用本来就在跑（进程号 12345）`
`--json`：`{ "running": true, "pid": 12345 }`

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

`--json`：数组，元素为 `{ "method": ..., "desc": ..., "params": ... }`（内容见第 5 节）。
**注意**：`methods` 本身也要经过应用（是底层方法），应用没跑时会失败。

#### `cli call <方法> ['<参数JSON>']`

| 项 | 说明 |
|---|---|
| 作用 | 万能口：直接调底层方法 |
| 参数 | `<方法>` 必填；`<参数JSON>` 可省略，默认 `{}` |
| 输出 | **永远输出漂亮的 JSON 结果本身**（不套 `{ok,result}`），`--json` 加不加都一样 |
| 失败 | 无 `--json`：`✗ 参数不是合法 JSON：...`（退出码 1）；有 `--json`：`{"ok": false, "error": "..."}` |

示例：

```bash
cli call ping '{}'                # → { "pong": true }
cli call get_data_dir '{}'        # → { "path": "C:\\Users\\你\\AppData\\Roaming\\Shangxian" }
cli call get_notify_enabled '{}'  # → { "enabled": true }
cli call get_autostart '{}'       # → { "enabled": false }
cli call get_logs '{"id":"tun-4d5e6f","lines":100}'   # → { "text": "..." }
cli call save_service '{"name":"x","localPort":9000,"watch":false,"autostart":false}'
                                  # → { "id": "svc-..." }
cli call set_notify_enabled '{"enabled":false}'       # → { "ok": true }
```

#### `cli guide`

打印内置的给 agent 的速查（一段固定文本，见附录）。**纯本地文本，应用没跑也能用。**

---

## 5. 底层方法表（`cli methods` 的真实内容）

`cli call <方法> '<参数JSON>'` 能调的全部方法，共 32 个（`app/src-tauri/src/ctl.rs` 的 `METHODS`）：

| 方法 | 说明 | 参数格式 |
|---|---|---|
| `ping` | 看应用在不在（会校验通行证） | `{}` |
| `status` | 一次拿全：应用信息 + 本机服务 + 隧道 + 服务器 | `{}` |
| `get_snapshot` | 只拿三张清单（不含应用信息） | `{}` |
| `methods` | 列出所有能调的方法（就是这张表） | `{}` |
| `list_local_ports` | 本机正在监听的端口（带进程名和程序路径） | `{}` |
| `scan_imports` | 扫描现有启动脚本，返回可导入的建议 | `{}` |
| `import_services` | 把导入建议变成服务（默认暂停） | `{items: [...]}` |
| `save_service` | 新增/编辑本机服务（不填 id 就是新增） | `{id?, name, localPort, launchExe?, launchArgs?, launchWorkdir?, watch, autostart, note?}` |
| `remove_service` | 删除本机服务（有隧道在用会拒绝） | `{id}` |
| `service_start` | 启动本机服务（用登记过的启动方式） | `{id}` |
| `service_stop` | 停止本机服务（只停应用自己启动的） | `{id}` |
| `service_restart` | 重启本机服务（先停再启） | `{id}` |
| `set_service_watch` | 开关看护（掉了自动拉起） | `{id, watch}` |
| `set_service_autostart` | 开关开机自启 | `{id, autostart}` |
| `get_service_logs` | 读本机服务的日志尾部 | `{id, lines?}` |
| `add_tunnel` | 新增发布隧道 | `{name, serviceId, serverId, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}` |
| `update_tunnel` | 编辑发布隧道（accessHash 传 null 表示不改密码） | `{id, name, remotePort, url?, access, accessUser?, accessHash?, accessDetail?}` |
| `remove_tunnel` | 删除发布隧道 | `{id}` |
| `tunnel_start` | 开始发布 | `{id}` |
| `tunnel_stop` | 暂停发布 | `{id}` |
| `tunnel_restart` | 立刻重连 | `{id}` |
| `get_logs` | 读隧道的日志尾部 | `{id, lines?}` |
| `save_server` | 新增/编辑云端服务器（不填 id 就是新增） | `{id?, name, host, sshPort, user, keyPath?}` |
| `delete_server` | 删除云端服务器（有隧道在用会拒绝） | `{id}` |
| `setup_passwordless` | 用密码登录一次，自动配好免密（密码不落盘） | `{serverId, password}` |
| `hash_password` | 在服务器上生成访问密码的哈希（apr1，给 nginx 用） | `{serverId, password}` |
| `get_nginx_snippet` | 生成某条隧道的 nginx 配置片段 | `{id}` |
| `get_notify_enabled` | 掉线通知开着吗 | `{}` |
| `set_notify_enabled` | 开关掉线通知 | `{enabled}` |
| `get_autostart` | 开机自启开着吗 | `{}` |
| `set_autostart` | 开关开机自启 | `{enabled}` |
| `get_data_dir` | 配置文件夹在哪 | `{}` |

补充：

| 方法 | 成功返回 |
|---|---|
| `ping` | `{"pong": true}` |
| `save_service` / `add_tunnel` / `save_server` | `{"id": "..."}` |
| `setup_passwordless` | `{"keyPath": "...", "message": "..."}` |
| `hash_password` | `{"hash": "$apr1$..."}` |
| `get_nginx_snippet` | `{"snippet": "...", "htpasswd": .../"...", "steps": [...]}` |
| `get_service_logs` / `get_logs` | `{"text": "..."}` |
| 其余写操作 | `{"ok": true}` |
| `get_notify_enabled` / `get_autostart` | `{"enabled": true/false}` |
| `get_data_dir` | `{"path": "..."}` |

> 这两个只是界面用的 Tauri 命令，**不在方法表里**：`open_data_dir`（打开配置文件夹）、`test_notification`（试弹通知）。
> `get_service_logs` / `get_logs` 的 `lines` 不传时默认 200。

---

## 6. 离线行为（应用没在跑时）

判断方法：`cli app status --json` → `{"running": false}`。

| 命令 | 能用吗 | 结果 |
|---|---|---|
| `status` / `services` / `tunnels` / `servers` | ✅ | 直接读 `%APPDATA%\Shangxian\config.json`；所有状态字段 = `"unknown"`；`app.running = false`、`pid = null`；退出码 0 |
| `app status` | ✅ | 读 `ctl.json` 并试连一次，输出 `{"running": false}`；退出码 0 |
| `guide` / `--help` / `--version` | ✅ | 纯本地文本 |
| `ports` / `logs` / `nginx` / `methods` / `call` | ❌ | 报错退出码 1 |
| `service *` / `tunnel *` / `server *` | ❌ | 报错退出码 1 |
| `app start` | ⚠️ | 尝试拉起应用；找不到 exe 或 30 秒通道不通就失败 |

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

离线快照的字段差异见 4.1 的说明（`unknown` 状态、缺 `reason`/`logPath`/`serviceOk` 等）。

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
  "appVersion": "0.1.0"
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

## 8. 配置文件字段表

### 位置

| 内容 | 路径 |
|---|---|
| 数据目录 | `%APPDATA%\Shangxian`（`APPDATA` 取不到时是 `./data`） |
| 配置 | `<数据目录>\config.json`（原子写入：临时文件 + 改名） |
| 老版本备份 | `<数据目录>\config.json.v3.bak`（v3→v4 迁移时自动生成，只生成一次） |
| 密钥 | `<数据目录>\keys\<服务器id>_ed25519`（+ `.pub`） |
| 日志 | `<数据目录>\logs\`：隧道 `<隧道名>.ssh.log`，本机服务 `service-<服务名>.log` |
| 通行证 | `<数据目录>\ctl.json`（应用运行时才有） |

### config.json（`core/src/config.rs`，JSON 用 camelCase）

顶层：

| 字段 | 类型 | 说明 |
|---|---|---|
| `version` | number | 当前 = **4** |
| `servers` | 数组 | 云端服务器 |
| `services` | 数组 | 本机服务 |
| `tunnels` | 数组 | 发布隧道 |
| `notifyEnabled` | bool | 掉线通知，默认 `true` |

`servers[]`（ServerConfig）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | `srv-` + 6 位十六进制 |
| `name` | string | 名字 |
| `host` | string | 主机名或 IP |
| `sshPort` | number | SSH 端口，缺省 22 |
| `user` | string | 登录用户名 |
| `keyPath` | string / null | 私钥路径；`null` = 用系统默认密钥 |

`services[]`（LocalService）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | `svc-` + 6 位十六进制 |
| `name` | string | 名字 |
| `localPort` | number | 本机端口 |
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
| `access` | string / null | `none` / `password` / `ip`（JSON 里可能是 `null`，等同 `none`） |
| `accessUser` | string / null | 访问用户名 |
| `accessHash` | string / null | apr1 哈希；明文密码只显示一次，不落盘 |
| `accessDetail` | string / null | 访问保护细节（IP 白名单，逗号分隔） |
| `enabled` | bool | 是否正在发布；`false` = 已暂停 |

### 迁移（v3 → v4）

- 老配置读进来先备份成 `config.json.v3.bak`，再拆成「本机服务 + 隧道」并写回。
- 老 service 的 id 保留给隧道（界面标识稳定）；同一个本机端口的多条老服务合成一台本机服务。
- 老数据没有看护/自启信息，迁移后默认关。

### 状态词与原始值对照

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

- `serviceOk`：本机服务标的是否「在跑」（端口真的通）；`false` 时界面标「本机服务没在跑（访问会失败）」。
- `serviceStatus`：本机服务的原始状态词。
- `reason`：最近一次出错的人话原因（比如 `启动失败：...`、`连续启动失败，已经停止自动重试`、`找不到云端服务器（srv-xxx）`），只有坏状态才有。
- `hasPassword`：`accessHash` 非空就是 `true`。
- `logPath`：本机服务日志的完整路径。

---

## 9. 常用配方

> 下面例子里 `<...>` 是占位符，用 `cli services --json` / `cli servers --json` / `cli tunnels --json` 查真实 id。

### 配方 1：把本机 8188 发布到公网（从零开始）

```bash
# 0. 确认应用在跑
cli app status --json

# 1. 登记本机服务（如果还没有）
cli service add --name comfyui --port 8188 \
  --exe "D:\AI\ComfyUI-aki-v1.7\python\python.exe" \
  --args "main.py" --workdir "D:\AI\ComfyUI-aki-v1.7" \
  --watch --json
# → {"ok":true,"result":{"id":"svc-1a2b3c"}}

# 2. 看服务器 id（没有就先 cli server add / cli server setup-key）
cli servers --json

# 3. 建隧道并开始发布（显式要求密码，别用默认的 none）
cli tunnel add --service svc-1a2b3c --server srv-7a8b9c \
  --remote-port 13000 --url comfy.example.com \
  --access password --access-user me --access-password '你的访问密码' --json
# → {"ok":true,"result":{"id":"tun-4d5e6f"}}

# 4. 生成云端 nginx 配置（复制粘贴到小皮面板 → 站点 → 配置修改 → 重载）
cli nginx tun-4d5e6f

# 5. 验证
cli tunnels --json          # 看 status 是不是 "running"
cli logs tun-4d5e6f --lines 20   # 看有没有 "remote forward success"
```

### 配方 2：看某条隧道为什么掉线

```bash
cli tunnels                    # 人话列表里坏状态会带 "↳ 原因"
cli tunnels --json             # 结构化：status / reason / serviceOk / serviceStatus
cli logs <隧道id> --lines 200  # 隧道侧日志（ssh 的 debug 输出）
cli logs <服务id> --kind service --lines 200   # 本机服务侧日志
cli ports --json               # 本机端口到底通不通、是哪个进程
```

判读：

| 看到什么 | 说明 |
|---|---|
| `status: "disconnected"` | 云端联系不上，会自动重试（2s 起、最长 30s 间隔） |
| `status: "connecting"` 且日志一直失败 | 看日志里的 `Permission denied` / `Connection refused` / `remote port forwarding failed` |
| `remote port forwarding failed` | 云端端口被占；应用会尝试精确清理残留 sshd 会话后重连 |
| `serviceOk: false` | 问题在本机：服务没在跑（先解决它，隧道没错） |
| `reason` 有值 | 直接读这句人话 |

### 配方 3：给隧道加 / 换访问密码

方式 A（简单，密码会短暂出现在本机命令行里）：

```bash
cli tunnel edit <隧道id> --access password --access-user me --access-password '新密码'
cli nginx <隧道id>     # 重新复制片段 + 密码文件内容，贴回面板
```

方式 B（推荐给 agent / 不想让密码进命令行）：

```bash
# 1. 在服务器上算哈希（密码走 stdin，不进命令行）
echo '新密码' | cli server hash-password <服务器id>
# → $apr1$xxxxxxxx$yyyyyyyyyyyyyyyyyyyyyy

# 2. 用 cli call 直接把哈希写进隧道（先取当前 name/remotePort）
cli tunnels --json
cli call update_tunnel '{"id":"<隧道id>","name":"comfyui 发布","remotePort":13000,"access":"password","accessUser":"me","accessHash":"$apr1$xxxxxxxx$yyyyyyyyyyyyyyyyyyyyyy"}'
# → {"ok": true}

# 3. 云端配置重新贴一遍（关键：不重贴，旧密码还有效）
cli nginx <隧道id>
```

换成白名单：`--access ip --access-ips "1.2.3.4,5.6.7.8"`；换回无保护：`--access none`（不推荐）。

### 配方 4：配免密登录

```bash
cli servers --json        # 找到 id，确认 keyPath 是 null（或密钥失效）
echo '服务器登录密码' | cli server setup-key <服务器id>
cli servers --json        # keyPath 应变成 ...\keys\<服务器id>_ed25519
```

注意：

- 密码从 stdin 进，**不要**写成参数。
- 服务器不允许密码登录时会返回人话原因；不要重复硬试，让用户手动配密钥。
- 配好后，新密钥对**之后启动**的隧道生效；已经在跑的隧道不会自动换密钥，需要 `cli tunnel restart <隧道id>`（界面：隧道卡片 ⋯ → 立刻重连）才会用上新密钥。

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

# 4. 确认没问题后，删掉老隧道（老服务器上占的端口随连接断开释放）
cli tunnel remove <老隧道id>
```

⚠️ 涉及 pi-web 的隧道**任何情况下都不要动**。

### 配方 6：应用没在跑

```bash
cli app status --json          # {"running": false}
cli app start                  # 在 cli.exe 同目录找 app.exe / 上线助手.exe 拉起来
cli app status --json          # 等它 ready（app start 内部最多等 30 秒）
cli status --json              # 现在能拿到实时状态了
```

如果 `cli app start` 找不到程序：说明主程序不在 `cli.exe` 同一个文件夹里，或者名字不在 `app.exe` / `Shangxian.exe` / `上线助手.exe` 这三个里——请让用户手动打开应用（命令行照样能连上）。

---

## 10. 输出与退出码约定

| 情况 | 人类输出 | `--json` 输出 | 退出码 |
|---|---|---|---|
| 成功（只读） | 中文表格 / 原文 | 结果本身 | 0 |
| 成功（写操作） | `✓ ...` | `{"ok": true, "result": <结果>}` | 0 |
| 成功（`cli call`） | — | 结果本身（`--json` 无效） | 0 |
| 成功（`server hash-password`） | 哈希 | `{"hash": "..."}`（无 ok 包装） | 0 |
| 业务失败（方法不存在、拒绝删除、端口冲突…） | `✗ 人话`（stderr） | `{"ok": false, "error": "人话"}` | 1 |
| 参数 JSON 不是合法 JSON | `✗ 参数不是合法 JSON：...` | `{"ok": false, "error": "..."}` | 1 |
| 命令拼错 / 缺必填参数 | clap 的英文错误 | — | 2 |
| `--help` / `--version` | 帮助 / 版本 | — | 0 |

失败时 JSON 的键顺序是字母序：`error` 在前、`ok` 在后（`serde_json` 默认行为）。

---

## 11. 已知坑与注意事项

| # | 事项 | 说明 |
|---|---|---|
| 1 | **pi-web 红线** | 本机 pi-web 服务 + 它的隧道是用户正在用的通道。任何时候不暂停、不重启、不删除 |
| 2 | `tunnel add --access` 默认 `none` | 界面向导默认 `password`，命令行默认**无保护**。agent 建隧道必须显式传 `--access password` 或 `--access ip` |
| 3 | `hash-password` 依赖服务器有 `openssl` | 实测本地靶机（最小化 sshd 容器）没有，报 `openssl: command not found`。需要哈希的操作会跟着失败 |
| 4 | `tunnel add --access-password` 是「先算哈希再建隧道」 | 服务器连不上或没 openssl → **整条命令失败**；界面向导允许先建隧道后补密码，两者行为不同 |
| 5 | `tunnel edit` 不能换服务/服务器 | 想换服务器用配方 5：先 add 新、验证、再 remove 旧 |
| 6 | `logs <不存在的id>` | 会直接报错「没找到隧道/本机服务「xxx」」并退出码 1（2026-10-11 已修） |
| 7 | `--kind` 只认 `service` | 其他任何值都按隧道日志处理 |
| 8 | `watch`/`autostart` 参数形态不一致 | `service add` 是开关（不带值）；`service edit` 要写 `true`/`false` |
| 9 | 密码不要进命令行 | `server setup-key`、`server hash-password` 走 stdin；`tunnel edit --access-password` 会短暂出现在本机进程命令行，agent 优先用配方 3 的方式 B |
| 10 | 写命令立刻落盘 | 应用会把 config.json 原子替换；界面和 CLI 看到的是同一份状态（同一个 Guardian） |
| 11 | 应用退出 = 隧道全停 | 不要把「应用不在跑」当成「隧道照跑」。离线时状态是 `unknown`，不是 `running` |
| 12 | 单实例 | 再开一个 app 实例只会把已有窗口显示出来，不会起第二份看护；`cli app start` 已运行时会立刻返回 |
| 13 | debug 版 app.exe 不能单独分发 | 现在 `target/debug/app.exe` 是 dev 构建，会去连 `http://localhost:1420`。给别人装要 `npm run tauri build` 打安装包 |
| 14 | 安装版主程序名 | `tauri.conf.json` 的 `productName` 是 `Shangxian`，安装后 exe 可能叫 `Shangxian.exe`；`cli app start` 已同时找 `app.exe` / `Shangxian.exe` / `上线助手.exe`（2026-10-11 已加），三个都不在就请用户手动打开 |
| 15 | 云端端口两台电脑共享 | 应用只能发现本配置内的端口冲突；另一台电脑占用的云端端口会以 `remote port forwarding failed` 形式出现，不会提前拦。跨电脑别用同一端口 |
| 16 | nginx 是人工步骤 | 应用只生成片段，不自动下发、不自动 reload。改端口/改访问保护后必须提醒用户重新贴 |
| 17 | 掉线通知 | 隧道持续异常超过 3 分钟才弹一次，恢复再弹一次；用户暂停的不报。可在设置页关（`set_notify_enabled`） |

---

## 附录：`cli guide` 原文

```
# 上线助手命令行速查（给 AI agent）

这个命令行是「上线助手」桌面应用的遥控器。应用才是真正看护隧道的那个，
它常驻在系统托盘里；命令行只负责查询和改配置。

## 三条铁律（别踩）
1. **不许碰 pi-web**：本机 pi-web 服务 + 它的隧道是用户正在用的通道，暂停/重启它会直接断掉会话。
2. 隧道暂停后别人就访问不了了；不确定要不要停就先问。
3. 删除本机服务/服务器前，先看它有没有被隧道引用（有引用时应用会拒绝并告诉你）。

## 最常用的十条
    cli status --json                       # 一次看全：应用 + 服务 + 隧道 + 服务器
    cli services --json                     # 本机服务清单（id、端口、状态、启动方式）
    cli tunnels --json                      # 发布隧道清单（服务→服务器、云端端口、状态）
    cli servers --json                      # 云端服务器清单
    cli ports --json                        # 本机正在监听的端口（带进程名和程序路径）
    cli tunnel start <隧道id>               # 开始发布
    cli tunnel stop <隧道id>                # 暂停发布
    cli service start <服务id>              # 拉起本机服务（用它登记的启动方式）
    cli logs <隧道id> --lines 80            # 看隧道日志（--kind service 看服务日志）
    cli nginx <隧道id>                      # 生成要贴到云端面板的 nginx 配置片段

## 配置类
    cli service add --name 我的网站 --port 8188 --exe C:\path\python.exe --args "main.py" --watch
    cli service edit <服务id> --port 8189 --watch true
    cli tunnel add --service <服务id> --server <服务器id> --remote-port 13000 --url 我的域名.com
    cli tunnel add ... --access password --access-user 用户名 --access-password 密码
    cli server add --name 香港-1 --host api.example.com --user user
    echo 服务器密码 | cli server setup-key <服务器id>     # 用密码登录一次，自动配好免密

## 应用没在跑怎么办
    cli app status        # 看它在不在
    cli app start         # 叫起来（静默进托盘）
只读命令（status/services/tunnels/servers）在没有应用时也能用——直接读配置文件，
但状态字段会是 unknown；要改东西必须先有应用在跑。

## 想调更底层的东西
    cli methods                       # 列出所有能调的方法 + 参数格式
    cli call <方法> '{"参数":...}'    # 直接调（万能口）

## 输出约定
- 默认中文人话；加 `--json` 输出机器格式（agent 一律加）。
- 失败时 `--json` 会输出 {"ok": false, "error": "人话原因"}，退出码 1。
- 成功时 `--json` 输出 {"ok": true, "result": ...}。
```
