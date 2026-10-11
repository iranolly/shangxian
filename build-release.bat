@echo off
rem 双击这个文件就能打发布包（内部调用 Git Bash 跑 build-release.sh）
rem 用法：双击，或者命令行里执行 build-release.bat
setlocal
cd /d "%~dp0"

set "BASH=%ProgramFiles%\Git\bin\bash.exe"
if not exist "%BASH%" set "BASH=%ProgramFiles(x86)%\Git\bin\bash.exe"
if not exist "%BASH%" set "BASH=%LOCALAPPDATA%\Programs\Git\bin\bash.exe"
if not exist "%BASH%" (
  echo [错误] 没找到 Git Bash。请装 Git for Windows，或者手动跑：bash build-release.sh
  pause
  exit /b 1
)

"%BASH%" build-release.sh
if errorlevel 1 (
  echo.
  echo [失败] 上面有错误信息。
) else (
  echo.
  echo [完成] 安装包在上面列出的路径里。
)
pause
