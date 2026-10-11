import { useCallback, useEffect, useState } from "react";

import "./styles/tokens.css";
import "./styles/app.css";
import { Sidebar, type PageKey } from "./components/Sidebar";
import { TitleBar } from "./components/TitleBar";
import { ServicesPage } from "./pages/Services";
import { TunnelsPage } from "./pages/Tunnels";
import { ServersPage } from "./pages/Servers";
import { PasswordsPage } from "./pages/Passwords";
import { SettingsPage } from "./pages/Settings";
import {
  getSnapshot,
  onConfigError,
  serviceStart,
  serviceStop,
  setServiceAutostart,
  setServiceWatch,
  tunnelRestart,
  tunnelStart,
  tunnelStop,
  type Snapshot,
} from "./api";

export default function App() {
  const [page, setPage] = useState<PageKey>("services");
  const [snapshot, setSnapshot] = useState<Snapshot>({
    services: [],
    tunnels: [],
    servers: [],
  });
  /** 操作失败时后端返回的原文（大白话），原样展示在页面顶部 */
  const [notice, setNotice] = useState("");

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await getSnapshot());
    } catch {
      // 一次刷新失败不影响界面，下一轮会再来
    }
  }, []);

  // 启动时应用保存的主题偏好
  useEffect(() => {
    const t = localStorage.getItem("theme");
    if (t === "light" || t === "dark") {
      document.documentElement.dataset.theme = t;
    }
  }, []);

  // 每 800ms 拉一次快照：状态变化能很快反映到界面上
  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const s = await getSnapshot();
        if (alive) setSnapshot(s);
      } catch {
        // 同上
      }
    };
    void tick();
    const timer = window.setInterval(tick, 800);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  // 后端读配置失败时会发一条 config-error：用固定红条告诉用户，不挡操作
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    void onConfigError((message) => {
      if (alive) setNotice(message);
    })
      .then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      })
      .catch(() => {
        // 监听失败不影响其它功能
      });
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  const act = useCallback(
    (fn: () => Promise<void>) => {
      void (async () => {
        try {
          await fn();
          setNotice("");
        } catch (e) {
          // 命令失败：把后端的话原样给用户看
          setNotice(String(e));
        }
        void refresh();
      })();
    },
    [refresh],
  );

  // 侧栏要的两条统计：正在发布中的、以及掉线/出错的
  const tunnelsRunning = snapshot.tunnels.filter(
    (t) => t.status === "running",
  ).length;
  const tunnelsDown = snapshot.tunnels.filter(
    (t) => t.status === "disconnected" || t.status === "error",
  ).length;

  return (
    <div className="app-shell">
      <TitleBar />
      {/* 报错固定浮在最上层：滚到列表下面也看得见 */}
      {notice && (
        <div
          style={{
            position: "fixed",
            top: 44,
            left: 0,
            right: 0,
            zIndex: 1000,
            display: "flex",
            justifyContent: "center",
            padding: "0 16px",
            pointerEvents: "none",
          }}
        >
          <div
            className="banner-error"
            style={{
              width: "100%",
              maxWidth: 696,
              pointerEvents: "auto",
              boxShadow: "0 6px 20px rgba(0, 0, 0, 0.28)",
            }}
          >
            <span>{notice}</span>
            <button className="link-btn" onClick={() => setNotice("")}>
              知道了
            </button>
          </div>
        </div>
      )}
      <div className="app">
        <Sidebar
          current={page}
          onNavigate={(p) => {
            setNotice("");
            setPage(p);
          }}
          counts={{
            services: snapshot.services.length,
            tunnels: snapshot.tunnels.length,
            servers: snapshot.servers.length,
            tunnelsRunning,
            tunnelsDown,
          }}
        />
        <main className="main">
          {page === "services" ? (
            <ServicesPage
              services={snapshot.services}
              hasServers={snapshot.servers.length > 0}
              onNavigate={(p) => {
                setNotice("");
                setPage(p);
              }}
              onStart={(id) => act(() => serviceStart(id))}
              onStop={(id, killForeign, expectedPid) =>
                serviceStop(id, killForeign, expectedPid)
              }
              onToggleWatch={(id, watch) =>
                act(() => setServiceWatch(id, watch))
              }
              onToggleAutostart={(id, autostart) =>
                act(() => setServiceAutostart(id, autostart))
              }
              onRefresh={() => void refresh()}
            />
          ) : page === "tunnels" ? (
            <TunnelsPage
              tunnels={snapshot.tunnels}
              services={snapshot.services}
              servers={snapshot.servers}
              onStart={(id) => act(() => tunnelStart(id))}
              onStop={(id) => act(() => tunnelStop(id))}
              onRestart={(id) => act(() => tunnelRestart(id))}
              onNavigate={(p) => {
                setNotice("");
                setPage(p);
              }}
              onRefresh={() => void refresh()}
            />
          ) : page === "servers" ? (
            <ServersPage
              servers={snapshot.servers}
              onChanged={() => void refresh()}
            />
          ) : page === "settings" ? (
            <SettingsPage />
          ) : (
            <PasswordsPage
              tunnels={snapshot.tunnels}
              onRefresh={() => void refresh()}
            />
          )}
        </main>
      </div>
    </div>
  );
}
