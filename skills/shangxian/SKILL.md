---
name: shangxian
description: 上线助手（Shangxian）——把本机服务一键发布到公网：管理 SSH 反向隧道、云端 nginx 配置与访问保护。当用户要发布/暂停/重启本机服务到公网，查看隧道/服务/服务器状态，排查隧道掉线，设置访问密码或 IP 白名单，配置免密登录，生成 nginx 配置片段，查看本机监听端口时使用。触发词：上线、发布、公网、隧道、端口转发、内网穿透、掉线、nginx、免密、上线助手、shangxian、tunnel。工具 = 托盘应用 + cli.exe 遥控器。GitHub 仓库：https://github.com/iranolly/shangxian
compatibility: Windows；需要先安装并运行「上线助手」桌面应用（自带 cli.exe）
---

# 上线助手（Shangxian）

把本机服务（ComfyUI、网站、任何端口上的程序）发布到公网。**应用（常驻托盘）负责干活；`cli.exe` 是遥控器**——查状态、改配置、起停服务与隧道。

- **GitHub 仓库**（源码 / 文档 / Releases 下载安装包和便携包）：
  https://github.com/iranolly/shangxian
- **完整 agent 文档**（全部字段表、35 个底层方法、已知坑、离线行为）：
  https://github.com/iranolly/shangxian/blob/main/docs/AI-AGENT.md
- 给人看的使用手册：仓库里 `docs/使用手册.md`

**前提**：电脑上已安装「上线助手」（Windows 桌面应用）。没装的话，先让用户去
Releases 下载安装包或便携包：https://github.com/iranolly/shangxian/releases

## 第一步：找到 cli.exe

下面的查找脚本以 Git Bash / bash 为例；能跑通就用找到的路径：

```bash
CLI=""
for c in "$(command -v cli 2>/dev/null)" \
         "$LOCALAPPDATA/Shangxian/cli.exe" \
         "$PROGRAMFILES/Shangxian/cli.exe" \
         "./target/release/cli.exe" \
         "./target/debug/cli.exe"; do
  if [ -f "$c" ]; then CLI="$c"; break; fi
done
echo "$CLI"
```

都找不到时：

- **安装版**：应用和 `cli.exe` 在同一个文件夹。让用户在开始菜单找到「上线助手」→ 右键 →「打开文件位置」，把那个文件夹里的 `cli.exe` 完整路径告诉你，加进上面的列表。
- **便携版**：`cli.exe` 就在解压出来的文件夹里，让用户告诉你解压到了哪。
- **源码运行**：如果你正待在源码仓库目录里，用 `./target/release/cli.exe`（或 `debug` 版）。

## 铁律（先读，别踩）

1. **不动正在被使用的服务 / 隧道**：停或重启任何东西前先问自己——用户是不是正在用它？特别是**可能承载你与用户当前连接的通道**（网页面板、会话入口这类），碰它等于立刻断线。用户或项目文档里标注的「不许碰」清单永远优先。拿不准就先问。
2. **暂停隧道 = 别人立刻访问不了**；不确定要不要停，先问用户。
3. **删除前先查引用**：删本机服务 / 服务器前，先看有没有隧道在用。应用会拒绝并告诉你先删哪几条——**不要绕过**（别手改 config.json 硬删）。
4. **密码不进命令行**：配免密、算访问密码哈希都走标准输入（`echo '密码' | ...`）；密码不写进任何参数、日志和文件。
5. **应用退出 = 所有隧道停止；本机服务保活**：本地服务不会跟着应用退，下次打开应用会按端口探测认领（**不做归属核对**，路径核对只在杀进程时做）；不要把「杀掉应用」当重启手段。想连本地服务一起停，用托盘的「退出并停止全部本地服务」（停应用自己这次启动的整棵进程树 + 能按程序路径核对出归属的认领进程；核对不过的，如包装脚本起的子进程，会保留并在退出通知里告知；CLI 没有退出应用的命令）。
6. **建隧道必须显式写访问保护**：`--access password` 或 `--access ip`（可重复/逗号分隔；两个都写 = 两个条件都要满足）；命令行默认 `none`（谁都能进），只有用户明确要求开放才用。
7. **改云端端口或改访问保护后**：nginx 片段要重新贴到云端面板才生效（应用只生成片段，不自动下发）。贴完用 `tunnel check <隧道id> --json` 核对云端是不是真的生效。
8. **停止不是应用启动的进程要用户点头**：`service stop` 对端口上不是应用这次启动的进程会先拒绝（`needsConfirm`，应用重启后认领来的旧进程也算），要用户明确同意后才加 `--force`；不要为了“停干净”自己绕过。只监控（没配启动方式）的服务绝不接管，`--force` 也必然失败——先补全启动方式再说。
9. **所有命令一律加 `--json`**（机器格式好判读）。

