---
name: shangxian
description: 上线助手（Shangxian）——把本机服务一键发布到公网：管理 SSH 反向隧道、云端 nginx 配置与访问保护。当用户要发布/暂停/重启本机服务到公网，查看隧道/服务/服务器状态，排查隧道掉线，跑体检或打诊断包，撤销误删，备份/恢复配置，设置访问密码或 IP 白名单，配置免密登录，生成 nginx 配置片段，查看本机监听端口时使用。触发词：上线、发布、公网、隧道、端口转发、内网穿透、掉线、nginx、免密、上线助手、shangxian、tunnel、体检、诊断、撤销。工具 = 托盘应用 + cli.exe 遥控器。GitHub 仓库：https://github.com/iranolly/shangxian
compatibility: Windows；需要先安装并运行「上线助手」桌面应用（自带 cli.exe）
---

# 上线助手（Shangxian）

把本机服务（ComfyUI、网站、任何端口上的程序）发布到公网。**应用（常驻托盘）负责干活；`cli.exe` 是遥控器**——查状态、改配置、起停服务与隧道、体检、备份。

- **GitHub 仓库**（源码 / 文档 / Releases 下载安装包和便携包）：
  https://github.com/iranolly/shangxian
- **完整 agent 文档**（全部字段表、61 个底层方法、已知坑、离线行为）：
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
4. **密码尽量不进命令行**：配免密、算哈希都走标准输入（`echo '密码' | ...`，交互输也不回显）；访问密码的加密（bcrypt）在**本机**算，服务器上不用装东西。`tunnel add/edit --access-password` 会把密码短暂带进本机命令行，介意就用 `server hash-password` 拿哈希再 `cli call add_tunnel/update_tunnel` 带 `accessHash`。密码不写进配置文件、日志和文档。
5. **访问密码（basic auth）必须配 HTTPS**：只有 80 端口的站点**不算发布完成**。发布完成的验收 = 用 `https://<域名>/` 打开一次（业务页、401 密码框、403 白名单拦截都算线路通），并且 `http://` 会跳 `https://`。发现没证书 / 没强制跳转，让用户去面板处理，别宣告完成。
6. **云端配置一律人工粘贴**：**永远不要自动写服务器配置、不要装 fail2ban（或任何软件）、不要动全局 `nginx.conf`**。应用只生成片段和文案，`nginx -t`、reload、面板保存都由用户自己来。要动 `http{}` 的东西（如 `limit_req`）默认不生成，只出强警告；防爆破先推 IP 白名单。
7. **应用退出 = 所有隧道停止；本机服务保活**：本地服务不会跟着应用退，下次打开应用会按端口探测认领（**不做归属核对**，路径核对只在杀进程时做）；不要把「杀掉应用」当重启手段。想连本地服务一起停，用托盘的「退出并停止全部本地服务」或 `cli app stop-all --yes`（升级/卸载前用；**只停运行、不改设置**，升级后原来开着的隧道会按原设置自己连回来）。
8. **建隧道必须显式写访问保护**：`--access password` 或 `--access ip`（可重复/逗号分隔；两个都写 = 两个条件都要满足）；命令行默认 `none`（谁都能进），只有用户明确要求开放才用。`--access` 写别的值会**直接报错**（不会静默变成无保护）；成功提示会回显最终保护状态。
9. **改云端端口或改访问保护后**：nginx 片段要重新贴到云端面板才生效（应用不自动下发）。贴完用 `tunnel check <隧道id> --json` 核对云端是不是真的生效（这条命令和界面「检查云端」同一套判定，最长约 30 秒）。
10. **停止不是应用启动的进程要用户点头**：`service stop` 对端口上不是应用这次启动的进程会先拒绝（`needsConfirm`，应用重启后认领来的旧进程也算），要用户明确同意后才加 `--force`；不要为了“停干净”自己绕过。只监控（没配启动方式）的服务绝不接管，`--force` 也必然失败——先补全启动方式再说。
11. **删除 / 暂停 / 重置指纹 / 停全部 / 导入配置先确认**：`tunnel stop|remove`、`service remove`、`server remove`、`tunnel forget-key`、`app stop-all`、`config import` 在交互终端会问 y/N；非交互（管道 / 脚本 / agent）**必须加 `--yes`**，不加会直接拒绝、什么都不动。`config import --dry-run` 例外：只检查、不用确认。
12. **误删有 10 分钟反悔期**：`cli undo`（最近一次）/ `cli undo list` / `cli undo und-xxxxxxxx`；隧道恢复后是**暂停**状态，服务不会自动启动。超过 10 分钟或 20 条就找不回。
13. **体检 / 诊断包只读**：`cli doctor` / `cli diag` 不改任何配置；诊断包已脱敏（不含密码哈希、密钥、通行证），但含域名/路径，**不要公开贴到网上**。
14. **所有命令一律加 `--json`**（机器格式好判读）。成功是 `{"ok":true,"result":...}`，失败是 `{"ok":false,"error":"..."}`；退出码 `0` 成功、`1` 出错（含 `doctor --strict` 有没过、undo 过期）、`2` 命令写法错、`3` 这个命令需要应用在跑但没在跑（`app stop-all` 是例外，没在跑也退 0）。

