// 发布新隧道向导：3 步 + 成功屏。
// 第 1 步选本机服务，第 2 步选云端服务器和端口（含发布预设），第 3 步定访问保护。
//
// E3 改造（D4 §3.8）：
// - 死胡同修复：0 服务 / 0 服务器给直达按钮，并留「回来继续」的入口；
// - 发布预设下拉（普通网站 / WebSocket / 大文件上传）；
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
  type TunnelPreset,
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
  const [preset, setPreset] = useState<TunnelPreset>("web");
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

  // 从「添加本机服务 / 服务器」回来时预选新加的那条（同一个 preset 只处理一次）
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
    setTestMsg("正在连服务器…");
    try {
      const msg = await testServer(server.id);
      setTestMsg(msg || `连上「${server.name}」了`);
    } catch (e) {
      setTestMsg(`连不上：${errorText(e)}`);
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
      setError("刚才选的服务或服务器已经不存在了，关掉向导重新开一次");
      return;
    }
    if (!wantPassword && !wantIp && !ackNoProtect) {
      setError("没开保护：任何人拿到网址都能访问。先勾下面的确认。");
      return;
    }
    if (wantPassword && !password.trim()) {
      setError("要密码才能进的话，密码不能为空；点「换一个」可以自动生成");
      return;
    }
    if (wantIp && !ips.trim()) {
      setError("勾了「只允许指定 IP 访问」的话，要填至少一个 IP");
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
          accessHash = await hashPassword(server.id, password);
        } catch (e) {
          setError("密码没算出来，没有保存：换短一点再试。");
          setErrorHint(
            `常见原因：密码超过 72 字节，或这台服务器已经不在配置里了。${e ? `（${String(e)}）` : ""}`,
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
        preset,
      });
      if (!id) {
        setError("没发布成功：等一下再试");
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
    if (!ok) toast.error("没复制成功：请用鼠标选中后按 Ctrl+C 复制");
  };

  const presetHint: Record<TunnelPreset, string> = {
    web: "普通网站：大多数情况用这个。",
    ws: "WebSocket：AI 聊天、实时进度这类长连接不断线。",
    upload: "大文件上传：上限放到 2GB，边传边转发。",
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
                ? "第 1 步，共 3 步：发布电脑上的哪台服务？"
                : step === 2
                  ? "第 2 步，共 3 步：从哪台服务器出去？"
                  : "第 3 步，共 3 步：谁能访问？"}
            </p>
          )}
        </div>

        <div className="modal-body">
          {done ? (
            <div className="success">
              <div className="success-mark">✓</div>
              <p className="success-title">隧道建好了，还差最后一步</p>
              <p
                className="success-sub"
                title="把云端 nginx 配置贴到服务器面板里并重载，别人才能打开这个网址。"
              >
                贴好云端配置并重载，网址才生效。
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
                  title="下一步贴配置时会提醒你先补网址：到「发布隧道」页编辑这条隧道就能补。"
                >
                  没填网址：现在还没有对外地址。
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
                title="以后要改保护或换密码，到「访问保护」页。"
              >
                {done.password
                  ? "密码已存好，贴配置时用它开启保护。"
                  : "想加保护，到「访问保护」页设置。"}
              </p>
            </div>
          ) : step === 1 ? (
            services.length === 0 ? (
              <>
                <p className="field-hint hint">
                  还没有登记本机服务。先加一台，回来就能发布。
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
                  title="只会发布选中的那台服务；它没在跑时别人会打不开，建好后到「本机服务」页点「启动」。"
                >
                  没在跑的话别人打不开。
                </p>
              </div>
            )
          ) : step === 2 ? (
            <>
              <div className="field">
                <label className="field-label">云端服务器</label>
                {servers.length === 0 ? (
                  <>
                    <p className="field-hint hint">还没有云端服务器。先加一台，回来继续。</p>
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
                    这台服务器还没配免密登录。
                    <div className="field-row" style={{ marginTop: 6 }}>
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => void doTest()}
                      >
                        测试连接
                      </button>
                      <button
                        className="link-btn"
                        title="没有密钥？用密码登录一次，帮你配好免密登录"
                        onClick={() => setKeyOpen((v) => !v)}
                      >
                        配免密
                      </button>
                    </div>
                    {testMsg && <div className="field-hint hint">{testMsg}</div>}
                    {keyOpen && (
                      <div className="field-row" style={{ marginTop: 6 }}>
                        <input
                          className="input"
                          type="password"
                          placeholder="服务器登录密码（只用一次）"
                          value={keyPassword}
                          onChange={(e) => setKeyPassword(e.target.value)}
                        />
                        <button
                          className="btn btn-primary btn-sm"
                          disabled={keyBusy || !keyPassword.trim()}
                          onClick={() => void doSetupKey()}
                        >
                          {keyBusy ? "配置中…" : "配好免密"}
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
                    ? "这个端口已经被这台服务器上的其他隧道用了，换一个。"
                    : "一般不用改，已经帮你挑了一个没人用的。"}
                </p>
              </div>
              <div className="field">
                <label className="field-label">
                  别人访问时用的网址（可留空）
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
                <p className="field-hint hint" title="留空也能发布；填了记得在买域名的地方加一条解析记录。">
                  留空也能发布，填了要加域名解析。
                </p>
              </div>
              <div className="field">
                <label className="field-label">发布预设</label>
                <select
                  className="select"
                  value={preset}
                  onChange={(e) => {
                    setPreset(e.target.value as TunnelPreset);
                    setDirty(true);
                  }}
                >
                  <option value="web">普通网站（默认）</option>
                  <option value="ws">WebSocket（实时通信）</option>
                  <option value="upload">大文件上传</option>
                </select>
                <p
                  className="field-hint hint"
                  title={`${presetHint[preset]} 不确定就保持默认，贴配置时会把对应几行一起给你。`}
                >
                  不确定就保持默认。
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
                    aria-label="要密码才能进（推荐）"
                    onChange={(e) => setWantPassword(e.target.checked)}
                    onClick={(e) => e.stopPropagation()}
                  />
                  <div className="option-text">
                    要密码才能进（推荐）
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
                title="两种可以同时开，都要满足才能进；发布时在本机算好密码、只把结果写进服务器，写不进去就发不出去。可以先关掉，建好后再到「访问保护」页补。"
              >
                两种可以同时开，都要满足才能进。
              </p>
              {!wantPassword && !wantIp && (
                <div className="danger-line">
                  ⚠ 两种保护都没开：任何人拿到网址都能访问，不要放私人内容。
                  <label className="switch" title="不推荐：任何人拿到网址都能访问">
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
                <label className="field-label">给这条隧道起个名字</label>
                <input
                  className="input"
                  value={name}
                  onChange={(e) => {
                    setName(e.target.value);
                    setDirty(true);
                  }}
                />
                <p className="field-hint hint">这个名字只有你自己看得到。</p>
              </div>

              <p className="field-hint hint">
                确认一下：{service?.name}（本机 {service?.localPort}）→{" "}
                {server?.name} · 云端 {remotePort}
                {url.trim() ? ` · ${url.trim()}` : ""}
                {preset !== "web"
                  ? ` · 预设：${preset === "ws" ? "WebSocket" : "大文件上传"}`
                  : ""}
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
                稍后再说
              </button>
              <span className="grow" />
              <button
                className="btn btn-primary"
                title="把新的几行贴到服务器面板里并重载 nginx"
                onClick={() => setNginxOpen(true)}
              >
                贴云端配置
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
          title="要关掉发布向导吗？"
          body="填的内容不会保存。"
          confirmLabel="关掉"
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
