// 发布隧道页：把本机服务接到云端服务器上，别人就能从公网访问。

import { useState } from "react";

import { EditTunnelModal } from "../components/EditTunnelModal";
import { LogsModal } from "../components/LogsModal";
import { NewTunnelWizard } from "../components/NewTunnelWizard";
import { NginxModal } from "../components/NginxModal";
import { TunnelCard } from "../components/TunnelCard";
import { TUNNEL_ORDER } from "../copy/status";
import {
  removeTunnel,
  type ServerView,
  type ServiceView,
  type TunnelView,
} from "../api";

interface Props {
  tunnels: TunnelView[];
  services: ServiceView[];
  servers: ServerView[];
  onStart: (id: string) => void;
  onStop: (id: string) => void;
  onRestart: (id: string) => void;
  /** 切换页面：卡片上"本机服务没在跑"能直接跳去本机服务页 */
  onNavigate: (page: "services") => void;
  onRefresh: () => void;
}

export function TunnelsPage({
  tunnels,
  services,
  servers,
  onStart,
  onStop,
  onRestart,
  onNavigate,
  onRefresh,
}: Props) {
  const [wizardOpen, setWizardOpen] = useState(false);
  const [editFor, setEditFor] = useState<TunnelView | null>(null);
  const [logsFor, setLogsFor] = useState<TunnelView | null>(null);
  const [nginxFor, setNginxFor] = useState<TunnelView | null>(null);
  /** 删除失败时后端返回的原文，按隧道 id 挂在对应卡片上 */
  const [notices, setNotices] = useState<Record<string, string>>({});

  const sorted = [...tunnels].sort(
    (a, b) => TUNNEL_ORDER[a.status] - TUNNEL_ORDER[b.status],
  );

  const serverName = (id: string) =>
    servers.find((s) => s.id === id)?.name ?? "（服务器已删除）";

  const remove = (t: TunnelView) => {
    if (
      !window.confirm(
        `删除隧道「${t.name}」？删掉后别人立刻打不开，配置也会一起删掉。`,
      )
    ) {
      return;
    }
    void (async () => {
      try {
        await removeTunnel(t.id);
        setNotices((n) => {
          const next = { ...n };
          delete next[t.id];
          return next;
        });
      } catch (e) {
        setNotices((n) => ({ ...n, [t.id]: String(e) }));
      }
      onRefresh();
    })();
  };

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">发布隧道</h1>
          <p className="page-sub">
            把某台本机服务接到某台云端服务器上，别人用网址就能打开。
          </p>
        </div>
        <button className="btn btn-primary" onClick={() => setWizardOpen(true)}>
          + 发布新隧道
        </button>
      </header>

      {sorted.length === 0 ? (
        <div className="empty">
          <p className="empty-title">还没有发布任何隧道</p>
          <p className="empty-text">
            把你电脑上的程序分享到公网，别人用手机、在外面都能打开。
          </p>
          <button className="btn btn-primary" onClick={() => setWizardOpen(true)}>
            + 发布第一条隧道
          </button>
        </div>
      ) : (
        <div className="service-list">
          {sorted.map((t) => (
            <TunnelCard
              key={t.id}
              tunnel={t}
              serverName={serverName(t.serverId)}
              onStart={() => onStart(t.id)}
              onStop={() => onStop(t.id)}
              onRestart={() => onRestart(t.id)}
              onEdit={() => setEditFor(t)}
              onRemove={() => remove(t)}
              onLogs={() => setLogsFor(t)}
              onNginx={() => setNginxFor(t)}
              onGoToServices={() => onNavigate("services")}
              notice={notices[t.id]}
              onDismissNotice={() =>
                setNotices((n) => {
                  const next = { ...n };
                  delete next[t.id];
                  return next;
                })
              }
            />
          ))}
        </div>
      )}

      {wizardOpen && (
        <NewTunnelWizard
          services={services}
          tunnels={tunnels}
          servers={servers}
          onClose={() => setWizardOpen(false)}
          onDone={() => {
            setWizardOpen(false);
            onRefresh();
          }}
        />
      )}

      {logsFor && (
        <LogsModal
          id={logsFor.id}
          name={logsFor.name}
          source="tunnel"
          onClose={() => setLogsFor(null)}
        />
      )}

      {nginxFor && (
        <NginxModal
          id={nginxFor.id}
          name={nginxFor.name}
          tunnel={nginxFor}
          onClose={() => setNginxFor(null)}
        />
      )}

      {editFor && (
        <EditTunnelModal
          tunnel={editFor}
          onClose={() => setEditFor(null)}
          onSaved={onRefresh}
        />
      )}
    </div>
  );
}
