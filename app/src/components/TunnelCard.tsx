// 一张发布隧道卡片（D4 §3.3 TunnelCard）：
// 状态三件套 + 保护/预设徽标 + 运行时长/重试倒计时 + 网格信息 + 修复带 + 批量勾选。
// 纪律：卡片只收稳定 props（id / 回调），实体字段自己从 store 订阅（React.memo 才有效）。

import { memo } from "react";

import { TUNNEL_STATUS_META } from "../copy/status";
import { actions } from "../store/actions";
import { usePending } from "../store/pendingStore";
import { useServer, useTunnel } from "../store/useStore";
import { CardMenu } from "./card/CardMenu";
import { CloudDriftBar } from "./card/CloudDriftBar";
import { FixRibbon } from "./card/FixRibbon";
import { PresetBadge } from "./card/PresetBadge";
import { ProtectionBadge } from "./card/ProtectionBadge";
import { RetryLine } from "./card/RetryLine";
import { UptimeLine } from "./card/UptimeLine";
import { toggleSelect, useIsSelected } from "./list/listPrefs";

interface Props {
  tunnelId: string;
  /** 批量模式：显示勾选框 */
  selectable: boolean;
  onEdit(id: string): void;
  onLogs(id: string): void;
  onNginx(id: string): void;
  onClone(id: string): void;
  onRemove(id: string): void;
  onOpenProtection?(id: string): void;
  onGoToServices?(): void;
}

function TunnelCardInner({
  tunnelId,
  selectable,
  onEdit,
  onLogs,
  onNginx,
  onClone,
  onRemove,
  onOpenProtection,
  onGoToServices,
}: Props) {
  const tunnel = useTunnel(tunnelId);
  const server = useServer(tunnel?.serverId ?? "");
  const pending = usePending(`tunnel:${tunnelId}`);
  const selected = useIsSelected("tunnels", tunnelId);

  if (!tunnel) return null;
  const meta = TUNNEL_STATUS_META[tunnel.status];
  const serverName = server?.name ?? "（服务器已删除）";
  const showRuntimeRow =
    (tunnel.status === "running" && Boolean(tunnel.connectedSince || tunnel.uptimeSecs)) ||
    tunnel.status === "disconnected" ||
    (tunnel.status === "connecting" && tunnel.retryState === "active" && tunnel.retryInSecs != null);

  return (
    <article className="card" data-pending={pending ? "true" : undefined}>
      <div className="card-row1">
        {selectable ? (
          <label className="select-box">
            <input
              type="checkbox"
              checked={selected}
              aria-label={`选择「${tunnel.name}」`}
              onChange={(e) => toggleSelect("tunnels", tunnelId, e.target.checked)}
            />
          </label>
        ) : null}
        <span className={`dot ${tunnel.status}`} aria-hidden="true" />
        <span className={`status-word ${tunnel.status}`}>{meta.word}</span>
        <span className="service-name">{tunnel.name}</span>
        <span className="card-badges">
          <ProtectionBadge tunnelId={tunnelId} onOpenProtection={onOpenProtection} />
          <PresetBadge preset={tunnel.preset} />
        </span>
        <span className="card-actions">
          {tunnel.enabled ? (
            <button
              className="btn btn-ghost btn-sm"
              disabled={pending}
              aria-busy={pending}
              onClick={() => actions.tunnelPause(tunnelId)}
            >
              {pending ? "正在暂停…" : "暂停"}
            </button>
          ) : (
            <button
              className="btn btn-primary btn-sm"
              disabled={pending}
              aria-busy={pending}
              onClick={() => actions.tunnelStart(tunnelId)}
            >
              {pending ? "处理中…" : "开始发布"}
            </button>
          )}
          <CardMenu
            tunnel={tunnel}
            pending={pending}
            onEdit={onEdit}
            onNginx={onNginx}
            onLogs={onLogs}
            onClone={onClone}
            onOpenProtection={onOpenProtection ?? (() => undefined)}
            onRemove={onRemove}
          />
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
        {tunnel.url ? (
          <>
            <span aria-hidden="true">·</span>
            <span className="mono url">{tunnel.url}</span>
          </>
        ) : null}
      </div>

      {showRuntimeRow ? (
        <div className="card-row3">
          <UptimeLine tunnelId={tunnelId} />
          <RetryLine tunnelId={tunnelId} />
        </div>
      ) : null}

      {!tunnel.serviceOk ? (
        <div className="card-note">
          <span
            className="chip warn"
            title="本机服务没在跑，外面访问会失败；点「去启动」把它开起来"
          >
            ⚠ 本机服务没在跑
          </span>
          {onGoToServices ? (
            <button className="link-btn" onClick={onGoToServices}>
              去启动
            </button>
          ) : null}
        </div>
      ) : null}

      <CloudDriftBar tunnelId={tunnelId} onOpenNginx={onNginx} />

      <FixRibbon tunnelId={tunnelId} onLogs={onLogs} onEditPort={onEdit} />
    </article>
  );
}

export const TunnelCard = memo(TunnelCardInner);
