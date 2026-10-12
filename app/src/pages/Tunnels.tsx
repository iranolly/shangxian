// 发布隧道页（D4 §3.8 重写；2026-10-12 起「访问保护」页合并到本页）：
// 列表工具条（搜索 / 筛选 / 排序 / 分组）+ 批量模式 + 卡片增强（含访问保护状态与「访问记录」）；
// 数据一律从 store 读。
//
// 兼容说明：App.tsx（外壳 lane）仍然在传旧 props；这里接收但只借用 onNavigate，
// 其余数据从 store 取（D4 §1.1 铁律：实体数据只从 store 读）。

import { useCallback, useEffect, useRef, useState } from "react";

import type { ShellIntent } from "../App";
import "../styles/lists.css";
import type { ServerView, ServiceView, TunnelView } from "../api";
import { AuditModal } from "../components/AuditModal";
import { CloneTunnelModal } from "../components/CloneTunnelModal";
import { ConfirmModal } from "../components/ConfirmModal";
import { EditTunnelModal } from "../components/EditTunnelModal";
import { LogsModal, type LogTarget } from "../components/LogsModal";
import { NewTunnelWizard } from "../components/NewTunnelWizard";
import { NginxModal } from "../components/NginxModal";
import { ProtectionModal } from "../components/ProtectionModal";
import type { PageKey } from "../components/Sidebar";
import { TunnelCard } from "../components/TunnelCard";
import { BulkBar } from "../components/list/BulkBar";
import { GroupHeader } from "../components/list/GroupHeader";
import { ListToolbar } from "../components/list/ListToolbar";
import {
  clearListFilters,
  setBulkMode,
  setTunnelPrefs,
  TUNNEL_SORTS,
  TUNNEL_FILTERS,
  useBulkMode,
  useTunnelPrefs,
  getTunnelPrefs,
} from "../components/list/listPrefs";
import {
  sameNumberArray,
  sameStringArray,
  sameTunnelGroups,
  selectTunnelGroups,
  selectTunnelIds,
  selectTunnelStatusCounts,
  selectTunnelTotal,
} from "../components/list/selectors";
import { actions } from "../store/actions";
import { getState, refreshNow } from "../store/snapshotStore";
import { toast } from "../store/toastStore";
import { useStore, useTunnel } from "../store/useStore";

/** 通知定位 / 意图定位：滚到卡片并闪一下（不新增 CSS，用动画闪边框） */
function focusCard(elementId: string): boolean {
  const el = document.getElementById(elementId);
  if (!el) return false;
  el.scrollIntoView({ block: "center" });
  const accent =
    getComputedStyle(el).getPropertyValue("--accent").trim() || "#4f5be8";
  try {
    el.animate(
      [
        { boxShadow: `0 0 0 3px ${accent}66`, borderRadius: "12px" },
        { boxShadow: "0 0 0 3px transparent", borderRadius: "12px" },
      ],
      { duration: 1200, easing: "ease-out" },
    );
  } catch {
    el.style.outline = `3px solid ${accent}`;
    window.setTimeout(() => {
      el.style.outline = "";
    }, 1200);
  }
  return true;
}

interface Props {
  /** 外壳给的数据 / 回调：本页只借用 onNavigate，其余保留只为兼容旧调用点 */
  tunnels?: TunnelView[];
  services?: ServiceView[];
  servers?: ServerView[];
  onStart?: (id: string) => void;
  onStop?: (id: string) => void;
  onNavigate?: (page: PageKey) => void;
  onRefresh?: () => void;
  /** 外壳下发的意图：通知定位 / 打开向导 / 打开云端配置 / 打开日志 */
  intent?: ShellIntent | null;
  /** 意图处理完通知外壳清掉 */
  onIntentDone?: () => void;
}

