# 上线助手 · 开发指南（AGENTS.md）

这份文件讲**怎么开发这个项目**：搭环境、编译、跑测试、打包发布、看懂目录、遵守约定。
读者是刚拿到源码的人，以及会读 AGENTS.md 的 AI 编程助手。

- 软件怎么用（给人）→ [README.md](README.md)、[docs/使用指南.md](docs/使用指南.md)
- 怎么用 `cli.exe` 遥控已装好的软件（给 AI agent）→ [docs/AGENTS.md](docs/AGENTS.md)（本文件不重复它的内容）

技术栈：Rust workspace（`core` + `cli` + `app/src-tauri` + `tools/capture`），界面是 React + TypeScript + Vite，外壳用 Tauri 2。只支持 Windows。

## 1. 环境准备（Windows + Git Bash）

所有命令都在 **Git Bash** 里跑（开始菜单搜 “Git Bash”）。不要用 PowerShell / CMD。

| 依赖 | 说明 |
|---|---|
| Rust / cargo | 本机装在 `~/.cargo/bin`，**没有进系统 PATH**。每个新开的 Git Bash 窗口先执行一次下面的 export。 |
| Node.js + npm | 装好后 `node --version`、`npm --version` 能输出版本即可。 |
| 系统 OpenSSH | Windows 10/11 自带；程序用系统 `ssh.exe` 建隧道。 |
| Docker | 只有跑「靶机集成测试」才需要，平时不用装。 |
| WebView2 | Tauri 界面依赖；较新的 Windows 自带，打包时按 `tauri.conf.json` 的 `webViewInstallMode` 处理。 |

（本机已经装好。换机器时 Rust 用 rustup 默认安装即可进 `~/.cargo/bin`，Node.js 用官方安装包。）

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```
> 作用：让当前 Git Bash 窗口能找到 cargo。每个新窗口都要执行一次；
> `bash build-release.sh` 内部会自己加这个 PATH，不用手动处理。

## 2. 目录速览

```
Cargo.toml              Rust 工作区：members = core / app/src-tauri / cli / tools/capture；release 优化也在根
core/                   核心库：SSH 隧道看护、配置、体检、清理……不依赖界面，测试最多
  tests/                集成测试：env_checks、keep_alive、guardian_docker（需靶机）等
cli/                    命令行 cli.exe：查询/改配置的遥控器（用法见 docs/AGENTS.md）
app/                    桌面应用
  src/                  前端：React + TypeScript + Vite（pages、components、store、mock……）
  src-tauri/            Tauri 2 外壳（Rust）：命令注册、托盘、单实例、通知、崩溃恢复
  src-tauri/nsis/       中文安装器文案和安装/卸载钩子
