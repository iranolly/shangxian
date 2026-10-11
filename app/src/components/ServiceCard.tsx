// 一张本机服务卡片：状态三件套（圆点 + 中文状态词 + 一句人话）+ 看护/自启开关 + 坏状态修复带。
// 规则：坏状态在卡片内就能修，最多两个动作，第一个必须是动词。

import { useRef, useState } from "react";

import { SERVICE_STATUS_META } from "../copy/status";
import type { ServiceView } from "../api";
import { useDismissOnOutside } from "./useModalA11y";

interface Props {
  service: ServiceView;
  onStart: () => void;
  onStop: () => void;
  onToggleWatch: (watch: boolean) => void;
  onToggleAutostart: (autostart: boolean) => void;
  onEdit: () => void;
  onLogs: () => void;
  onRemove: () => void;
  /** 删除失败时后端返回的原文（原样展示） */
  notice?: string;
  onDismissNotice?: () => void;
}

/** 从完整路径里取程序名，卡片上只显示名字就够了 */
function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function ServiceCard({
  service,
  onStart,
  onStop,
  onToggleWatch,
  onToggleAutostart,
  onEdit,
  onLogs,
  onRemove,
  notice,
  onDismissNotice,
}: Props) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLSpanElement | null>(null);
  useDismissOnOutside(menuRef, menuOpen, () => setMenuOpen(false));
  const meta = SERVICE_STATUS_META[service.status];
  // 没填启动程序 = 只监控：看护 / 开机自启都用不上
  const noLaunch = !service.launchExe;
  const isBad =
    service.status === "not_listening" || service.status === "failed";
  // 「正在跑」的三种状态（启动中/在跑/端口不通）都显示「停止」
  const isUp =
    service.status === "starting" ||
    service.status === "running" ||
    service.status === "not_listening";

  return (
    <article className="card">
      <div className="card-row1">
        <span className={`dot ${service.status}`} aria-hidden="true" />
        <span className={`status-word ${service.status}`}>{meta.word}</span>
        <span className="service-name">{service.name}</span>
        <span className="card-actions">
          {!service.launchExe ? null : isUp ? (
            <button className="btn btn-ghost btn-sm" onClick={onStop}>
              停止
            </button>
          ) : (
            <button className="btn btn-primary btn-sm" onClick={onStart}>
              启动
            </button>
          )}
          <span className="menu-anchor" ref={menuRef}>
            <button
              className="icon-btn"
              aria-label="更多操作"
              onClick={() => setMenuOpen(!menuOpen)}
            >
              ⋯
            </button>
            {menuOpen && (
              <div className="menu" onMouseLeave={() => setMenuOpen(false)}>
                <button
                  className="menu-item"
                  onClick={() => {
                    setMenuOpen(false);
                    onEdit();
                  }}
                >
                  编辑本机服务
                </button>
                <button
                  className="menu-item"
                  onClick={() => {
                    setMenuOpen(false);
                    onLogs();
                  }}
                >
                  看程序日志
                </button>
                <button
                  className="menu-item danger"
                  onClick={() => {
                    setMenuOpen(false);
                    onRemove();
                  }}
                >
                  删除本机服务
                </button>
              </div>
            )}
          </span>
        </span>
      </div>

      <div className="card-row2">
        <span>
          本机端口 <span className="mono">{service.localPort}</span>
        </span>
        <span aria-hidden="true">·</span>
        {service.launchExe ? (
          <span>启动程序 {baseName(service.launchExe)}</span>
        ) : (
          <span>只监控，不帮你启动</span>
        )}
        {service.note && (
          <>
            <span aria-hidden="true">·</span>
            <span>{service.note}</span>
          </>
        )}
      </div>

      <div className="card-row3">
        <label className="switch" title={noLaunch ? "没填启动程序，用不了" : undefined}>
          <input
            type="checkbox"
            checked={service.watch}
            disabled={noLaunch}
            onChange={(e) => onToggleWatch(e.target.checked)}
          />
          看护
        </label>
        <label
          className="switch"
          title={noLaunch ? "没填启动程序，用不了" : undefined}
        >
          <input
            type="checkbox"
            checked={service.autostart}
            disabled={noLaunch}
            onChange={(e) => onToggleAutostart(e.target.checked)}
          />
          开机自启
        </label>
        <span
          className="card-foot"
          title={service.tunnelNames.join("、") || undefined}
        >
          {service.tunnelCount > 0
            ? `已发布到 ${service.tunnelCount} 条隧道`
            : "还没发布"}
        </span>
      </div>

      {noLaunch && (
        <p className="card-no-launch">
          只监控，不帮你启动：没填启动程序时「看护」和「开机自启」用不上。想用的话，点「⋯」→「编辑本机服务」把启动程序填上。
        </p>
      )}

      {notice ? (
        <div className="fix-ribbon error">
          <span className="fix-text">⚠ {notice}</span>
          <span className="fix-actions">
            <button className="link-btn" onClick={onDismissNotice}>
              知道了
            </button>
          </span>
        </div>
      ) : isBad ? (
        <div className={`fix-ribbon ${service.status}`}>
          <span className="fix-text">
            ⚠ {service.reason ? `${service.reason}` : meta.sentence}
          </span>
          <span className="fix-actions">
            {service.status === "failed" && service.launchExe && (
              <button className="btn btn-primary btn-sm" onClick={onStart}>
                再试一次
              </button>
            )}
            <button className="link-btn" onClick={onLogs}>
              看日志
            </button>
            {service.status === "not_listening" && (
              <button className="link-btn" onClick={onEdit}>
                改端口
              </button>
            )}
          </span>
        </div>
      ) : null}
    </article>
  );
}
