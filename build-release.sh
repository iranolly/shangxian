#!/usr/bin/env bash
# 一键发布：先把命令行构建成 sidecar（安装包会带上它），再打应用安装包，
# 最后出一个免安装便携包和 SHA256SUMS 校验文件。
#
# 用法（在项目根目录）：
#   bash build-release.sh          # Windows 上也可以双击 build-release.bat
#
# 出来的东西（都在 target/release/bundle/nsis/ 下）：
#   Shangxian_<版本>_x64-setup.exe   安装包
#   shangxian-v<版本>-portable.zip   便携包：app.exe + cli.exe + shangxian.portable + 说明.txt
#                                    + AGENTS.md + SKILL.md（给 AI 助手看）+ 使用指南.md（给人看），
#                                    三份文档都在 exe 同层
#   SHA256SUMS                       上面两个文件的 SHA256 校验和
#
# 安装包和便携包都会带上给 AI 助手看的 docs/AGENTS.md、skills/shangxian/SKILL.md，以及给人看的 docs/使用指南.md：
# 安装包走 app/src-tauri/tauri.conf.json 的 bundle.resources（落在 exe 同层），
# 便携包走下面第 ④ 步的 cp。
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

# 版本号只从 tauri.conf.json 读，免得三处版本号各说各的
VER="$(node -p "require('./app/src-tauri/tauri.conf.json').version")"
OUT="target/release/bundle/nsis"
SETUP="$OUT/Shangxian_${VER}_x64-setup.exe"
PORTABLE="$OUT/shangxian-v${VER}-portable.zip"

if [ ! -f "$SETUP" ]; then
  echo "✗ 没找到安装包：$SETUP（tauri build 是不是失败了？）" >&2
  exit 1
fi

echo "④ 打便携包（app.exe + cli.exe + AGENTS.md + SKILL.md + 使用指南.md + shangxian.portable 空文件 + 说明.txt）…"
rm -rf "$OUT/portable"
mkdir -p "$OUT/portable"
cp target/release/app.exe target/release/cli.exe "$OUT/portable/"
# 文档放根层与 exe 同层（安装包里同名文件由 bundle.resources 带上，见文件头说明）：
# 前两份给 AI 助手看，使用指南.md 给人看
cp docs/AGENTS.md skills/shangxian/SKILL.md docs/使用指南.md "$OUT/portable/"
# 便携标记按 D1 §12：exe 旁边有这个文件 → 数据全放 <程序目录>\data，拷走即搬家
: > "$OUT/portable/shangxian.portable"
sed "s/<版本>/${VER}/g" packaging/说明.txt > "$OUT/portable/说明.txt"

if command -v powershell.exe >/dev/null 2>&1; then
  PS=powershell.exe
elif command -v powershell >/dev/null 2>&1; then
  PS=powershell
else
  echo "✗ 没找到 PowerShell，打不了 zip。请手动把 $OUT/portable 里的内容压成 $PORTABLE" >&2
  exit 1
fi
# 用 -Path '<目录>/*'，保证 zip 里 app.exe 在根，不套一层 portable 目录
"$PS" -NoProfile -Command "Compress-Archive -Path '$OUT/portable/*' -DestinationPath '$PORTABLE' -Force"

echo "⑤ 算 SHA256 校验和…"
( cd "$OUT" && sha256sum "Shangxian_${VER}_x64-setup.exe" "shangxian-v${VER}-portable.zip" > SHA256SUMS )

echo
echo "✓ 完成，产物在 target/release/bundle/nsis/："
ls -1 "$OUT"/*.exe "$OUT"/*.zip "$OUT/SHA256SUMS" 2>/dev/null || true
echo
echo "校验：cd $OUT && sha256sum -c SHA256SUMS"
echo
echo "提示：装完以后 cli.exe 跟应用在同一个文件夹；命令行在任何地方都能用"
echo "      （它靠 %APPDATA%\\Shangxian\\ctl.json 找到正在跑的应用；便携版靠同目录的 shangxian.portable）。"
