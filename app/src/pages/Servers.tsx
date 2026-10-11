// 云端服务器页：列表 + 添加/编辑/删除。

import { useState } from "react";

import {
  deleteServer,
  saveServer,
  setupPasswordless,
  type ServerInput,
  type ServerView,
} from "../api";

interface Props {
  servers: ServerView[];
  onChanged: () => void;
}

export function ServersPage({ servers, onChanged }: Props) {
  const [editing, setEditing] = useState<ServerInput | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  // 用密码配免密：只在这台服务器已经保存过（有 id）时能用
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

  const runPasswordless = async () => {
    if (!editing?.id || !password) return;
    setPwBusy(true);
    setPwError("");
    setPwDone("");
    try {
      const msg = await setupPasswordless(editing.id, password);
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
    });
  };

  const save = async () => {
    if (!editing) return;
    setBusy(true);
    setError("");
    try {
      await saveServer({ ...editing, keyPath: editing.keyPath || null });
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
            </article>
          ))}
        </div>
      )}
      {error && !editing && <div className="error-line">{error}</div>}

      {editing && (
        <div className="modal-backdrop">
          <div className="modal" role="dialog" aria-modal="true">
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

              {/* 没有密钥也能用：填一次密码，助手帮你配好免密 */}
              <div className="field">
                <label className="field-label">没有密钥？</label>
                {!editing.id ? (
                  <p className="field-hint">
                    先把这台服务器保存一下，就能用密码自动配好免密登录。
                  </p>
                ) : pwDone ? (
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
                      密码只在内存里用一次，不会存下来
                    </span>
                  </div>
                )}
                {pwError && <div className="error-line">{pwError}</div>}
              </div>
              {error && <div className="error-line">{error}</div>}
            </div>
            <div className="modal-foot">
              <button
                className="btn btn-ghost"
                onClick={() => {
                  setEditing(null);
                  setError("");
                  resetPw();
                }}
              >
                取消
              </button>
              <span className="grow" />
              <button
                className="btn btn-primary"
                disabled={busy}
                onClick={save}
              >
                {busy ? "保存中…" : "保存"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
