// 云端服务器页：列表 + 添加/编辑/删除。

import { useEffect, useRef, useState, type ReactNode } from "react";

import type { ShellIntent } from "../App";
import { ConfirmModal } from "../components/ConfirmModal";
import { useModalA11y } from "../components/useModalA11y";
import {
  setupPasswordless,
  type CloudEnv,
  type ServerInput,
  type ServerView,
} from "../api";
import { ENV_PANEL_WORD, ENV_WEB_WORD } from "../copy/status";
import { actions } from "../store/actions";
import { useNotice } from "../store/noticeStore";
import { getState } from "../store/snapshotStore";
import { toast } from "../store/toastStore";
import { useStore } from "../store/useStore";

interface Props {
  servers: ServerView[];
  onChanged: () => void;
  /** 外壳下发的意图：通知定位服务器 / 从发布向导来添加服务器 */
  intent?: ShellIntent | null;
  /** 意图处理完通知外壳清掉 */
  onIntentDone?: () => void;
}

/** 编辑弹层里的表单：比 ServerInput 多一个「站点目录」（可空 = 没填）。 */
type ServerForm = ServerInput & { vhostDir?: string | null };

/** 提交给后端的服务器输入：站点目录空着就送 null，按「没填」处理。 */
type ServerFormInput = ServerInput & { vhostDir: string | null };

/** 列表数据可能已经带上后端新加的 vhostDir；老数据没有就当成没填。 */
type ServerViewWithDir = ServerView & { vhostDir?: string | null };

/** 通知定位 / 意图定位：滚到卡片并闪一下（不新增 CSS，用动画闪边框） */
function focusCard(elementId: string): boolean {
  const el = document.getElementById(elementId);
  if (!el) return false;
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
  return true;
}

// ---------- 环境识别（D3 §3 / §15） ----------

const UI_DISMISSED_KEY = "ui.dismissed";
/** 「我手动确认过了」的冷静期：7 天内不再显示这条横幅（D4 §3.5） */
const PANEL_DISMISS_TTL_MS = 7 * 24 * 60 * 60 * 1000;

const ENV_TEXT_APACHE =
  "这台服务器上跑的是 Apache，不是 nginx。这个应用生成的是 nginx 的配置片段，贴了不会生效。两个办法：① 在面板里新建站点时选 nginx（宝塔/小皮都支持）；② 先把片段留着，换成 nginx 再用。";
const ENV_TEXT_BOTH =
  "这台机器上 nginx 和 Apache 都在。贴之前先确认你的站点是哪一个在管（面板里的网站列表一般会写 nginx 还是 Apache）。";
const ENV_TEXT_WEB_NONE =
  "没找到 nginx，也没有 Apache。可能网站服务没装，或者装在特殊路径。这种情况先别贴片段。";
const ENV_TEXT_UNKNOWN =
  "没认出来这台服务器用的什么面板和网站服务（nginx 还是 Apache 都没找到）。先别贴片段，怕贴错地方；可以在服务器上执行 nginx -v 或 httpd -v 看看，再点「重新识别」。";
const ENV_TEXT_PANEL_NONE =
  "这台服务器没发现面板，但装了 nginx。配置要自己改 nginx 的站点文件；下面给的片段还是能用的。";
const ENV_TEXT_PANEL_UNKNOWN =
  "没认出来这台服务器用的什么面板；不过网站服务是 nginx，配置片段能用，站点目录可能要手动确认。";

interface EnvNote {
  tone: "ok" | "warn" | "error";
  text: string;
  /** true = 要显示成醒目横幅；false = 只是一行说明 */
  banner: boolean;
}

function envNote(env: CloudEnv): EnvNote {
  if (env.web === "apache") return { tone: "error", banner: true, text: ENV_TEXT_APACHE };
  if (env.web === "both") return { tone: "warn", banner: true, text: ENV_TEXT_BOTH };
  if (env.web === "none") return { tone: "warn", banner: true, text: ENV_TEXT_WEB_NONE };
  if (env.web === "unknown") return { tone: "warn", banner: true, text: ENV_TEXT_UNKNOWN };
  if (env.panel === "unknown") return { tone: "warn", banner: true, text: ENV_TEXT_PANEL_UNKNOWN };
  if (env.panel === "none") return { tone: "warn", banner: true, text: ENV_TEXT_PANEL_NONE };
  if (env.panel === "bt") {
    return {
      tone: "ok",
      banner: false,
      text: `云服务器认出来了：宝塔面板 + nginx。识别到的站点目录：${env.vhostDir ?? "/www/server/panel/vhost"}。点「编辑」把它填进去，不对可以改。`,
    };
  }
  return {
    tone: "ok",
    banner: false,
    text: `云服务器认出来了：小皮面板 + nginx。识别到的站点目录：${env.vhostDir ?? "/xp/panel/vhost"}。点「编辑」把它填进去。`,
  };
}

