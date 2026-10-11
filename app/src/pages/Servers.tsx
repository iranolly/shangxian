// 云端服务器页：列表 + 添加/编辑/删除。

import { useRef, useState, type ReactNode } from "react";

import { useModalA11y } from "../components/useModalA11y";
import {
  deleteServer,
  saveServer,
  setupPasswordless,
  testServer,
  type ServerInput,
  type ServerView,
} from "../api";

interface Props {
  servers: ServerView[];
  onChanged: () => void;
}

/** 编辑弹层里的表单：比 ServerInput 多一个「站点目录」（可空 = 没填）。 */
type ServerForm = ServerInput & { vhostDir?: string | null };

/** 提交给后端的服务器输入：站点目录空着就送 null，按「没填」处理。 */
type ServerFormInput = ServerInput & { vhostDir: string | null };

/** 列表数据可能已经带上后端新加的 vhostDir；老数据没有就当成没填。 */
type ServerViewWithDir = ServerView & { vhostDir?: string | null };

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

export function ServersPage({ servers, onChanged }: Props) {
  const [editing, setEditing] = useState<ServerForm | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  // 「测试连接」的结果：按服务器 id 分别记，互不干扰
  const [test, setTest] = useState<
    Record<string, { phase: "loading" | "ok" | "fail"; text: string }>
  >({});
  // 用密码配免密
  const [pwOpen, setPwOpen] = useState(false);
  const [password, setPassword] = useState("");
  const [pwBusy, setPwBusy] = useState(false);
  const [pwError, setPwError] = useState("");
  const [pwDone, setPwDone] = useState("");

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
        id = await saveServer(input);
        const newId = id;
        setEditing((cur) => (cur ? { ...cur, ...input, id: newId } : cur));
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

  const openNew = () => {
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
      await saveServer(input);
      setEditing(null);
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const del = async (s: ServerView) => {
    if (!window.confirm(`删除服务器「${s.name}」？`)) return;
    try {
      await deleteServer(s.id);
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  /** 点「测试连接」：真去 SSH 连一次，成功/失败就显示在这张卡片上 */
  const testConn = async (s: ServerView) => {
    setTest((m) => ({
      ...m,
      [s.id]: { phase: "loading", text: "正在测试连接…" },
    }));
    try {
      const msg = await testServer(s.id);
      setTest((m) => ({
        ...m,
        [s.id]: {
          phase: "ok",
          text: typeof msg === "string" && msg.trim() ? msg : "连接成功",
        },
      }));
    } catch (e) {
      setTest((m) => ({ ...m, [s.id]: { phase: "fail", text: String(e) } }));
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
        <button className="btn btn-primary" onClick={openNew}>
          + 添加服务器
        </button>
      </header>

      {servers.length === 0 ? (
        <div className="empty">
          <p className="empty-title">还没有云端服务器</p>
          <p className="empty-text">
            先添加一台你正在用的云服务器，才能发布隧道。
          </p>
          <button className="btn btn-primary" onClick={openNew}>
            + 添加第一台
          </button>
        </div>
      ) : (
        <div className="service-list">
          {servers.map((s) => (
            <article className="card" key={s.id}>
              <div className="server-card">
                <div className="server-main">
                  <div className="server-name">{s.name}</div>
                  <div className="server-meta">
                    {s.user}@{s.host}
                    {s.sshPort !== 22 ? `:${s.sshPort}` : ""}
                    {s.keyPath ? ` · 密钥 ${s.keyPath}` : " · 默认密钥"}
                  </div>
                </div>
                <button
                  className="btn btn-ghost btn-sm"
                  disabled={test[s.id]?.phase === "loading"}
                  onClick={() => void testConn(s)}
                >
                  {test[s.id]?.phase === "loading" ? "测试中…" : "测试连接"}
                </button>
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() => openEdit(s)}
                >
                  编辑
                </button>
                <button
                  className="btn btn-ghost btn-sm"
                  onClick={() => del(s)}
                >
                  删除
                </button>
              </div>
              {test[s.id]?.phase === "ok" && (
                <p className="field-hint">✓ {test[s.id].text}</p>
              )}
              {test[s.id]?.phase === "fail" && (
                <div className="error-line">{test[s.id].text}</div>
              )}
            </article>
          ))}
        </div>
      )}
      {error && !editing && <div className="error-line">{error}</div>}

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
            <button className="btn btn-primary" disabled={busy} onClick={save}>
              {busy ? "保存中…" : "保存"}
            </button>
          </div>
        </ModalShell>
      )}
    </div>
  );
}
