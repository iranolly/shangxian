// 通知中心（D4 §1.7 / §3.4，⑤）：
// - 历史从 events.jsonl 读（query_logs 的 source=app，只看 origin=events），只放内存；
// - 未读位置存 localStorage（D0 第 9 条：localStorage 只存已读位置）；
// - sys-notice 只弹 toast，不进历史（D0 第 9 条）。
//
// TitleBar 里调用 useNotificationFeed() 起轮询，并在打开时渲染 <NotificationCenter />。

import { useEffect, useRef, useSyncExternalStore } from "react";

import { listen } from "@tauri-apps/api/event";

import { queryLogs } from "../api";
import { T } from "../copy/toasts";
import { toast } from "../store/toastStore";
import "../styles/modals.css";

type NotifyLevel = "info" | "warn" | "error";

interface NotifyEntity {
  type: "app" | "tunnel" | "service" | "server";
  id: string;
  name: string;
}

interface NotifyEvent {
  id: string;
  at: number;
  kind: "app" | "tunnel" | "service" | "protection" | "update" | "doctor";
  level: NotifyLevel;
  code: string | null;
  title: string;
  entity: NotifyEntity | null;
}

const READ_KEY = "notif.lastReadTs";
/** 界面静音：存静音截止时间（D4 §1.7，只压界面提醒，不动系统通知） */
const MUTE_KEY = "notif.toastMutedUntil";
const MUTE_MS = 60 * 60 * 1000;
const MAX_EVENTS = 200;
const POLL_OPEN_MS = 5000;
const POLL_CLOSED_MS = 10_000;

const inTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function readLastReadTs(): number {
  try {
    const raw = localStorage.getItem(READ_KEY);
    const value = raw ? Number(raw) : 0;
    return Number.isFinite(value) && value > 0 ? value : 0;
  } catch {
    return 0;
  }
}

function readMutedUntil(): number {
  try {
    const raw = localStorage.getItem(MUTE_KEY);
    const value = raw ? Number(raw) : 0;
    return Number.isFinite(value) && value > 0 ? value : 0;
  } catch {
    return 0;
  }
}

let mutedUntil = readMutedUntil();

function isMuted(): boolean {
  return mutedUntil > Date.now();
}

function setMutedUntil(until: number | null): void {
  mutedUntil = until ?? 0;
  try {
    if (mutedUntil > 0) localStorage.setItem(MUTE_KEY, String(mutedUntil));
    else localStorage.removeItem(MUTE_KEY);
  } catch {
    // 存不了就只在本次会话里静音
  }
  emit();
}

let events: NotifyEvent[] = [];
let lastReadTs = readLastReadTs();
let open = false;
let loading = false;
let listeners = new Set<() => void>();

