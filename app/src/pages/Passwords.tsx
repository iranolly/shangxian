// 访问密码页：一眼看清每条隧道的访问保护状态，缺保护的当场能修。

import { useState } from "react";

import { EditTunnelModal } from "../components/EditTunnelModal";
import { NginxModal } from "../components/NginxModal";
import {
  ProtectionStatus,
  accessFlags,
  protectionSummary,
} from "../components/ProtectionStatus";
import type { TunnelView } from "../api";

interface Props {
  tunnels: TunnelView[];
  onRefresh: () => void;
}

/** 编辑按钮的文案：按当前设置的组合给一个最合适的说法 */
function actionFor(t: TunnelView): string {
  const { password, ip } = accessFlags(t.access);
  if (password && ip) return t.hasPassword ? "修改保护" : "设密码";
  if (password) return t.hasPassword ? "换密码" : "设密码";
  if (ip) return "改白名单";
  return "加保护";
}

export function PasswordsPage({ tunnels, onRefresh }: Props) {
  const [editFor, setEditFor] = useState<TunnelView | null>(null);
  const [nginxFor, setNginxFor] = useState<TunnelView | null>(null);

  const warnCount = tunnels.filter(
    (t) => protectionSummary(t).tone === "warn",
  ).length;

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">访问密码</h1>
          <p className="page-sub">
            给每条隧道加一道门：要密码、只让指定的人进，两种保护可以同时开。
          </p>
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
                      : `有 ${warnCount} 条隧道还没设好保护，网址一泄露谁都能看。`}
                  </div>
                </div>
              </div>
            </article>

            {tunnels.map((t) => {
              const ipOn = accessFlags(t.access).ip;
              return (
                <article className="card" key={t.id}>
                  <div className="server-card">
                    <div className="server-main">
                      <div className="server-name">{t.name}</div>
                      <div className="server-meta">
                        {t.url ?? "还没填公网网址"}
                        {ipOn && t.accessDetail
                          ? ` · 允许：${t.accessDetail}`
                          : ""}
                      </div>
                    </div>
                    <span className="row-actions">
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => setEditFor(t)}
                      >
                        {actionFor(t)}
                      </button>
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => setNginxFor(t)}
                      >
                        云端配置
                      </button>
                    </span>
                  </div>
                  <ProtectionStatus tunnel={t} />
                </article>
              );
            })}

            <p className="page-hint">
              密码不会存在本地（只存加密结果），忘了就在这里点「换密码」重新设一个；
              改完密码后，记得重新复制一次云端配置贴到面板里，再点「检查云端」确认真的生效了。
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