export function TunnelsPage({ onNavigate, intent, onIntentDone }: Props = {}) {
  const prefs = useTunnelPrefs();
  const visibleIds = useStore(selectTunnelIds, sameStringArray);
  const groups = useStore(selectTunnelGroups, sameTunnelGroups);
  const statusCounts = useStore(selectTunnelStatusCounts, sameNumberArray);
  const total = useStore(selectTunnelTotal);
  const bulkMode = useBulkMode("tunnels");

  const [wizard, setWizard] = useState<{ serviceId?: string; serverId?: string } | null>(
    null,
  );
  const [editFor, setEditFor] = useState<string | null>(null);
  const [logsFor, setLogsFor] = useState<string | null>(null);
  const [logsTarget, setLogsTarget] = useState<LogTarget | null>(null);
  const [nginxFor, setNginxFor] = useState<string | null>(null);
  const [protectFor, setProtectFor] = useState<string | null>(null);
  const [cloneFor, setCloneFor] = useState<string | null>(null);
  const [auditFor, setAuditFor] = useState<string | null>(null);
  const [removeFor, setRemoveFor] = useState<string | null>(null);

  const openEdit = useCallback((id: string) => setEditFor(id), []);
  const openLogs = useCallback((id: string) => setLogsFor(id), []);
  const openNginx = useCallback((id: string) => setNginxFor(id), []);
  const openClone = useCallback((id: string) => setCloneFor(id), []);
  const askRemove = useCallback((id: string) => setRemoveFor(id), []);
  const openProtection = useCallback((id: string) => setProtectFor(id), []);
  const openAudit = useCallback((id: string) => setAuditFor(id), []);
  const goServices = useCallback(() => onNavigate?.("services"), [onNavigate]);

  const exitBulk = useCallback(() => {
    setBulkMode("tunnels", false);
  }, []);

  // 切走页面就退出批量模式（D4 §3.3 退出时机）
  useEffect(() => () => setBulkMode("tunnels", false), []);

  // 外壳意图：定位卡片 / 打开云端配置 / 打开发布向导 / 打开日志
  const handledIntent = useRef<ShellIntent | null>(null);
  useEffect(() => {
    if (!intent || handledIntent.current === intent) return;
    handledIntent.current = intent;
    if (intent.kind === "locate" && intent.entity.type === "tunnel") {
      const id = intent.entity.id;
      if (!getState().byTunnelId[id]) {
        toast.info("这条隧道已不存在，可能刚被删除");
      } else if (!focusCard(`tunnel-card-${id}`)) {
        // 被搜索 / 筛选 / 折叠挡住了：先清掉限制，再定位一次
        clearListFilters("tunnels");
        setTunnelPrefs({ collapsedServers: [] });
        window.setTimeout(() => void focusCard(`tunnel-card-${id}`), 80);
      }
    } else if (intent.kind === "open-nginx") {
      setNginxFor(intent.tunnelId);
    } else if (intent.kind === "open-wizard") {
      setWizard({ serviceId: intent.presetServiceId, serverId: intent.presetServerId });
    } else if (intent.kind === "open-logs") {
      if (intent.target.kind === "tunnel") setLogsFor(intent.target.id);
      else setLogsTarget(intent.target);
    }
    onIntentDone?.();
  }, [intent, onIntentDone]);

  const toggleBulk = useCallback(() => {
    setBulkMode("tunnels", !bulkMode);
  }, [bulkMode]);

  const toggleCollapse = useCallback((serverId: string) => {
    const current = getTunnelPrefs().collapsedServers;
    const next = current.includes(serverId)
      ? current.filter((x) => x !== serverId)
      : [...current, serverId];
    setTunnelPrefs({ collapsedServers: next });
  }, []);

  const filters = TUNNEL_FILTERS.map((f, i) => ({
    value: f.value,
    label: f.label,
    count: statusCounts[i] ?? 0,
  }));

  const renderCard = (id: string) => (
    <div key={id} id={`tunnel-card-${id}`}>
      <TunnelCard
        tunnelId={id}
        selectable={bulkMode}
        onEdit={openEdit}
        onLogs={openLogs}
        onNginx={openNginx}
        onClone={openClone}
        onRemove={askRemove}
        onOpenProtection={openProtection}
        onAudit={openAudit}
        onGoToServices={onNavigate ? goServices : undefined}
      />
    </div>
  );

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">发布隧道</h1>
          <p className="page-sub">把本机服务接入云端；每条都可添加访问保护。</p>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            className="btn btn-ghost"
            title="一键检查应用、网络、服务器、隧道、证书，发现问题会给出处理建议。"
            onClick={() =>
              window.dispatchEvent(new CustomEvent("shangxian:open-doctor"))
            }
          >
            体检
          </button>
          <button className="btn btn-primary" title="发布一条新隧道" onClick={() => setWizard({})}>
            + 发布
          </button>
        </div>
      </header>

      {total === 0 ? (
        <div className="empty">
          <p className="empty-title">还没有发布任何隧道</p>
          <p className="empty-text">分享到公网后，在外部用手机也能打开。</p>
          <button className="btn btn-primary" title="发布第一条隧道" onClick={() => setWizard({})}>
            + 发布
          </button>
        </div>
      ) : (
        <>
          <ListToolbar
            page="tunnels"
            shown={visibleIds.length}
            total={total}
            filters={filters}
            sorts={TUNNEL_SORTS}
            groupLabel="按服务器分组"
            bulkMode={bulkMode}
            onToggleBulk={toggleBulk}
          />

          {visibleIds.length === 0 ? (
            <div className="empty">
              <p className="empty-title">没有符合条件的隧道</p>
              <p className="empty-text">请更换关键词，或清空筛选条件。</p>
              <button
                className="btn btn-primary"
                onClick={() => clearListFilters("tunnels")}
              >
                清空筛选
              </button>
            </div>
          ) : prefs.groupByServer ? (
            groups.map((g) => {
              const collapsed = prefs.collapsedServers.includes(g.serverId);
              return (
                <section className="list-group" key={"g:" + g.serverId}>
                  <GroupHeader
                    serverId={g.serverId}
                    count={g.count}
                    downCount={g.downCount}
                    collapsed={collapsed}
                    onToggle={toggleCollapse}
                  />
                  {collapsed ? null : <div className="service-list">{g.ids.map(renderCard)}</div>}
                </section>
              );
            })
          ) : (
            <div className="service-list">{visibleIds.map(renderCard)}</div>
          )}

          {bulkMode ? (
            <BulkBar page="tunnels" visibleIds={visibleIds} onExit={exitBulk} />
          ) : null}
        </>
      )}

      {wizard ? (
        <WizardHost
          presetServiceId={wizard.serviceId}
          presetServerId={wizard.serverId}
          onClose={() => setWizard(null)}
        />
      ) : null}
      {editFor ? (
        <EditHost
          id={editFor}
          onClose={() => setEditFor(null)}
          onNeedNginx={(id) => {
            setEditFor(null);
            setNginxFor(id);
          }}
        />
      ) : null}
      {logsFor ? <TunnelLogsHost id={logsFor} onClose={() => setLogsFor(null)} /> : null}
      {auditFor ? <AuditHost id={auditFor} onClose={() => setAuditFor(null)} /> : null}
      {logsTarget ? (
        <LogsModal target={logsTarget} onClose={() => setLogsTarget(null)} />
      ) : null}
      {nginxFor ? <NginxHost id={nginxFor} onClose={() => setNginxFor(null)} /> : null}
      {protectFor ? (
        <ProtectionHost
          id={protectFor}
          onClose={() => setProtectFor(null)}
          onSaved={() => {
            const savedId = protectFor;
            setProtectFor(null);
            // 保存成功后直接打开云端配置：贴到服务器面板里才算真的生效。
            setNginxFor(savedId);
            refreshNow();
          }}
        />
      ) : null}
      {cloneFor ? (
        <CloneTunnelModal
          sourceId={cloneFor}
          onClose={() => setCloneFor(null)}
          onOpenEdit={(id) => {
            setCloneFor(null);
            setEditFor(id);
          }}
        />
      ) : null}
      {removeFor ? (
        <RemoveTunnelConfirm id={removeFor} onClose={() => setRemoveFor(null)} />
      ) : null}
    </div>
  );
}

