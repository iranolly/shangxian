// 本机服务页（D4 §3.8 重写）：
// 列表工具条（搜索 / 筛选 / 排序）+ 批量模式 + 卡片增强（健康徽标 / lastFailure / 撤销删除）。
// 数据一律从 store 读；兼容旧外壳传进来的 props（只借用 onNavigate）。

import { useCallback, useEffect, useRef, useState } from "react";

import type { ShellIntent } from "../App";
import "../styles/lists.css";
import type { ServiceStopResult, ServiceView } from "../api";
import { ConfirmModal } from "../components/ConfirmModal";
import { ImportModal } from "../components/ImportModal";
import { LogsModal, type LogTarget } from "../components/LogsModal";
import { ServiceCard } from "../components/ServiceCard";
import { ServiceModal } from "../components/ServiceModal";
import type { PageKey } from "../components/Sidebar";
import { BulkBar } from "../components/list/BulkBar";
import { ListToolbar } from "../components/list/ListToolbar";
import {
  clearListFilters,
  SERVICE_FILTERS,
  SERVICE_SORTS,
  setBulkMode,
  useBulkMode,
} from "../components/list/listPrefs";
import {
  sameNumberArray,
  sameStringArray,
  selectServiceIds,
  selectServiceStatusCounts,
} from "../components/list/selectors";
import { actions } from "../store/actions";
import { setNotice } from "../store/noticeStore";
import { getState, refreshNow } from "../store/snapshotStore";
import { toast } from "../store/toastStore";
import { useService, useStore } from "../store/useStore";

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
  services?: ServiceView[];
  hasServers?: boolean;
  onNavigate?: (page: PageKey) => void;
  onStart?: (id: string) => void;
  onStop?: (
    id: string,
    killForeign?: boolean,
    expectedPid?: number | null,
  ) => Promise<ServiceStopResult>;
  onToggleWatch?: (id: string, watch: boolean) => void;
  onToggleAutostart?: (id: string, autostart: boolean) => void;
  onRefresh?: () => void;
  /** 外壳下发的意图：通知定位 / 打开新建弹层 / 打开日志 */
  intent?: ShellIntent | null;
  /** 意图处理完通知外壳清掉 */
  onIntentDone?: () => void;
}