function loadDismissed(): Record<string, number> {
  try {
    const raw = localStorage.getItem(UI_DISMISSED_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? (parsed as Record<string, number>) : {};
  } catch {
    return {};
  }
}

function writeDismissed(map: Record<string, number>): void {
  try {
    localStorage.setItem(UI_DISMISSED_KEY, JSON.stringify(map));
  } catch {
    // 存不了就算了，最多下次再提示一遍
  }
}

function formatDay(at?: number | null): string {
  if (typeof at !== "number" || at <= 0) return "还没记下时间";
  const d = new Date(at);
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const clock = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) return `今天 ${clock}`;
  return `${d.getMonth() + 1} 月 ${d.getDate()} 日 ${clock}`;
}

/** 识别把握（D3 §3 置信度）翻成人话 */
function confidenceWord(value?: string | null): string {
  if (value === "high") return "高";
  if (value === "medium") return "中";
  if (value === "low") return "低";
  return "不确定";
}

/** 给内嵌的编辑弹层套一层：挂 ref 并接上无障碍（Esc 关闭、Tab 焦点圈定、焦点归还）。 */
function ModalShell({
  onClose,
  children,
}: {
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useModalA11y(ref, onClose);
  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={ref}>
        {children}
      </div>
    </div>
  );
}

/**
 * 卡片上的统一失败提示：测试连接 / 识别环境走 actions 后，失败都挂到 `server:{id}`；
 * 这里就地显示，和隧道 / 服务卡片的 FixRibbon 同一个错误来源。
 */
function ServerErrorLine({ serverId }: { serverId: string }) {
  const notice = useNotice(`server:${serverId}`);
  return notice ? <div className="error-line">{notice}</div> : null;
}

