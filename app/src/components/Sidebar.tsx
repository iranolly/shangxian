// 左侧导航：四个目的地（本机服务 / 发布隧道 / 云端服务器 / 设置）
// 文案纪律：说人话，不用"反向隧道、端口转发、节点"等黑话。
// 计数与状态徽标由侧栏自己订阅 store（D4 §3.8：避免整树重渲染）。

import { useStore } from "../store/useStore";

export type PageKey = "services" | "tunnels" | "servers" | "settings";

interface Counts {
  services: number;
  tunnels: number;
  servers: number;
  /** 正在发布的隧道数 */
  tunnelsRunning?: number;
  /** 掉线/出错的隧道数 */
  tunnelsDown?: number;
}

interface Props {
  current: PageKey;
  onNavigate: (page: PageKey) => void;
  /** 兼容旧外壳的 props；现在侧栏自己从 store 订阅计数（D4 §3.8） */
  counts?: Counts;
}

const stroke = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.5,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

function IconServices() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <rect x="1.8" y="2.5" width="12.4" height="4.2" rx="1.2" {...stroke} />
      <rect x="1.8" y="9.3" width="12.4" height="4.2" rx="1.2" {...stroke} />
    </svg>
  );
}

function IconTunnels() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path
        d="M4.4 9.4h7.1a2.2 2.2 0 0 0 .2-4.4 3.2 3.2 0 0 0-6-1A2.4 2.4 0 0 0 4.4 9.4Z"
        {...stroke}
      />
      <path d="M8 10.4v3.4M6.2 12l1.8 1.8L9.8 12" {...stroke} />
    </svg>
  );
}

function IconServers() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <rect x="2.2" y="2.2" width="11.6" height="5" rx="1.2" {...stroke} />
      <rect x="2.2" y="8.8" width="11.6" height="5" rx="1.2" {...stroke} />
      <path d="M4.6 4.7h.01M4.6 11.3h.01" {...stroke} />
    </svg>
  );
}

function IconSettings() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" {...stroke} />
      <circle cx="6" cy="4.5" r="1.4" {...stroke} fill="var(--bg-sidebar)" />
      <circle cx="10.5" cy="8" r="1.4" {...stroke} fill="var(--bg-sidebar)" />
      <circle cx="5" cy="11.5" r="1.4" {...stroke} fill="var(--bg-sidebar)" />
    </svg>
  );
}

export function Sidebar({ current, onNavigate }: Props) {
  const servicesCount = useStore((s) => s.services.length);
  const serversCount = useStore((s) => s.servers.length);
  const tunnelsTotal = useStore((s) => s.tunnels.length);
  const tunnelsRunning = useStore(
    (s) => s.tunnels.filter((t) => t.status === "running").length,
  );
  const tunnelsDown = useStore(
    (s) => s.tunnels.filter((t) => t.status === "disconnected" || t.status === "error").length,
  );

  return (
    <aside className="sidebar">
      <div className="brand">
        <span className="brand-mark">上</span>
        上线助手
      </div>

      <nav className="nav" aria-label="主导航">
        <button
          className={`nav-item${current === "services" ? " active" : ""}`}
          aria-current={current === "services" ? "page" : undefined}
          onClick={() => onNavigate("services")}
        >
          <IconServices />
          本机服务
          {servicesCount > 0 ? <span className="nav-count">{servicesCount}</span> : null}
        </button>

        <button
          className={`nav-item${current === "tunnels" ? " active" : ""}`}
          aria-current={current === "tunnels" ? "page" : undefined}
          onClick={() => onNavigate("tunnels")}
        >
          <IconTunnels />
          <span className="nav-text">
            <span>发布隧道</span>
            {tunnelsTotal > 0 ? (
              <span className="nav-sub">
                发布中 {tunnelsRunning} · 断开 {tunnelsDown}
              </span>
            ) : null}
          </span>
          {tunnelsDown > 0 ? (
            <span className="nav-badge" title={`${tunnelsDown} 条断开`}>
              {tunnelsDown}
            </span>
          ) : tunnelsTotal > 0 ? (
            <span className="nav-count">{tunnelsTotal}</span>
          ) : null}
        </button>

        <button
          className={`nav-item${current === "servers" ? " active" : ""}`}
          aria-current={current === "servers" ? "page" : undefined}
          onClick={() => onNavigate("servers")}
        >
          <IconServers />
          云端服务器
          {serversCount > 0 ? <span className="nav-count">{serversCount}</span> : null}
        </button>
      </nav>

      <div className="sidebar-bottom">
        <button
          className={`nav-item${current === "settings" ? " active" : ""}`}
          aria-current={current === "settings" ? "page" : undefined}
          onClick={() => onNavigate("settings")}
        >
          <IconSettings />
          设置
        </button>
      </div>
    </aside>
  );
}
