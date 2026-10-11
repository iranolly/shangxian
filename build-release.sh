#!/usr/bin/env bash
# 一键发布：先把命令行构建成 sidecar（安装包会带上它），再打应用安装包。
#
# 用法（在项目根目录）：
#   bash build-release.sh
#
# 出来的东西：
#   target/release/bundle/nsis/Shangxian_<版本>_x64-setup.exe   安装包（推荐）
#   target/release/bundle/msi/Shangxian_<版本>_x64_en-US.msi    安装包（备用）
# 装完以后，应用和 cli.exe 在同一个文件夹里。
set -e
cd "$(dirname "$0")"

TRIPLE="x86_64-pc-windows-msvc"
CARGO="${CARGO:-$HOME/.cargo/bin/cargo.exe}"
if [ ! -x "$CARGO" ]; then
  CARGO="cargo"
fi

# tauri 打包时要自己调 cargo；本机 cargo 装在 ~/.cargo/bin，没进系统 PATH，这里补上
export PATH="$HOME/.cargo/bin:$PATH"

echo "① 构建命令行（release）…"
"$CARGO" build --release -p shangxian-cli

echo "② 把 cli.exe 放到 sidecar 位置（安装包会把它一起装上）…"
mkdir -p app/src-tauri/binaries
cp target/release/cli.exe "app/src-tauri/binaries/cli-${TRIPLE}.exe"
ls -la "app/src-tauri/binaries/cli-${TRIPLE}.exe"

echo "③ 打应用安装包（第一次会下载打包工具，之后很快）…"
(cd app && npm run tauri build)

echo
echo "✓ 完成，安装包在："
ls -1 target/release/bundle/nsis/*.exe 2>/dev/null || true
ls -1 target/release/bundle/msi/*.msi 2>/dev/null || true
echo
echo "提示：装完以后 cli.exe 跟应用在同一个文件夹；命令行在任何地方都能用"
echo "      （它靠 %APPDATA%\\Shangxian\\ctl.json 找到正在跑的应用）。"