export function ServersPage({ servers, onChanged, intent, onIntentDone }: Props) {
  // 配置读不出来时后端不允许写：保存按钮禁用并说明原因
  const configTrusted = useStore((s) => s.app.configTrusted);
  const [editing, setEditing] = useState<ServerForm | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  // 「测试连接」的结果：按服务器 id 分别记，互不干扰（失败统一走 ServerErrorLine）
  const [test, setTest] = useState<
    Record<string, { phase: "loading" | "ok"; text: string }>
  >({});
  // 用密码配免密
  const [pwOpen, setPwOpen] = useState(false);
  const [password, setPassword] = useState("");
  const [pwBusy, setPwBusy] = useState(false);
  const [pwError, setPwError] = useState("");
  const [pwDone, setPwDone] = useState("");
  const [confirmDel, setConfirmDel] = useState<ServerView | null>(null);
  const [envBusy, setEnvBusy] = useState<Record<string, boolean>>({});
  const [dismissed, setDismissed] = useState<Record<string, number>>(() => loadDismissed());
  /** 从「发布向导」死胡同跳过来加的：保存后要回去重开向导并预选新服务器 */
  const [returnToWizard, setReturnToWizard] = useState(false);

  const resetPw = () => {
    setPwOpen(false);
    setPassword("");
    setPwBusy(false);
    setPwError("");
    setPwDone("");
  };

  /** 关掉编辑弹层：清表单、清错误，也把「用密码配免密」的状态归零。 */
  const closeEdit = () => {
    setEditing(null);
    setError("");
    setReturnToWizard(false);
    resetPw();
  };

  /** 表单整理成可保存的内容：名字留空就用地址当名字；地址/账号没填返回 null。 */
  const cleanInput = (): ServerFormInput | null => {
    if (!editing) return null;
    const host = editing.host.trim();
    const user = editing.user.trim();
    if (!host || !user) return null;
    return {
      ...editing,
      name: editing.name.trim() || host,
      host,
      user,
      keyPath: editing.keyPath?.trim() ? editing.keyPath.trim() : null,
      // 站点目录空着就送 null：后端按「没填」处理（默认小皮面板目录）
      vhostDir: editing.vhostDir?.trim() ? editing.vhostDir.trim() : null,
    };
  };

  const runPasswordless = async () => {
    if (!editing || !password) return;
    setPwBusy(true);
    setPwError("");
    setPwDone("");
    try {
      let id = editing.id;
      if (!id) {
        // 还没保存过：先把现在填的存下来，拿到 id 才能配免密
        const input = cleanInput();
        if (!input) {
          setPwError("先填服务器地址和登录账号。");
          return;
        }
        // 写操作统一走 actions（写命令的警示 / 失败提示都在这条链上）
        const savedId = await actions.serverSave(input);
        if (!savedId) {
          setPwError("没保存成功：等一下再试");
          return;
        }
        id = savedId;
        setEditing((cur) => (cur ? { ...cur, ...input, id: savedId } : cur));
        onChanged();
      }
      const msg = await setupPasswordless(id, password);
      setPassword(""); // 密码用完就不留着了
      setPwDone(msg);
      setPwOpen(false);
      onChanged();
    } catch (e) {
      setPwError(String(e));
    } finally {
      setPwBusy(false);
    }
  };

  /** 打开「添加服务器」弹层；backToWizard = 从发布向导死胡同跳过来的 */
  const openNew = (backToWizard = false) => {
    setReturnToWizard(backToWizard);
    resetPw();
    setEditing({
      id: null,
      name: "",
      host: "",
      sshPort: 22,
      user: "",
      keyPath: "",
      vhostDir: "",
    });
  };

  const openEdit = (s: ServerView) => {
    resetPw();
    setEditing({
      id: s.id,
      name: s.name,
      host: s.host,
      sshPort: s.sshPort,
      user: s.user,
      keyPath: s.keyPath ?? "",
      vhostDir: (s as ServerViewWithDir).vhostDir ?? "",
    });
  };

  // 外壳意图：通知定位服务器 / 从发布向导来打开添加弹层
  const handledIntent = useRef<ShellIntent | null>(null);
  useEffect(() => {
    if (!intent || handledIntent.current === intent) return;
    handledIntent.current = intent;
    if (intent.kind === "locate" && intent.entity.type === "server") {
      const id = intent.entity.id;
      if (!getState().byServerId[id]) {
        toast.info("这台服务器已经不在了，可能刚被删掉");
      } else {
        focusCard(`server-card-${id}`);
      }
    } else if (intent.kind === "open-new-server") {
      openNew(intent.returnToWizard === true);
    }
    onIntentDone?.();
  }, [intent, onIntentDone]);

  const save = async () => {
    if (!editing) return;
    const input = cleanInput();
    if (!input) {
      setError("先填服务器地址和登录账号。");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const wasNew = !editing.id;
      const id = await actions.serverSave(input);
      if (!id) {
        setError("没保存成功：等一下再试");
        return;
      }
      const backToWizard = returnToWizard;
      setEditing(null);
      setReturnToWizard(false);
      onChanged();
      if (wasNew && backToWizard && id) {
        // 向导死胡同回来：加完服务器就回「发布隧道」重开向导，并预选刚加的服务器
        window.dispatchEvent(
          new CustomEvent("shangxian:nav-intent", {
            detail: { page: "tunnels", openWizard: true, presetServerId: id },
          }),
        );
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const doDelete = (s: ServerView) => {
    void actions.serverRemove(s.id);
  };

  /** 点「识别一下 / 重新识别」：走 actions（pending + 统一失败提示），成功后刷新快照。 */
  const scanEnv = async (s: ServerView) => {
    setEnvBusy((m) => ({ ...m, [s.id]: true }));
    try {
      const result = await actions.serverScanEnv(s.id);
      if (result) onChanged();
    } finally {
      setEnvBusy((m) => ({ ...m, [s.id]: false }));
    }
  };

  /** 「我手动确认过了」：7 天内不再显示这台服务器的环境横幅。 */
  const dismissEnvBanner = (id: string) => {
    const next = { ...dismissed, [`panel:${id}`]: Date.now() };
    setDismissed(next);
    writeDismissed(next);
  };

  /** 点「测试连接」：走 actions（pending + 统一失败提示）；成功的结果留在卡片上 */
  const testConn = async (s: ServerView) => {
    setTest((m) => ({
      ...m,
      [s.id]: { phase: "loading", text: "正在测试连接…" },
    }));
    try {
      const msg = await actions.serverTest(s.id);
      setTest((m) => {
        const next = { ...m };
        if (typeof msg === "string") {
          next[s.id] = {
            phase: "ok",
            text: msg.trim() ? msg : "连接成功",
          };
        } else {
          delete next[s.id];
        }
        return next;
      });
    } catch {
      // 失败提示已经由 actions 挂到 `server:{id}`（ServerErrorLine 显示）
      setTest((m) => {
        const next = { ...m };
        delete next[s.id];
        return next;
      });
    }
  };

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">云端服务器</h1>
          <p className="page-sub">
            一台一直开着的电脑，负责把别人的访问转给你的电脑。
          </p>
        </div>
        <button className="btn btn-primary" onClick={() => openNew()}>
          + 添加服务器
        </button>
      </header>

      {servers.length === 0 ? (
        <div className="empty">
          <p className="empty-title">还没有云端服务器</p>
          <p className="empty-text">
            先添加一台你正在用的云服务器，才能发布隧道。
          </p>
          <button className="btn btn-primary" onClick={() => openNew()}>
            + 添加第一台
          </button>
        </div>
      ) : (
        <div className="service-list">
          {servers.map((s) => {
            const env = s.cloudEnv ?? null;
            const note = env ? envNote(env) : null;
            const panelDismissed =
              typeof dismissed[`panel:${s.id}`] === "number" &&
              Date.now() - dismissed[`panel:${s.id}`] < PANEL_DISMISS_TTL_MS;
            const busy = envBusy[s.id] === true;
            return (
              <article className="card" key={s.id} id={`server-card-${s.id}`}>
                <div className="server-card">
                  <div className="server-main">
                    <div className="server-name">{s.name}</div>
                    <div className="server-meta">
                      {s.user}@{s.host}
                      {s.sshPort !== 22 ? `:${s.sshPort}` : ""}
                      {s.keyPath ? ` · 密钥 ${s.keyPath}` : " · 默认密钥"}
                    </div>
                    <div className="server-meta">
                      站点目录：{s.vhostDir || "还没填（留空就按默认找）"}
                    </div>
                  </div>
                  <button
                    className="btn btn-ghost btn-sm"
                    disabled={busy}
                    onClick={() => void scanEnv(s)}
                  >
                    {busy ? "识别中…" : env ? "重新识别" : "识别一下"}
                  </button>
                  <button
                    className="btn btn-ghost btn-sm"
                    disabled={test[s.id]?.phase === "loading"}
                    onClick={() => void testConn(s)}
                  >
                    {test[s.id]?.phase === "loading" ? "测试中…" : "测试连接"}
                  </button>
                  <button className="btn btn-ghost btn-sm" onClick={() => openEdit(s)}>
                    编辑
                  </button>
                  <button className="btn btn-ghost btn-sm" onClick={() => setConfirmDel(s)}>
                    删除
                  </button>
                </div>

                {env && note ? (
                  <>
                    <p className={`env-line ${note.tone}`}>
                      环境：{ENV_PANEL_WORD[env.panel]} + {ENV_WEB_WORD[env.web]}
                      <span className="env-meta">
                        {" "}
                        · 识别把握：面板 {confidenceWord(env.panelConfidence)} / 网站服务{" "}
                        {confidenceWord(env.webConfidence)} · 上次识别：{formatDay(env.detectedAt)}
                      </span>
                    </p>
                    {note.banner && !panelDismissed ? (
                      <div className={`env-banner ${note.tone}`}>
                        <p>{note.text}</p>
                        <div className="env-banner-actions">
                          <button className="link-btn" disabled={busy} onClick={() => void scanEnv(s)}>
                            重新识别
                          </button>
                          <button className="link-btn" onClick={() => dismissEnvBanner(s.id)}>
                            我手动确认过了
                          </button>
                        </div>
                      </div>
                    ) : note.tone === "ok" ? (
                      <p className="env-meta">{note.text}</p>
                    ) : (
                      <p className="env-meta">这条提示已经确认过，7 天内不再提醒。</p>
                    )}
                    {env.vhostDir && !s.vhostDir && (
                      <p className="env-meta">
                        自动识别的站点目录：{env.vhostDir}（点「编辑」填进去，检查云端和密码文件都按它找）
                      </p>
                    )}
                    {env.vhostDir && env.vhostDirExists === false && (
                      <p className="env-meta">
                        识别到的站点目录在服务器上不存在（{env.vhostDir}），贴配置前先确认面板里的站点目录。
                      </p>
                    )}
                    {env.webEvidence && (
                      <p className="env-meta">识别依据：{env.webEvidence}</p>
                    )}
                  </>
                ) : (
                  <p className="env-meta">还没识别这台服务器的环境，点「识别一下」看看面板和网站服务。</p>
                )}
                <ServerErrorLine serverId={s.id} />

                {test[s.id]?.phase === "ok" && (
                  <p className="field-hint">✓ {test[s.id].text}</p>
                )}
              </article>
            );
          })}
        </div>
      )}
      {error && !editing && <div className="error-line">{error}</div>}

      {confirmDel && (
        <ConfirmModal
          title={`删除服务器「${confirmDel.name}」？`}
          body="删掉后引用它的隧道会连不上；这台服务器上的东西不会被删。5 秒内可以撤销。"
          confirmLabel="删除"
          danger
          onClose={() => setConfirmDel(null)}
          onConfirm={() => {
            const target = confirmDel;
            setConfirmDel(null);
            void doDelete(target);
          }}
        />
      )}

      {editing && (
        <ModalShell onClose={closeEdit}>
          <div className="modal-head">
            <h2 className="modal-title">
              {editing.id ? "编辑服务器" : "添加服务器"}
            </h2>
            <p className="modal-sub">
              密钥留空就用系统默认密钥（~/.ssh 里的那把）。
            </p>
          </div>
          <div className="modal-body">
            <div className="field">
              <label className="field-label">名字</label>
              <input
                className="input"
                value={editing.name}
                placeholder="例如 香港-1"
                onChange={(e) =>
                  setEditing({ ...editing, name: e.target.value })
                }
              />
            </div>
            <div className="field">
              <label className="field-label">服务器地址</label>
              <input
                className="input mono"
                value={editing.host}
                placeholder="例如 api.example.com 或 1.2.3.4"
                onChange={(e) =>
                  setEditing({ ...editing, host: e.target.value })
                }
              />
            </div>
            <div className="field" style={{ display: "flex", gap: 12 }}>
              <div style={{ flex: 1 }}>
                <label className="field-label">登录账号</label>
                <input
                  className="input mono"
                  value={editing.user}
                  onChange={(e) =>
                    setEditing({ ...editing, user: e.target.value })
                  }
                />
              </div>
              <div style={{ width: 120 }}>
                <label className="field-label">SSH 端口</label>
                <input
                  className="input mono"
                  type="number"
                  value={editing.sshPort}
                  onChange={(e) =>
                    setEditing({
                      ...editing,
                      sshPort: parseInt(e.target.value, 10) || 22,
                    })
                  }
                />
              </div>
            </div>
            <div className="field">
              <label className="field-label">密钥文件（可留空）</label>
              <input
                className="input mono"
                value={editing.keyPath ?? ""}
                placeholder="例如 C:\Users\你\.ssh\id_ed25519"
                onChange={(e) =>
                  setEditing({ ...editing, keyPath: e.target.value })
                }
              />
            </div>
            <div className="field">
              <label className="field-label">站点目录（可留空）</label>
              <input
                className="input mono"
                value={editing.vhostDir ?? ""}
                placeholder="例如 /www/server/panel/vhost"
                onChange={(e) =>
                  setEditing({ ...editing, vhostDir: e.target.value })
                }
              />
              <p className="field-hint">
                不知道就留空，按小皮面板默认 /xp/panel/vhost 找。宝塔等面板填
                /www/server/panel/vhost；密码文件和「检查云端」都按这个目录找。
              </p>
            </div>

            {/* 没有密钥也能用：填一次密码，助手帮你配好免密 */}
            <div className="field">
              <label className="field-label">没有密钥？</label>
              {pwDone ? (
                <p className="field-hint">✓ {pwDone}</p>
              ) : pwOpen ? (
                <div style={{ display: "flex", gap: 8 }}>
                  <input
                    className="input mono"
                    type="password"
                    autoFocus
                    value={password}
                    placeholder="这台服务器的登录密码"
                    onChange={(e) => setPassword(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") void runPasswordless();
                    }}
                  />
                  <button
                    className="btn btn-primary btn-sm"
                    disabled={pwBusy || !password}
                    onClick={() => void runPasswordless()}
                  >
                    {pwBusy ? "配置中…" : "开始配置"}
                  </button>
                </div>
              ) : (
                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() => setPwOpen(true)}
                  >
                    用密码登录一次，帮我配好免密
                  </button>
                  <span className="field-hint" style={{ margin: 0 }}>
                    {editing.id
                      ? "密码只在内存里用一次，不会存下来"
                      : "会先把这台服务器保存下来；密码只在内存里用一次，不会存下来"}
                  </span>
                </div>
              )}
              {pwError && <div className="error-line">{pwError}</div>}
            </div>
            {error && <div className="error-line">{error}</div>}
          </div>
          <div className="modal-foot">
            <button className="btn btn-ghost" onClick={closeEdit}>
              取消
            </button>
            <span className="grow" />
            <button
              className="btn btn-primary"
              disabled={busy || !configTrusted}
              title={!configTrusted ? "配置文件读不出来，先修好再改设置" : undefined}
              onClick={save}
            >
              {busy ? "保存中…" : "保存"}
            </button>
          </div>
        </ModalShell>
      )}
    </div>
  );
}