## 常用命令

```bash
"$CLI" status --json                # 一次看全：应用 + 服务 + 隧道 + 服务器
"$CLI" services --json              # 本机服务清单（id、端口、状态、启动方式）
"$CLI" tunnels --json               # 隧道清单（服务→服务器、云端端口、状态、重试倒计时）
"$CLI" servers --json               # 云端服务器清单
"$CLI" ports --json                 # 本机正在监听的端口（带进程名和程序路径）
"$CLI" logs <id> --lines 100        # 读隧道日志；--source service 读本机服务日志；--source app 读事件+应用日志
"$CLI" logs --source all --format json --json   # 全部来源合并读（管道用；不带 --json 是 JSONL）
"$CLI" logs <id> --archive 1        # 看上一份轮转备份（0/1/2）
"$CLI" nginx <隧道id>               # 生成 nginx 片段 + 密码文件内容（给人贴到云端面板）
"$CLI" tunnel check <隧道id> --json  # 读云端实际生效的访问保护（和「检查云端」同路，最长约 30 秒；对照 tunnels --json 里的 access 看齐不齐）
"$CLI" tunnel start|stop <隧道id>            # 开始发布 / 暂停
"$CLI" tunnel stop|remove <隧道id> --yes     # 非交互环境必须带 --yes（删除还有 service/server remove）
"$CLI" tunnel forget-key <隧道id> --yes      # 服务器重装过、指纹变了才用（TOFU 重信任）
"$CLI" service start|stop|restart <服务id>   # 停止默认只停应用自己启动的；配了启动方式的服务，不是它启动的才加 --force（只监控的服务会被直接拒绝）
"$CLI" doctor                       # 体检九步（不连服务器）；--deep 连云端/域名；--strict 有没过退 1
"$CLI" diag --out 诊断包.zip        # 脱敏诊断包（发给帮忙排查的人）
"$CLI" stats                        # 运行统计：运行多久、重连几次、是不是在退避重试
"$CLI" undo / undo list / undo <令牌>         # 撤销删除（10 分钟内、最多 20 条）
"$CLI" config export --out 备份.json          # 导出配置（默认脱敏）
"$CLI" config import 备份.json --dry-run      # 先看会改什么；真导入加 --yes
"$CLI" app status --json            # 应用在不在（没在跑时退出码 3）
"$CLI" app start                    # 应用没跑时把它叫起来（静默进托盘）
"$CLI" app stop-all --yes           # 升级/卸载前停全部隧道和服务（只停运行、不改设置；没在跑也退 0）
"$CLI" app update-check             # 查新版（只查，不下载、不安装）
"$CLI" guide                        # 内置速查（应用没跑也能看）
"$CLI" methods --json               # 全部底层方法（61 个）
"$CLI" call test_server '{"serverId":"<服务器id>"}'   # 真连一次这台服务器（agent 可用的测试连接）
"$CLI" call check_tunnel_cloud '{"id":"<隧道id>"}' --json  # 完整云端检查：HTTPS/证书/端口/审计
"$CLI" call <方法> '{"参数":...}'    # 万能口：直接调底层方法（加 --json 才有 {"ok":true,"result":...} 信封）
```

- 只读 4 个命令（`status`/`services`/`tunnels`/`servers`）应用没跑也能用，但状态是 `unknown`；`doctor`、`undo list`、`config export` 离线也能用；**要改任何东西，先确认 `cli app status --json` 里 `running: true`**（不行就 `app start`）。
- 命令用法记不清：`"$CLI" <命令> --help`。

## 隧道 / 服务状态速读（`tunnels --json` 的字段）

