// 一张发布隧道卡片：服务名 → 服务器名 → 云端端口 → 网址 + 状态词。
// 本机服务没在跑时明确标出来，出错时带原因 + 重连。
// 访问保护状态与「检查云端」不在这里，统一在「访问保护」页看和改。

import { useRef, useState } from "react";

import { TUNNEL_STATUS_META } from "../copy/status";
import type { TunnelView } from "../api";
import { useDismissOnOutside } from "./useModalA11y";

interface Props {
  tunnel: TunnelView;
  serverName: string;
  onStart: () => void;
  onStop: () => void;
  onRestart: () => void;
  onEdit: () => void;
  onRemove: () => void;
  onLogs: () => void;
  onNginx: () => void;
  /** 跳到「本机服务」页（由页面传入） */
  onGoToServices?: () => void;
  /** 删除失败时后端返回的原文（原样展示） */
  notice?: string;
  onDismissNotice?: () => void;
}

export function TunnelCard({
  tunnel,
  serverName,
  onStart,
  onStop,
  onRestart,
  onEdit,
  onRemove,
  onLogs,
  onNginx,
  onGoToServices,
  notice,
  onDismissNotice,
}: Props) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLSpanElement | null>(null);
  useDismissOnOutside(menuRef, menuOpen, () => setMenuOpen(false));
  const meta = TUNNEL_STATUS_META[tunnel.status];
  const isBad = tunnel.status === "disconnected" || tunnel.status === "error";
  // 有失败原因时，「连接中」也要把原因摆出来，不能只让人干等
  const showFix =
    isBad || (tunnel.status === "connecting" && Boolean(tunnel.reason));

  return (
    <article className="card">
      <div className="card-row1">
        <span className={`dot ${tunnel.status}`} aria-hidden="true" />
        <span className={`status-word ${tunnel.status}`}>{meta.word}</span>
        <span className="service-name">{tunnel.name}</span>
        <span className="card-actions">
          {tunnel.enabled ? (
            <button className="btn btn-ghost btn-sm" onClick={onStop}>
              暂停
            </button>
          ) : (
            <button className="btn btn-primary btn-sm" onClick={onStart}>
              开始发布
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
                    onRestart();
                  }}
                >
                  立刻重连
                </button>
                <button
                  className="menu-item"
                  onClick={() => {
                    setMenuOpen(false);
                    onEdit();
                  }}
                >
                  编辑隧道
                </button>
                <button
                  className="menu-item"
                  onClick={() => {
                    setMenuOpen(false);
                    onNginx();
                  }}
                >
                  云端配置（nginx）
                </button>
                <button
                  className="menu-item"
                  onClick={() => {
                    setMenuOpen(false);
                    onLogs();
                  }}
                >
                  看发布日志
                </button>
                <button
                  className="menu-item danger"
                  onClick={() => {
                    setMenuOpen(false);
                    onRemove();
                  }}
                >
                  删除隧道
                </button>
              </div>
            )}
          </span>
        </span>
      </div>

      <div className="card-row2">
        <span>{tunnel.serviceName}</span>
        <span aria-hidden="true">→</span>
        <span>{serverName}</span>
        <span aria-hidden="true">·</span>
        <span>
          云端 <span className="mono">{tunnel.remotePort}</span>
        </span>
        {tunnel.url && (
          <>
            <span aria-hidden="true">·</span>
            <span className="mono url">{tunnel.url}</span>
          </>
        )}
      </div>

      {!tunnel.serviceOk && (
        <div className="card-note">
          <span className="chip warn">本机服务没在跑（访问会失败）</span>
          {onGoToServices && (
            <button className="link-btn" onClick={onGoToServices}>
              去本机服务页看看
            </button>
          )}
        </div>
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
      ) : showFix ? (
        <div className={`fix-ribbon ${tunnel.status}`}>
          <span className="fix-text">
            ⚠{" "}
            {tunnel.reason
              ? tunnel.status === "error"
                ? `发布失败：${tunnel.reason}。`
                : tunnel.status === "connecting"
                  ? `上次连接失败：${tunnel.reason}。正在重试。`
                  : `连不上云端：${tunnel.reason}。`
              : meta.sentence}
          </span>
          <span className="fix-actions">
            <button className="btn btn-primary btn-sm" onClick={onRestart}>
              {tunnel.status === "error" ? "重试" : "立刻重连"}
            </button>
            <button className="link-btn" onClick={onLogs}>
              看日志
            </button>
          </span>
        </div>
      ) : null}
    </article>
  );
}
