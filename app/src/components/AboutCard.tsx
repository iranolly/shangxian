// 关于卡片（D4 §3.7）：版本 / 数据目录 / SSH 可用性 / 后台守卫 / 配置状态 / 检查更新 / 诊断包。
// 数据从快照 app 读；SSH 行复用最近一次体检结果（会话内缓存）。

import { useState } from "react";

import { openUrl } from "@tauri-apps/plugin-opener";

import { checkUpdate, openDataDir, type UpdateInfo } from "../api";
import { DOCTOR_STATUS_META, type DoctorStatus } from "../copy/status";
import { refreshNow } from "../store/snapshotStore";
import { toast } from "../store/toastStore";
import { useApp } from "../store/useStore";
import { useLastDoctorReport } from "./DoctorModal";

interface Props {
  onOpenDoctor(): void;
  onOpenDiagnostics(): void;
}

function formatDay(at?: number | null): string {
  if (typeof at !== "number" || at <= 0) return "尚未记录时间";
  const d = new Date(at);
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const clock = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1);
  const isYesterday =
    d.getFullYear() === yesterday.getFullYear() &&
    d.getMonth() === yesterday.getMonth() &&
    d.getDate() === yesterday.getDate();
  if (sameDay) return `今天 ${clock}`;
  if (isYesterday) return `昨天 ${clock}`;
  return `${d.getMonth() + 1} 月 ${d.getDate()} 日 ${clock}`;
}

export function AboutCard({ onOpenDoctor, onOpenDiagnostics }: Props) {
  const app = useApp();
  const report = useLastDoctorReport();
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState("");
  // 刚查到的结果优先显示，不等下一份快照
  const [liveUpdate, setLiveUpdate] = useState<UpdateInfo | null>(null);

  const update = liveUpdate ?? app.updateInfo;
  const ssh = report?.steps.find((s) => s.id === "ssh");
  const sshMeta = ssh ? DOCTOR_STATUS_META[ssh.status as DoctorStatus] : null;
  // 旧后端可能给 ok / skipped，这里一并认成通过
  const sshOk = ssh ? ssh.status === "pass" || (ssh.status as string) === "ok" : false;

  const runCheck = async () => {
    setChecking(true);
    setCheckError("");
    try {
      const result = await checkUpdate();
      setLiveUpdate(result);
      refreshNow();
    } catch (e) {
      setCheckError(String(e) || "检查更新失败");
    } finally {
      setChecking(false);
    }
  };

  const download = () => {
    if (!update.url) {
      toast.error("未获取到下载地址，请点击「再试一次」");
      return;
    }
    void openUrl(update.url).catch(() => toast.error("无法打开下载页，请手动前往发布页"));
  };

  const updateText = (() => {
    switch (update.status) {
      case "upToDate":
        return `已是最新版本（v${update.latest ?? (app.appVersion || "未知")}）`;
      case "available":
        return `发现新版本 ${update.latest ?? "？"}，当前版本 v${app.appVersion || "未知"}`;
      case "error":
        return `检查失败：${update.error ?? "原因未知"}`;
      default:
        return "尚未检查";
    }
  })();
  const updateTone =
    update.status === "upToDate"
      ? "ok"
      : update.status === "available"
        ? "warn"
        : update.status === "error"
          ? "error"
          : "muted";

  return (
    <article className="card about-card">
      <div className="about-head">
        <h2 className="about-title">关于</h2>
        <span className="about-version">v{app.appVersion || "未知"}</span>
      </div>

      <div className="about-row">
        <span className="about-label">数据目录</span>
        <span className="about-value mono">{app.dataDir || "尚未获取"}</span>
        <span className="about-actions">
          <button
            className="btn btn-ghost btn-sm"
            title="配置、日志、密钥都在这里"
            onClick={() => {
              void openDataDir().catch(() => toast.error("无法打开数据目录，请手动到文件夹中查找"));
            }}
          >
            打开
          </button>
        </span>
      </div>

      <div className="about-row">
        <span className="about-label">SSH 可用性</span>
        {ssh && sshMeta ? (
          <span className={`about-value ${sshMeta.tone}`}>
            {sshMeta.icon} {ssh.detail}
          </span>
        ) : (
          <span className="about-value muted" title="运行一次体检，检查 SSH 是否可用">
            点击「去体检」查看
          </span>
        )}
        {(!ssh || !sshOk) && (
          <span className="about-actions">
            <button className="btn btn-ghost btn-sm" onClick={onOpenDoctor}>
              去体检
            </button>
          </span>
        )}
      </div>

      <div className="about-row">
        <span className="about-label">后台守卫</span>
        {app.jobGuardOk ? (
          <span className="about-value ok">✓ 正常</span>
        ) : (
          <span className="about-value warn">⚠ {app.jobGuardError ?? "后台守卫未正确配置"}</span>
        )}
        {!app.jobGuardOk && (
          <span className="about-actions">
            <button className="btn btn-ghost btn-sm" onClick={onOpenDoctor}>
              去体检
            </button>
          </span>
        )}
      </div>

      <div className="about-row">
        <span className="about-label">配置状态</span>
        {app.configTrusted ? (
          <span className="about-value ok">✓ 正常</span>
        ) : (
          <span className="about-value warn">⚠ 配置无法读取，改动暂时不能保存</span>
        )}
      </div>

      <div className="about-row">
        <span className="about-label">检查更新</span>
        <span className={`about-value ${updateTone}`}>
          {updateText}
          <span className="about-hint hint">
            上次检查：{update.checkedAt ? formatDay(update.checkedAt) : "尚未检查"}
          </span>
          {checkError && <span className="about-hint hint">⚠ {checkError}</span>}
        </span>
        <span className="about-actions">
          {update.status === "available" ? (
            <button className="btn btn-primary btn-sm" onClick={download}>
              去下载
            </button>
          ) : update.status === "error" ? (
            <button className="btn btn-ghost btn-sm" disabled={checking} onClick={() => void runCheck()}>
              再试一次
            </button>
          ) : (
            <button className="btn btn-ghost btn-sm" disabled={checking} onClick={() => void runCheck()}>
              {checking ? "检查中…" : "检查更新"}
            </button>
          )}
        </span>
      </div>

      <div className="about-row">
        <span className="about-label">诊断包</span>
        <span className="about-value" title="诊断包供排查人员使用">
          密码、密钥、令牌不会放进去。
        </span>
        <span className="about-actions">
          <button className="btn btn-ghost btn-sm" onClick={onOpenDiagnostics}>
            导出诊断包
          </button>
        </span>
      </div>
    </article>
  );
}
