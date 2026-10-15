// 发布新隧道向导：3 步 + 成功屏。
// 第 1 步选本机服务，第 2 步选云端服务器和端口，第 3 步定访问保护。
//
// E3 改造（D4 §3.8）：
// - 死胡同修复：0 服务 / 0 服务器给直达按钮，并留「回来继续」的入口；
// - 成功屏二维码按钮（离线生成）；
// - 表单改过再按 Esc / 取消要确认（dirty）。
//
// 旧页面传 services / tunnels / servers 数组；新页面按 D4 只传 presetServiceId 等，
// 不传数组时自动从 store 取。

import { useEffect, useMemo, useRef, useState } from "react";

import {
  hashPassword,
  setupPasswordless,
  testServer,
  type ServerView,
  type ServiceView,
  type TunnelView,
} from "../api";
import { copyText } from "../copy/clipboard";
import { T } from "../copy/toasts";
import { actions } from "../store/actions";
import { errorText } from "../store/commands";
import { toast } from "../store/toastStore";
import { useStore } from "../store/useStore";
import { ConfirmModal } from "./ConfirmModal";
import { NginxModal } from "./NginxModal";
import { QrModal } from "./QRModal";
import { useModalA11y } from "./useModalA11y";

function generatePassword(): string {
  const chars = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => chars[b % chars.length]).join("");
}

/** 自动挑一个没人用的云端端口（从 13000 开始往上找） */
function suggestRemotePort(used: Set<number>): number {
  let p = 13000;
  while (used.has(p) && p < 65500) p += 1;
  return p;
}

interface Props {
  services?: ServiceView[];
  tunnels?: TunnelView[];
  servers?: ServerView[];
  /** 从「添加本机服务」回来时预选这条服务 */
  presetServiceId?: string;
  /** 从「添加云端服务器」回来时预选这台服务器 */
  presetServerId?: string;
  /** 点「+ 添加本机服务」：页面负责关向导、跳页、开添加弹层并让保存后回到向导 */
  onAddService?: () => void;
  /** 点「+ 添加云端服务器」，同上 */
  onAddServer?: () => void;
  onClose(): void;
  onDone(): void;
}

