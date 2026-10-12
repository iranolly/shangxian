// 一张本机服务卡片（D4 §3.3 相关）：状态三件套 + 健康徽标 + 看护/自启开关 + 修复带 + 批量勾选。
// 卡片只收稳定 props，实体字段自己从 store 订阅。

import { memo } from "react";

import { SERVICE_STATUS_META } from "../copy/status";
import { actions } from "../store/actions";
import { usePending } from "../store/pendingStore";
import { useService } from "../store/useStore";
import { FixRibbon } from "./card/FixRibbon";
import { MenuButton } from "./card/Menu";
import { formatDay } from "./card/time";
import { toggleSelect, useIsSelected } from "./list/listPrefs";

interface Props {
  serviceId: string;
  selectable: boolean;
  onStop(id: string): void;
  onEdit(id: string): void;
  onLogs(id: string): void;
  onRemove(id: string): void;
}

/** 从完整路径里取程序名，卡片上只显示名字就够了 */
function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function healthChip(service: { health?: "up" | "down" | "unknown" }) {
  if (service.health === "up") {
    return (
      <span className="chip health up" title="本机端口能连上">
        ✓ 端口在听
      </span>
    );
  }
  if (service.health === "down") {
    return (
      <span className="chip health down" title="本机端口连不上">
        端口不通
      </span>
    );
  }
  return (
    <span className="chip health unknown" title="还没探测过这个端口">
      还没探过
    </span>
  );
}

function ServiceCardInner({ serviceId, selectable, onStop, onEdit, onLogs, onRemove }: Props) {
  const service = useService(serviceId);
  const pending = usePending(`service:${serviceId}`);
  const selected = useIsSelected("services", serviceId);

  if (!service) return null;
  const meta = SERVICE_STATUS_META[service.status];
  // 没填启动程序 = 只监控：看护 / 开机自启都用不上
  const noLaunch = !service.launchExe;
  const isBad = service.status === "not_listening" || service.status === "failed";
  // 「正在跑」的三种状态（启动中/在跑/端口不通）都显示「停止」
  const isUp =
    service.status === "starting" ||
    service.status === "running" ||
    service.status === "not_listening";

  return (
    <article className="card" data-pending={pending ? "true" : undefined}>
      <div className="card-row1">
        {selectable ? (
          <label className="select-box">
            <input
              type="checkbox"
              checked={selected}
              aria-label={`选择「${service.name}」`}
              onChange={(e) => toggleSelect("services", serviceId, e.target.checked)}
            />
          </label>
        ) : null}
        <span className={`dot ${service.status}`} aria-hidden="true" />
        <span className={`status-word ${service.status}`}>{meta.word}</span>
        <span className="service-name">{service.name}</span>
        <span className="card-badges">
          {healthChip(service)}
          {service.logOverflowed ? (
            <span className="chip warn" title="程序日志太多，只保留了最近一部分；归档可以在看日志时切换">
              日志只留了最近一段
            </span>
          ) : null}
        </span>
        <span className="card-actions">
          {!service.launchExe ? null : isUp ? (
            <button
              className="btn btn-ghost btn-sm"
              disabled={pending}
              aria-busy={pending}
              onClick={() => onStop(serviceId)}
            >
              {pending ? "正在停止…" : "停止"}
            </button>
          ) : (
            <button
              className="btn btn-primary btn-sm"
              disabled={pending}
              aria-busy={pending}
              onClick={() => actions.serviceStart(serviceId)}
            >
              {pending ? "正在启动…" : "启动"}
            </button>
          )}
          <MenuButton
            disabled={false}
            items={[
              { label: "编辑本机服务", run: () => onEdit(serviceId) },
              { label: "看程序日志", run: () => onLogs(serviceId) },
              {
                label: "删除本机服务",
                run: () => onRemove(serviceId),
                danger: true,
                dividerBefore: true,
                disabled: pending,
              },
            ]}
          />
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
        {service.note ? (
          <>
            <span aria-hidden="true">·</span>
            <span>{service.note}</span>
          </>
        ) : null}
      </div>

      <div className="card-row3">
        <label className="switch" title={noLaunch ? "没填启动程序，用不了" : undefined}>
          <input
            type="checkbox"
            checked={service.watch}
            disabled={noLaunch || pending}
            onChange={(e) => actions.serviceToggleWatch(serviceId, e.target.checked)}
          />
          看护
        </label>
        <label className="switch" title={noLaunch ? "没填启动程序，用不了" : undefined}>
          <input
            type="checkbox"
            checked={service.autostart}
            disabled={noLaunch || pending}
            onChange={(e) => actions.serviceToggleAutostart(serviceId, e.target.checked)}
          />
          开机自启
        </label>
        <span className="card-foot" title={service.tunnelNames.join("、") || undefined}>
          {service.tunnelCount > 0 ? `已发布到 ${service.tunnelCount} 条隧道` : "还没发布"}
        </span>
      </div>

      {!isBad && service.lastFailure ? (
        <div className="card-note">
          <span className="uber-small">
            上次失败（{formatDay(service.lastFailure.ts)}）：{service.lastFailure.reason}
          </span>
        </div>
      ) : null}

      {noLaunch ? (
        <p
          className="hint card-no-launch"
          title="没填启动程序时「看护」和「开机自启」用不上。想用就点「⋯」→「编辑本机服务」把启动程序填上"
        >
          只监控，不帮你启动
        </p>
      ) : null}

      <FixRibbon serviceId={serviceId} onLogs={onLogs} onEditPort={onEdit} />
    </article>
  );
}

export const ServiceCard = memo(ServiceCardInner);
