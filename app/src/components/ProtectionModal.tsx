// 访问保护弹层（只出现在「访问保护」页）：专管门禁——网页访问密码 + IP 白名单。
// 规则：密码留空 = 保留原来的密码；保存成功后由页面接着打开「云端配置」作为下一步。
// 注意：「密码是否开启」看 tunnel.access 里有没有 "password"，不用 hasPassword。

import { useRef, useState } from "react";

import {
  hashPassword,
  updateTunnel,
  type TunnelView,
  type UpdateTunnelInput,
} from "../api";
import { accessFlags } from "./ProtectionStatus";
import { useModalA11y } from "./useModalA11y";

interface Props {
  tunnel: TunnelView;
  onClose: () => void;
  /** 保存成功后调用：页面用它直接打开这条隧道的「云端配置」弹层 */
  onSaved: () => void;
}

export function ProtectionModal({ tunnel, onClose, onSaved }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const flags = accessFlags(tunnel.access);
  const [wantPassword, setWantPassword] = useState(flags.password);
  const [wantIp, setWantIp] = useState(flags.ip);
  const [accessUser, setAccessUser] = useState(
    tunnel.accessUser ?? "shangxian",
  );
  const [password, setPassword] = useState("");
  const [ips, setIps] = useState(tunnel.accessDetail ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const save = async () => {
    setError("");
    // 只有这条隧道还没设过密码时才必须要新密码；已设过的留空就沿用。
    if (wantPassword && !password.trim() && !tunnel.hasPassword) {
      setError("这条隧道要密码才能进，但还没设过密码：请填一条新密码");
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
      const input: UpdateTunnelInput = {
        id: tunnel.id,
        // 名称 / 网址 / 云端端口原样带回：这里只改保护，别的不动。
        name: tunnel.name,
        remotePort: tunnel.remotePort,
        url: tunnel.url ?? null,
        access,
        accessUser: wantPassword ? accessUser.trim() || "shangxian" : null,
        accessDetail: wantIp ? ips.trim() || null : null,
      };
      // 没填新密码就不传 accessHash：后端保持原来的密码不变。
      if (wantPassword && password.trim()) {
        input.accessHash = await hashPassword(
          tunnel.serverId,
          password.trim(),
        );
      }
      await updateTunnel(input);
      onSaved();
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
          <h2 className="modal-title">访问保护 · {tunnel.name}</h2>
          <p className="modal-sub">
            {tunnel.url ?? "还没填公网网址"}；这里的密码是别人打开网页时输的，
            不是「云端服务器」页里连服务器的登录密码。
          </p>
        </div>
        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}

          <div className="field">
            <label className="field-label">访问保护</label>
            <div className="option-list">
              <label
                className={`option ${wantPassword ? "selected" : ""}`}
              >
                <input
                  type="checkbox"
                  checked={wantPassword}
                  onChange={(e) => setWantPassword(e.target.checked)}
                />
                <div className="option-text">
                  要密码才能进（推荐）
                  <span className="option-hint">
                    {tunnel.hasPassword
                      ? "已设过密码，不填新的就不换"
                      : "还没设密码，需要填一条新的"}
                  </span>
                </div>
              </label>
              <label className={`option ${wantIp ? "selected" : ""}`}>
                <input
                  type="checkbox"
                  checked={wantIp}
                  onChange={(e) => setWantIp(e.target.checked)}
                />
                <div className="option-text">
                  只允许指定 IP 访问
                  <span className="option-hint">
                    适合固定上网地点的场景
                  </span>
                </div>
              </label>
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
                  新密码
                  {tunnel.hasPassword ? "（留空 = 不换）" : ""}
                </label>
                <input
                  className="input"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder={tunnel.hasPassword ? "留空就不换密码" : "必须填"}
                />
                <p className="field-hint">
                  密码不会存在这台电脑上：保存时立刻在这台电脑上算好，只把加密结果写进服务器上的密码文件。
                  连服务器用的登录密码在「云端服务器」页，不要混用。
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

          <p className="field-hint">
            保存后会打开这条隧道的「云端配置」：把新的几行贴到服务器面板里并重载
            nginx，保护才算真的生效。想改名称、网址、云端端口，请到「发布隧道」页。
          </p>
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