export function ServicesPage({ onNavigate, intent, onIntentDone }: Props = {}) {
  const visibleIds = useStore(selectServiceIds, sameStringArray);
  const statusCounts = useStore(selectServiceStatusCounts, sameNumberArray);
  const total = useStore((s) => s.services.length);
  const hasServers = useStore((s) => s.servers.length > 0);
  const bulkMode = useBulkMode("services");

  const [addOpen, setAddOpen] = useState(false);
  const [editFor, setEditFor] = useState<string | null>(null);
  const [autoRead, setAutoRead] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [logsFor, setLogsFor] = useState<string | null>(null);
  const [logsTarget, setLogsTarget] = useState<LogTarget | null>(null);
  /** 从「发布向导」的死胡同跳过来加的：保存后要回去重开向导并预选新服务 */
  const [returnToWizard, setReturnToWizard] = useState(false);
  const [removeFor, setRemoveFor] = useState<string | null>(null);
  const [stopFor, setStopFor] = useState<{ id: string; who: string; pid: number | null } | null>(
    null,
  );

  const openEdit = useCallback((id: string) => {
    setAutoRead(false);
    setEditFor(id);
  }, []);
  const openLogs = useCallback((id: string) => setLogsFor(id), []);
  const askRemove = useCallback((id: string) => setRemoveFor(id), []);
  /** 打开「添加本机服务」弹层；backToWizard = 从发布向导死胡同跳过来的 */
  const openAdd = useCallback((backToWizard = false) => {
    setReturnToWizard(backToWizard);
    setAutoRead(false);
    setEditFor(null);
    setAddOpen(true);
  }, []);
  const switchToEdit = useCallback((id: string) => {
    setAutoRead(true);
    setEditFor(id);
  }, []);

  const closeModal = useCallback(() => {
    setAddOpen(false);
    setEditFor(null);
    setAutoRead(false);
    setReturnToWizard(false);
  }, []);

  const exitBulk = useCallback(() => {
    setBulkMode("services", false);
  }, []);

  // 切走页面就退出批量模式（D4 §3.3 退出时机）
  useEffect(() => () => setBulkMode("services", false), []);

  // 外壳意图：定位卡片 / 打开添加弹层 / 打开日志
  const handledIntent = useRef<ShellIntent | null>(null);
  useEffect(() => {
    if (!intent || handledIntent.current === intent) return;
    handledIntent.current = intent;
    if (intent.kind === "locate" && intent.entity.type === "service") {
      const id = intent.entity.id;
      if (!getState().byServiceId[id]) {
        toast.info("这台本机服务已经不在了，可能刚被删掉");
      } else if (!focusCard(`service-card-${id}`)) {
        // 被搜索 / 筛选挡住了：先清掉限制，再定位一次
        clearListFilters("services");
        window.setTimeout(() => void focusCard(`service-card-${id}`), 80);
      }
    } else if (intent.kind === "open-new-service") {
      openAdd(intent.returnToWizard === true);
    } else if (intent.kind === "open-logs") {
      if (intent.target.kind === "service") setLogsFor(intent.target.id);
      else setLogsTarget(intent.target);
    }
    onIntentDone?.();
  }, [intent, onIntentDone, openAdd]);

  /** 停止：不是本应用启动的进程要先确认（D4 §3.4 用 ConfirmModal 替换 window.confirm） */
  const requestStop = useCallback((id: string) => {
    void (async () => {
      try {
        const first = await actions.serviceStop(id);
        if (first.kind === "needsConfirm") {
          setStopFor({
            id,
            who: first.exe ?? "一个不是本应用启动的程序",
            pid: first.pid ?? null,
          });
        }
      } catch {
        // runCommand 已把后端原文挂在卡片的修复带上
      } finally {
        refreshNow();
      }
    })();
  }, []);

  const confirmStop = () => {
    const target = stopFor;
    setStopFor(null);
    if (!target) return;
    void (async () => {
      try {
        await actions.serviceStop(target.id, true, target.pid);
      } catch {
        // 同上：就地错误
      } finally {
        refreshNow();
      }
    })();
  };

  const filters = SERVICE_FILTERS.map((f, i) => ({
    value: f.value,
    label: f.label,
    count: statusCounts[i] ?? 0,
  }));

  const renderCard = (id: string) => (
    <div key={id} id={`service-card-${id}`}>
      <ServiceCard
        serviceId={id}
        selectable={bulkMode}
        onStop={requestStop}
        onEdit={openEdit}
        onLogs={openLogs}
        onRemove={askRemove}
      />
    </div>
  );

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">本机服务</h1>
          <p className="page-sub">记下端口和启动方式，帮你启动和看护。</p>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            className="btn btn-ghost"
            title="从已有脚本导入本机服务"
            onClick={() => setImportOpen(true)}
          >
            导入脚本
          </button>
          <button className="btn btn-primary" onClick={() => openAdd()}>
            + 添加
          </button>
        </div>
      </header>

      {total === 0 ? (
        <div className="empty">
          <p className="empty-title">还没有登记本机服务</p>
          <p className="empty-text">第一次用？按下面三步走。</p>
          <div className="empty-steps">
            <button type="button" className="card empty-step" onClick={() => onNavigate?.("servers")}>
              <span className={`empty-step-num${hasServers ? " done" : ""}`}>
                {hasServers ? "✓" : "①"}
              </span>
              <span className="empty-step-main">
                <span className="server-name">添加云端服务器</span>
                <span className="server-meta">用哪台服务器转发访问。</span>
              </span>
              <span className="link-btn">{hasServers ? "去看 →" : "去添加 →"}</span>
            </button>

            <button type="button" className="card empty-step" onClick={() => openAdd()}>
              <span className="empty-step-num">②</span>
              <span className="empty-step-main">
                <span className="server-name">登记本机服务</span>
                <span className="server-meta">要发布出去的程序：端口 + 启动方式。</span>
              </span>
              <span className="link-btn">去添加 →</span>
            </button>

            <button type="button" className="card empty-step" onClick={() => onNavigate?.("tunnels")}>
              <span className="empty-step-num">③</span>
              <span className="empty-step-main">
                <span className="server-name">发布隧道</span>
                <span className="server-meta">把本机服务接到云端，生成公网网址。</span>
              </span>
              <span className="link-btn">去看看 →</span>
            </button>
          </div>
        </div>
      ) : (
        <>
          <ListToolbar
            page="services"
            shown={visibleIds.length}
            total={total}
            filters={filters}
            sorts={SERVICE_SORTS}
            bulkMode={bulkMode}
            onToggleBulk={() => setBulkMode("services", !bulkMode)}
          />

          {visibleIds.length === 0 ? (
            <div className="empty">
              <p className="empty-title">没有符合条件的服务</p>
              <p className="empty-text">换个关键词，或者把筛选清掉。</p>
              <button
                className="btn btn-primary"
                onClick={() => clearListFilters("services")}
              >
                清空筛选
              </button>
            </div>
          ) : (
            <div className="service-list">{visibleIds.map(renderCard)}</div>
          )}

          {bulkMode ? (
            <BulkBar page="services" visibleIds={visibleIds} onExit={exitBulk} />
          ) : null}
        </>
      )}

      {addOpen || editFor ? (
        <ServiceModalHost
          editId={editFor}
          autoRead={autoRead}
          onSwitchToEdit={switchToEdit}
          onClose={closeModal}
          onSavedNew={(serviceId) => {
            if (!returnToWizard) return;
            setReturnToWizard(false);
            // 向导死胡同回来：加完服务就回「发布隧道」重开向导，并预选刚加的服务
            window.dispatchEvent(
              new CustomEvent("shangxian:nav-intent", {
                detail: {
                  page: "tunnels",
                  openWizard: true,
                  presetServiceId: serviceId,
                },
              }),
            );
          }}
        />
      ) : null}

      {importOpen ? (
        <ImportModal
          onClose={() => setImportOpen(false)}
          onDone={() => {
            setImportOpen(false);
            refreshNow();
          }}
        />
      ) : null}

      {logsFor ? <ServiceLogsHost id={logsFor} onClose={() => setLogsFor(null)} /> : null}
      {logsTarget ? (
        <LogsModal target={logsTarget} onClose={() => setLogsTarget(null)} />
      ) : null}
      {removeFor ? (
        <RemoveServiceConfirm id={removeFor} onClose={() => setRemoveFor(null)} />
      ) : null}

      {stopFor ? (
        <ConfirmModal
          title="这台服务不是你启动的"
          body={`正在监听的是 ${stopFor.who}${stopFor.pid != null ? `（PID ${stopFor.pid}）` : ""}。确定停止吗？`}
          confirmLabel="停止"
          danger
          onConfirm={confirmStop}
          onClose={() => setStopFor(null)}
        />
      ) : null}
    </div>
  );
}