// ---------- 弹层宿主：只存 id，渲染时取最新实体（D4 §1.1 铁律 1） ----------

function WizardHost({
  presetServiceId,
  presetServerId,
  onClose,
}: {
  presetServiceId?: string;
  presetServerId?: string;
  onClose(): void;
}) {
  const services = useStore((s) => s.services);
  const tunnels = useStore((s) => s.tunnels);
  const servers = useStore((s) => s.servers);
  return (
    <NewTunnelWizard
      services={services}
      tunnels={tunnels}
      servers={servers}
      presetServiceId={presetServiceId}
      presetServerId={presetServerId}
      onClose={onClose}
      onDone={() => {
        onClose();
        refreshNow();
      }}
    />
  );
}

function EditHost({
  id,
  onClose,
  onNeedNginx,
}: {
  id: string;
  onClose(): void;
  onNeedNginx(id: string): void;
}) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return (
    <EditTunnelModal
      tunnel={tunnel}
      onClose={onClose}
      onSaved={() => refreshNow()}
      onNeedNginx={(t) => onNeedNginx(t.id)}
    />
  );
}

function TunnelLogsHost({ id, onClose }: { id: string; onClose(): void }) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return <LogsModal id={id} name={tunnel.name} source="tunnel" onClose={onClose} />;
}

function AuditHost({ id, onClose }: { id: string; onClose(): void }) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return <AuditModal tunnelId={id} tunnelName={tunnel.name} onClose={onClose} />;
}

function NginxHost({ id, onClose }: { id: string; onClose(): void }) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return <NginxModal id={id} name={tunnel.name} tunnel={tunnel} onClose={onClose} />;
}

function ProtectionHost({
  id,
  onClose,
  onSaved,
}: {
  id: string;
  onClose(): void;
  onSaved(): void;
}) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return <ProtectionModal tunnel={tunnel} onClose={onClose} onSaved={onSaved} />;
}

function RemoveTunnelConfirm({ id, onClose }: { id: string; onClose(): void }) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return (
    <ConfirmModal
      title={`删除隧道「${tunnel.name}」？`}
      body="外部访客将立即无法访问，配置一并删除；5 秒内可撤销。"
      confirmLabel="删除"
      danger
      onConfirm={() => {
        onClose();
        void actions.tunnelRemove(id);
      }}
      onClose={onClose}
    />
  );
}
