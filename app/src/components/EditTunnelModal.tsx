// 编辑隧道弹层：只改发布相关的东西——名称 / 公网网址 / 云端端口。
// 访问保护（网页密码、IP 白名单）请到「访问保护」页改；
// 这里保存时把当前的 access / accessUser / accessDetail 原样回传，保证不误动保护。
// 规则：云端端口改了会自动断开重连一次；端口 / 网址改了要重新贴一次云端配置。
//
// E3 改造：保存后如果改过端口或网址，明确提示「需要重新贴云端配置」，并能直接去贴。

import { useRef, useState } from "react";

import type { TunnelView } from "../api";
import { actions } from "../store/actions";
import { useStore } from "../store/useStore";
import { useModalA11y } from "./useModalA11y";

interface Props {
  tunnel: TunnelView;
  onClose(): void;
  onSaved(): void;
  /** 「下一步：贴云端配置」：页面用它打开这条隧道的 NginxModal */
  onNeedNginx?: (tunnel: TunnelView) => void;
}

export function EditTunnelModal({ tunnel, onClose, onSaved, onNeedNginx }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const configTrusted = useStore((s) => s.app.configTrusted);
  const [name, setName] = useState(tunnel.name);
  const [remotePort, setRemotePort] = useState(String(tunnel.remotePort));
  const [url, setUrl] = useState(tunnel.url ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  /** 保存完成后是否要提醒重新贴云端配置 */
  const [needPaste, setNeedPaste] = useState(false);

  const portChanged = Number(remotePort) !== tunnel.remotePort;
  const urlChanged = (url.trim() || null) !== (tunnel.url ?? null);

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
      const ok = await actions.tunnelSave({
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
      if (!ok) {
        setError("没保存成功：看页面顶上的红色提示，改好再点保存");
        return;
      }
      window.dispatchEvent(new CustomEvent("shangxian:notif-refresh"));
      onSaved();
      if (portChanged || urlChanged) {
        setNeedPaste(true);
      } else {
        onClose();
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef} style={{ width: 560 }}>
        <div className="modal-head">
          <h2 className="modal-title">编辑隧道 · {tunnel.name}</h2>
          <p
            className="modal-sub"
            title="改端口保存后会断开重连一次，还要重新贴云端配置；访问保护（网页密码 / IP 白名单）在「访问保护」页改。"
          >
            本机服务：{tunnel.serviceName}（本机端口 {tunnel.localPort}）。
          </p>
        </div>
        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}

          {needPaste ? (
            <div
              className="warn-line"
              title="服务器上贴过的 nginx 片段要跟着更新一次，不然外面还是按老配置走。"
            >
              保存好了：{portChanged && "端口改了，"}
              {urlChanged && "网址改了，"}云端配置要重贴一次。
            </div>
          ) : (
            <>
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
                <div className="warn-line" title="保存后会断开重连一次，还要重新贴一次云端配置。">
                  端口改了，会重连并要重贴配置。
                </div>
              )}
              {urlChanged && !portChanged && (
                <div className="warn-line" title="云端认了新网址才生效。">
                  网址改了，要重贴配置才认。
                </div>
              )}
            </>
          )}
        </div>
        <div className="modal-foot">
          {needPaste ? (
            <>
              <button className="btn btn-ghost" onClick={onClose}>
                稍后再说
              </button>
              <span className="grow" />
              <button
                className="btn btn-primary"
                title="把新的几行贴到服务器面板里并重载 nginx"
                onClick={() => {
                  onNeedNginx?.(tunnel);
                  onClose();
                }}
              >
                贴云端配置
              </button>
            </>
          ) : (
            <>
              <button className="btn btn-ghost" onClick={onClose}>
                取消
              </button>
              <span className="grow" />
              <button
                className="btn btn-primary"
                disabled={busy || !configTrusted}
                title={!configTrusted ? "配置文件读不出来，先修好再改设置" : undefined}
                onClick={() => void save()}
              >
                {busy ? "保存中…" : "保存"}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
