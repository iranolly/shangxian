import { useCallback, useEffect, useState } from "react";

import "./styles/tokens.css";
import "./styles/app.css";
import { Sidebar, type PageKey } from "./components/Sidebar";
import { TitleBar } from "./components/TitleBar";
import { Toaster } from "./components/Toaster";
import { SrAnnouncer } from "./components/SrAnnouncer";
import { ServicesPage } from "./pages/Services";
import { TunnelsPage } from "./pages/Tunnels";
import { ServersPage } from "./pages/Servers";
import { SettingsPage } from "./pages/Settings";
import { onConfigError } from "./api";
import { actions } from "./store/actions";
import { refreshNow, startPolling } from "./store/snapshotStore";
import { useStore } from "./store/useStore";
import type { LogTarget } from "./components/LogsModal";

/** 快照失败到第几次才在侧栏顶部出细条（D4 §1.2） */
const POLL_WARN_AFTER = 3;

/** configTrusted=false 且没收到后端事件时的兜底文案（D2 §13.2 的固定文案在事件里带 found/supported） */
const CONFIG_UNTRUSTED_FALLBACK =
  "配置文件无法读取，改动暂时无法保存。请先升级到最新版本再打开；设置文件未被修改。";

interface Banner {
  id: string;
  text: string;
  actionLabel: string;
  run: () => void;
}

/** 通知点击定位的实体（NotificationCenter 的 shangxian:locate detail） */
export interface LocateTarget {
  type: "app" | "tunnel" | "service" | "server";
  id: string;
  name?: string;
}

/** 外壳下发给页面的「意图」：页面处理完调 onIntentDone 清掉，避免切页后重复触发 */
export type ShellIntent =
  | { kind: "locate"; entity: LocateTarget }
  | { kind: "open-new-service"; returnToWizard?: boolean }
  | { kind: "open-new-server"; returnToWizard?: boolean }
  | { kind: "open-wizard"; presetServiceId?: string; presetServerId?: string }
  | { kind: "open-nginx"; tunnelId: string }
  | { kind: "open-logs"; target: LogTarget }
  | { kind: "focus-settings"; focus: "quiet" | "refresh" | "doctor" };

/** shangxian:nav-intent 的 detail：兼容「action 形态」和「page 形态」两种写法 */
interface NavIntentDetail {
  action?: "add-service" | "add-server";
  returnTo?: "wizard";
  page?: PageKey;
  openNew?: boolean;
  openWizard?: boolean;
  openNginxFor?: string;
  presetServiceId?: string;
  presetServerId?: string;
}