| 字段 | 含义 |
|---|---|
| `status: "running"` | 正在发布（正常） |
| `status: "paused"` | 用户暂停了（别人访问不了） |
| `status: "connecting" / "disconnected"` | 正在连 / 掉线自动重连中（退避 2→30 秒，稳定 60 秒重置） |
| `status: "error"` | 出错，看 `reason` |
| `reason` / `reasonCode` | 最近一次出错的人话原因 / 机器码（只有坏状态才有值） |
| `retryState` / `attempt` / `retryInSecs` | `active`=还会重试 / `stopped`=不再试；试到第几次；还有几秒重试 |
| `serviceOk: false` | 问题在本机：服务没在跑（先解决它，不是隧道的错） |
| `serviceStatus` / `health` | 本机服务原始状态词 / `up`（端口在听）`down`（探过不通）`unknown`（没探过） |
| `enabled` | 是不是「应该发布」；暂停时是 false |
| `access` | 数组：可以同时含 `"password"` 和 `"ip"`（两个条件都要满足）；`[]` = 无保护 |
| `preset` | nginx 片段预设：`web` / `ws` / `upload` |
| `protectionCheck` | 最近一次云端核对（手动或后台自动）：`status` = `unknown`/`ok`/`drift`/`error`；`source` = `manual`/`auto`；对不上看 `driftFields` |

## 常用配方

### 1. 看现状（用户问「我的服务都正常吗」）

`status --json`：看每条隧道的 `status`/`reason`、每台服务的 `status`/`health`、`app.jobGuardOk`/`configTrusted`，用大白话汇报。

### 2. 从零把一个服务发布到公网（含验收）

```bash
"$CLI" app status --json                 # 1. 应用在跑吗
"$CLI" ports --json                      # 2. 找本机端口 + 进程（用户说了端口就直接用）
"$CLI" services --json                   # 3. 拿服务 id；没有就先登记（见配方 3）
"$CLI" servers --json                    # 4. 拿服务器 id；没有就先 server add + 配免密
"$CLI" tunnel add --service <服务id> --server <服务器id> --remote-port <云端端口> \
       --url <域名> --access password --access-user <用户名> --access-password '<密码>' --json
# 两个门都要（密码 + 白名单）就再加：--access ip --access-ips "1.2.3.4"
"$CLI" nginx <新隧道id>                  # 5. 把片段 + 步骤交给用户贴到云端面板
# 6. 用户贴完后做验收（三项都要）：
"$CLI" tunnel check <新隧道id> --json    #    云端实际生效的保护和设置一致
"$CLI" doctor --deep                     #    云端配置 / 公网访问（https）/ 域名解析 都 pass
#    再用 https://<域名>/ 打开一次（业务页 / 401 / 403 都算线路通）
"$CLI" tunnels --json                    # 7. status = running
"$CLI" logs <新隧道id> --lines 20        #    看到 remote forward success 就是通了
```

- **只有 80 端口、没证书的站点不算发布完成**；`check_tunnel_cloud` 的 `extra.https` 里 `plainOpen=true` 或 `redirect80=false` 时，让用户去面板打开「强制 HTTPS」。
- 云端端口写死、冲突会报错，不会自动换；跨电脑别用同一个云端端口。
- 访问密码哈希超过 72 字节会被拒绝（bcrypt 的限制），换短一点。

### 3. 登记本机服务

```bash
"$CLI" service add --name <名字> --port <端口> \
       [--exe <程序>] [--args <参数>] [--workdir <目录>] [--watch] [--autostart] --json
```

- `--watch`＝掉了自动拉起；`--autostart`＝开机自启。add 时是开关（不带值）；edit 时不带值 = 开、`--watch false` = 关、不传 = 不动。
- 不填 `--exe` = 只监控，不帮忙启动。
- 新建服务器：`"$CLI" server add --name <名字> --host <地址> --user <账号> [--port 22] --json`。

### 4. 排查掉线 / 用户说「不知道哪出问题」

```bash
"$CLI" doctor                    # 九步体检：程序/文件夹/SSH/端口/服务器/隧道/云端/公网/域名（先跑这个）
"$CLI" doctor --deep             # 要连云端和查域名时用（单步 20s、整次 60s 上限）
"$CLI" tunnels --json            # 先读 reason / reasonCode；serviceOk=false 说明问题在本机
"$CLI" logs <隧道id> --lines 200 # 关键行：Permission denied / Connection refused / remote port forwarding failed
"$CLI" logs --source app --lines 100   # 事件流：什么时候断的、为什么
"$CLI" ports --json              # 本机端口到底通不通、是谁在听
"$CLI" diag --out 诊断包.zip     # 还查不出来就打包（已脱敏），让用户发给帮忙排查的人
```

`remote port forwarding failed` = 云端端口被占（应用会尝试清掉残留会话再重连，每分钟最多清理一次）。