export function NewTunnelWizard({
  services: servicesProp,
  tunnels: tunnelsProp,
  servers: serversProp,
  presetServiceId,
  presetServerId,
  onAddService,
  onAddServer,
  onClose,
  onDone,
}: Props) {
  const storeServices = useStore((s) => s.services);
  const storeTunnels = useStore((s) => s.tunnels);
  const storeServers = useStore((s) => s.servers);
  const configTrusted = useStore((s) => s.app.configTrusted);
  const services = servicesProp ?? storeServices;
  const tunnels = tunnelsProp ?? storeTunnels;
  const servers = serversProp ?? storeServers;

  const [step, setStep] = useState(1);
  const [serviceId, setServiceId] = useState(
    presetServiceId ?? services[0]?.id ?? "",
  );
  const [serverId, setServerId] = useState(
    presetServerId ?? servers[0]?.id ?? "",
  );
  const [remotePort, setRemotePort] = useState(0);
  const [url, setUrl] = useState("");
  const [wantPassword, setWantPassword] = useState(true);
  const [wantIp, setWantIp] = useState(false);
  const [accessUser, setAccessUser] = useState("shangxian");
  const [password, setPassword] = useState(generatePassword);
  const [ips, setIps] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  /** 长错误细节：界面上只留一句，鼠标停留再看 */
  const [errorHint, setErrorHint] = useState("");
  /** 选了「无保护」发布时的确认勾选：不勾不让发布 */
  const [ackNoProtect, setAckNoProtect] = useState(false);
  const [nginxOpen, setNginxOpen] = useState(false);
  const [qrOpen, setQrOpen] = useState(false);
  /** 表单改过：Esc / 取消时要确认 */
  const [dirty, setDirty] = useState(false);
  const [confirmClose, setConfirmClose] = useState(false);
  /** 没有密钥时的「密码登录一次配免密」小流程 */
  const [keyOpen, setKeyOpen] = useState(false);
  const [keyPassword, setKeyPassword] = useState("");
  const [keyBusy, setKeyBusy] = useState(false);
  const [keyMsg, setKeyMsg] = useState("");
  const [testMsg, setTestMsg] = useState("");
  const [done, setDone] = useState<{
    id: string;
    name: string;
    url: string | null;
    password: string | null;
  } | null>(null);

  // 弹层无障碍：Esc 关闭 + 焦点圈定；成功屏再开「云端配置」时由内层接管
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const requestClose = () => {
    if (dirty && !done) setConfirmClose(true);
    else onClose();
  };
  useModalA11y(dialogRef, requestClose);

  const service = services.find((s) => s.id === serviceId) ?? null;
  const server = servers.find((s) => s.id === serverId) ?? null;

  useEffect(() => {
    if (!serverId && servers.length > 0) setServerId(servers[0].id);
  }, [servers, serverId]);

  // 从「添加本机服务 / 服务器」回来时预选新加的那条（同一个预选值只处理一次）
  const appliedPresetService = useRef<string | null>(null);
  const appliedPresetServer = useRef<string | null>(null);
  useEffect(() => {
    if (!presetServiceId || appliedPresetService.current === presetServiceId) return;
    if (services.some((s) => s.id === presetServiceId)) {
      appliedPresetService.current = presetServiceId;
      setServiceId(presetServiceId);
      setStep(1);
    }
  }, [presetServiceId, services]);

  useEffect(() => {
    if (!presetServerId || appliedPresetServer.current === presetServerId) return;
    if (servers.some((s) => s.id === presetServerId)) {
      appliedPresetServer.current = presetServerId;
      setServerId(presetServerId);
      setStep(2);
    }
  }, [presetServerId, servers]);

  // 同一台服务器上已经被占用的云端端口
  const usedRemote = useMemo(() => {
    const set = new Set<number>();
    tunnels.forEach((t) => {
      if (t.serverId === serverId) set.add(t.remotePort);
    });
    return set;
  }, [tunnels, serverId]);

  // 只在切换服务器（和刚打开向导）时自动挑一次端口，不要每轮快照都把用户填的值冲掉
  useEffect(() => {
    setRemotePort(suggestRemotePort(usedRemote));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverId]);

  useEffect(() => {
    if (step === 3 && !name && service) setName(`${service.name} 发布`);
  }, [step, name, service]);

  const remoteOk =
    remotePort > 0 && remotePort < 65536 && !usedRemote.has(remotePort);

  // 死胡同：没有服务 / 服务器时，关掉向导并记下「回来继续」的意图
  const goAdd = (kind: "service" | "server") => {
    const handler = kind === "service" ? onAddService : onAddServer;
    if (handler) handler();
    else {
      window.dispatchEvent(
        new CustomEvent("shangxian:nav-intent", {
          detail: { action: kind === "service" ? "add-service" : "add-server", returnTo: "wizard" },
        }),
      );
    }
    onClose();
  };

  const doTest = async () => {
    if (!server) return;
    setTestMsg("正在连接服务器…");
    try {
      const msg = await testServer(server.id);
      setTestMsg(msg || `已连接「${server.name}」`);
    } catch (e) {
      setTestMsg(`无法连接：${errorText(e)}`);
    }
  };

  const doSetupKey = async () => {
    if (!server || !keyPassword.trim()) return;
    setKeyBusy(true);
    setKeyMsg("");
    try {
      await setupPasswordless(server.id, keyPassword);
      toast.success(T.keyReady(server.name));
      setKeyOpen(false);
      setKeyPassword("");
    } catch (e) {
      setKeyMsg(errorText(e));
    } finally {
      setKeyBusy(false);
    }
  };

  const submit = async () => {
    setErrorHint("");
    if (!service || !server) {
      setError("刚才选择的服务或服务器已不存在，请关闭向导后重新打开");
      return;
    }
    if (!wantPassword && !wantIp && !ackNoProtect) {
      setError("未开启保护：任何人获得网址均可访问。请先勾选下方确认项。");
      return;
    }
    if (wantPassword && !password.trim()) {
      setError("启用密码保护后，密码不能为空；点击「换一个」可自动生成");
      return;
    }
    if (wantIp && !ips.trim()) {
      setError("勾选「只允许指定 IP 访问」后，需填写至少一个 IP");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const access: string[] = [];
      if (wantPassword) access.push("password");
      if (wantIp) access.push("ip");
      let accessHash: string | null = null;
      if (wantPassword) {
        try {
          accessHash = (await hashPassword(server.id, password)).hash;
        } catch (e) {
          setError("密码计算失败，未保存：请缩短密码后重试。");
          setErrorHint(
            `常见原因：密码超过 72 字节，或该服务器已不在配置中。${e ? `（${String(e)}）` : ""}`,
          );
          return;
        }
      }
      const tunnelName = name.trim() || `${service.name} 发布`;
      const id = await actions.tunnelAdd({
        name: tunnelName,
        serviceId: service.id,
        serverId: server.id,
        remotePort,
        url: url.trim() || null,
        access,
        accessUser: wantPassword ? accessUser.trim() || "shangxian" : null,
        accessHash,
        accessDetail: wantIp ? ips.trim() || null : null,
      });
      if (!id) {
        setError("发布失败：请稍后重试");
        return;
      }
      window.dispatchEvent(new CustomEvent("shangxian:notif-refresh"));
      setDone({
        id,
        name: tunnelName,
        url: url.trim() || null,
        password: wantPassword ? password : null,
      });
      setDirty(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const copy = async (text: string) => {
    const ok = await copyText(text);
    if (!ok) toast.error("复制失败：请用鼠标选中后按 Ctrl+C 复制");
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef}>
        <div className="modal-head">
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
            }}
          >
            <h2 className="modal-title">发布新隧道</h2>
            <div className="steps" aria-hidden="true">
              {[1, 2, 3].map((i) => (
                <span
                  key={i}
                  className={`step-dot ${
                    step === i ? "active" : step > i ? "done" : ""
                  }`}
                />
              ))}
            </div>
          </div>
          {!done && (
            <p className="modal-sub">
              {step === 1
                ? "第 1 步，共 3 步：选择要发布的本机服务"
                : step === 2
                  ? "第 2 步，共 3 步：选择出口服务器"
                  : "第 3 步，共 3 步：设置访问权限"}
            </p>
          )}
        </div>

        <div className="modal-body">
          {done ? (
            <div className="success">
              <div className="success-mark">✓</div>
              <p className="success-title">隧道已创建，还差最后一步</p>
              <p
                className="success-sub"
                title="将云端 nginx 配置粘贴到服务器面板并重载后，外部（访客）才能访问这个网址。"
              >
                粘贴云端配置并重载后，网址才会生效。
              </p>
              {done.url ? (
                <>
                  <div className="success-line">
                    <span className="success-url">https://{done.url}</span>
                    <button
                      className="link-btn"
                      onClick={() => void copy(`https://${done.url}`)}
                    >
                      复制
                    </button>
                  </div>
                  <div style={{ marginTop: 8 }}>
                    <button
                      className="btn btn-ghost btn-sm"
                      onClick={() => setQrOpen(true)}
                    >
                      显示二维码
                    </button>
                  </div>
                </>
              ) : (
                <p
                  className="field-hint hint"
                  title="下一步粘贴配置时会提醒先补网址：到「发布隧道」页编辑这条隧道即可补。"
                >
                  未填写网址：当前暂无对外地址。
                </p>
              )}
              {done.password && (
                <div className="success-line">
                  <span className="secret">{done.password}</span>
                  <button
                    className="link-btn"
                    onClick={() => void copy(done.password!)}
                  >
                    复制密码
                  </button>
                </div>
              )}
              <p
                className="field-hint hint"
                title="如需修改保护设置或更换密码，请在「发布隧道」页点击卡片上的保护状态。"
              >
                {done.password
                  ? "密码已保存，粘贴配置时用它开启保护。"
                  : "如需添加保护，发布后在卡片上点击「无访问保护」。"}
              </p>
            </div>
          ) : step === 1 ? (
            services.length === 0 ? (
              <>
                <p className="field-hint hint">
                  尚未登记本机服务。先添加一台，返回后即可发布。
                </p>
                <button
                  className="btn btn-primary"
                  onClick={() => goAdd("service")}
                >
                  添加本机服务
                </button>
              </>
            ) : (
              <div className="option-list">
                {services.map((s) => (
                  <label
                    key={s.id}
                    className={`option ${serviceId === s.id ? "selected" : ""}`}
                  >
                    <input
                      type="radio"
                      name="wizard-service"
                      checked={serviceId === s.id}
                      onChange={() => {
                        setServiceId(s.id);
                        setDirty(true);
                      }}
                    />
                    <span className="option-text">{s.name}</span>
                    <span className="mono">本机 {s.localPort}</span>
                  </label>
                ))}
                <p
                  className="field-hint hint"
                  title="只会发布选中的服务；该服务未运行时外部（访客）无法访问，创建完成后请到「本机服务」页点击「启动」。"
                >
                  服务未运行时，外部（访客）无法访问。
                </p>
              </div>
            )
          ) : step === 2 ? (
            <>
              <div className="field">
                <label className="field-label">云端服务器</label>
                {servers.length === 0 ? (
                  <>
                    <p className="field-hint hint">尚未添加云端服务器。先添加一台，返回后继续。</p>
                    <button
                      className="btn btn-primary"
                      onClick={() => goAdd("server")}
                    >
                      添加服务器
                    </button>
                  </>
                ) : (
                  <select
                    className="select"
                    value={serverId}
                    onChange={(e) => {
                      setServerId(e.target.value);
                      setDirty(true);
                    }}
                  >
                    {servers.map((s) => (
                      <option key={s.id} value={s.id}>
                        {s.name}（{s.host}）
                      </option>
                    ))}
                  </select>
                )}
                {server && !server.keyPath && (
                  <div className="warn-line" style={{ marginTop: 8 }}>
                    该服务器尚未配置免密登录。
                    <div className="field-row" style={{ marginTop: 6 }}>
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => void doTest()}
                      >
                        测试连接
                      </button>
                      <button
                        className="link-btn"
                        title="没有密钥？用密码登录一次即可配置免密登录"
                        onClick={() => setKeyOpen((v) => !v)}
                      >
                        配置免密
                      </button>
                    </div>
                    {testMsg && <div className="field-hint hint">{testMsg}</div>}
                    {keyOpen && (
                      <div className="field-row" style={{ marginTop: 6 }}>
                        <input
                          className="input"
                          type="password"
                          placeholder="服务器登录密码（仅使用一次）"
                          value={keyPassword}
                          onChange={(e) => setKeyPassword(e.target.value)}
                        />
                        <button
                          className="btn btn-primary btn-sm"
                          disabled={keyBusy || !keyPassword.trim()}
                          onClick={() => void doSetupKey()}
                        >
                          {keyBusy ? "配置中…" : "配置免密"}
                        </button>
                      </div>
                    )}
                    {keyMsg && <div className="error-line">{keyMsg}</div>}
                  </div>
                )}
              </div>
              <div className="field">
                <label className="field-label">云端端口</label>
                <input
                  className="input mono"
                  type="number"
                  value={remotePort || ""}
                  onChange={(e) => {
                    setRemotePort(parseInt(e.target.value, 10) || 0);
                    setDirty(true);
                  }}
                />
                <p className="field-hint hint">
                  {usedRemote.has(remotePort)
                    ? "该端口已被这台服务器上的其他隧道占用，请更换端口。"
                    : "通常无需修改，已自动选择一个空闲端口。"}
                </p>
              </div>
              <div className="field">
                <label className="field-label">
                  外部访问使用的网址（可留空）
                </label>
                <input
                  className="input"
                  placeholder="例如 album.你的域名.com"
                  value={url}
                  onChange={(e) => {
                    setUrl(e.target.value);
                    setDirty(true);
                  }}
                />
                <p className="field-hint hint" title="留空也可发布；填写后请在域名服务商处添加一条解析记录。">
                  留空也可发布；填写后需添加域名解析。
                </p>
              </div>
            </>
          ) : (
            <>
              <div className="option-list">
                <div
                  className={`option ${wantPassword ? "selected" : ""}`}
                  onClick={() => {
                    setWantPassword(!wantPassword);
                    setAckNoProtect(false);
                    setDirty(true);
                  }}
                >
                  <input
                    type="checkbox"
                    checked={wantPassword}
                    aria-label="密码保护（推荐）"
                    onChange={(e) => setWantPassword(e.target.checked)}
                    onClick={(e) => e.stopPropagation()}
                  />
                  <div className="option-text">
                    密码保护（推荐）
                    {wantPassword && (
                      <div
                        className="field-row"
                        style={{ marginTop: 6 }}
                        onClick={(e) => e.stopPropagation()}
                      >
                        <div className="field" style={{ marginBottom: 0 }}>
                          <label className="field-label">用户名</label>
                          <input
                            className="input"
                            value={accessUser}
                            onChange={(e) => setAccessUser(e.target.value)}
                          />
                        </div>
                        <div className="field" style={{ marginBottom: 0 }}>
                          <label className="field-label">密码</label>
                          <input
                            className="input mono"
                            value={password}
                            onChange={(e) => setPassword(e.target.value)}
                          />
                        </div>
                        <button
                          className="link-btn"
                          style={{ alignSelf: "flex-end" }}
                          onClick={() => {
                            setPassword(generatePassword());
                            setDirty(true);
                          }}
                        >
                          换一个
                        </button>
                      </div>
                    )}
                  </div>
                </div>
                <div
                  className={`option ${wantIp ? "selected" : ""}`}
                  onClick={() => {
                    setWantIp(!wantIp);
                    setAckNoProtect(false);
                    setDirty(true);
                  }}
                >
                  <input
                    type="checkbox"
                    checked={wantIp}
                    aria-label="只允许指定 IP 访问"
                    onChange={(e) => setWantIp(e.target.checked)}
                    onClick={(e) => e.stopPropagation()}
                  />
                  <div className="option-text">
                    只允许指定 IP 访问
                    {wantIp && (
                      <input
                        className="input mono"
                        style={{ marginTop: 6 }}
                        placeholder="1.2.3.4, 5.6.7.8"
                        value={ips}
                        onChange={(e) => setIps(e.target.value)}
                        onClick={(e) => e.stopPropagation()}
                      />
                    )}
                  </div>
                </div>
              </div>

              <p
                className="field-hint hint"
                title="两项可同时开启，均满足后方可访问；发布时在本机计算密码并将结果写入服务器，写入失败则无法发布。可先关闭，创建完成后在卡片上补充。"
              >
                两项可同时开启，均满足后方可访问。
              </p>
              {!wantPassword && !wantIp && (
                <div className="danger-line">
                  ⚠ 两项保护均未开启：任何人获得网址均可访问，请勿放置私人内容。
                  <label className="switch" title="不推荐：任何人获得网址均可访问">
                    <input
                      type="checkbox"
                      checked={ackNoProtect}
                      onChange={(e) => setAckNoProtect(e.target.checked)}
                    />
                    我知道，仍然发布
                  </label>
                </div>
              )}

              <div className="field" style={{ marginTop: 16 }}>
                <label className="field-label">隧道名称</label>
                <input
                  className="input"
                  value={name}
                  onChange={(e) => {
                    setName(e.target.value);
                    setDirty(true);
                  }}
                />
                <p className="field-hint hint">该名称仅本机可见。</p>
              </div>

              <p className="field-hint hint">
                请确认：{service?.name}（本机 {service?.localPort}）→{" "}
                {server?.name} · 云端 {remotePort}
                {url.trim() ? ` · ${url.trim()}` : ""}
              </p>
            </>
          )}

          {error && (
            <div className="error-line" title={errorHint || undefined}>
              {error}
            </div>
          )}
        </div>

        <div className="modal-foot">
          {done ? (
            <>
              <button className="btn btn-ghost" onClick={onDone}>
                稍后处理
              </button>
              <span className="grow" />
              <button
                className="btn btn-primary"
                title="将新增配置行粘贴到服务器面板并重载 nginx"
                onClick={() => setNginxOpen(true)}
              >
                粘贴云端配置
              </button>
            </>
          ) : (
            <>
              <button className="btn btn-ghost" onClick={requestClose}>
                取消
              </button>
              <span className="grow" />
              {step > 1 && (
                <button
                  className="btn btn-ghost"
                  onClick={() => setStep(step - 1)}
                >
                  上一步
                </button>
              )}
              {step < 3 ? (
                <button
                  className="btn btn-primary"
                  disabled={step === 1 ? !service : !server || !remoteOk}
                  onClick={() => setStep(step + 1)}
                >
                  下一步
                </button>
              ) : (
                <button
                  className="btn btn-primary"
                  disabled={
                    busy || !configTrusted || (!wantPassword && !wantIp && !ackNoProtect)
                  }
                  title={!configTrusted ? "配置文件读不出来，先修好再改设置" : undefined}
                  onClick={() => void submit()}
                >
                  {busy ? "发布中…" : "开始发布"}
                </button>
              )}
            </>
          )}
        </div>
      </div>

      {nginxOpen && done && (
        <NginxModal
          id={done.id}
          name={done.name}
          onClose={() => setNginxOpen(false)}
        />
      )}
      {qrOpen && done?.url && (
        <QrModal
          url={`https://${done.url}`}
          name={done.name}
          onClose={() => setQrOpen(false)}
        />
      )}

      {confirmClose && (
        <ConfirmModal
          title="要关闭发布向导吗？"
          body="填写的内容不会保存。"
          confirmLabel="关闭"
          cancelLabel="继续填写"
          onConfirm={() => {
            setConfirmClose(false);
            onClose();
          }}
          onClose={() => setConfirmClose(false)}
        />
      )}
    </div>
  );
}
