// 访问保护弹层（只出现在「访问保护」页）：专管门禁——网页访问密码 + IP 白名单。
// 规则：密码留空 = 保留原来的密码；保存成功后由页面接着打开「云端配置」作为下一步。
// 注意：「密码是否开启」看 tunnel.access 里有没有 "password"，不用 hasPassword。
//
// E3 改造（D4 §3.5，⑱）：
// - IP 白名单输入框下加「帮我查我的公网 IP」助手（get_public_ip）；
// - 保存前如果白名单里没有自己的公网 IP，先弹确认（防把自己锁在外面）；
// - 提示动态 IP 会变；提示证书续期用的 /.well-known 临时放行。

import { useRef, useState } from "react";

import {
  getPublicIp,
  hashPassword,
  type TunnelView,
} from "../api";
import { actions, type TunnelProtectionInput } from "../store/actions";
import { errorText } from "../store/commands";
import { useStore } from "../store/useStore";
import { accessFlags } from "./ProtectionStatus";
import { ConfirmModal } from "./ConfirmModal";
import { useModalA11y } from "./useModalA11y";

interface Props {
  tunnel: TunnelView;
  onClose(): void;
  /** 保存成功后调用：页面用它直接打开这条隧道的「云端配置」弹层 */
  onSaved: () => void;
}

