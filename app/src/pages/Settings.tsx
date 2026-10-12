// 设置页（D4 §3.8）：外观 / 开机自启 / 掉线提醒 / 刷新频率 / 启动检查更新 /
// 免打扰时段 / 数据目录 / 体检 / 关于卡片 / 诊断包。
// 设置项写 config（整份 set_settings，就地「已保存」）；主题仍走 localStorage。

import { useCallback, useEffect, useRef, useState } from "react";

import type { ShellIntent } from "../App";
import { AboutCard } from "../components/AboutCard";
import { DiagExportModal } from "../components/DiagExportModal";
import { DoctorModal } from "../components/DoctorModal";
import { InfoTip } from "../components/InfoTip";
import {
  getAutostart,
  getDataDirInfo,
  getNotifyEnabled,
  listDeleted,
  openDataDir,
  setAutostart,
  setNotifyEnabled,
  setSettings,
  testNotification,
  type DataDirInfo,
  type Settings,
  type TrashItem,
} from "../api";
import { actions } from "../store/actions";
import { refreshNow } from "../store/snapshotStore";
import { useApp } from "../store/useStore";
import { toast } from "../store/toastStore";
import "../styles/settings.css";

type Theme = "system" | "light" | "dark";

const THEMES: { value: Theme; label: string }[] = [
  { value: "system", label: "跟随系统" },
  { value: "light", label: "浅色" },
  { value: "dark", label: "深色" },
];

/** 刷新频率档位（D0 第 7 条：500 / 800 默认 / 1000 / 2000 / 5000） */
const REFRESH_OPTIONS: { value: number; label: string }[] = [
  { value: 500, label: "500 毫秒（最灵敏）" },
  { value: 800, label: "800 毫秒（默认）" },
  { value: 1000, label: "1000 毫秒（1 秒）" },
  { value: 2000, label: "2000 毫秒（2 秒）" },
  { value: 5000, label: "5000 毫秒（5 秒）" },
];

const DATA_DIR_KIND_WORD: Record<string, string> = {
  portable: "便携模式",
  env: "环境变量",
  local: "本机应用数据",
  legacyRoaming: "漫游应用数据",
  roaming: "漫游应用数据",
  preview: "浏览器预览",
};

const UNTRUSTED_HINT = "配置文件读不出来，先修好再改设置";

const TRASH_TYPE_WORD: Record<TrashItem["type"], string> = {
  tunnel: "隧道",
  service: "本机服务",
  server: "服务器",
};

/** 「最近删除」还剩多久：剩 9 分 30 秒 / 剩 45 秒 / 已过期 */
function countdownText(expiresAt: number): string {
  const left = Math.max(0, expiresAt - Date.now());
  if (left <= 0) return "已过期";
  const mins = Math.floor(left / 60000);
  const secs = Math.floor((left % 60000) / 1000);
  return mins > 0 ? `剩 ${mins} 分 ${secs} 秒` : `剩 ${secs} 秒`;
}

/** 两份 settings 是否完全一样（快照每次都会新建对象，得比值再决定要不要刷草稿） */
function sameSettings(a: Settings, b: Settings): boolean {
  return (
    a.refreshIntervalMs === b.refreshIntervalMs &&
    a.checkUpdateOnStart === b.checkUpdateOnStart &&
    a.quietHours.enabled === b.quietHours.enabled &&
    a.quietHours.start === b.quietHours.start &&
    a.quietHours.end === b.quietHours.end
  );
}

interface Props {
  /** 外壳下发的意图：通知中心「免打扰设置」定位到免打扰 / 刷新频率区 */
  intent?: ShellIntent | null;
  /** 意图处理完通知外壳清掉 */
  onIntentDone?: () => void;
}

