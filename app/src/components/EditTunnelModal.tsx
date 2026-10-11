// 编辑隧道弹层：只改发布相关的东西——名称 / 公网网址 / 云端端口。
// 访问保护（网页密码、IP 白名单）请到「访问保护」页改；
// 这里保存时把当前的 access / accessUser / accessDetail 原样回传，保证不误动保护。
// 规则：云端端口改了会自动断开重连一次。

import { useRef, useState } from "react";

import { updateTunnel, type TunnelView } from "../api";
import { useModalA11y } from "./useModalA11y";

interface Props {
  tunnel: TunnelView;
  onClose: () => void;
  onSaved: () => void;
}

export function EditTunnelModal({ tunnel, onClose, onSaved }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const [name, setName] = useState(tunnel.name);
  const [remotePort, setRemotePort] = useState(String(tunnel.remotePort));
  const [url, setUrl] = useState(tunnel.url ?? "");
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

    setBusy(true);
    try {
      await updateTunnel({
        id: tunnel.id,
        name: name.trim(),
        remotePort: rp,
        url: url.trim() || null,
        // 访问保护原样带回（不传 accessHash = 密码保持不变）：
        // 这个弹层只负责发布相关字段，避免改名称/网址/端口时误改保护。
        access: tunnel.access ?? [],
        accessUser: tunnel.accessUser ?? null,
        accessDetail: tunnel.accessDetail ?? null,
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
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef} style={{ width: 560 }}>
        <div className="modal-head">
          <h2 className="modal-title">编辑隧道 · {tunnel.name}</h2>
          <p className="modal-sub">
            本机服务：{tunnel.serviceName}（本机端口 {tunnel.localPort}）；
            改端口保存后会断开重连一次。访问保护（网页密码 / IP 白名单）在「访问保护」页改。
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