function splitIps(text: string): string[] {
  return text
    .split(/[,，\s]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

export function ProtectionModal({ tunnel, onClose, onSaved }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const configTrusted = useStore((s) => s.app.configTrusted);
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
  /** 查到的公网 IP（助手用；null = 还没查过） */
  const [myIp, setMyIp] = useState<string | null>(null);
  const [ipBusy, setIpBusy] = useState(false);
  const [ipError, setIpError] = useState("");
  /** 保存前发现白名单里没有自己的 IP：先确认 */
  const [lockConfirm, setLockConfirm] = useState<null | string>(null);
  const [ackAddMyIp, setAckAddMyIp] = useState(true);

  const ipList = splitIps(ips);
  const myIpListed = myIp !== null && ipList.includes(myIp);

  const findMyIp = async () => {
    setIpBusy(true);
    setIpError("");
    try {
      const info = await getPublicIp();
      if (!info.ip) {
        setMyIp(null);
        setIpError("没查到你的公网 IP。可以到搜索引擎里搜“IP”看自己的地址。");
      } else {
        setMyIp(info.ip);
      }
    } catch (e) {
      setMyIp(null);
      setIpError(`没查到：${errorText(e)}。可以到搜索引擎里搜“IP”看自己的地址。`);
    } finally {
      setIpBusy(false);
    }
  };

  const addMyIp = () => {
    if (!myIp || ipList.includes(myIp)) return;
    setIps(ipList.length === 0 ? myIp : `${ips.trimEnd()}, ${myIp}`);
  };

  const doSave = async (overrideIps?: string) => {
    setError("");
    const finalIps = overrideIps ?? ips;
    // 只有这条隧道还没设过密码时才必须要新密码；已设过的留空就沿用。
    if (wantPassword && !password.trim() && !tunnel.hasPassword) {
      setError("这条隧道要密码才能进，但还没设过密码：请填一条新密码");
      return;
    }
    if (wantIp && !finalIps.trim()) {
      setError("勾了「只允许指定 IP」的话，要填至少一个 IP");
      return;
    }

    setBusy(true);
    try {
      const access: string[] = [];
      if (wantPassword) access.push("password");
      if (wantIp) access.push("ip");
      // 只交 id + 访问三件套：名称 / 云端端口 / 网址让 actions 从最新快照补，
      // 不把弹层打开时的旧快照整份回传。
      const input: TunnelProtectionInput = {
        id: tunnel.id,
        access,
        accessUser: wantPassword ? accessUser.trim() || "shangxian" : null,
        accessDetail: wantIp ? finalIps.trim() || null : null,
      };
      // 没填新密码就不传 accessHash：后端保持原来的密码不变。
      if (wantPassword && password.trim()) {
        input.accessHash = await hashPassword(
          tunnel.serverId,
          password.trim(),
        );
      }
      await actions.tunnelProtectionSave(input);
      window.dispatchEvent(new CustomEvent("shangxian:notif-refresh"));
      onSaved();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  /** 保存前先拦一道：开了白名单但没把自己加进去 */
  const save = () => {
    if (wantIp && ips.trim() && myIp && !ipList.includes(myIp)) {
      setAckAddMyIp(true);
      setLockConfirm(myIp);
      return;
    }
    void doSave();
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef} style={{ width: 560 }}>
        <div className="modal-head">
          <h2 className="modal-title">访问保护 · {tunnel.name}</h2>
          <p
            className="modal-sub"
            title="这里的密码是别人打开网页时输的，不是「云端服务器」页里连服务器的登录密码。"
          >
            {tunnel.url ?? "还没填公网网址"}；这是网页访问密码。
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
                  <span className="option-hint hint">
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
                  <span className="option-hint hint">
                    适合固定上网地点的场景
                  </span>
                </div>
              </label>
            </div>
            <p className="field-hint hint">两种可以同时开，都要满足才能进。</p>
            {!wantPassword && !wantIp && (
              <div className="danger-line">
                ⚠ 两种保护都没开：任何人知道网址就能访问，不要放私人内容
              </div>
            )}
            {(wantPassword || wantIp) && (
              <p className="field-hint hint" title="只给证书续期用，别的地址照样要密码 / 白名单。">
                保存后证书续期会自动放行一小块。
              </p>
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
                <p
                  className="field-hint hint"
                  title="保存时立刻在这台电脑上算好，只把加密结果写进服务器上的密码文件；连服务器用的登录密码在「云端服务器」页。"
                >
                  密码只在本机加密，不存明文。
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
              <div className="ip-assist">
                <button
                  className="btn btn-ghost btn-sm"
                  disabled={ipBusy}
                  onClick={() => void findMyIp()}
                >
                  {ipBusy ? "查询中…" : myIp ? "重新查" : "查公网 IP"}
                </button>
                {myIp ? (
                  <>
                    <span className="ip-assist-result">你的公网 IP 是 {myIp}</span>
                    <button
                      className="link-btn"
                      disabled={myIpListed}
                      title={myIpListed ? "已经在白名单里了" : undefined}
                      onClick={addMyIp}
                    >
                      加进白名单
                    </button>
                  </>
                ) : null}
              </div>
              {ipError && <div className="error-line">{ipError}</div>}
              {myIp && (
                <p
                  className="field-hint hint"
                  title="换网络或重启宽带后可能就变了；加完还是进不来，可能是运营商共享 IP，改用密码保护更省心。"
                >
                  服务器看到你的上网 IP 是 {myIp}。
                </p>
              )}
              {ips.trim() ? (
                <p className="field-hint hint" title="变了以后白名单会把自己挡在外面，到时来这里把新 IP 加进去。">
                  宽带 IP 会变，变了要回来加新的。
                </p>
              ) : null}
            </div>
          )}

          <p
            className="field-hint hint"
            title="把新的几行贴到服务器面板里并重载 nginx，保护才算真的生效。想改名称、网址、云端端口，请到「发布隧道」页。"
          >
            保存后接着打开「云端配置」。
          </p>
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
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
      </div>

      {lockConfirm !== null && (
        <ConfirmModal
          title="这个 IP 不在白名单里"
          body="保存后，你自己也可能打不开这条隧道（除非换网络）。"
          confirmLabel="保存"
          ack={{
            label: `把我的 IP ${lockConfirm} 也加进去`,
            checked: ackAddMyIp,
            onChange: setAckAddMyIp,
          }}
          onConfirm={() => {
            const nextIps =
              ackAddMyIp && !ipList.includes(lockConfirm)
                ? ipList.length === 0
                  ? lockConfirm
                  : `${ips.trimEnd()}, ${lockConfirm}`
                : ips;
            setIps(nextIps);
            setLockConfirm(null);
            void doSave(nextIps);
          }}
          onClose={() => setLockConfirm(null)}
        />
      )}
    </div>
  );
}
