; 上线助手 —— NSIS 安装器钩子（Tauri bundle.windows.nsis.installerHooks 引入）
; 模板会在 4 个固定位置 !insertmacro 这里的宏（先 include 本文件，宏体在插入点展开，
; 所以宏里引用 ${PRODUCTNAME} / ${UNINSTKEY} / ${MANUPRODUCTKEY} 是安全的）。
;
; 本文件负责：
;   1) 结束页「现在就打开 上线助手」/ 卸载确认页大白话文案；
;   2) 安装/卸载前先停掉应用拉起的隧道和本机服务（cli.exe app stop-all --yes）；
;   3) 覆盖升级时把 HKCU Run 自启项存一边、装完还回去（模板卸载会删 Run 值）；
;   4) 把模板建的 Shangxian.lnk 改名成 上线助手.lnk（开始菜单 / 桌面）；
;   5) 卸载时按勾选框清理真实数据目录 %APPDATA%\Shangxian 与 %LOCALAPPDATA%\Shangxian。
; 依赖：模板已 include MUI2(LogicLib) / FileFunc / StrFunc；nsExec 是 NSIS 自带插件。
; 编码：UTF-8（带 BOM）。

; ---------- 结束页 / 卸载确认页文案（必须在对应页面宏插入前、也就是本文件被 include 时定义） ----------
!define MUI_FINISHPAGE_RUN_TEXT "立即打开 上线助手"
!define MUI_UNCONFIRMPAGE_TEXT_TOP "确定要卸载 上线助手 吗？$\r$\n$\r$\n• 发布中的隧道将先停止；应用启动的本机服务也会停止$\r$\n• 配置和密钥默认保留，如需一并删除请勾选下方「同时删除配置和密钥」$\r$\n$\r$\n点击「卸载」开始。"

; ---------- 公共动作 ----------

; 停掉应用负责的一切（隧道 + 它拉起的本机服务）；cli.exe 不在或应用没在跑就什么都不做。
; nsExec 会隐藏控制台窗口并等它退出；cli 侧有 15 秒硬上限（D5 S5.3），不会把安装器卡死。
!macro SX_STOP_ALL
  ${If} ${FileExists} "$INSTDIR\cli.exe"
    DetailPrint "正在停止发布中的隧道和本机服务…"
    nsExec::ExecToLog '"$INSTDIR\cli.exe" app stop-all --yes'
    Pop $0
  ${EndIf}
!macroend

; 把 HKCU Run 自启项的内容存一边（升级走「先卸载再安装」时，卸载器会删掉 Run 值）。
!macro SX_STASH_AUTOSTART
  ClearErrors
  ReadRegStr $R9 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${If} $R9 != ""
    WriteRegStr HKCU "${MANUPRODUCTKEY}" "AutostartBackup" "$R9"
  ${EndIf}
!macroend

; 装完把自启项还回去（只在用户原来就开着、而且现在确实没有时才写）。
!macro SX_RESTORE_AUTOSTART
  ClearErrors
  ReadRegStr $R9 HKCU "${MANUPRODUCTKEY}" "AutostartBackup"
  ${If} $R9 != ""
    ReadRegStr $R8 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
    ${If} $R8 == ""
      WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}" "$R9"
    ${EndIf}
    DeleteRegValue HKCU "${MANUPRODUCTKEY}" "AutostartBackup"
  ${EndIf}
!macroend