## 常用命令

```bash
"$CLI" status --json                # 一次看全：应用 + 服务 + 隧道 + 服务器
"$CLI" services --json              # 本机服务清单（id、端口、状态、启动方式）
"$CLI" tunnels --json               # 隧道清单（服务→服务器、云端端口、状态）
"$CLI" servers --json               # 云端服务器清单
"$CLI" ports --json                 # 本机正在监听的端口（带进程名和程序路径）
"$CLI" logs <id> --lines 100        # 读隧道日志；--kind service 读本机服务日志
"$CLI" nginx <隧道id>               # 生成 nginx 片段 + 密码文件内容（给人贴到云端面板）
"$CLI" tunnel check <隧道id> --json  # 读云端实际生效的访问保护（对照 tunnels --json 里的 access 看齐不齐）
"$CLI" tunnel start|stop|restart <隧道id>    # 开始发布 / 暂停 / 立刻重连
"$CLI" service start|stop|restart <服务id>   # 停止默认只停应用自己启动的；配了启动方式的服务，不是它启动的才加 --force（只监控的服务会被直接拒绝）
"$CLI" app status --json            # 应用在不在
"$CLI" app start                    # 应用没跑时把它叫起来（静默进托盘）
"$CLI" guide                        # 内置速查（应用没跑也能看）
"$CLI" methods --json               # 全部底层方法（35 个）
"$CLI" call <方法> '{"参数":...}'    # 万能口：直接调底层方法
```

- 只读 4 个命令（`status`/`services`/`tunnels`/`servers`）应用没跑也能用，但状态是 `unknown`；**要改任何东西，先确认 `cli app status --json` 里 `running: true`**（不行就 `app start`）。
- 命令用法记不清：`"$CLI" <命令> --help`。

## 隧道 / 服务状态速读（`tunnels --json` 的字段）

| 字段 | 含义 |
|---|---|
| `status: "running"` | 正在发布（正常） |
| `status: "paused"` | 用户暂停了（别人访问不了） |
| `status: "connecting" / "disconnected"` | 正在连 / 掉线自动重连中（间隔 2s 起退避，最长 30s） |
| `status: "error"` | 出错，看 `reason` |
| `reason` | 最近一次出错的人话原因（只有坏状态才有值） |
| `serviceOk: false` | 问题在本机：服务没在跑（先解决它，不是隧道的错） |
| `enabled` | 是不是「应该发布」；暂停时是 false |
| `access` | 数组：可以同时含 `"password"` 和 `"ip"`（两个条件都要满足）；`[]` = 无保护 |

## 常用配方

### 1. 看现状（用户问「我的服务都正常吗」）

`status --json`：看每条隧道的 `status` 和 `reason`、每台服务的 `status` 和 `serviceOk`，用大白话汇报。

### 2. 从零把一个服务发布到公网

