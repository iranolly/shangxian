// 访问保护弹层（从隧道卡片的保护小牌或 ⋯ 菜单打开）：专管门禁——网页访问密码 + IP 白名单。
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
        setIpError("未能获取公网 IP。可到搜索引擎中搜索“IP”查看自己的地址。");
      } else {
        setMyIp(info.ip);
      }
    } catch (e) {
      setMyIp(null);
      setIpError(`查询失败：${errorText(e)}。可到搜索引擎中搜索“IP”查看自己的地址。`);
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
      setError("该隧道已启用密码保护，但尚未设置密码：请填写新密码");
      return;
    }
    if (wantIp && !finalIps.trim()) {
      setError("勾选「只允许指定 IP」后，需填写至少一个 IP");
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
            title="此密码是访客打开网页时输入的，不是「云端服务器」页中连接服务器的登录密码。"
          >
            {tunnel.url ?? "尚未填写公网网址"}；这是网页访问密码。
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
                  密码保护（推荐）
                  <span className="option-hint hint">
                    {tunnel.hasPassword
                      ? "已设置密码；不填写新密码则保持不变"
                      : "尚未设置密码，需填写新密码"}
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
            <p className="field-hint hint">两项可同时开启，均满足后方可访问。</p>
            {!wantPassword && !wantIp && (
              <div className="danger-line">
                ⚠ 两项保护均未开启：任何人知道网址即可访问，请勿放置私人内容
              </div>
            )}
            {(wantPassword || wantIp) && (
              <p className="field-hint hint" title="仅用于证书续期，其他地址仍需密码 / 白名单。">
                保存后，证书续期所需路径会自动放行。
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
                  {tunnel.hasPassword ? "（留空 = 不修改）" : ""}
                </label>
                <input
                  className="input"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder={tunnel.hasPassword ? "留空则不修改密码" : "必填"}
                />
                <p
                  className="field-hint hint"
                  title="保存时在本机计算，仅将加密结果写入服务器上的密码文件；连接服务器使用的登录密码在「云端服务器」页。"
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
                  {ipBusy ? "查询中…" : myIp ? "重新查询" : "查公网 IP"}
                </button>
                {myIp ? (
                  <>
                    <span className="ip-assist-result">本机公网 IP：{myIp}</span>
                    <button
                      className="link-btn"
                      disabled={myIpListed}
                      title={myIpListed ? "已在白名单中" : undefined}
                      onClick={addMyIp}
                    >
                      加入白名单
                    </button>
                  </>
                ) : null}
              </div>
              {ipError && <div className="error-line">{ipError}</div>}
              {myIp && (
                <p
                  className="field-hint hint"
                  title="更换网络或重启宽带后可能变化；添加后仍无法访问，可能是运营商共享 IP，建议改用密码保护。"
                >
                  服务器看到的访问 IP 是 {myIp}。
                </p>
              )}
              {ips.trim() ? (
                <p className="field-hint hint" title="IP 变化后白名单会将本机拦截，请返回此处添加新 IP。">
                  宽带 IP 会变化，变化后需返回添加新 IP。
                </p>
              ) : null}
            </div>
          )}

          <p
            className="field-hint hint"
            title="将新增配置行粘贴到服务器面板并重载 nginx 后，保护才会生效。如需修改名称、网址、云端端口，请到「发布隧道」页。"
          >
            保存后将打开「云端配置」。
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
          title="该 IP 不在白名单中"
          body="保存后，本机也可能无法访问这条隧道（除非更换网络）。"
          confirmLabel="保存"
          ack={{
            label: `同时加入本机 IP ${lockConfirm}`,
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
