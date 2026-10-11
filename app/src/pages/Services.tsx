// 本机服务页：这台电脑上的程序（端口 + 启动方式 + 看护开关）。
// 这里只看「本机」；发布到公网在「发布隧道」页。

import { useState } from "react";

import { ImportModal } from "../components/ImportModal";
import { LogsModal } from "../components/LogsModal";
import { ServiceCard } from "../components/ServiceCard";
import { ServiceModal } from "../components/ServiceModal";
import { SERVICE_ORDER } from "../copy/status";
import {
  removeService,
  type ServiceStopResult,
  type ServiceView,
} from "../api";

interface Props {
  services: ServiceView[];
  onStart: (id: string) => void;
  /** 停止：第一次只问后端；needsConfirm 时再弹确认框，确认后带 killForeign=true + pid 再来一次 */
  onStop: (
    id: string,
    killForeign?: boolean,
    expectedPid?: number | null,
  ) => Promise<ServiceStopResult>;
  onToggleWatch: (id: string, watch: boolean) => void;
  onToggleAutostart: (id: string, autostart: boolean) => void;
  onRefresh: () => void;
}

export function ServicesPage({
  services,
  onStart,
  onStop,
  onToggleWatch,
  onToggleAutostart,
  onRefresh,
}: Props) {
  const [modalOpen, setModalOpen] = useState(false);
  const [editFor, setEditFor] = useState<ServiceView | null>(null);
  /** 跳到某台服务的编辑界面时，让弹层自动读取补全一次 */
  const [autoReadFor, setAutoReadFor] = useState<string | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  const [logsFor, setLogsFor] = useState<ServiceView | null>(null);
  /** 删除/停止失败时后端返回的原文，按服务 id 挂在对应卡片上 */
  const [notices, setNotices] = useState<Record<string, string>>({});

  const sorted = [...services].sort(
    (a, b) => SERVICE_ORDER[a.status] - SERVICE_ORDER[b.status],
  );

  const clearNotice = (id: string) => {
    setNotices((n) => {
      if (!(id in n)) return n;
      const next = { ...n };
      delete next[id];
      return next;
    });
  };

  /** 点已登记端口行：切到那台服务的编辑界面，并自动读取补全 */
  const switchToEdit = (serviceId: string) => {
    const target = services.find((s) => s.id === serviceId);
    if (!target) return;
    setModalOpen(false);
    setEditFor(target);
    setAutoReadFor(serviceId);
  };

  /** 启动：用户主动重启后，旧的「启动方式已改」提示就该收了 */
  const start = (s: ServiceView) => {
    clearNotice(s.id);
    onStart(s.id);
  };

  /** 停止：不是本应用启动的进程要用户确认后才会真的关掉 */
  const stop = async (s: ServiceView) => {
    try {
      const first = await onStop(s.id);
      if (first.kind === "needsConfirm") {
        const who = first.exe ?? "一个不是本应用启动的程序";
        const pidText = first.pid != null ? `（PID ${first.pid}）` : "";
        const ok = window.confirm(
          `这台服务不是你启动的，正在监听的是 ${who}${pidText}。\n停止会真的把它关掉，确定吗？`,
        );
        if (!ok) {
          // 取消也要收掉旧报错，并走 finally 刷新一次（状态可能已经变了）
          clearNotice(s.id);
          return;
        }
        await onStop(s.id, true, first.pid ?? null);
      }
      clearNotice(s.id);
    } catch (e) {
      // 后端的话已经是大白话（例：这台服务没配启动方式…），原样给用户
      setNotices((n) => ({ ...n, [s.id]: String(e) }));
    } finally {
      onRefresh();
    }
  };

  const remove = (s: ServiceView) => {
    if (
      !window.confirm(
        `删除本机服务「${s.name}」？如果还有隧道在用它，程序会拦下来并告诉你哪几条。`,
      )
    ) {
      return;
    }
    void (async () => {
      try {
        await removeService(s.id);
        setNotices((n) => {
          const next = { ...n };
          delete next[s.id];
          return next;
        });
      } catch (e) {
        // 后端的话已经是大白话（例：还有 2 条隧道在用它…），原样给用户
        setNotices((n) => ({ ...n, [s.id]: String(e) }));
      }
      onRefresh();
    })();
  };

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">本机服务</h1>
          <p className="page-sub">
            这台电脑上的程序：记下端口和启动方式，就能帮你启动、看护。
          </p>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button className="btn btn-ghost" onClick={() => setImportOpen(true)}>
            导入现有脚本
          </button>
          <button className="btn btn-primary" onClick={() => setModalOpen(true)}>
            + 添加本机服务
          </button>
        </div>
      </header>

      {sorted.length === 0 ? (
        <div className="empty">
          <p className="empty-title">还没有登记本机服务</p>
          <p className="empty-text">
            先把你电脑上的程序记下来（端口 + 启动程序），再去「发布隧道」页让它能从公网访问。
          </p>
          <button className="btn btn-primary" onClick={() => setModalOpen(true)}>
            + 添加第一个本机服务
          </button>
        </div>
      ) : (
        <div className="service-list">
          {sorted.map((s) => (
            <ServiceCard
              key={s.id}
              service={s}
              onStart={() => start(s)}
              onStop={() => void stop(s)}
              onToggleWatch={(w) => onToggleWatch(s.id, w)}
              onToggleAutostart={(a) => onToggleAutostart(s.id, a)}
              onEdit={() => {
                setAutoReadFor(null);
                setEditFor(s);
              }}
              onLogs={() => setLogsFor(s)}
              onRemove={() => remove(s)}
              notice={notices[s.id]}
              onDismissNotice={() => clearNotice(s.id)}
            />
          ))}
        </div>
      )}

      {(modalOpen || editFor) && (
        <ServiceModal
          key={editFor?.id ?? "new"}
          service={editFor}
          services={services}
          autoRead={Boolean(editFor && autoReadFor === editFor.id)}
          onSwitchToEdit={switchToEdit}
          onClose={() => {
            setModalOpen(false);
            setEditFor(null);
          }}
          onSaved={onRefresh}
        />
      )}

      {importOpen && (
        <ImportModal
          onClose={() => setImportOpen(false)}
          onDone={() => {
            setImportOpen(false);
            onRefresh();
          }}
        />
      )}

      {logsFor && (
        <LogsModal
          id={logsFor.id}
          name={logsFor.name}
          source="service"
          onClose={() => setLogsFor(null)}
        />
      )}
    </div>
  );
}
