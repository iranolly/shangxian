// 左侧导航：五个目的地（本机服务 / 发布隧道 / 云端服务器 / 访问保护 / 设置）
// 文案纪律：说人话，不用"反向隧道、端口转发、节点"等黑话。

export type PageKey =
  | "services"
  | "tunnels"
  | "servers"
  | "passwords"
  | "settings";

interface Props {
  current: PageKey;
  onNavigate: (page: PageKey) => void;
  counts?: {
    services: number;
    tunnels: number;
    servers: number;
    /** 正在发布的隧道数（页面传了才显示「X 条发布中」） */
    tunnelsRunning?: number;
    /** 掉线/出错的隧道数 */
    tunnelsDown?: number;
  };
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

function IconPasswords() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="5.4" cy="8" r="2.6" {...stroke} />
      <path d="M8 8h6M11.4 8v2.2M13.6 8v1.6" {...stroke} />
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

export function Sidebar({ current, onNavigate, counts }: Props) {
  return (
    <aside className="sidebar">
      <div className="brand">
        <span className="brand-mark">上</span>
        上线助手
      </div>

      <nav className="nav">
        <button
          className={`nav-item${current === "services" ? " active" : ""}`}
          onClick={() => onNavigate("services")}
        >
          <IconServices />
          本机服务
          {counts && counts.services > 0 ? (
            <span className="nav-count">{counts.services}</span>
          ) : null}
        </button>
        <button
          className={`nav-item${current === "tunnels" ? " active" : ""}`}
          onClick={() => onNavigate("tunnels")}
        >
          <IconTunnels />
          <span className="nav-text">
            <span>发布隧道</span>
            {counts && counts.tunnels > 0 && counts.tunnelsRunning != null ? (
              <span className="nav-sub">
                {counts.tunnelsRunning} 条发布中 · {counts.tunnelsDown ?? 0} 条掉线
              </span>
            ) : null}
          </span>
          {counts && counts.tunnels > 0 && counts.tunnelsRunning == null ? (
            <span className="nav-count">{counts.tunnels}</span>
          ) : null}
        </button>
        <button
          className={`nav-item${current === "servers" ? " active" : ""}`}
          onClick={() => onNavigate("servers")}
        >
          <IconServers />
          云端服务器
          {counts && counts.servers > 0 ? (
            <span className="nav-count">{counts.servers}</span>
          ) : null}
        </button>
        <button
          className={`nav-item${current === "passwords" ? " active" : ""}`}
          onClick={() => onNavigate("passwords")}
        >
          <IconPasswords />
          访问保护
        </button>
      </nav>

      <div className="sidebar-bottom">
        <button
          className={`nav-item${current === "settings" ? " active" : ""}`}
          onClick={() => onNavigate("settings")}
        >
          <IconSettings />
          设置
        </button>
      </div>
    </aside>
  );
}
