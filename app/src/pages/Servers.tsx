// 云端服务器页：列表 + 添加/编辑/删除。

import { useEffect, useRef, useState, type ReactNode } from "react";

import type { ShellIntent } from "../App";
import { ConfirmModal } from "../components/ConfirmModal";
import { useModalA11y } from "../components/useModalA11y";
import {
  setupPasswordless,
  type ServerInput,
  type ServerView,
} from "../api";
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
 * 卡片上的统一失败提示：测试连接走 actions 后，失败都挂到 `server:{id}`；
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
          <p className="page-sub">一直开着的电脑，帮你把访问转给本机。</p>
        </div>
        <button
          className="btn btn-primary"
          title="添加一台云端服务器"
          onClick={() => openNew()}
        >
          + 添加
        </button>
      </header>

      {servers.length === 0 ? (
        <div className="empty">
          <p className="empty-title">还没有云端服务器</p>
          <p className="empty-text">先添加一台云服务器，才能发布隧道。</p>
          <button
            className="btn btn-primary"
            title="添加一台云端服务器"
            onClick={() => openNew()}
          >
            + 添加
          </button>
        </div>
      ) : (
        <div className="service-list">
          {servers.map((s) => {
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
                      站点目录：{s.vhostDir || "还没填（按默认找）"}
                    </div>
                  </div>
                  <button
                    className="btn btn-ghost btn-sm"
                    disabled={test[s.id]?.phase === "loading"}
                    title="测一下 SSH 能不能连上这台服务器"
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

                <ServerErrorLine serverId={s.id} />

                {test[s.id]?.phase === "ok" && (
                  <p className="hint">✓ {test[s.id].text}</p>
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
          body="引用它的隧道会连不上；服务器上的文件不删，5 秒内可撤销。"
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
            <p className="modal-sub" title="默认用 ~/.ssh 里的密钥">
              密钥留空就用系统默认密钥。
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
                title="留空就按默认目录找，例如 /www/server/panel/vhost。密码文件和「检查云端」也按这个目录找。"
                onChange={(e) =>
                  setEditing({ ...editing, vhostDir: e.target.value })
                }
              />
              <p className="hint">留空就按默认目录找。</p>
            </div>

            {/* 没有密钥也能用：填一次密码，助手帮你配好免密 */}
            <div className="field">
              <label className="field-label">没有密钥？</label>
              {pwDone ? (
                <p className="hint">✓ {pwDone}</p>
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
                    title={
                      editing.id
                        ? "填一次登录密码，助手帮你配好免密；密码不保存"
                        : "会先保存这台服务器；填一次登录密码，助手帮你配好免密，密码不保存"
                    }
                    onClick={() => setPwOpen(true)}
                  >
                    用密码配免密
                  </button>
                  <span className="hint" style={{ margin: 0 }}>
                    密码只用一次，不保存
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
