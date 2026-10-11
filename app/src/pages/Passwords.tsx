// 访问保护页：一眼看清每条隧道的门禁状态，缺保护的当场能修。
// 这里只管门禁：网页访问密码、IP 白名单、检查云端是否生效；
// 发布/启停/改名/改网址/改云端端口都在「发布隧道」页。

import { useState } from "react";

import { NginxModal } from "../components/NginxModal";
import { ProtectionModal } from "../components/ProtectionModal";
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

/** 行按钮的文案：按当前设置的组合给一个最合适的说法 */
function actionFor(t: TunnelView): string {
  const { password, ip } = accessFlags(t.access);
  if (password && ip) return "改保护";
  if (password) return "改密码 / 用户名";
  if (ip) return "改白名单";
  return "加保护";
}

export function PasswordsPage({ tunnels, onRefresh }: Props) {
  const [protectFor, setProtectFor] = useState<TunnelView | null>(null);
  const [nginxFor, setNginxFor] = useState<TunnelView | null>(null);

  const warnCount = tunnels.filter(
    (t) => protectionSummary(t).tone === "warn",
  ).length;

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1 className="page-title">访问保护</h1>
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
                    {warnCount === 0 ? (
                      <>
                        {tunnels.length} 条隧道本地设置都齐全。
                        <span className="chip warn" style={{ margin: "0 6px" }}>
                          待核对
                        </span>
                        点每条下面的「检查云端」，云端真的生效了才算数。
                      </>
                    ) : (
                      `有 ${warnCount} 条隧道还没设好保护，网址一泄露谁都能看。`
                    )}
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
                        onClick={() => setProtectFor(t)}
                      >
                        {actionFor(t)}
                      </button>
                    </span>
                  </div>
                  <ProtectionStatus tunnel={t} />
                </article>
              );
            })}

            <p className="page-hint">
              这里的密码是别人打开网页时输的；连服务器用的登录密码在「云端服务器」页。
              密码不会存在本地（只存加密结果），忘了就在这里点「改密码 / 用户名」重新设一个；
              保存成功后会直接打开「云端配置」，把新的几行贴到面板里，再点「检查云端」确认真的生效了。
            </p>
          </>
        )}
      </div>

      {protectFor && (
        <ProtectionModal
          tunnel={protectFor}
          onClose={() => setProtectFor(null)}
          onSaved={() => {
            const saved = protectFor;
            setProtectFor(null);
            // 保存成功后直接打开云端配置：贴到服务器面板里才算真的生效。
            setNginxFor(saved);
            onRefresh();
          }}
        />
      )}
      {nginxFor && (
        <NginxModal
          id={nginxFor.id}
          name={nginxFor.name}
          tunnel={nginxFor}
          onClose={() => setNginxFor(null)}
        />
      )}
    </div>
  );
}