function emit(): void {
  for (const fn of listeners) fn();
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

function kindOf(source: string, code: string | null): NotifyEvent["kind"] {
  if (source === "tunnel") return "tunnel";
  if (source === "service") return "service";
  const c = code ?? "";
  if (c.startsWith("protection")) return "protection";
  if (c.startsWith("update")) return "update";
  if (c.startsWith("doctor")) return "doctor";
  return "app";
}

function levelOf(level: string | null): NotifyLevel {
  return level === "warn" || level === "error" ? level : "info";
}

/** 拉一次事件历史；失败静默（下次轮询再试）。 */
async function fetchNotifications(): Promise<void> {
  if (loading) return;
  loading = true;
  try {
    const res = await queryLogs({ source: "app", lines: MAX_EVENTS, format: "json" });
    const items: NotifyEvent[] = [];
    for (const entry of res.entries ?? []) {
      if (entry.origin !== "events") continue;
      const subject = entry.subject ?? null;
      const title = entry.message ?? entry.raw;
      if (!title) continue;
      items.push({
        id: `${entry.ts ?? 0}|${entry.code ?? ""}|${subject?.id ?? "app"}|${title}`,
        at: entry.ts ?? Date.now(),
        kind: kindOf(entry.source, entry.code ?? null),
        level: levelOf(entry.level),
        code: entry.code ?? null,
        title,
        entity: subject
          ? { type: subject.type, id: subject.id, name: subject.name }
          : null,
      });
    }
    // 按时间倒序，同 id 去重
    const seen = new Set<string>();
    const deduped: NotifyEvent[] = [];
    for (const item of items.sort((a, b) => b.at - a.at)) {
      if (seen.has(item.id)) continue;
      seen.add(item.id);
      deduped.push(item);
      if (deduped.length >= MAX_EVENTS) break;
    }
    events = deduped;
    emit();
  } catch {
    // 读不到就保持现状
  } finally {
    loading = false;
  }
}

export function openNotificationCenter(): void {
  open = true;
  emit();
  void fetchNotifications();
}

export function closeNotificationCenter(): void {
  open = false;
  emit();
}

function markAllRead(): void {
  lastReadTs = Math.max(Date.now(), ...events.map((e) => e.at));
  try {
    localStorage.setItem(READ_KEY, String(lastReadTs));
  } catch {
    // 存不了就只在本次会话里已读
  }
  emit();
  toast.success(T.allRead());
}

/** 点击某条：这一条及更早的都算已读（只存一个已读位置）。 */
function markReadUntil(at: number): void {
  if (at <= lastReadTs) return;
  lastReadTs = at;
  try {
    localStorage.setItem(READ_KEY, String(lastReadTs));
  } catch {
    // 同上
  }
  emit();
}

function getNotifications(): NotifyEvent[] {
  return events;
}

/** 未读数：界面静音时不往铃铛徽标上顶（静音只压提醒，不删数据） */
function getUnreadCount(): number {
  if (isMuted()) return 0;
  return events.filter((e) => e.at > lastReadTs).length;
}

function useNotifications(): NotifyEvent[] {
  return useSyncExternalStore(subscribe, getNotifications, getNotifications);
}

function useMuted(): boolean {
  return useSyncExternalStore(subscribe, isMuted, isMuted);
}

export function useUnreadCount(): number {
  return useSyncExternalStore(subscribe, getUnreadCount, getUnreadCount);
}

export function useNotificationOpen(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => open,
    () => open,
  );
}

/** TitleBar 常驻调用：轮询事件历史 + 监听 sys-notice（只弹 toast，不进历史）。 */
export function useNotificationFeed(): void {
  useEffect(() => {
    void fetchNotifications();

    let timer: number | null = null;
    const restartTimer = () => {
      if (timer !== null) window.clearInterval(timer);
      timer = window.setInterval(
        () => void fetchNotifications(),
        open ? POLL_OPEN_MS : POLL_CLOSED_MS,
      );
    };
    restartTimer();

    const onVisible = () => {
      if (!document.hidden) void fetchNotifications();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);

    // 写操作成功后（F7/F9）会 dispatch：立刻重拉事件，不用等下一轮轮询
    const onNotifRefresh = () => void fetchNotifications();
    window.addEventListener("shangxian:notif-refresh", onNotifRefresh);

    let unlisten: (() => void) | undefined;
    let alive = true;
    if (inTauri) {
      void listen<{ level?: string; text?: string; sticky?: boolean }>(
        "sys-notice",
        (event) => {
          if (isMuted()) return; // 界面静音：只压弹窗；系统通知由后端免打扰时段管
          const level = levelOf(event.payload?.level ?? null);
          const text = event.payload?.text?.trim();
          if (!text) return;
          const sticky = event.payload?.sticky === true;
          let id = "";
          id = toast.show({
            kind: level,
            text,
            sticky,
            durationMs: sticky ? 0 : undefined,
            action: sticky
              ? { label: "知道了", run: () => toast.dismiss(id) }
              : undefined,
          });
        },
      ).then((fn) => {
        if (alive) unlisten = fn;
        else fn();
      });
    }

    // 打开面板时换成 5 秒一轮
    const unsub = subscribe(() => {
      restartTimer();
    });

    return () => {
      alive = false;
      if (timer !== null) window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
      window.removeEventListener("shangxian:notif-refresh", onNotifRefresh);
      unlisten?.();
      unsub();
    };
  }, []);
}

/** 点击通知后的定位：交给宿主页面 / App（navIntent 还没落地时先发事件）。 */
function locate(entity: NotifyEntity): void {
  window.dispatchEvent(
    new CustomEvent("shangxian:locate", { detail: entity }),
  );
}

function dayKey(at: number): "today" | "yesterday" | "earlier" {
  const now = new Date();
  const d = new Date(at);
  const same = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
  if (same(now, d)) return "today";
  const yesterday = new Date(now.getTime() - 24 * 60 * 60 * 1000);
  if (same(yesterday, d)) return "yesterday";
  return "earlier";
}