export function SettingsPage({ intent, onIntentDone }: Props = {}) {
  const app = useApp();
  const [theme, setTheme] = useState<Theme>(
    () => (localStorage.getItem("theme") as Theme) || "system",
  );
  const [autostart, setAutostartState] = useState(false);
  const [notify, setNotifyState] = useState(true);
  const [dirInfo, setDirInfo] = useState<DataDirInfo | null>(null);
  const [draft, setDraft] = useState<Settings>(app.settings);
  const [saving, setSaving] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [doctorOpen, setDoctorOpen] = useState(false);
  const [diagOpen, setDiagOpen] = useState(false);
  /** 「最近删除」列表：null = 还没读回来 */
  const [trash, setTrash] = useState<TrashItem[] | null>(null);
  const [trashError, setTrashError] = useState("");
  const [trashBusy, setTrashBusy] = useState(false);
  /** 剩多久要每秒走：只用来触发重渲染 */
  const [, setTick] = useState(0);

  const trustRef = useRef(0);
  const savedTimer = useRef<number | null>(null);

  const untrusted = !app.configTrusted;

  // 主题：立即应用到 <html data-theme>，并存 localStorage
  useEffect(() => {
    if (theme === "system") {
      delete document.documentElement.dataset.theme;
    } else {
      document.documentElement.dataset.theme = theme;
    }
    localStorage.setItem("theme", theme);
  }, [theme]);

  // 开机自启 / 掉线提醒 / 数据目录来源：启动时各读一次
  useEffect(() => {
    getAutostart()
      .then(setAutostartState)
      .catch(() => {});
    getNotifyEnabled()
      .then(setNotifyState)
      .catch(() => {});
    getDataDirInfo()
      .then(setDirInfo)
      .catch(() => setDirInfo(null));
  }, []);

  // 快照里的 settings 变了就同步草稿；刚保存完的 3 秒内不信旧快照
  useEffect(() => {
    if (Date.now() < trustRef.current) return;
    setDraft((cur) => (sameSettings(cur, app.settings) ? cur : app.settings));
  }, [app.settings]);

  // 外壳意图：通知中心「免打扰设置」定位到免打扰时段区（D4 §3.8）
  const handledIntent = useRef<ShellIntent | null>(null);
  useEffect(() => {
    if (!intent || handledIntent.current === intent) return;
    handledIntent.current = intent;
    if (intent.kind === "focus-settings") {
      if (intent.focus === "doctor") {
        // 「去体检」：不走滚动定位，直接打开体检弹层（App 横幅的入口）
        setDoctorOpen(true);
      } else {
        const id = intent.focus === "refresh" ? "settings-refresh" : "settings-quiet";
        const el = document.getElementById(id);
        if (el) {
          el.scrollIntoView({ block: "center" });
          const accent =
            getComputedStyle(el).getPropertyValue("--accent").trim() || "#4f5be8";
          try {
            el.animate(
              [
                { boxShadow: `0 0 0 3px ${accent}66`, borderRadius: "12px" },
                { boxShadow: "0 0 0 3px transparent", borderRadius: "12px" },
              ],
              { duration: 1200, easing: "ease-out" },
            );
          } catch {
            el.style.outline = `3px solid ${accent}`;
            window.setTimeout(() => {
              el.style.outline = "";
            }, 1200);
          }
        }
      }
    }
    onIntentDone?.();
  }, [intent, onIntentDone]);

  // 最近删除：进设置页读一次；有东西在倒计时就每秒刷一次显示
  const loadTrash = useCallback(async () => {
    try {
      const result = await listDeleted();
      setTrash(result.items ?? []);
      setTrashError("");
    } catch {
      setTrashError("读不到「最近删除」列表；删掉的东西不受影响。");
    }
  }, []);

  useEffect(() => {
    void loadTrash();
  }, [loadTrash]);

  useEffect(() => {
    if (!trash || trash.length === 0) return;
    const timer = window.setInterval(() => setTick((t) => t + 1), 1000);
    return () => window.clearInterval(timer);
  }, [trash]);

  const restoreTrash = useCallback(
    async (item: TrashItem) => {
      setTrashBusy(true);
      try {
        await actions.undo(item.token, item.name);
      } finally {
        setTrashBusy(false);
        void loadTrash();
      }
    },
    [loadTrash],
  );

  const restoreAllTrash = useCallback(async () => {
    if (!trash || trash.length === 0) return;
    setTrashBusy(true);
    try {
      await actions.undoMany(
        trash.map((item) => ({ token: item.token, name: item.name })),
      );
    } finally {
      setTrashBusy(false);
      void loadTrash();
    }
  }, [trash, loadTrash]);

  useEffect(
    () => () => {
      if (savedTimer.current !== null) window.clearTimeout(savedTimer.current);
    },
    [],
  );

  const toggleAutostart = async (next: boolean) => {
    setError("");
    try {
      await setAutostart(next);
      setAutostartState(next);
      refreshNow();
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleNotify = async (next: boolean) => {
    setError("");
    try {
      await setNotifyEnabled(next);
      setNotifyState(next);
    } catch (e) {
      setError(String(e));
    }
  };

  /** 整份写 settings；成功就地亮「已保存」，失败回到快照里的旧值并给原文。 */
  const save = async (next: Settings, key: string) => {
    if (untrusted) return;
    setDraft(next);
    setError("");
    setSaving(key);
    try {
      const written = await setSettings(next);
      trustRef.current = Date.now() + 3000;
      setDraft(written);
      refreshNow();
      setSaved(key);
      if (savedTimer.current !== null) window.clearTimeout(savedTimer.current);
      savedTimer.current = window.setTimeout(() => {
        setSaved((cur) => (cur === key ? null : cur));
      }, 1600);
    } catch (e) {
      setError(String(e));
      setDraft(app.settings);
    } finally {
      setSaving(null);
    }
  };

  const savedNote = (key: string) =>
    saved === key ? <span className="saved-note">已保存</span> : null;

  const intervalOptions = REFRESH_OPTIONS.some(
    (o) => o.value === draft.refreshIntervalMs,
  )
    ? REFRESH_OPTIONS
    : [
        ...REFRESH_OPTIONS,
        { value: draft.refreshIntervalMs, label: `${draft.refreshIntervalMs} 毫秒` },
      ].sort((a, b) => a.value - b.value);

  const dirKind = dirInfo?.kind ?? "";
  const dirKindWord = DATA_DIR_KIND_WORD[dirKind] ?? (dirKind || "未知来源");
  const quietSame = draft.quietHours.start === draft.quietHours.end;

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">设置</h1>
          <p className="page-sub">刷新频率、免打扰、数据目录都在这儿。</p>
        </div>
      </header>
      <div className="service-list">
        <article className="card">
          <fieldset className="settings-fieldset">
            <legend className="settings-legend">外观</legend>
            <div className="option-list" style={{ flexDirection: "row", gap: 8 }}>
              {THEMES.map((t) => (
                <label
                  key={t.value}
                  className={`option ${theme === t.value ? "selected" : ""}`}
                  style={{ flex: 1, justifyContent: "center" }}
                >
                  <input
                    type="radio"
                    name="theme"
                    value={t.value}
                    checked={theme === t.value}
                    onChange={() => setTheme(t.value)}
                  />
                  {t.label}
                </label>
              ))}
            </div>
          </fieldset>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">开机自动启动</div>
              <div className="server-meta hint">登录后在托盘里悄悄运行。</div>
            </div>
            <button
              className={autostart ? "btn btn-ghost btn-sm" : "btn btn-primary btn-sm"}
              onClick={() => void toggleAutostart(!autostart)}
            >
              {autostart ? "取消" : "开启"}
            </button>
          </div>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">掉线提醒</div>
              <div className="server-meta hint">掉线超过 3 分钟或恢复时弹通知。</div>
            </div>
            <span className="row-actions">
              {notify && (
                <button
                  className="link-btn"
                  onClick={() => {
                    void testNotification().catch((e) => setError(String(e)));
                  }}
                >
                  测试通知
                </button>
              )}
              <button
                className={notify ? "btn btn-ghost btn-sm" : "btn btn-primary btn-sm"}
                onClick={() => void toggleNotify(!notify)}
              >
                {notify ? "取消" : "开启"}
              </button>
            </span>
          </div>
        </article>

        <article className="card" id="settings-refresh">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">
                刷新频率
                <InfoTip text="只影响界面多久看一次状态，不影响后台看护。切到别的窗口会暂停刷新，回来立刻刷新。" />
              </div>
              <div className="server-meta hint">多久看一次状态，不影响后台看护。</div>
            </div>
            <span className="row-actions">
              {savedNote("refresh")}
              <select
                className="select"
                aria-label="刷新频率"
                value={draft.refreshIntervalMs}
                disabled={untrusted || saving === "refresh"}
                title={untrusted ? UNTRUSTED_HINT : undefined}
                onChange={(e) =>
                  void save(
                    { ...draft, refreshIntervalMs: Number(e.target.value) },
                    "refresh",
                  )
                }
              >
                {intervalOptions.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </span>
          </div>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">启动时查更新</div>
              <div className="server-meta hint">启动 30 秒后看有没有新版本。</div>
            </div>
            <span className="row-actions">
              {savedNote("updateCheck")}
              <label
                className="switch"
                title={untrusted ? UNTRUSTED_HINT : undefined}
              >
                <input
                  type="checkbox"
                  checked={draft.checkUpdateOnStart}
                  disabled={untrusted || saving === "updateCheck"}
                  onChange={(e) =>
                    void save({ ...draft, checkUpdateOnStart: e.target.checked }, "updateCheck")
                  }
                />
                {draft.checkUpdateOnStart ? "已打开" : "已关闭"}
              </label>
            </span>
          </div>
        </article>

        <article className="card" id="settings-quiet">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">免打扰时段</div>
              <div className="server-meta hint" title="这段时间里通知照记，但不弹系统提醒；回到应用里还能看到。">
                这段时间不弹系统提醒，通知照记。
              </div>
              <p className="settings-note hint">
                {draft.quietHours.enabled ? "到点自动安静，次日恢复。" : "现在全天都能弹通知。"}
                {quietSame ? "开始和结束一样，等于没设。" : ""}
              </p>
            </div>
            <span className="row-actions">
              {savedNote("quiet")}
              <label className="switch" title={untrusted ? UNTRUSTED_HINT : undefined}>
                <input
                  type="checkbox"
                  checked={draft.quietHours.enabled}
                  disabled={untrusted || saving === "quiet"}
                  onChange={(e) =>
                    void save(
                      {
                        ...draft,
                        quietHours: { ...draft.quietHours, enabled: e.target.checked },
                      },
                      "quiet",
                    )
                  }
                />
                开启
              </label>
              <span className="time-range">
                <input
                  className="input input-time"
                  type="time"
                  aria-label="免打扰开始时间"
                  value={draft.quietHours.start}
                  disabled={untrusted || saving === "quiet"}
                  onChange={(e) =>
                    void save(
                      {
                        ...draft,
                        quietHours: { ...draft.quietHours, start: e.target.value },
                      },
                      "quiet",
                    )
                  }
                />
                <span>到</span>
                <input
                  className="input input-time"
                  type="time"
                  aria-label="免打扰结束时间"
                  value={draft.quietHours.end}
                  disabled={untrusted || saving === "quiet"}
                  onChange={(e) =>
                    void save(
                      {
                        ...draft,
                        quietHours: { ...draft.quietHours, end: e.target.value },
                      },
                      "quiet",
                    )
                  }
                />
              </span>
            </span>
          </div>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">
                数据目录 <span className="env-meta">（{dirKindWord}）</span>
              </div>
              <div className="server-meta mono hint">
                {dirInfo?.path || app.dataDir || "还不确定"}
              </div>
              <div className="server-meta hint" title="搬家时整个文件夹拷走就行">
                配置、日志、密钥都在这里。
              </div>
              {dirKind === "portable" && (
                <p className="settings-note hint">便携模式：数据在程序旁边。</p>
              )}
            </div>
            <button
              className="btn btn-ghost btn-sm"
              onClick={() => {
                void openDataDir().catch(() => toast.error("打不开数据目录，手动去文件夹里看看吧"));
              }}
            >
              打开
            </button>
          </div>
        </article>

        <article className="card" id="settings-trash">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">最近删除</div>
              <div className="server-meta hint">删除后的 10 分钟内可以在这里恢复。</div>
              {trashError ? <p className="settings-note hint">{trashError}</p> : null}
              {trash === null ? (
                <p className="settings-note hint">正在读…</p>
              ) : trash.length === 0 ? (
                <p className="settings-note hint">没有可撤销的删除</p>
              ) : (
                <>
                  <p className="settings-note hint">点「恢复」就能找回来。</p>
                  {trash.map((item) => {
                    const expired = item.expiresAt <= Date.now();
                    return (
                      <div
                        className="row-actions"
                        key={item.token}
                        style={{ marginTop: 6 }}
                      >
                        <span className="server-name" style={{ fontSize: 13 }}>
                          {TRASH_TYPE_WORD[item.type]}「{item.name}」
                        </span>
                        <span className="settings-note hint" style={{ margin: 0 }}>
                          {expired ? "已过期" : countdownText(item.expiresAt)}
                        </span>
                        <button
                          className="link-btn"
                          disabled={trashBusy || expired}
                          onClick={() => void restoreTrash(item)}
                        >
                          恢复
                        </button>
                      </div>
                    );
                  })}
                </>
              )}
            </div>
            {trash && trash.length > 1 ? (
              <button
                className="btn btn-ghost btn-sm"
                disabled={trashBusy}
                onClick={() => void restoreAllTrash()}
              >
                全部恢复
              </button>
            ) : null}
          </div>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">体检</div>
              <div
                className="server-meta hint"
                title="检查应用、网络、服务器、隧道、证书，哪里不对告诉你怎么办。"
              >
                一键检查，哪里不对告诉你怎么办。
              </div>
            </div>
            <button className="btn btn-ghost btn-sm" onClick={() => setDoctorOpen(true)}>
              开始体检
            </button>
          </div>
        </article>

        <AboutCard
          onOpenDoctor={() => setDoctorOpen(true)}
          onOpenDiagnostics={() => setDiagOpen(true)}
        />

        {error && <div className="error-line settings-error">{error}</div>}
      </div>

      {doctorOpen && <DoctorModal onClose={() => setDoctorOpen(false)} />}
      {diagOpen && <DiagExportModal onClose={() => setDiagOpen(false)} />}
    </div>
  );
}
