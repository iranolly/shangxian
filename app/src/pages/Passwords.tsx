// 访问密码页：一眼看清每条隧道的访问保护状态，缺保护的当场能修。

import { useState } from "react";

import { EditTunnelModal } from "../components/EditTunnelModal";
import { NginxModal } from "../components/NginxModal";
import type { TunnelView } from "../api";

interface Props {
  tunnels: TunnelView[];
  onRefresh: () => void;
}

interface Protection {
  tone: "ok" | "warn";
  label: string;
  action: string;
}

function describe(t: TunnelView): Protection {
  if (t.access === "password" && t.hasPassword) {
    return {
      tone: "ok",
      label: `要密码（用户名 ${t.accessUser ?? "shangxian"}）`,
      action: "换密码",
    };
  }
  if (t.access === "password") {
    return { tone: "warn", label: "选了要密码，但还没设", action: "设密码" };
  }
  if (t.access === "ip") {
    return { tone: "ok", label: "只允许指定 IP", action: "改白名单" };
  }
  return { tone: "warn", label: "谁都能进", action: "加保护" };
}

export function PasswordsPage({ tunnels, onRefresh }: Props) {
  const [editFor, setEditFor] = useState<TunnelView | null>(null);
  const [nginxFor, setNginxFor] = useState<TunnelView | null>(null);

  const warnCount = tunnels.filter((t) => describe(t).tone === "warn").length;

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">访问密码</h1>
          <p className="page-sub">给每条隧道加一道门：要密码、或者只让指定的人进。</p>
        </div>
      </header>
      <div className="service-list">
        {tunnels.length === 0 ? (
          <div className="empty">
            <p className="empty-title">还没有发布隧道</p>
            <p className="empty-text">
              先到「发布隧道」页发布一条，再回来给它加访问保护。
            </p>
          </div>
        ) : (
          <>
            <article className="card">
              <div className="server-card">
                <div className="server-main">
                  <div className="server-name">保护情况</div>
                  <div className="server-meta">
                    {warnCount === 0
                      ? `${tunnels.length} 条隧道都设了保护。`
                      : `有 ${warnCount} 条隧道没保护，网址一泄露谁都能看。`}
                  </div>
                </div>
              </div>
            </article>

            {tunnels.map((t) => {
              const p = describe(t);
              return (
                <article className="card" key={t.id}>
                  <div className="server-card">
                    <div className="server-main">
                      <div className="server-name">{t.name}</div>
                      <div className="server-meta">
                        {t.url ?? "还没填公网网址"}
                        {t.access === "ip" && t.accessDetail
                          ? ` · 允许：${t.accessDetail}`
                          : ""}
                      </div>
                    </div>
                    <span className={`chip ${p.tone}`}>{p.label}</span>
                    <span className="row-actions">
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => setEditFor(t)}
                      >
                        {p.action}
                      </button>
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => setNginxFor(t)}
                      >
                        云端配置
                      </button>
                    </span>
                  </div>
                </article>
              );
            })}

            <p className="page-hint">
              密码不会存在本地（只存加密结果），忘了就在这里点「换密码」重新设一个；
              改完密码后，记得重新复制一次云端配置贴到面板里。
            </p>
          </>
        )}
      </div>

      {editFor && (
        <EditTunnelModal
          tunnel={editFor}
          onClose={() => setEditFor(null)}
          onSaved={onRefresh}
        />
      )}
      {nginxFor && (
        <NginxModal
          id={nginxFor.id}
          name={nginxFor.name}
          onClose={() => setNginxFor(null)}
        />
      )}
    </div>
  );
}
