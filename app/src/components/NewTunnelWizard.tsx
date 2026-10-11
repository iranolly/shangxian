// 发布新隧道向导：3 步 + 成功屏。
// 第 1 步选本机服务，第 2 步选云端服务器和端口，第 3 步定访问保护。

import { useEffect, useMemo, useState } from "react";

import {
  addTunnel,
  hashPassword,
  type ServerView,
  type ServiceView,
  type TunnelView,
} from "../api";

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
  services: ServiceView[];
  tunnels: TunnelView[];
  servers: ServerView[];
  onClose: () => void;
  onDone: () => void;
}

export function NewTunnelWizard({
  services,
  tunnels,
  servers,
  onClose,
  onDone,
}: Props) {
  const [step, setStep] = useState(1);
  const [serviceId, setServiceId] = useState(services[0]?.id ?? "");
  const [serverId, setServerId] = useState(servers[0]?.id ?? "");
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
  const [done, setDone] = useState<{
    url: string | null;
    password: string | null;
  } | null>(null);

  const service = services.find((s) => s.id === serviceId) ?? null;
  const server = servers.find((s) => s.id === serverId) ?? null;

  useEffect(() => {
    if (!serverId && servers.length > 0) setServerId(servers[0].id);
  }, [servers, serverId]);

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

  const submit = async () => {
    if (!service || !server) {
      setError("刚才选的服务或服务器已经不存在了，关掉向导重新开一次");
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
          // 建隧道时服务器上必须已经有密码哈希，算不出来就绝不会创建成功；
          // 这里直接拦住，不能让用户以为「稍后能补」。
          setError(
            `没能在服务器上把密码存好（可能是连不上服务器）。请先到「云端服务器」页确认这台服务器能连上，再回来重新发布。${e ? `（${String(e)}）` : ""}`,
          );
          return;
        }
      }
      await addTunnel({
        name: name.trim() || `${service.name} 发布`,
        serviceId: service.id,
        serverId: server.id,
        remotePort,
        url: url.trim() || null,
        access,
        accessUser: wantPassword ? accessUser.trim() || "shangxian" : null,
        accessHash,
        accessDetail: wantIp ? ips.trim() || null : null,
      });
      setDone({
        url: url.trim() || null,
        password: wantPassword ? password : null,
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const copy = (text: string) => {
    void navigator.clipboard.writeText(text);
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true">
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
                  ? "第 2 步，共 3 步：从哪台服务器出去、别人用哪个网址？"
                  : "第 3 步，共 3 步：谁能访问？"}
            </p>
          )}
        </div>

        <div className="modal-body">
          {done ? (
            <div className="success">
              <div className="success-mark">✓</div>
              <p className="success-title">发布成功，现在就能访问了</p>
              {done.url && (
                <div className="success-line">
                  <span className="success-url">https://{done.url}</span>
                  <button
                    className="link-btn"
                    onClick={() => copy(`https://${done.url}`)}
                  >
                    复制
                  </button>
                </div>
              )}
              {done.password && (
                <div className="success-line">
                  <span className="secret">{done.password}</span>
                  <button
                    className="link-btn"
                    onClick={() => copy(done.password!)}
                  >
                    复制密码
                  </button>
                </div>
              )}
              <p className="field-hint">
                {done.password
                  ? "密码已经存好了；配置云端 nginx 时用它启用访问保护。以后要改保护或换密码，到「访问保护」页。"
                  : "隧道已经开始发布，状态在「发布隧道」页能看到。以后想加保护，到「访问保护」页设置。"}
              </p>
            </div>
          ) : step === 1 ? (
            services.length === 0 ? (
              <>
                <p className="field-hint">
                  还没有登记本机服务。先到左边「本机服务」页，把要发布的程序添加上（端口 +
                  启动程序），再回来发布。
                </p>
              </>
            ) : (
              <div className="option-list">
                {services.map((s) => (
                  <div
                    key={s.id}
                    className={`option ${serviceId === s.id ? "selected" : ""}`}
                    onClick={() => setServiceId(s.id)}
                  >
                    <input type="radio" checked={serviceId === s.id} readOnly />
                    <span className="option-text">{s.name}</span>
                    <span className="mono">本机 {s.localPort}</span>
                  </div>
                ))}
                <p className="field-hint">
                  只会发布你选中的这台服务；它没在跑的时候，别人访问会失败。
                </p>
              </div>
            )
          ) : step === 2 ? (
            <>
              <div className="field">
                <label className="field-label">云端服务器</label>
                {servers.length === 0 ? (
                  <p className="field-hint">
                    还没有云端服务器，先去「云端服务器」页添加一台（地址、账号、密钥）。
                  </p>
                ) : (
                  <select
                    className="select"
                    value={serverId}
                    onChange={(e) => setServerId(e.target.value)}
                  >
                    {servers.map((s) => (
                      <option key={s.id} value={s.id}>
                        {s.name}（{s.host}）
                      </option>
                    ))}
                  </select>
                )}
              </div>
              <div className="field">
                <label className="field-label">云端端口</label>
                <input
                  className="input mono"
                  type="number"
                  value={remotePort || ""}
                  onChange={(e) =>
                    setRemotePort(parseInt(e.target.value, 10) || 0)
                  }
                />
                <p className="field-hint">
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
                  onChange={(e) => setUrl(e.target.value)}
                />
                <p className="field-hint">
                  留空也能发布；填了记得在买域名的地方加一条解析记录。
                </p>
              </div>
            </>
          ) : (
            <>
              <div className="option-list">
                <div
                  className={`option ${wantPassword ? "selected" : ""}`}
                  onClick={() => setWantPassword(!wantPassword)}
                >
                  <input type="checkbox" checked={wantPassword} readOnly />
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
                          onClick={() => setPassword(generatePassword())}
                        >
                          换一个
                        </button>
                      </div>
                    )}
                  </div>
                </div>
                <div
                  className={`option ${wantIp ? "selected" : ""}`}
                  onClick={() => setWantIp(!wantIp)}
                >
                  <input type="checkbox" checked={wantIp} readOnly />
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

              <p className="field-hint">
                两种保护可以同时勾选；同时开时，两个条件都要满足才能进。
                这一步就要定好：开了「要密码才能进」，发布时必须已经在服务器上存好密码，
                连不上服务器就发不出去（可以先关掉密码、建好后再到「访问保护」页补）。
              </p>
              {!wantPassword && !wantIp && (
                <div className="danger-line">
                  ⚠ 两种保护都没开：任何人知道网址就能访问，不要放私人内容
                </div>
              )}

              <div className="field" style={{ marginTop: 16 }}>
                <label className="field-label">给这条隧道起个名字</label>
                <input
                  className="input"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
                <p className="field-hint">这个名字只有你自己看得到。</p>
              </div>

              <p className="field-hint">
                确认一下：{service?.name}（本机 {service?.localPort}）→{" "}
                {server?.name} · 云端 {remotePort}
                {url.trim() ? ` · ${url.trim()}` : ""}
              </p>
            </>
          )}

          {error && <div className="error-line">{error}</div>}
        </div>

        <div className="modal-foot">
          {done ? (
            <button
              className="btn btn-primary"
              style={{ marginLeft: "auto" }}
              onClick={onDone}
            >
              完成
            </button>
          ) : (
            <>
              <button className="btn btn-ghost" onClick={onClose}>
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
                  disabled={
                    step === 1 ? !service : !server || !remoteOk
                  }
                  onClick={() => setStep(step + 1)}
                >
                  下一步
                </button>
              ) : (
                <button
                  className="btn btn-primary"
                  disabled={busy}
                  onClick={() => void submit()}
                >
                  {busy ? "发布中…" : "开始发布"}
                </button>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
