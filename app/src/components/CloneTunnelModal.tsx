// 克隆隧道弹层（D4 §3.3 CloneTunnelModal）：
// 名字默认「{源名} 的副本」，服务器/访问保护默认沿用源隧道；
// 公网网址默认空（原网址不能给两条隧道同时用）；云端端口默认挑一个空闲的。
//
// 与 D4 的一点实现差异：密码的加密结果拿不到（界面里只有 hasPassword 布尔值），
// 所以选了「要密码」时让用户给新隧道设一条新密码（默认随机预填），提交前在本机算哈希。

import { useEffect, useRef, useState } from "react";

import {
  hashPassword,
  type ServerView,
  type TunnelView,
} from "../api";
import { errorText } from "../store/commands";
import { getState } from "../store/snapshotStore";
import { actions, type CloneDraft } from "../store/actions";
import { useStore } from "../store/useStore";
import { useModalA11y } from "./useModalA11y";

interface Props {
  sourceId: string;
  onClose(): void;
  /** 克隆成功后 toast 里的「去编辑」 */
  onOpenEdit(id: string): void;
}

function generatePassword(): string {
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => chars[b % chars.length]).join("");
}

/** 在指定服务器上从 start 往上找一个没被别的隧道占用的云端端口 */
function suggestPort(serverId: string, start: number): number {
  const used = new Set(
    getState()
      .tunnels.filter((t) => t.serverId === serverId)
      .map((t) => t.remotePort),
  );
  let port = Math.max(1, Math.min(65_500, start));
  while (used.has(port) && port < 65_500) port += 1;
  return port;
}

export function CloneTunnelModal({ sourceId, onClose, onOpenEdit }: Props) {
  const source = useTunnelSource(sourceId);
  if (!source) return null;
  return <CloneForm source={source} onClose={onClose} onOpenEdit={onOpenEdit} />;
}

function useTunnelSource(id: string): TunnelView | undefined {
  const tunnels = useStore((s) => s.tunnels);
  return tunnels.find((t) => t.id === id);
}

function CloneForm({
  source,
  onClose,
  onOpenEdit,
}: {
  source: TunnelView;
  onClose(): void;
  onOpenEdit(id: string): void;
}) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const servers = useStore((s) => s.servers);

  const access = source.access ?? [];
  const [name, setName] = useState(`${source.name} 的副本`);
  const [serverId, setServerId] = useState(source.serverId);
  const [portText, setPortText] = useState(() =>
    String(suggestPort(source.serverId, source.remotePort + 1)),
  );
  const [url, setUrl] = useState("");
  const [wantPassword, setWantPassword] = useState(access.includes("password"));
  const [wantIp, setWantIp] = useState(access.includes("ip"));
  const [accessUser, setAccessUser] = useState(source.accessUser ?? "shangxian");
  const [password, setPassword] = useState(generatePassword);
  const [ips, setIps] = useState(source.accessDetail ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // 换服务器时重新挑端口（只依赖 serverId；隧道列表现取现用，避免每次快照重置）
  useEffect(() => {
    setPortText(String(suggestPort(serverId, source.remotePort + 1)));
  }, [serverId, source.remotePort]);

  const serverOptions: ServerView[] = servers;

  const submit = async () => {
    setError("");
    const trimmedName = name.trim();
    if (!trimmedName) {
      setError("隧道名称不能为空");
      return;
    }
    const remotePort = Number(portText);
    if (!Number.isInteger(remotePort) || remotePort < 1 || remotePort > 65535) {
      setError("云端端口需填写 1~65535 的数字");
      return;
    }
    if (wantPassword && !password.trim()) {
      setError("该隧道已启用密码保护，请先填写新密码");
      return;
    }
    if (wantIp && !ips.trim()) {
      setError("已勾选「只允许指定 IP」，请先填写允许访问的 IP");
      return;
    }

    setBusy(true);
    try {
      const accessHash = wantPassword
        ? (await hashPassword(serverId, password)).hash
        : null;
      const flags: string[] = [];
      if (wantPassword) flags.push("password");
      if (wantIp) flags.push("ip");
      const draft: CloneDraft = {
        name: trimmedName,
        serverId,
        remotePort,
        url: url.trim() || null,
        access: flags,
        accessUser: wantPassword ? accessUser.trim() || "shangxian" : null,
        accessHash,
        accessDetail: wantIp ? ips.trim() : null,
      };
      const id = await actions.tunnelClone(source.id, draft, onOpenEdit);
      if (id) onClose();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef} style={{ width: 560 }}>
        <div className="modal-head">
          <h2 className="modal-title">克隆隧道 · {source.name}</h2>
          <p
            className="modal-sub"
            title="复制一条设置相近的隧道；创建后为暂停状态，确认无误后再开始发布。"
          >
            按该隧道复制；创建完成后为暂停状态。
          </p>
        </div>
        <div className="modal-body">
          {error ? <div className="error-line">{error}</div> : null}

          <div className="field">
            <label className="field-label">隧道名称</label>
            <input className="input" value={name} onChange={(e) => setName(e.target.value)} />
          </div>

          <div className="field">
            <label className="field-label">云端服务器</label>
            <select
              className="select"
              value={serverId}
              onChange={(e) => setServerId(e.target.value)}
            >
              {serverOptions.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}（{s.host}）
                </option>
              ))}
            </select>
          </div>

          <div className="field">
            <label className="field-label">云端端口</label>
            <input
              className="input mono"
              style={{ maxWidth: 180 }}
              value={portText}
              inputMode="numeric"
              onChange={(e) => setPortText(e.target.value)}
            />
          </div>

          <div className="field">
            <label className="field-label">公网网址（可留空）</label>
            <input
              className="input"
              value={url}
              placeholder="例：album2.example.com"
              onChange={(e) => setUrl(e.target.value)}
            />
            <p className="warn-line">原网址不能同时用于两条隧道；如需填写请更换。
            </p>
          </div>

          <fieldset className="field">
            <legend className="field-label">访问保护</legend>
            <label className="switch">
              <input
                type="checkbox"
                checked={wantPassword}
                onChange={(e) => setWantPassword(e.target.checked)}
              />
              密码保护
            </label>
            <label className="switch" style={{ marginLeft: 16 }}>
              <input
                type="checkbox"
                checked={wantIp}
                onChange={(e) => setWantIp(e.target.checked)}
              />
              IP 白名单
            </label>
            {wantPassword ? (
              <>
                <p
                  className="field-hint hint"
                  title="原密码的加密结果无法复制，请为新的隧道设置密码（可与原密码相同）。"
                >
                  为新的隧道设置一条新密码。
                </p>
                <div className="field-row">
                  <div className="field">
                    <label className="field-label">访问用户名</label>
                    <input
                      className="input"
                      value={accessUser}
                      onChange={(e) => setAccessUser(e.target.value)}
                    />
                  </div>
                  <div className="field">
                    <label className="field-label">新密码</label>
                    <input
                      className="input mono"
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                    />
                  </div>
                </div>
              </>
            ) : null}
            {wantIp ? (
              <div className="field">
                <label className="field-label">允许访问的 IP</label>
                <input
                  className="input"
                  value={ips}
                  placeholder="例：1.2.3.4, 5.6.7.8"
                  onChange={(e) => setIps(e.target.value)}
                />
              </div>
            ) : null}
          </fieldset>
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            取消
          </button>
          <span className="grow" />
          <button className="btn btn-primary" disabled={busy} aria-busy={busy} onClick={() => void submit()}>
            {busy ? "处理中…" : "克隆这条隧道"}
          </button>
        </div>
      </div>
    </div>
  );
}