// ---------- 弹层宿主：只存 id，渲染时取最新实体（D4 §1.1 铁律 1） ----------

function ServiceModalHost({
  editId,
  autoRead,
  onClose,
  onSwitchToEdit,
  onSavedNew,
}: {
  editId: string | null;
  autoRead: boolean;
  onClose(): void;
  onSwitchToEdit(serviceId: string): void;
  onSavedNew(serviceId: string): void;
}) {
  const services = useStore((s) => s.services);
  const service = useStore((s) => (editId ? s.byServiceId[editId] : undefined));
  return (
    <ServiceModal
      key={editId ?? "new"}
      service={service ?? null}
      services={services}
      autoRead={autoRead}
      onSwitchToEdit={onSwitchToEdit}
      onClose={onClose}
      onSaved={(launchChanged, savedId) => {
        if (!editId) onSavedNew(savedId);
        if (editId && launchChanged && service) {
          const isUp =
            service.status === "starting" ||
            service.status === "running" ||
            service.status === "not_listening";
          if (isUp) {
            setNotice(
              `service:${editId}`,
              "启动方式已改；下次重启服务才生效。",
            );
          }
        }
        refreshNow();
      }}
    />
  );
}

function ServiceLogsHost({ id, onClose }: { id: string; onClose(): void }) {
  const service = useService(id);
  if (!service) return null;
  return <LogsModal id={id} name={service.name} source="service" onClose={onClose} />;
}

function RemoveServiceConfirm({ id, onClose }: { id: string; onClose(): void }) {
  const service = useService(id);
  if (!service) return null;
  return (
    <ConfirmModal
      title={`删除本机服务「${service.name}」？`}
      body="有隧道在用它会被拦下；5 秒内可撤销。"
      confirmLabel="删除"
      danger
      onConfirm={() => {
        onClose();
        void actions.serviceRemove(id);
      }}
      onClose={onClose}
    />
  );
}
