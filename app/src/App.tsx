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

  return (
    <div className="app-shell">
      <TitleBar />
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
          }}
        />
        <main className="main">
          {notice && (
            <div className="page banner-page">
              <div className="banner-error">
                <span>{notice}</span>
                <button className="link-btn" onClick={() => setNotice("")}>
                  知道了
                </button>
              </div>
            </div>
          )}
          {page === "services" ? (
            <ServicesPage
              services={snapshot.services}
              onStart={(id) => act(() => serviceStart(id))}
              onStop={(id) => act(() => serviceStop(id))}
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
