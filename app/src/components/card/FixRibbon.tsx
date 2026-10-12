// 卡片底部的修复带（D4 §3.3 FixRibbon）：
// 命令失败（noticeStore 就地错误）优先，其次是坏状态原因；最多两个动作，第一个必须是动词。

import { SERVICE_STATUS_META, TUNNEL_STATUS_META } from "../../copy/status";
import { actions } from "../../store/actions";
import { clearNotice, useNotice } from "../../store/noticeStore";
import { usePending } from "../../store/pendingStore";
import { useService, useTunnel } from "../../store/useStore";
import { formatDay } from "./time";

interface Props {
  tunnelId?: string;
  serviceId?: string;
  onLogs?(id: string): void;
  onEditPort?(id: string): void;
}

export function FixRibbon({ tunnelId, serviceId, onLogs, onEditPort }: Props) {
  const key = tunnelId ? `tunnel:${tunnelId}` : serviceId ? `service:${serviceId}` : "";
  const notice = useNotice(key);
  const pending = usePending(key);
  const tunnel = useTunnel(tunnelId ?? "");
  const service = useService(serviceId ?? "");

  if (notice) {
    return (
      <div className="fix-ribbon error" role="status">
        <span className="fix-text">⚠ {notice}</span>
        <span className="fix-actions">
          <button className="link-btn" onClick={() => clearNotice(key)}>
            知道了
          </button>
        </span>
      </div>
    );
  }

  if (tunnelId && tunnel) {
    const tunnelBad =
      tunnel.status === "error" ||
      tunnel.status === "disconnected" ||
      (tunnel.status === "connecting" && Boolean(tunnel.reason || tunnel.lastFailure));
    if (!tunnelBad) return null;
    const meta = TUNNEL_STATUS_META[tunnel.status];
    const reason = tunnel.reason || tunnel.lastFailure?.reason || meta.sentence;
    const portBusy = tunnel.reasonCode === "remotePortBusy";
    return (
      <div className={`fix-ribbon ${tunnel.status}`} role="status">
        <span className="fix-text" title={tunnel.lastFailure ? `上次失败：${formatDay(tunnel.lastFailure.ts)}` : undefined}>
          ⚠ {tunnel.status === "error" ? `发布失败：${reason}` : reason}
        </span>
        <span className="fix-actions">
          {portBusy && onEditPort ? (
            <button className="btn btn-primary btn-sm" disabled={pending} onClick={() => onEditPort(tunnelId)}>
              修改云端端口
            </button>
          ) : null}
          {onLogs ? (
            <button className="link-btn" onClick={() => onLogs(tunnelId)}>
              查看日志
            </button>
          ) : null}
        </span>
      </div>
    );
  }

  if (serviceId && service) {
    const serviceBad = service.status === "not_listening" || service.status === "failed";
    if (!serviceBad) return null;
    const meta = SERVICE_STATUS_META[service.status];
    const reason = service.reason || service.lastFailure?.reason || meta.sentence;
    return (
      <div className={`fix-ribbon ${service.status}`} role="status">
        <span className="fix-text" title={service.lastFailure ? `上次失败：${formatDay(service.lastFailure.ts)}` : undefined}>
          ⚠ {reason}
        </span>
        <span className="fix-actions">
          {service.status === "failed" && service.launchExe ? (
            <button
              className="btn btn-primary btn-sm"
              disabled={pending}
              aria-busy={pending}
              onClick={() => actions.serviceStart(serviceId)}
            >
              再试一次
            </button>
          ) : null}
          {onLogs ? (
            <button className="link-btn" onClick={() => onLogs(serviceId)}>
              查看日志
            </button>
          ) : null}
          {service.status === "not_listening" && onEditPort ? (
            <button className="link-btn" onClick={() => onEditPort(serviceId)}>
              修改端口
            </button>
          ) : null}
        </span>
      </div>
    );
  }

  return null;
}
