// 日志弹层（D4 §3.6）：搜索 + 高亮、过滤、自动刷新、复制 / 导出、归档切换、
// 显示路径 + 打开文件夹、全部日志按来源分组。
//
// 兼容两种用法：
//  - 新：<LogsModal target={{kind:"tunnel"|"service"|"all", ...}} onClose={...} />
//  - 旧：<LogsModal id={id} name={name} source="tunnel"|"service" onClose={...} />
//
// 数据走 query_logs；旧后端 / 浏览器预览没有这个命令时，退回 get_logs / get_service_logs
// 的纯文本（只保留搜索 / 过滤 / 复制 / 自动刷新，其余按钮置灰）。

import {
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import {
  getLogArchives,
  getLogs,
  getServiceLogs,
  openDataDir,
  openLogFolder,
  queryLogs,
  saveTextFile,
  type LogArchive,
  type LogEntry,
} from "../api";
import { copyText } from "../copy/clipboard";
import { T } from "../copy/toasts";
import { classifyLine, type LogFilter } from "../lib/logFilter";
import { errorText } from "../store/commands";
import { toast } from "../store/toastStore";
import { useApp, useStore } from "../store/useStore";
import { useModalA11y } from "./useModalA11y";
import "../styles/modals.css";

export type LogTarget =
  | { kind: "tunnel"; id: string; name: string }
  | { kind: "service"; id: string; name: string }
  | { kind: "all" };

type LogsModalProps =
  | { target: LogTarget; onClose(): void; id?: never; name?: never; source?: never }
  | {
      id: string;
      name: string;
      source: "tunnel" | "service";
      onClose(): void;
      target?: never;
    };

/** 日志查看器的一小块状态（D4 §3.6） */
interface LogsViewState {
  query: string;
  filter: LogFilter;
  lines: 200 | 500 | 1000 | 2000;
  autoRefresh: boolean;
  archive: 0 | 1 | 2;
}

interface LogGroup {
  key: string;
  /** 来源标签：`应用` / `发布:{名字}` / `程序:{名字}` */
  label: string;
  entries: LogEntry[];
}

interface RenderedLine {
  time: string;
  label: string;
  body: string;
  level: string | null;
  human: boolean;
  error: boolean;
}

const PREF_KEY = "logs.prefs";
/** 全部日志里来源超过这个数就改用「合并查询 + 按类分组」，避免一次打太多请求 */
const MAX_SOURCE_FETCH = 12;
/** 全部日志一次最多显示多少行（D2 总量上限 2000） */
const MAX_TOTAL_LINES = 2000;

const DEFAULT_VIEW: LogsViewState = {
  query: "",
  filter: "all",
  lines: 500,
  autoRefresh: true,
  archive: 0,
};

function loadPrefs(): LogsViewState {
  try {
    const raw = localStorage.getItem(PREF_KEY);
    if (!raw) return DEFAULT_VIEW;
    const saved = JSON.parse(raw) as Partial<LogsViewState>;
    return {
      query: "",
      filter:
        saved.filter === "human" || saved.filter === "error" ? saved.filter : "all",
      lines:
        saved.lines === 200 || saved.lines === 1000 || saved.lines === 2000
          ? saved.lines
          : 500,
      autoRefresh: saved.autoRefresh !== false,
      archive: saved.archive === 1 || saved.archive === 2 ? saved.archive : 0,
    };
  } catch {
    return DEFAULT_VIEW;
  }
}

function savePrefs(view: LogsViewState): void {
  try {
    const { query: _query, ...rest } = view;
    localStorage.setItem(PREF_KEY, JSON.stringify(rest));
  } catch {
    // 存不了就算了，只是记不住偏好
  }
}

function hhmmss(ts: number | null | undefined): string {
  if (!ts || !Number.isFinite(ts)) return "--:--:--";
  const d = new Date(ts);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** 旧日志行可能自带时间前缀：`[HH:MM:SS]` 或 `[epoch-ms]`。 */
function splitTimePrefix(raw: string): { time: string | null; body: string } {
  const clock = /^\[(\d{2}:\d{2}:\d{2})\]\s?/.exec(raw);
  if (clock) return { time: clock[1], body: raw.slice(clock[0].length) };
  const epoch = /^\[(\d{9,})\]\s?/.exec(raw);
  if (epoch) return { time: hhmmss(Number(epoch[1])), body: raw.slice(epoch[0].length) };
  return { time: null, body: raw };
}

function detectLevel(text: string): string | null {
  const lower = text.toLowerCase();
  if (!lower.includes("[error]") && !lower.includes("[fail]") && !lower.includes("[fatal]")) {
    if (lower.includes("[warn]")) return "warn";
    if (lower.includes("[info]")) return "info";
    return null;
  }
  return "error";
}

function legacyEntry(line: string, source: "tunnel" | "service"): LogEntry {
  return {
    ts: null,
    source,
    origin: source === "tunnel" ? "tunnel" : "service",
    level: detectLevel(line),
    code: null,
    subject: null,
    message: null,
    raw: line,
  };
}

function entryKey(entry: LogEntry, index: number): string {
  return `${entry.ts ?? "x"}|${entry.origin}|${entry.raw.slice(0, 120)}|${index}`;
}

function labelForEntry(entry: LogEntry, fallback: string): string {
  const name = entry.subject?.name;
  if (!name) return fallback;
  if (entry.source === "tunnel") return `发布:${name}`;
  if (entry.source === "service") return `程序:${name}`;
  return "应用";
}

function renderEntry(entry: LogEntry, fallbackLabel: string): RenderedLine {
  const kind = classifyLine(entry);
  const human = kind !== "tech";
  const error = kind === "error";
  if (entry.origin === "events") {
    return {
      time: hhmmss(entry.ts),
      label: labelForEntry(entry, fallbackLabel),
      body: entry.message ?? entry.raw,
      level: entry.level,
      human,
      error,
    };
  }
  const prefixed = splitTimePrefix(entry.raw);
  return {
    time: entry.ts ? hhmmss(entry.ts) : prefixed.time ?? "--:--:--",
    label: labelForEntry(entry, fallbackLabel),
    body: prefixed.body,
    level: entry.level,
    human,
    error,
  };
}

function highlight(text: string, query: string): ReactNode {
  if (!query) return text;
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  const out: ReactNode[] = [];
  let from = 0;
  let n = 0;
  for (;;) {
    const idx = lower.indexOf(q, from);
    if (idx < 0) {
      out.push(text.slice(from));
      break;
    }
    if (idx > from) out.push(text.slice(from, idx));
    out.push(<mark key={n}>{text.slice(idx, idx + q.length)}</mark>);
    n += 1;
    from = idx + q.length;
  }
  return out;
}

function countHits(text: string, query: string): number {
  if (!query) return 0;
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  let count = 0;
  let from = 0;
  for (;;) {
    const idx = lower.indexOf(q, from);
    if (idx < 0) break;
    count += 1;
    from = idx + q.length;
  }
  return count;
}

function sanitizeFileName(name: string): string {
  return name.replace(/[\\/:*?"<>|\s]+/g, "-").slice(0, 40) || "logs";
}

export function LogsModal(props: LogsModalProps) {
  const target: LogTarget = props.target ?? {
    kind: props.source,
    id: props.id,
    name: props.name,
  };
  const { onClose } = props;
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);

  const inTauri =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  const app = useApp();
  const tunnels = useStore((s) => s.tunnels);
  const services = useStore((s) => s.services);
  const dataDir = app.dataDir;
  // 名字只用来显示分组标题：放进 ref，避免快照每 800ms 的字段变化把取数函数重新创建
  const tunnelsRef = useRef(tunnels);
  tunnelsRef.current = tunnels;
  const servicesRef = useRef(services);
  servicesRef.current = services;

  const [view, setView] = useState<LogsViewState>(loadPrefs);
  const [groups, setGroups] = useState<LogGroup[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [legacy, setLegacy] = useState(!inTauri);
  const [archives, setArchives] = useState<LogArchive[]>([]);
  const [pathOpen, setPathOpen] = useState(false);
  const [busyAction, setBusyAction] = useState<"" | "copy" | "export">("");
  const [cursor, setCursor] = useState(0);
  const [autoScroll, setAutoScroll] = useState(true);
  const [newLines, setNewLines] = useState(0);
  /** 从某条隧道的日志里点「全部日志」时，本弹层内部切到全部视图 */
  const [allOverride, setAllOverride] = useState(false);

  const effectiveTarget: LogTarget = allOverride ? { kind: "all" } : target;
  const isAll = effectiveTarget.kind === "all";
  const targetKey =
    effectiveTarget.kind === "all"
      ? "all"
      : `${effectiveTarget.kind}:${effectiveTarget.id}`;
  // 只把原始值放进 useCallback / useEffect 的依赖里：props 里的对象每次渲染都是新的
  const targetKind = effectiveTarget.kind;
  const targetId = effectiveTarget.kind === "all" ? "" : effectiveTarget.id;
  const targetName = effectiveTarget.kind === "all" ? "" : effectiveTarget.name;

  const autoScrollRef = useRef(autoScroll);
  autoScrollRef.current = autoScroll;
  const previousKeys = useRef<Set<string>>(new Set());
  const loadedOnceRef = useRef(false);
  const lineRefs = useRef<Map<number, HTMLDivElement>>(new Map());
  const viewerRef = useRef<HTMLDivElement | null>(null);

  const nameOf = useCallback(
    (kind: "tunnel" | "service", id: string): string | undefined => {
      if (kind === "tunnel") return tunnelsRef.current.find((t) => t.id === id)?.name;
      return servicesRef.current.find((s) => s.id === id)?.name;
    },
    [],
  );

  // ---------- 取数 ----------

  const fetchAllGroups = useCallback(
    async (sources: string[]): Promise<{ groups: LogGroup[]; truncated: boolean }> => {
      const appSources = sources.filter((s) => s === "events" || s === "app");
      const remoteSources = sources.filter(
        (s) => s.startsWith("tunnel:") || s.startsWith("service:"),
      );
      const results = new Map<string, LogEntry[]>();
      let truncated = false;

      if (appSources.length > 0) {
        const res = await queryLogs({
          source: "app",
          lines: view.lines,
          archive: view.archive,
          format: "json",
        });
        results.set("events", (res.entries ?? []).filter((e) => e.origin === "events"));
        results.set("app", (res.entries ?? []).filter((e) => e.origin !== "events"));
        truncated = truncated || res.truncated;
      }

      const fetched = await Promise.all(
        remoteSources.map(async (id) => {
          const kind = id.startsWith("tunnel:") ? "tunnel" : "service";
          const entityId = id.slice(kind.length + 1);
          const res = await queryLogs({
            source: kind,
            id: entityId,
            lines: view.lines,
            archive: view.archive,
            format: "json",
          });
          return { id, entries: res.entries ?? [], truncated: res.truncated === true };
        }),
      );
      for (const item of fetched) {
        results.set(item.id, item.entries);
        truncated = truncated || item.truncated;
      }

      const out: LogGroup[] = [];
      let total = 0;
      let appDone = false;
      for (const id of sources) {
        let group: LogGroup | null = null;
        if (id === "events" || id === "app") {
          if (appDone) continue;
          appDone = true;
          const entries = [...(results.get("events") ?? []), ...(results.get("app") ?? [])];
          if (entries.length === 0) continue;
          group = { key: "app", label: "应用", entries };
        } else if (id.startsWith("tunnel:")) {
          const tunnelId = id.slice("tunnel:".length);
          const entries = results.get(id) ?? [];
          if (entries.length === 0) continue;
          group = {
            key: id,
            label: `发布:${nameOf("tunnel", tunnelId) ?? tunnelId}`,
            entries,
          };
        } else if (id.startsWith("service:")) {
          const serviceId = id.slice("service:".length);
          const entries = results.get(id) ?? [];
          if (entries.length === 0) continue;
          group = {
            key: id,
            label: `程序:${nameOf("service", serviceId) ?? serviceId}`,
            entries,
          };
        }
        if (!group) continue;
        if (total + group.entries.length > MAX_TOTAL_LINES) {
          group = {
            ...group,
            entries: group.entries.slice(0, Math.max(0, MAX_TOTAL_LINES - total)),
          };
          truncated = true;
        }
        total += group.entries.length;
        if (group.entries.length > 0) out.push(group);
      }
      return { groups: out, truncated };
    },
    [view.lines, view.archive, nameOf],
  );

  const load = useCallback(async () => {
    // 自动刷新时不要每 2 秒闪一次「读取中」；只有第一次取数才显示加载态
    if (!loadedOnceRef.current) setLoading(true);
    setError("");
    try {
      if (legacy) {
        if (targetKind === "all") {
          setGroups([]);
          setError("当前后台看不了全部日志，只能单看一条。");
          return;
        }
        const text =
          targetKind === "tunnel"
            ? await getLogs(targetId, view.lines)
            : await getServiceLogs(targetId, view.lines);
        const lines = text.split(/\r?\n/);
        if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
        const entries = lines.map((line) => legacyEntry(line, targetKind));
        setGroups([
          {
            key: targetKey,
            label:
              targetKind === "tunnel" ? `发布:${targetName}` : `程序:${targetName}`,
            entries,
          },
        ]);
        setTruncated(false);
        return;
      }

      if (targetKind === "all") {
        const probe = await queryLogs({
          source: "all",
          lines: 1,
          archive: view.archive,
          format: "json",
        });
        const sources = probe.sources ?? [];
        if (sources.length > MAX_SOURCE_FETCH) {
          // 来源太多：一次合并查询，按大类分组（不逐个隧道拆）
          const res = await queryLogs({
            source: "all",
            lines: view.lines,
            archive: view.archive,
            format: "json",
          });
          const entries = res.entries ?? [];
          const appEntries = entries.filter(
            (e) => e.origin === "events" || e.origin === "appLog",
          );
          const tunnelEntries = entries.filter((e) => e.source === "tunnel");
          const serviceEntries = entries.filter((e) => e.source === "service");
          const coarse: LogGroup[] = [];
          if (appEntries.length) coarse.push({ key: "app", label: "应用", entries: appEntries });
          if (tunnelEntries.length)
            coarse.push({ key: "tunnel-all", label: "发布日志", entries: tunnelEntries });
          if (serviceEntries.length)
            coarse.push({ key: "service-all", label: "程序日志", entries: serviceEntries });
          setGroups(coarse);
          setTruncated(res.truncated === true);
          return;
        }
        const result = await fetchAllGroups(sources);
        setGroups(result.groups);
        // probe 用了 lines=1（只看有哪些来源），它的 truncated 没有意义，不能用
        setTruncated(result.truncated);
        return;
      }

      const res = await queryLogs({
        source: targetKind,
        id: targetId,
        lines: view.lines,
        archive: view.archive,
        format: "json",
      });
      setGroups([
        {
          key: targetKey,
          label:
            targetKind === "tunnel" ? `发布:${targetName}` : `程序:${targetName}`,
          entries: res.entries ?? [],
        },
      ]);
      setTruncated(res.truncated === true);
    } catch (e) {
      if (!legacy) {
        // 旧后端没有 query_logs：切到旧接口，下次进这里走纯文本
        setLegacy(true);
        setError("这个后台还没有新日志接口，已切到简单模式");
        return;
      }
      setError(errorText(e));
    } finally {
      loadedOnceRef.current = true;
      setLoading(false);
    }
  }, [legacy, targetKind, targetId, targetName, targetKey, view.lines, view.archive, fetchAllGroups]);

  // 打开 / 目标 / 行数 / 归档变化时重新取数
  useEffect(() => {
    void load();
  }, [load]);

  // 归档列表（旧接口没有）
  useEffect(() => {
    if (legacy) {
      setArchives([]);
      return;
    }
    let alive = true;
    getLogArchives()
      .then((r) => {
        if (alive) setArchives(r.items ?? []);
      })
      .catch(() => {
        if (alive) setArchives([]);
      });
    return () => {
      alive = false;
    };
  }, [legacy]);

  useEffect(() => {
    savePrefs(view);
  }, [view]);

  // 自动刷新：约 2 秒一次（D4 §3.6）；旧接口也一样能刷
  useEffect(() => {
    if (!view.autoRefresh) return;
    const timer = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(timer);
  }, [view.autoRefresh, load]);

  // 新行计数（用户往上滚之后不再自动跳到底）
  useEffect(() => {
    const keys = new Set<string>();
    let index = 0;
    for (const group of groups) {
      for (const entry of group.entries) keys.add(entryKey(entry, index++));
    }
    if (!autoScrollRef.current) {
      let fresh = 0;
      for (const key of keys) if (!previousKeys.current.has(key)) fresh += 1;
      if (fresh > 0) setNewLines((n) => n + fresh);
    }
    previousKeys.current = keys;
  }, [groups]);

  // ---------- 过滤 / 搜索 ----------

  const flat = useMemo(() => {
    const out: Array<{ group: LogGroup; entry: LogEntry; render: RenderedLine }> = [];
    const q = view.query.trim();
    for (const group of groups) {
      for (const entry of group.entries) {
        const kind = classifyLine(entry);
        if (view.filter === "human" && kind !== "human") continue;
        if (view.filter === "error" && kind !== "error") continue;
        if (q && !entry.raw.toLowerCase().includes(q.toLowerCase())) continue;
        out.push({ group, entry, render: renderEntry(entry, group.label) });
      }
    }
    return out;
  }, [groups, view.filter, view.query]);

  const hitCountTotal = useMemo(() => {
    const q = view.query.trim();
    if (!q) return 0;
    return flat.reduce((n, item) => n + countHits(item.render.body, q), 0);
  }, [flat, view.query]);

  const hitIndexes = useMemo(() => {
    const q = view.query.trim().toLowerCase();
    if (!q) return [] as number[];
    return flat
      .map((item, i) => (item.render.body.toLowerCase().includes(q) ? i : -1))
      .filter((i) => i >= 0);
  }, [flat, view.query]);

  useEffect(() => {
    setCursor(0);
  }, [view.query]);

  useEffect(() => {
    if (!view.query.trim() || hitIndexes.length === 0) return;
    const index = hitIndexes[Math.min(cursor, hitIndexes.length - 1)];
    lineRefs.current.get(index)?.scrollIntoView({ block: "center" });
  }, [cursor, hitIndexes, view.query]);

  const jump = useCallback(
    (dir: 1 | -1) => {
      if (hitIndexes.length === 0) return;
      setCursor((c) => (c + dir + hitIndexes.length) % hitIndexes.length);
    },
    [hitIndexes.length],
  );

  // 内容变化时贴到底部（用户没往上滚的话）
  useEffect(() => {
    if (!autoScroll) return;
    const el = viewerRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [flat, autoScroll]);

  const onViewerScroll = () => {
    const el = viewerRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    if (!atBottom && autoScroll) {
      setAutoScroll(false);
      setNewLines(0);
    }
  };

  const backToLatest = () => {
    setAutoScroll(true);
    setNewLines(0);
    setCursor(0);
    void load();
    const el = viewerRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  };

  // ---------- 复制 / 导出 / 打开 ----------

  const plainText = useMemo(
    () =>
      flat
        .map((item) => `[${item.render.time}] [${item.render.label}] ${item.render.body}`)
        .join("\n"),
    [flat],
  );

  const doCopy = async () => {
    if (plainText === "") {
      toast.info("这里还没有日志");
      return;
    }
    setBusyAction("copy");
    const ok = await copyText(plainText);
    setBusyAction("");
    if (ok) toast.show({ kind: "success", text: T.logCopied() });
    else toast.error("没复制成功：选中内容按 Ctrl+C");
  };

  const doExport = async () => {
    if (legacy) return;
    if (plainText === "") {
      toast.info("这里还没有日志");
      return;
    }
    setBusyAction("export");
    const name = `${sanitizeFileName(
      isAll ? "全部日志" : effectiveTarget.name,
    )}-${new Date().toISOString().slice(0, 19).replace(/[T:]/g, "-")}.log`;
    try {
      const saved = await saveTextFile(name, plainText);
      toast.success(`日志已导出到「${saved.path}」`);
    } catch (e) {
      // 保存不了就退回「复制 + 自己粘贴保存」
      const ok = await copyText(plainText);
      if (ok) toast.info("没写进文件，已复制到剪贴板");
      else toast.error(`导出没成功：${errorText(e)}`);
    } finally {
      setBusyAction("");
    }
  };

  const doOpenFolder = async () => {
    if (legacy) return;
    try {
      await openLogFolder();
    } catch {
      try {
        await openDataDir();
      } catch (e) {
        toast.error(`打开文件夹没成功：${errorText(e)}`);
      }
    }
  };

  // ---------- 文案 ----------

  const title =
    effectiveTarget.kind === "all"
      ? "全部日志"
      : `${effectiveTarget.kind === "tunnel" ? "发布日志" : "程序日志"} · ${effectiveTarget.name}`;
  const sourceHint =
    effectiveTarget.kind === "all"
      ? "所有隧道和程序的日志按来源分组。出问题把这里的内容发出来就能定位。"
      : effectiveTarget.kind === "tunnel"
        ? "这条隧道的连接记录。出问题把这里的内容发出来就能定位。"
        : "程序自己打印的内容（含看护记录）。出问题把内容发出来就能定位。";

  const hasArchives = archives.length > 0;
  const legacyTitle = "这个功能需要新版本后台";

  const disabledLegacyProps = (fallbackTitle: string) =>
    legacy ? { disabled: true, title: fallbackTitle } : {};

  return (
    <div className="modal-backdrop">
      <div
        className={`modal logs-modal ${isAll ? "all" : ""}`}
        role="dialog"
        aria-modal="true"
        ref={dialogRef}
      >
        <div className="modal-head">
          <h2 className="modal-title">{title}</h2>
          <p className="modal-sub" title={sourceHint}>
            最近 {view.lines} 行。
          </p>
        </div>

        <div className="logs-toolbar">
          <div className="logs-row">
            <input
              className="input logs-search"
              placeholder="搜日志里的字…"
              value={view.query}
              onChange={(e) => setView((v) => ({ ...v, query: e.target.value }))}
              onKeyDown={(e) => {
                if (e.key !== "Enter") return;
                e.preventDefault();
                jump(e.shiftKey ? -1 : 1);
              }}
            />
            {view.query ? (
              <>
                <span className="logs-hit">{hitCountTotal} 处</span>
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() => jump(-1)}
                  disabled={hitIndexes.length === 0}
                >
                  上一个
                </button>
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() => jump(1)}
                  disabled={hitIndexes.length === 0}
                >
                  下一个
                </button>
                <button
                  className="link-btn"
                  onClick={() => setView((v) => ({ ...v, query: "" }))}
                >
                  清空
                </button>
              </>
            ) : null}
          </div>

          <div className="logs-row">
            <div className="seg" role="group" aria-label="日志过滤">
              <button
                className={view.filter === "all" ? "active" : ""}
                onClick={() => setView((v) => ({ ...v, filter: "all" }))}
              >
                全部
              </button>
              <button
                className={view.filter === "human" ? "active" : ""}
                onClick={() => setView((v) => ({ ...v, filter: "human" }))}
              >
                只看人话行
              </button>
              <button
                className={view.filter === "error" ? "active" : ""}
                onClick={() => setView((v) => ({ ...v, filter: "error" }))}
              >
                只看错误
              </button>
            </div>

            <label className="logs-tool">
              显示行数
              <select
                className="select"
                style={{ width: "auto" }}
                value={view.lines}
                onChange={(e) =>
                  setView((v) => ({
                    ...v,
                    lines: Number(e.target.value) as LogsViewState["lines"],
                  }))
                }
              >
                <option value={200}>200</option>
                <option value={500}>500</option>
                <option value={1000}>1000</option>
                <option value={2000}>2000</option>
              </select>
            </label>

            <label className="logs-tool">
              <input
                type="checkbox"
                checked={view.autoRefresh}
                onChange={(e) =>
                  setView((v) => ({ ...v, autoRefresh: e.target.checked }))
                }
              />
              自动刷新
            </label>

            <span className="grow" style={{ flex: 1 }} />

            <button
              className="btn btn-ghost btn-sm"
              disabled={busyAction !== ""}
              onClick={() => void doCopy()}
            >
              {busyAction === "copy" ? "复制中…" : "复制全部"}
            </button>
            <button
              className="btn btn-ghost btn-sm"
              {...disabledLegacyProps(legacyTitle)}
              disabled={legacy || busyAction !== ""}
              onClick={() => void doExport()}
            >
              {busyAction === "export" ? "导出中…" : "导出文件…"}
            </button>

            <label className="logs-tool">
              归档
              <select
                className="select"
                style={{ width: "auto" }}
                value={view.archive}
                disabled={legacy}
                title={legacy ? legacyTitle : undefined}
                onChange={(e) =>
                  setView((v) => ({
                    ...v,
                    archive: Number(e.target.value) as LogsViewState["archive"],
                  }))
                }
              >
                <option value={0}>当前日志</option>
                <option value={1} disabled={!hasArchives}>
                  上一份
                </option>
                <option value={2} disabled={archives.length < 2}>
                  上上份
                </option>
              </select>
            </label>

            <button
              className="link-btn"
              {...disabledLegacyProps(legacyTitle)}
              disabled={legacy}
              onClick={() => setPathOpen((open) => !open)}
            >
              显示路径
            </button>

            {!isAll && (
              <button
                className="btn btn-ghost btn-sm"
                onClick={() => setAllOverride(true)}
              >
                全部日志
              </button>
            )}
          </div>

          {pathOpen && (
            <div className="logs-path-row">
              <span className="mono">{dataDir ? `${dataDir}\\logs` : "还没拿到数据目录"}</span>
              <button
                className="link-btn"
                disabled={legacy}
                title={legacy ? legacyTitle : undefined}
                onClick={() => void doOpenFolder()}
              >
                打开文件夹
              </button>
            </div>
          )}
        </div>

        <div className="modal-body" style={{ paddingTop: 12 }}>
          {error && <div className="error-line">{error}</div>}
          {loading && flat.length === 0 && !error ? (
            <p className="field-hint hint">读取中…</p>
          ) : flat.length === 0 ? (
            <div className="log-empty">
              {view.query.trim() || view.filter !== "all" ? (
                <>
                  <p className="log-empty-title">没有符合条件的日志</p>
                  <p>换个关键词，或者把过滤切回「全部」。</p>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() =>
                      setView((v) => ({ ...v, query: "", filter: "all" }))
                    }
                  >
                    清空筛选
                  </button>
                </>
              ) : (
                <>
                  <p className="log-empty-title">这里还没有日志</p>
                  <p title="一直空着就先体检一下">等它跑一会儿再来看看。</p>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() =>
                      window.dispatchEvent(new CustomEvent("shangxian:open-doctor"))
                    }
                  >
                    先体检一下
                  </button>
                </>
              )}
            </div>
          ) : (
            <div style={{ position: "relative" }}>
              <div
                className="logs-viewer log-viewer"
                role="log"
                aria-live="off"
                ref={viewerRef}
                onScroll={onViewerScroll}
              >
                {flat.map((item, index) => {
                  // warn 单独给黄色，其余按分类红 / 人话 / 机器
                  const level =
                    item.render.level === "warn"
                      ? "warn"
                      : item.render.error
                        ? "error"
                        : item.render.human
                          ? "human"
                          : "tech";
                  const isCurrent =
                    view.query.trim() !== "" &&
                    hitIndexes[Math.min(cursor, hitIndexes.length - 1)] === index;
                  return (
                    <div key={entryKey(item.entry, index)}>
                      {isAll &&
                      (index === 0 || flat[index - 1].group.key !== item.group.key) ? (
                        <div className="log-group-head">
                          {item.group.label}
                          <span className="log-group-count">
                            {item.group.entries.length} 条
                          </span>
                        </div>
                      ) : null}
                      <div
                        className={`log-line ${level} ${isCurrent ? "current" : ""}`}
                        ref={(node) => {
                          if (node) lineRefs.current.set(index, node);
                          else lineRefs.current.delete(index);
                        }}
                      >
                        <span className="log-time">[{item.render.time}]</span>{" "}
                        <span className="log-src">[{item.render.label}]</span>{" "}
                        {highlight(item.render.body, view.query.trim())}
                      </div>
                    </div>
                  );
                })}
                {truncated && (
                  <div className="log-truncated">日志太多，只显示了这些</div>
                )}
              </div>
              {!autoScroll && (
                <div className="log-float-bar" aria-live="polite">
                  {newLines > 0 ? `有 ${newLines} 行新日志` : "已暂停自动刷新"}
                  <button className="link-btn" onClick={backToLatest}>
                    回到最新
                  </button>
                </div>
              )}
            </div>
          )}
        </div>

        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={() => void load()}>
            {loading ? "刷新中…" : "刷新"}
          </button>
          <span className="grow" />
          <button className="btn btn-primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
