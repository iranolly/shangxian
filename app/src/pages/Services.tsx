// 本机服务页：这台电脑上的程序（端口 + 启动方式 + 看护开关）。
// 这里只看「本机」；发布到公网在「发布隧道」页。

import { useState } from "react";

import { ImportModal } from "../components/ImportModal";
import { LogsModal } from "../components/LogsModal";
import { ServiceCard } from "../components/ServiceCard";
import { ServiceModal } from "../components/ServiceModal";
import { SERVICE_ORDER } from "../copy/status";
import { removeService, type ServiceView } from "../api";

interface Props {
  services: ServiceView[];
  onStart: (id: string) => void;
  onStop: (id: string) => void;
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
  const [importOpen, setImportOpen] = useState(false);
  const [logsFor, setLogsFor] = useState<ServiceView | null>(null);
  /** 删除失败时后端返回的原文，按服务 id 挂在对应卡片上 */
  const [notices, setNotices] = useState<Record<string, string>>({});

  const sorted = [...services].sort(
    (a, b) => SERVICE_ORDER[a.status] - SERVICE_ORDER[b.status],
  );

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
              onStart={() => onStart(s.id)}
              onStop={() => onStop(s.id)}
              onToggleWatch={(w) => onToggleWatch(s.id, w)}
              onToggleAutostart={(a) => onToggleAutostart(s.id, a)}
              onEdit={() => setEditFor(s)}
              onLogs={() => setLogsFor(s)}
              onRemove={() => remove(s)}
              notice={notices[s.id]}
              onDismissNotice={() =>
                setNotices((n) => {
                  const next = { ...n };
                  delete next[s.id];
                  return next;
                })
              }
            />
          ))}
        </div>
      )}

      {(modalOpen || editFor) && (
        <ServiceModal
          service={editFor}
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
