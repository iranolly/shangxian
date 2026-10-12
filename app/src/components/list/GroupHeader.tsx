// 分组头（D4 §3.3 GroupHeader）：`▸ {服务器名} · {n} 条（{m} 条掉线）`，点击折叠/展开。

import { useServer } from "../../store/useStore";

interface Props {
  serverId: string;
  count: number;
  downCount: number;
  collapsed: boolean;
  onToggle(serverId: string): void;
}

export function GroupHeader({ serverId, count, downCount, collapsed, onToggle }: Props) {
  const server = useServer(serverId);
  const name = server?.name ?? "（服务器已删除）";
  return (
    <button
      type="button"
      className="group-head"
      aria-expanded={!collapsed}
      onClick={() => onToggle(serverId)}
    >
      <span className="group-caret" aria-hidden="true">
        {collapsed ? "▸" : "▾"}
      </span>
      <span className="group-name">{name}</span>
      <span className="group-count">· {count} 条</span>
      {downCount > 0 ? <span className="group-down">（{downCount} 条断开）</span> : null}
    </button>
  );
}