```bash
"$CLI" app status --json                 # 1. 应用在跑吗
"$CLI" ports --json                      # 2. 找本机端口 + 进程（用户说了端口就直接用）
"$CLI" services --json                   # 3. 拿服务 id；没有就先登记（见配方 3）
"$CLI" servers --json                    # 4. 拿服务器 id；没有就先 server add + 配免密
"$CLI" tunnel add --service <服务id> --server <服务器id> --remote-port <云端端口> \
       --url <域名> --access password --access-user <用户名> --access-password '<密码>' --json
# 两个门都要（密码 + 白名单）就再加：--access ip --access-ips "1.2.3.4"
"$CLI" nginx <新隧道id>                  # 5. 把片段 + 步骤交给用户贴到云端面板
"$CLI" tunnel check <新隧道id> --json    # 6. 核对云端实际生效的保护
"$CLI" tunnels --json                    # 7. 验证 status = running
"$CLI" logs <新隧道id> --lines 20        #    看到 remote forward success 就是通了
```

- 云端端口写死、冲突会报错，不会自动换；跨电脑别用同一个云端端口。
- 服务器连不上或没装 openssl 时，`--access-password` 会让整条命令失败；密码介意进本机命令行的，可以先用 `hash-password` 拿哈希，再用 `cli call add_tunnel` 带 `accessHash`。

### 3. 登记本机服务

```bash
"$CLI" service add --name <名字> --port <端口> \
       [--exe <程序>] [--args <参数>] [--workdir <目录>] [--watch] [--autostart] --json
```

- `--watch`＝掉了自动拉起；`--autostart`＝开机自启。add 时是开关（不带值）；edit 时要写 `--watch true|false`。
- 不填 `--exe` = 只监控，不帮忙启动。
- 新建服务器：`"$CLI" server add --name <名字> --host <地址> --user <账号> [--port 22] --json`。

### 4. 排查掉线

```bash
"$CLI" tunnels --json                    # 先读 reason；serviceOk=false 说明问题在本机
"$CLI" logs <隧道id> --lines 200         # 关键行：Permission denied / Connection refused / remote port forwarding failed
"$CLI" ports --json                      # 本机端口到底通不通、是谁在听
```

`remote port forwarding failed` = 云端端口被占（应用会尝试清掉残留会话再重连）。

### 5. 加 / 换访问保护（密码不进命令行，推荐；密码和 IP 白名单可叠加）

```bash
echo '新密码' | "$CLI" server hash-password <服务器id>     # → $apr1$xxxx...
"$CLI" tunnels --json                                       # 先记下当前 name / remotePort
"$CLI" call update_tunnel '{"id":"<隧道id>","name":"<名字>","remotePort":<端口>,"access":["password"],"accessUser":"<用户名>","accessHash":"<上面的哈希>"}'
# 密码 + 白名单一起要："access":["password","ip"]，再带 "accessDetail":"1.2.3.4,5.6.7.8"
"$CLI" nginx <隧道id>                                       # 提醒用户重新贴，旧密码才失效
"$CLI" tunnel check <隧道id> --json                         # 核对云端实际生效的保护
```

⚠️ `update_tunnel` 的 JSON 必须用**单引号**包住：哈希里有 `$`，放双引号里会被 shell 吃掉。

### 6. 配免密登录

```bash
echo '服务器登录密码' | "$CLI" server setup-key <服务器id>
```

配好后，**已经在跑**的隧道要 `tunnel restart <隧道id>` 才会用上新密钥。

### 7. 换服务器

`tunnel edit` 不能换服务器。步骤：在新服务器 `tunnel add` 建新隧道 → 验证 `running` 并贴新 nginx → 确认没问题再 `tunnel remove` 老隧道。

### 8. 应用没在跑

```bash
"$CLI" app start             # 在 cli.exe 同目录找 app.exe / Shangxian.exe / 上线助手.exe
"$CLI" app status --json     # 确认 running
```

找不到程序：让用户手动打开应用；命令行照样能连上（它靠 `%APPDATA%\Shangxian\ctl.json` 找到应用）。

## 深入细节

- 完整字段表、全部方法、已知坑、离线行为：
  https://github.com/iranolly/shangxian/blob/main/docs/AI-AGENT.md
- 仓库地址：https://github.com/iranolly/shangxian
