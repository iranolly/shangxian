// 编辑隧道弹层：改名 / 改云端端口 / 改网址 / 改访问保护。
// 规则：密码留空 = 不换密码；云端端口改了会自动断开重连一次。

import { useState } from "react";

import { hashPassword, updateTunnel, type TunnelView } from "../api";

interface Props {
  tunnel: TunnelView;
  onClose: () => void;
  onSaved: () => void;
}

export function EditTunnelModal({ tunnel, onClose, onSaved }: Props) {
  const [name, setName] = useState(tunnel.name);
  const [remotePort, setRemotePort] = useState(String(tunnel.remotePort));
  const [url, setUrl] = useState(tunnel.url ?? "");
  const [wantPassword, setWantPassword] = useState(
    (tunnel.access ?? []).includes("password"),
  );
  const [wantIp, setWantIp] = useState((tunnel.access ?? []).includes("ip"));
  const [accessUser, setAccessUser] = useState(
    tunnel.accessUser ?? "shangxian",
  );
  const [password, setPassword] = useState("");
  const [ips, setIps] = useState(tunnel.accessDetail ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const portChanged = Number(remotePort) !== tunnel.remotePort;

  const save = async () => {
    setError("");
    if (!name.trim()) {
      setError("隧道名字不能为空");
      return;
    }
    const rp = Number(remotePort);
    if (!Number.isInteger(rp) || rp < 1 || rp > 65535) {
      setError("云端端口要填 1~65535 的数字");
      return;
    }
    if (wantPassword && !tunnel.hasPassword && !password.trim()) {
      setError("这条隧道勾了「要密码才能进」，请先设置访问密码");
      return;
    }
    if (wantIp && !ips.trim()) {
      setError("勾了「只允许指定 IP」的话，要填至少一个 IP");
      return;
    }

    setBusy(true);
    try {
      const access: string[] = [];
      if (wantPassword) access.push("password");
      if (wantIp) access.push("ip");
      let hash: string | null = null;
      if (wantPassword && password.trim()) {
        hash = await hashPassword(tunnel.serverId, password.trim());
      }
      await updateTunnel({
        id: tunnel.id,
        name: name.trim(),
        remotePort: rp,
        url: url.trim() || null,
        access,
        accessUser: wantPassword ? accessUser.trim() || "shangxian" : null,
        accessHash: hash,
        accessDetail: wantIp ? ips.trim() || null : null,
      });
      onSaved();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" style={{ width: 560 }}>
        <div className="modal-head">
          <h2 className="modal-title">编辑隧道 · {tunnel.name}</h2>
          <p className="modal-sub">
            本机服务：{tunnel.serviceName}（本机端口 {tunnel.localPort}）；
            改端口保存后会断开重连一次。
          </p>
        </div>
        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}

          <div className="field">
            <label className="field-label">隧道名字</label>
            <input
              className="input"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </div>

          <div className="field">
            <label className="field-label">公网网址（可空）</label>
            <input
              className="input"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="例：album.example.com"
            />
          </div>

          <div className="field">
            <label className="field-label">云端端口</label>
            <input
              className="input mono"
              style={{ maxWidth: 180 }}
              value={remotePort}
              onChange={(e) => setRemotePort(e.target.value)}
              inputMode="numeric"
            />
          </div>
          {portChanged && (
            <div className="warn-line">端口改了：保存后会断开重连一次。</div>
          )}

          <div className="field">
            <label className="field-label">访问保护</label>
            <div className="option-list">
              <div
                className={`option ${wantPassword ? "selected" : ""}`}
                onClick={() => setWantPassword(!wantPassword)}
              >
                <input type="checkbox" checked={wantPassword} readOnly />
                <div className="option-text">
                  要密码才能进（推荐）
                  {tunnel.hasPassword && (
                    <span className="option-hint">已设过密码，不填就不换</span>
                  )}
                </div>
              </div>
              <div
                className={`option ${wantIp ? "selected" : ""}`}
                onClick={() => setWantIp(!wantIp)}
              >
                <input type="checkbox" checked={wantIp} readOnly />
                <div className="option-text">只允许指定 IP 访问</div>
              </div>
            </div>
            <p className="field-hint">
              两种保护可以同时勾选；同时开时，两个条件都要满足才能进。
            </p>
            {!wantPassword && !wantIp && (
              <div className="danger-line">
                ⚠ 两种保护都没开：任何人知道网址就能访问，不要放私人内容
              </div>
            )}
          </div>

          {wantPassword && (
            <>
              <div className="field">
                <label className="field-label">访问用户名</label>
                <input
                  className="input"
                  value={accessUser}
                  onChange={(e) => setAccessUser(e.target.value)}
                />
              </div>
              <div className="field">
                <label className="field-label">
                  新密码{tunnel.hasPassword ? "（留空 = 不换）" : ""}
                </label>
                <input
                  className="input"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder={tunnel.hasPassword ? "已设过密码" : "必须填"}
                />
                <p className="field-hint">
                  密码不会存在本地：保存时立刻在服务器上算好，只存加密结果。
                </p>
              </div>
            </>
          )}

          {wantIp && (
            <div className="field">
              <label className="field-label">允许访问的 IP（逗号分隔）</label>
              <textarea
                className="input"
                rows={3}
                value={ips}
                onChange={(e) => setIps(e.target.value)}
                placeholder="例：1.2.3.4, 5.6.7.8"
              />
            </div>
          )}
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            取消
          </button>
          <span className="grow" />
          <button
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void save()}
          >
            {busy ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