### 5. 加 / 换访问保护（密码不进命令行，推荐；密码和 IP 白名单可叠加）

```bash
echo '新密码' | "$CLI" server hash-password <服务器id>     # → $2y$12$xxxx...（本机算，不碰服务器）
"$CLI" tunnels --json                                       # 先记下当前 name / remotePort
"$CLI" call update_tunnel '{"id":"<隧道id>","name":"<名字>","remotePort":<端口>,"access":["password"],"accessUser":"<用户名>","accessHash":"<上面的哈希>"}' --json
# 密码 + 白名单一起要："access":["password","ip"]，再带 "accessDetail":"1.2.3.4,5.6.7.8"
"$CLI" nginx <隧道id>                                       # 提醒用户重新贴，旧密码才失效
"$CLI" call check_tunnel_cloud '{"id":"<隧道id>"}' --json    # 核对云端实际生效 + HTTPS/证书/端口
```

⚠️ `update_tunnel` 的 JSON 必须用**单引号**包住：哈希里有 `$`，放双引号里会被 shell 吃掉。
⚠️ 改完密码必须确认用户能用 `https://` 打开（basic auth 走明文 http 等于把密码写在明信片上）。

### 6. 配免密登录

```bash
echo '服务器登录密码' | "$CLI" server setup-key <服务器id>
```

配好后，**已经在跑**的隧道要停一下再开（`tunnel stop <隧道id> --yes` → `tunnel start <隧道id>`）才会用上新密钥。

### 7. 换服务器

`tunnel edit` 不能换服务器。步骤：在新服务器 `tunnel add` 建新隧道 → 贴新 nginx、用 https 验证、跑 `doctor --deep` → 确认没问题再 `tunnel remove <老隧道id> --yes` 老隧道。

### 8. 误删恢复 / 备份迁移

```bash
"$CLI" undo list                          # 还能撤销哪些（删除后 10 分钟内、最多 20 条）
"$CLI" undo                               # 撤销最近一次
"$CLI" config export --out 备份.json      # 备份（默认脱敏：不含密码哈希、不含密钥路径）
"$CLI" config import 备份.json --dry-run  # 新机器上先看会改什么
"$CLI" config import 备份.json --yes      # 按 id 合并导入
```

- 隧道撤销后是暂停状态，要再 `tunnel start`；服务只恢复登记、不启动。
- 脱敏文件导回来不会清空密码/密钥（`hasPassword`/`keyPathPresent` 保留原值）。

### 9. 应用没在跑

```bash
"$CLI" app start             # 在 cli.exe 同目录找 app.exe / Shangxian.exe / 上线助手.exe
"$CLI" app status --json     # 确认 running（没在跑时退出码 3）
```

找不到程序：让用户手动打开应用；命令行照样能连上（它靠配置文件夹里的 `ctl.json` 找到应用）。

### 10. 站点目录（密码文件 / nginx 配置放哪）

密码文件目录的解析顺序：**服务器的「站点目录」字段（`vhostDir`）→ 自动识别结果（`state.json` 缓存）→ 环境变量 `SHANGXIAN_HTPASSWD_DIR` → 默认 `/xp/panel/vhost`**。站点配置在 `<目录>/nginx/<域名>.conf`，密码文件在 `<目录>/<域名>.htpasswd`。

要改站点目录直接用 `cli server edit`（新增时 `cli server add` 也支持 `--vhost-dir`）；不写 = 保留原来的目录，要清空写 `--vhost-dir ""`：

```bash
"$CLI" server add --name 宝塔机 --host api.example.com --user root --vhost-dir /www/server/panel/vhost --json
"$CLI" server edit srv-xxxxxx --vhost-dir /www/server/panel/vhost --json
```

### 11. 日志文件

- 事件流：`events.jsonl`（一行一个 JSON，2MB 轮转留 2 份）——`cli logs --source app` 读。
- 应用日志：`logs\app.log`（2MB 轮转留 2 份）。
- 隧道日志：`logs\<隧道id>-<8位代码>.ssh.log`，改名不换文件；单文件超 5MB 轮转，保留 `.1`、`.2` 两份备份。
- 本机服务日志：`logs\service-<服务id>.log`（同样 5MB×2）。
- 界面和 `cli logs` 只读当前这些名字的日志；按名字命名的旧日志不再被读取。

## 深入细节

- 完整字段表、全部方法、已知坑、离线行为、`state.json`/`events.jsonl` 结构：
  https://github.com/iranolly/shangxian/blob/main/docs/AI-AGENT.md
- 仓库地址：https://github.com/iranolly/shangxian