export default function App() {
  const app = useStore((s) => s.app);
  const services = useStore((s) => s.services);
  const tunnels = useStore((s) => s.tunnels);
  const servers = useStore((s) => s.servers);
  const polling = useStore((s) => s.polling);
  const [page, setPage] = useState<PageKey>("services");
  /** 后端 config-error 事件的原文：整段不自动消失，直到用户点「知道了」 */
  const [configError, setConfigError] = useState("");
  const [dismissed, setDismissed] = useState<Record<string, boolean>>({});
  /** 外壳意图：页面处理完调用 clearIntent 清掉 */
  const [intent, setIntent] = useState<ShellIntent | null>(null);

  const clearIntent = useCallback(() => setIntent(null), []);
  /** 手动切页时顺手清掉未消费的意图，避免过一会儿跳回旧目标 */
  const navigate = useCallback((next: PageKey) => {
    setIntent(null);
    setPage(next);
  }, []);

  // 启动时应用保存的主题偏好
  useEffect(() => {
    const t = localStorage.getItem("theme");
    if (t === "light" || t === "dark") {
      document.documentElement.dataset.theme = t;
    }
  }, []);

  // 快照轮询：间隔读 settings.refreshIntervalMs；隐藏停刷、恢复可见立刻刷一次
  useEffect(() => startPolling(), []);

  // 窗口几何由 Rust 侧权威管理（mainwindow.rs + <dataDir>/window-state.json），
  // 前端不再设置窗口大小 / 位置，也不需要额外的 window 写权限。

  // 后端读配置失败时会发一条 config-error：固定红条告诉用户，不挡操作
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    void onConfigError((message) => {
      if (alive) setConfigError(message);
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

  // 全局事件 → 外壳意图：通知点击定位、向导死胡同跳转、通知中心「免打扰设置」、去体检
  useEffect(() => {
    const onLocate = (event: Event) => {
      const detail = (event as CustomEvent<Partial<LocateTarget> | undefined>).detail;
      if (!detail?.type || !detail.id) return;
      if (detail.type === "tunnel") {
        setIntent({
          kind: "locate",
          entity: { type: "tunnel", id: detail.id, name: detail.name },
        });
        setPage("tunnels");
      } else if (detail.type === "service") {
        setIntent({
          kind: "locate",
          entity: { type: "service", id: detail.id, name: detail.name },
        });
        setPage("services");
      } else if (detail.type === "server") {
        setIntent({
          kind: "locate",
          entity: { type: "server", id: detail.id, name: detail.name },
        });
        setPage("servers");
      }
    };

    const onNavIntent = (event: Event) => {
      const detail = (event as CustomEvent<NavIntentDetail | undefined>).detail ?? {};
      const returnToWizard = detail.returnTo === "wizard";
      if (detail.action === "add-service") {
        setIntent({ kind: "open-new-service", returnToWizard });
        setPage("services");
        return;
      }
      if (detail.action === "add-server") {
        // 向导死胡同：切到服务器页并自动打开添加弹层，保存后由页面跳回向导
        setIntent({ kind: "open-new-server", returnToWizard });
        setPage("servers");
        return;
      }
      if (detail.page === "services" && detail.openNew) {
        setIntent({ kind: "open-new-service", returnToWizard });
        setPage("services");
        return;
      }
      if (detail.page === "tunnels") {
        if (detail.openNginxFor) {
          setIntent({ kind: "open-nginx", tunnelId: detail.openNginxFor });
          setPage("tunnels");
          return;
        }
        if (detail.openWizard || detail.openNew) {
          setIntent({
            kind: "open-wizard",
            presetServiceId: detail.presetServiceId,
            presetServerId: detail.presetServerId,
          });
          setPage("tunnels");
          return;
        }
      }
      if (detail.page) setPage(detail.page);
    };

    const onOpenSettings = (event: Event) => {
      // 通知中心「免打扰设置」：切设置页并定位。detail.focus 可指明去免打扰还是刷新频率
      const focus = (event as CustomEvent<{ focus?: string } | undefined>).detail?.focus;
      setIntent({
        kind: "focus-settings",
        focus: focus === "refresh" ? "refresh" : "quiet",
      });
      setPage("settings");
    };

    const onOpenDoctor = () => {
      // 「去体检」入口（App 横幅等）：切设置页并把 focus='doctor' 交给设置页直开体检弹层
      setIntent({ kind: "focus-settings", focus: "doctor" });
      setPage("settings");
    };

    window.addEventListener("shangxian:locate", onLocate);
    window.addEventListener("shangxian:nav-intent", onNavIntent);
    window.addEventListener("shangxian:open-settings", onOpenSettings);
    window.addEventListener("shangxian:open-doctor", onOpenDoctor);
    return () => {
      window.removeEventListener("shangxian:locate", onLocate);
      window.removeEventListener("shangxian:nav-intent", onNavIntent);
      window.removeEventListener("shangxian:open-settings", onOpenSettings);
      window.removeEventListener("shangxian:open-doctor", onOpenDoctor);
    };
  }, []);

  // 横幅优先级：配置读不出来 > 后台守卫；noticeStore 的卡片级失败由页面就地展示
  const banners: Banner[] = [];
  if (configError) {
    banners.push({
      id: "config-error",
      text: configError,
      actionLabel: "知道了",
      run: () => setConfigError(""),
    });
  }
  if (banners.length === 0 && !app.configTrusted && !dismissed.configUntrusted) {
    banners.push({
      id: "config-untrusted",
      text: CONFIG_UNTRUSTED_FALLBACK,
      actionLabel: "知道了",
      run: () => setDismissed((d) => ({ ...d, configUntrusted: true })),
    });
  }
  if (banners.length === 0 && !app.jobGuardOk && !dismissed.jobGuard) {
    banners.push({
      id: "job-guard",
      text: app.jobGuardError ?? "子进程保底清理未正确装配，退出时可能有残留进程。",
      actionLabel: "去体检",
      run: () => {
        // 走全局事件：App 的监听会切到设置页并直接打开 DoctorModal
        window.dispatchEvent(new CustomEvent("shangxian:open-doctor"));
        setDismissed((d) => ({ ...d, jobGuard: true }));
      },
    });
  }
  const banner = banners[0];

  // 侧栏要的两条统计：正在发布中的、以及掉线/出错的
  const tunnelsRunning = tunnels.filter((t) => t.status === "running").length;
  const tunnelsDown = tunnels.filter(
    (t) => t.status === "disconnected" || t.status === "error",
  ).length;

  return (
    <div className="app-shell">
      <TitleBar
        onOpenSettings={() => {
          setIntent({ kind: "focus-settings", focus: "quiet" });
          setPage("settings");
        }}
      />
      {/* 报错固定浮在最上层：滚到列表下面也看得见；不自动消失，直到用户处理 */}
      {banner ? (
        <div className="banner-hold">
          <div className="banner-error">
            <span>{banner.text}</span>
            <button className="link-btn" onClick={banner.run}>
              {banner.actionLabel}
            </button>
          </div>
        </div>
      ) : null}
      <SrAnnouncer />
      <div className="app">
        <div className="sidebar-wrap">
          {polling.failStreak >= POLL_WARN_AFTER ? (
            <div className="poll-warn" role="status">
              刷新失败 {polling.failStreak} 次，重试中…
            </div>
          ) : null}
          <Sidebar
            current={page}
            onNavigate={navigate}
            counts={{
              services: services.length,
              tunnels: tunnels.length,
              servers: servers.length,
              tunnelsRunning,
              tunnelsDown,
            }}
          />
        </div>
        <main className="main">
          {page === "services" ? (
            <ServicesPage
              services={services}
              hasServers={servers.length > 0}
              onNavigate={navigate}
              onStart={actions.serviceStart}
              onStop={actions.serviceStop}
              onToggleWatch={actions.serviceToggleWatch}
              onToggleAutostart={actions.serviceToggleAutostart}
              onRefresh={refreshNow}
              intent={intent}
              onIntentDone={clearIntent}
            />
          ) : page === "tunnels" ? (
            <TunnelsPage
              tunnels={tunnels}
              services={services}
              servers={servers}
              onStart={actions.tunnelStart}
              onStop={actions.tunnelPause}
              onNavigate={navigate}
              onRefresh={refreshNow}
              intent={intent}
              onIntentDone={clearIntent}
            />
          ) : page === "servers" ? (
            <ServersPage
              servers={servers}
              onChanged={refreshNow}
              intent={intent}
              onIntentDone={clearIntent}
            />
          ) : (
            <SettingsPage intent={intent} onIntentDone={clearIntent} />
          )}
        </main>
      </div>
      <Toaster />
    </div>
  );
}