tools/capture/          开发用截图工具（把窗口截成 BMP）
tools/cdp.mjs           开发用：通过 Chrome DevTools 协议查看/截图运行中的界面
docs/                   使用指南.md（给人看）+ AGENTS.md（给 AI 用 cli 的说明书）
skills/shangxian/       给 AI agent 的 skill（SKILL.md）
packaging/说明.txt       便携包里给用户看的说明
DESIGN.md / PROGRESS.md 完整设计 / 开发进度
.setup/                 本机测试环境和临时记录（不进版本库；里面可能有私有信息）
target/                 编译产物（不进版本库）
```

## 3. 编译与运行

```bash
cargo build -p shangxian-core --bins
```
> 在仓库根目录跑。编译核心库和 spike 验证程序（对应 `core/src/bin/spike.rs`）。
> 第一次要下依赖、编译几分钟；之后几十秒。成功最后一行是 `Finished dev profile`，产物 `target/debug/spike.exe`。

```bash
cargo build -p shangxian-cli
```
> 在仓库根目录跑。编译命令行遥控器。
> 通常几十秒（core 已编译时几秒）。产物 `target/debug/cli.exe`。

```bash
cd app
npm install
```
> 在 `app/` 目录跑，第一次装前端依赖才需要。
> 几分钟，结束后出现 `app/node_modules/`。

```bash
cd app
npm run tauri dev
```
> 在 `app/` 目录跑。开发模式：自动起 Vite（端口 1420，见 `tauri.conf.json`）并打开应用窗口。
> 第一次要编译 Rust/Tauri 壳，几分钟；改前端会热更新，改 Rust 会自动重编重启。
> 先退出正在运行的正式版「上线助手」：同一台电脑只允许一个实例（全局互斥量 `Global\Shangxian.App.v1`，见 `app/src-tauri/src/singleton.rs`）。

```bash
cd app
npm run build
```
> 在 `app/` 目录跑。只检查前端：脚本是 `tsc && vite build`（见 `app/package.json`）。
> 几秒到几十秒。成功产出 `app/dist/`；改完界面先跑这个确认没有 TypeScript 错误。

## 4. 测试

```bash
cargo test -p shangxian-core
```
> 在仓库根目录跑。核心库主测试：单元测试 + 集成测试（实测 359 个通过、0 失败、8 个默认跳过；跳过的是需要 Docker 靶机等环境的场景）。
> 第一次编译几分钟；之后约半分钟。每个测试文件最后一行是 `test result: ok`。

```bash
cargo test -p shangxian-cli
```
> 在仓库根目录跑。命令行解析等 22 个用例，编译后瞬间跑完。

```bash
cargo test -p shangxian-core --test env_checks
```
> 只跑一个测试文件（快速自查，不依赖 Docker）。约 10 秒，10 个用例通过。

```bash
cargo test -p shangxian-core --test guardian_docker -- --ignored
```
> 需要本地 Docker sshd 靶机（`127.0.0.1:2222`）和 `.setup/target_key`（见 `core/tests/guardian_docker.rs` 文件头）。
> 会自建/清理测试容器；没有靶机会失败，平时不用跑。

```bash
cargo test -p app
```
> Tauri 外壳的单元测试。**必须先退出正在运行的「上线助手」**，否则 `singleton::second_acquire_is_blocked` 会失败（全局互斥量被占）。应用在跑时实测 27 通过 / 1 失败。

## 5. 打包发布

```bash
bash build-release.sh
```
> 在仓库根目录跑（Windows 上也可以双击 `build-release.bat`，它内部调 Git Bash 跑同一个脚本）。
> 一键流程：编译 release 版 cli.exe → 放到 sidecar 位置 → `npm run tauri build` → 打便携 zip → 算 SHA256。
> 第一次会下载 Tauri 打包工具、做 release 编译，比较久（几分钟到十几分钟）；之后快。
> 成功末尾会列出产物。

产物都在 `target/release/bundle/nsis/`：

- `Shangxian_<版本>_x64-setup.exe`：中文 NSIS 安装包（内含 app.exe + cli.exe + 三份文档）
- `shangxian-v<版本>-portable.zip`：免安装便携包，解压即用
- `SHA256SUMS`：上面两个文件的 SHA256 校验和

**版本号以 `app/src-tauri/tauri.conf.json` 的 `version` 为准：打包脚本只读这里**——安装包名、便携包名都由此得来。改版本号时，`core` / `cli` / `app/src-tauri` 的 `Cargo.toml` 和 `app/package.json`、根 `package.json` 里的 `version` 也一并改成同一个数，免得各处对不上。可以这样核对：

```bash
node -p "require('./app/src-tauri/tauri.conf.json').version"
```
> 在仓库根目录跑，输出当前版本号（例如 `0.6.2`）。

打包注意：

- **不要在 `app/` 里单独跑 `npm run tauri build`**：`cli.exe` sidecar 还没就位，会失败。要打包就在仓库根目录跑 `build-release.sh`。
- `app/src-tauri/binaries/cli-x86_64-pc-windows-msvc.exe` 是脚本生成的 sidecar，已被 `.gitignore` 忽略，不要手改或提交。

## 6. 约定

- **注释和提交信息一律用中文**：这是仓库的既有习惯。
- **版本号单源**：见上一节，只改 `app/src-tauri/tauri.conf.json`。
- **工作区成员**：新加 crate 要登记到根 `Cargo.toml` 的 `members`（见文件顶部）。
- **不要提交**：`target/`、`app/node_modules/`、`app/dist/`、`app/src-tauri/gen/`、`app/src-tauri/binaries/`、`.setup/`、日志、密钥（对照 `.gitignore`）。`.setup/` 是本机测试环境，里面有私有信息。
- **安全红线**：私钥、密码、控制令牌不进代码、日志、版本库；诊断包和配置导出默认脱敏；清理远端只按端口找持有者，绝不按用户名全杀。更多见 README 的「安全约定」。
- **文档分工**：`docs/AGENTS.md` 只讲怎么用 `cli.exe` 遥控软件；给人看的使用指南在 `docs/使用指南.md`（会随发布包一起发）；开发相关的内容写进根目录这份 AGENTS.md。

## 7. 常见坑

- 新开的 Git Bash 里 `cargo: command not found`：先跑 `export PATH="$HOME/.cargo/bin:$PATH"`。
- 跑 `npm run tauri dev` 或 `cargo test -p app` 前，先退出托盘里的正式版「上线助手」。
- 开发模式读写的是本机真实数据目录（`%LOCALAPPDATA%\Shangxian`，或旧的 `%APPDATA%\Shangxian`；便携版是 exe 旁的 `data\`）。
  想隔离，就用绝对路径的 `SHANGXIAN_DATA_DIR` 起开发版：

```bash
cd app
SHANGXIAN_DATA_DIR="D:/AI/SSH_Tunnel_manager/target/dev-data" npm run tauri dev
```
> 在 `app/` 目录跑。让开发版用独立配置，不动你的真实隧道和服务器（来源：`core/src/config.rs` 的数据目录解析顺序）。

- 第一次 `cargo test`、`npm install`、打包都很慢，是在下依赖或编译，不是卡死。