; 快捷方式改名：模板建的是 Shangxian.lnk，改成 上线助手.lnk（保留 AUMID）。
; 新装：模板建在开始菜单文件夹里；覆盖升级：老版本可能建在开始菜单根目录，挪进文件夹里。
!macro SX_RENAME_SHORTCUTS
  !insertmacro MUI_STARTMENU_GETFOLDER Application $AppStartMenuFolder

  ; 模板刚建在开始菜单文件夹里的
  ${If} ${FileExists} "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
    Delete "$SMPROGRAMS\$AppStartMenuFolder\上线助手.lnk"
    Rename "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$SMPROGRAMS\$AppStartMenuFolder\上线助手.lnk"
  ${EndIf}

  ; 老版本留在开始菜单根目录的：能拿到文件夹名就挪进文件夹；
  ; 用户这次选了「不要创建快捷方式」（变量是 ">"）时留在根目录，只改名
  ${If} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${If} $AppStartMenuFolder != ""
    ${AndIf} $AppStartMenuFolder != ">"
      CreateDirectory "$SMPROGRAMS\$AppStartMenuFolder"
      Delete "$SMPROGRAMS\$AppStartMenuFolder\上线助手.lnk"
      Rename "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$SMPROGRAMS\$AppStartMenuFolder\上线助手.lnk"
    ${Else}
      Delete "$SMPROGRAMS\上线助手.lnk"
      Rename "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$SMPROGRAMS\上线助手.lnk"
    ${EndIf}
  ${EndIf}

  ; 桌面
  ${If} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
    Delete "$DESKTOP\上线助手.lnk"
    Rename "$DESKTOP\${PRODUCTNAME}.lnk" "$DESKTOP\上线助手.lnk"
  ${EndIf}
!macroend

; 卸载时清理真实数据目录（只有勾了「同时删除配置和密钥」且不是 /UPDATE 升级时才删）。
; 模板没删对目录（它删的是 %APPDATA%\com.shangxian.zhushou，根本不存在），这里补上。
!macro SX_DELETE_DATA
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    DetailPrint "正在删除配置和密钥…"
    SetShellVarContext current

    ; 老安装用 %APPDATA%，新安装用 %LOCALAPPDATA%，两个都清
    RMDir /r "$APPDATA\Shangxian"
    RMDir /r "$LOCALAPPDATA\Shangxian"

    ; 模板删的那两个 BUNDLEID 目录（历史遗留，兼容一下）
    RMDir /r "$APPDATA\${BUNDLEID}"
    RMDir /r "$LOCALAPPDATA\${BUNDLEID}"

    ; 删不干净时说清楚，别假装成功
    ${If} ${FileExists} "$APPDATA\Shangxian\*.*"
      MessageBox MB_ICONEXCLAMATION "配置文件夹未能完全删除（可能有文件被占用）。请手动删除：$APPDATA\Shangxian"
    ${EndIf}
    ${If} ${FileExists} "$LOCALAPPDATA\Shangxian\*.*"
      MessageBox MB_ICONEXCLAMATION "配置文件夹未能完全删除（可能有文件被占用）。请手动删除：$LOCALAPPDATA\Shangxian"
    ${EndIf}
  ${EndIf}
!macroend

; ---------- 4 个约定钩子（插入顺序与模板一致） ----------

!macro NSIS_HOOK_PREINSTALL
  !insertmacro SX_STOP_ALL          ; 先停隧道和本机服务，再让模板去处理 app.exe
  !insertmacro SX_STASH_AUTOSTART   ; 记下自启项
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; 「应用和功能」里显示中文名 + 中文发布者。发布者走 hook 而不是 bundle.publisher，
  ; 这样 MANUPRODUCTKEY 保持 HKCU\Software\shangxian\Shangxian 不变——覆盖升级时
  ; 老安装（含用户自选的安装目录）能被 RestorePreviousInstallLocation 读到。
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayName" "上线助手"
  WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "上线助手"
  !insertmacro SX_RENAME_SHORTCUTS
  !insertmacro SX_RESTORE_AUTOSTART
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro SX_STOP_ALL
  !insertmacro SX_STASH_AUTOSTART
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    !insertmacro MUI_STARTMENU_GETFOLDER Application $AppStartMenuFolder
    Delete "$SMPROGRAMS\$AppStartMenuFolder\上线助手.lnk"
    Delete "$SMPROGRAMS\上线助手.lnk"
    Delete "$DESKTOP\上线助手.lnk"
    RMDir "$SMPROGRAMS\$AppStartMenuFolder"
  ${EndIf}
  !insertmacro SX_DELETE_DATA
!macroend
