// 列表工具条（D4 §3.3 ListToolbar）：搜索 / 状态 chips / 排序下拉 / 分组开关 / 批量管理。
// 搜索本地先更新（输入不卡），120ms 后才写进偏好；Esc 或 ✕ 立刻清空。

import { useEffect, useRef, useState } from "react";

import type { ServiceStatus } from "../../copy/status";
import {
  setServicePrefs,
  setTunnelPrefs,
  useServicePrefs,
  useTunnelPrefs,
  type ListPage,
  type TunnelFilterKey,
} from "./listPrefs";

interface FilterOption {
  value: string;
  label: string;
  count: number;
}

interface SortOption {
  value: string;
  label: string;
}

interface Props {
  page: ListPage;
  shown: number;
  total: number;
  filters: FilterOption[];
  sorts: SortOption[];
  /** 传了才显示「按服务器分组」开关（本机服务页不分组） */
  groupLabel?: string;
  bulkMode: boolean;
  onToggleBulk(): void;
}

const SEARCH_PLACEHOLDER: Record<ListPage, string> = {
  tunnels: "搜索名称、网址、端口…",
  services: "搜索名称、端口…",
};

export function ListToolbar({
  page,
  shown,
  total,
  filters,
  sorts,
  groupLabel,
  bulkMode,
  onToggleBulk,
}: Props) {
  const tunnelPrefs = useTunnelPrefs();
  const servicePrefs = useServicePrefs();
  const query = page === "tunnels" ? tunnelPrefs.query : servicePrefs.query;
  const sort = page === "tunnels" ? tunnelPrefs.sort : servicePrefs.sort;
  const selected: string[] | "all" =
    page === "tunnels" ? tunnelPrefs.status : servicePrefs.status;

  const [text, setText] = useState(query);
  const timer = useRef<number | null>(null);

  // 外部清空筛选时同步输入框
  useEffect(() => {
    setText(query);
  }, [query]);

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );

  const writeQuery = (next: string) => {
    if (page === "tunnels") setTunnelPrefs({ query: next });
    else setServicePrefs({ query: next });
  };

  const changeText = (next: string) => {
    setText(next);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => writeQuery(next), 120);
  };

  const commitText = (next: string) => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    setText(next);
    writeQuery(next);
  };

  const isActive = (value: string) => {
    if (value === "all") return selected === "all";
    return selected !== "all" && selected.includes(value);
  };

  const toggleFilter = (value: string) => {
    if (value === "all") {
      if (page === "tunnels") setTunnelPrefs({ status: "all" });
      else setServicePrefs({ status: "all" });
      return;
    }
    const current: string[] = selected === "all" ? [] : [...selected];
    const next = current.includes(value)
      ? current.filter((v) => v !== value)
      : [...current, value];
    if (page === "tunnels") {
      setTunnelPrefs({ status: next.length === 0 ? "all" : (next as TunnelFilterKey[]) });
    } else {
      setServicePrefs({ status: next.length === 0 ? "all" : (next as ServiceStatus[]) });
    }
  };

  const changeSort = (value: string) => {
    if (page === "tunnels") setTunnelPrefs({ sort: value as typeof tunnelPrefs.sort });
    else setServicePrefs({ sort: value as typeof servicePrefs.sort });
  };

  const toggleGroup = (on: boolean) => setTunnelPrefs({ groupByServer: on });

  return (
    <div className="list-toolbar">
      <div className="toolbar-row">
        <div className="search-box">
          <input
            className="input search-input"
            value={text}
            placeholder={SEARCH_PLACEHOLDER[page]}
            onChange={(e) => changeText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                commitText("");
              }
            }}
          />
          {text ? (
            <button
              type="button"
              className="search-clear"
              aria-label="清空搜索"
              title="清空搜索"
              onClick={() => commitText("")}
            >
              ✕
            </button>
          ) : null}
        </div>
        <select
          className="select"
          aria-label="排序方式"
          value={sort}
          onChange={(e) => changeSort(e.target.value)}
        >
          {sorts.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>
        {groupLabel ? (
          <label className="switch">
            <input
              type="checkbox"
              checked={tunnelPrefs.groupByServer}
              onChange={(e) => toggleGroup(e.target.checked)}
            />
            {groupLabel}
          </label>
        ) : null}
        <button className="btn btn-ghost btn-sm" onClick={onToggleBulk}>
          {bulkMode ? "退出批量" : "批量管理"}
        </button>
      </div>

      <div className="toolbar-row toolbar-chips" role="group" aria-label="状态筛选">
        {filters.map((f) => (
          <button
            key={f.value}
            type="button"
            className={`chip-btn${isActive(f.value) ? " active" : ""}`}
            aria-pressed={isActive(f.value)}
            onClick={() => toggleFilter(f.value)}
          >
            {f.label}
            <span className="chip-count">{f.count}</span>
          </button>
        ))}
        {query ? (
          <span className="hint toolbar-hint">
            找到 {shown} 条（共 {total} 条）
          </span>
        ) : (
          <span className="hint toolbar-hint">共 {total} 条</span>
        )}
      </div>
    </div>
  );
}