function formatClock(at: number): string {
  const d = new Date(at);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

const LEVEL_ICON: Record<NotifyLevel, string> = {
  info: "•",
  warn: "!",
  error: "×",
};

const GROUP_TITLE = {
  today: "今天",
  yesterday: "昨天",
  earlier: "更早",
} as const;

interface Props {
  onClose(): void;
  /** 点「免打扰设置」：没给就走全局事件（App 接） */
  onOpenSettings?: () => void;
}

export function NotificationCenter({ onClose, onOpenSettings }: Props) {
  const items = useNotifications();
  const muted = useMuted();
  // 面板自己的「全部标为已读」用真实未读数：静音只压铃铛徽标，不影响这里
  const unread = items.filter((item) => item.at > lastReadTs).length;
  const panelRef = useRef<HTMLDivElement | null>(null);
  const openerRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    openerRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    panelRef.current?.focus();
    return () => {
      const opener = openerRef.current;
      if (opener && document.body.contains(opener)) opener.focus();
    };
  }, []);

  const groups: Array<{ key: "today" | "yesterday" | "earlier"; items: NotifyEvent[] }> = [];
  for (const item of items) {
    const key = dayKey(item.at);
    const last = groups[groups.length - 1];
    if (last && last.key === key) last.items.push(item);
    else groups.push({ key, items: [item] });
  }

  const handleClick = (item: NotifyEvent) => {
    markReadUntil(item.at);
    if (item.entity) {
      locate(item.entity);
      onClose();
    }
  };

  const openSettings = () => {
    onOpenSettings?.();
    if (!onOpenSettings) {
      window.dispatchEvent(new CustomEvent("shangxian:open-settings"));
    }
    onClose();
  };

  return (
    <div
      className="notif-panel"
      role="dialog"
      aria-label="通知"
      ref={panelRef}
      tabIndex={-1}
    >
      <div className="notif-head">
        <h2 className="notif-title">通知</h2>
        <span className="grow" style={{ flex: 1 }} />
        {unread > 0 ? (
          <button className="link-btn" onClick={markAllRead}>
            全部标为已读
          </button>
        ) : null}
        <button
          className="link-btn"
          title="只压界面提醒；系统通知请用「免打扰时段」"
          onClick={() => setMutedUntil(muted ? null : Date.now() + MUTE_MS)}
        >
          {muted ? "取消界面静音" : "界面静音 1 小时"}
        </button>
      </div>

      {items.length === 0 ? (
        <div className="notif-empty">
          <p className="notif-empty-title">还没有通知</p>
          <p>掉线、恢复、体检失败这些事会记在这里。</p>
        </div>
      ) : (
        <div className="notif-list">
          {groups.map((group) => (
            <div key={group.key}>
              <div className="notif-group">{GROUP_TITLE[group.key]}</div>
              {group.items.map((item) => {
                const isUnread = item.at > lastReadTs;
                return (
                  <button
                    key={item.id}
                    className={`notif-item ${isUnread ? "unread" : ""}`}
                    onClick={() => handleClick(item)}
                  >
                    <span
                      className="notif-level"
                      style={{
                        color:
                          item.level === "error"
                            ? "var(--st-error-fg)"
                            : item.level === "warn"
                              ? "var(--st-disconnected-fg)"
                              : "var(--text-3)",
                      }}
                      aria-hidden="true"
                    >
                      {LEVEL_ICON[item.level]}
                    </span>
                    <span className="notif-body">
                      <span className="notif-text">{item.title}</span>
                      <span
                        className="notif-time"
                        style={{ display: "block" }}
                      >
                        {formatClock(item.at)}
                        {item.entity ? ` · ${item.entity.name}` : ""}
                      </span>
                    </span>
                    {isUnread ? <span className="notif-dot" aria-hidden="true" /> : null}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      )}

      <div className="notif-foot">
        <button className="link-btn" onClick={openSettings}>
          免打扰设置
        </button>
        <span className="grow" style={{ flex: 1 }} />
        <span style={{ fontSize: 11, color: "var(--text-3)" }}>
          掉线、恢复会记在这里
        </span>
      </div>
    </div>
  );
}
