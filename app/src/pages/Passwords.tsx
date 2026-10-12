// 访问保护页：一眼看清每条隧道的门禁状态，缺保护的当场能修。
// 这里只管门禁：网页访问密码、IP 白名单、检查云端是否生效、访问记录；
// 发布/启停/改名/改网址/改云端端口都在「发布隧道」页。

import { useState } from "react";

import { NginxModal } from "../components/NginxModal";
import { ProtectionModal } from "../components/ProtectionModal";
import {
  ProtectionStatus,
  accessFlags,
  protectionSummary,
} from "../components/ProtectionStatus";
import { runAccessAudit, type AccessAudit, type TunnelView } from "../api";
import { useTunnel } from "../store/useStore";

interface Props {
  tunnels: TunnelView[];
  onRefresh: () => void;
}

/** 「打开云端配置 / 看怎么办」：跳「发布隧道」页开弹层，那边按 id 取最新数据 */
function openNginxInTunnels(t: TunnelView): void {
  window.dispatchEvent(
    new CustomEvent("shangxian:nav-intent", {
      detail: { page: "tunnels", openNginxFor: t.id },
    }),
  );
}

/** 云端和设置对不上时，把机器码翻成人话（D4 §3.5）。 */
const DRIFT_FIELD_TEXT: Record<string, string> = {
  conf: "云端没找到这条隧道的站点配置",
  password: "密码开关不一样",
  ip: "IP 白名单开关不一样",
  allowIps: "白名单里的 IP 不一样",
  htpasswd: "服务器上还没有密码文件",
};

type AuditState =
  | { phase: "loading" }
  | { phase: "done"; data: AccessAudit }
  | { phase: "error"; message: string };

/** 行按钮的文案：按当前设置的组合给一个最合适的说法 */
function actionFor(t: TunnelView): string {
  const { password, ip } = accessFlags(t.access);
  if (password && ip) return "改保护";
  if (password) return "改密码 / 用户名";
  if (ip) return "改白名单";
  return "加保护";
}

/** 待核对 = 没查过 / unknown / 上次核对超过 24 小时（D2 §6.4 展示规则） */
function protectionPending(t: TunnelView): boolean {
  const p = t.protectionCheck;
  if (!p || p.status === "unknown") return true;
  if (p.status !== "ok") return false;
  if (typeof p.ts !== "number") return true;
  return Date.now() - p.ts > 24 * 60 * 60 * 1000;
}

function AuditView({ data }: { data: AccessAudit }) {
  const blocked = data.count401 + data.count403;
  // 防弹：后端任何一段缺字段都不能把整个界面带崩（曾因此黑屏）。
  const topIps = data.topIps ?? [];
  const warnings = data.warnings ?? [];
  const entries = data.entries ?? [];
  const top = topIps[0];
  return (
    <>
      {!data.available ? (
        <p className="audit-summary">
          没找到访问日志（也可能日志只有 root 能看），这一步跳过。
        </p>
      ) : blocked === 0 ? (
        <p className="audit-summary">
          最近 {data.window} 行日志里没有被挡的访问，看起来没人乱试。
        </p>
      ) : (
        <p className="audit-summary">
          最近 {data.window} 行日志里，密码错误被挡了 {data.count401} 次，白名单被挡了{" "}
          {data.count403} 次。
          {top ? `次数最多的是 ${top.ip}（${top.count} 次）。` : ""}
        </p>
      )}
      {topIps.length > 1 && (
        <p className="audit-note">
          被挡最多的几个：
          {topIps
            .slice(0, 5)
            .map((item) => `${item.ip}（${item.count} 次）`)
            .join("、")}
        </p>
      )}
      {data.available && (
        <p className="audit-note">这些记录只在你电脑上显示，不会上传到任何地方。</p>
      )}
      {data.truncated && <p className="audit-note">记录太多，只看了最近的一部分。</p>}
      {data.path && <p className="audit-note mono">日志文件：{data.path}</p>}
      {warnings.map((w, index) => (
        <p className="audit-note" key={index}>
          ⚠ {w}
        </p>
      ))}
      {entries.length > 0 && (
        <div className="audit-entries">
          {entries.slice(-10).map((e, index) => (
            <span key={index}>
              {e.raw ??
                [e.time, e.ip, e.method, e.path, e.status].filter(Boolean).join(" ")}
            </span>
          ))}
        </div>
      )}
    </>
  );
}

export function PasswordsPage({ tunnels, onRefresh }: Props) {
  // 弹层只存 id，宿组件内部订阅 store 取最新数据（Tunnels / Services 同款）
  const [protectForId, setProtectForId] = useState<string | null>(null);
  const [nginxForId, setNginxForId] = useState<string | null>(null);
  const [ignoredDrift, setIgnoredDrift] = useState<Record<string, boolean>>({});
  const [auditFor, setAuditFor] = useState<string | null>(null);
  const [audits, setAudits] = useState<Record<string, AuditState>>({});

  const warnCount = tunnels.filter(
    (t) => protectionSummary(t).tone === "warn",
  ).length;
  const driftCount = tunnels.filter(
    (t) => t.protectionCheck?.status === "drift",
  ).length;
  const pendingCount = tunnels.filter(protectionPending).length;

  const toggleAudit = async (t: TunnelView) => {
    if (auditFor === t.id) {
      setAuditFor(null);
      return;
    }
    setAuditFor(t.id);
    setAudits((m) => ({ ...m, [t.id]: { phase: "loading" } }));
    try {
      const data = await runAccessAudit(t.id);
      setAudits((m) => ({ ...m, [t.id]: { phase: "done", data } }));
    } catch (e) {
      setAudits((m) => ({ ...m, [t.id]: { phase: "error", message: String(e) } }));
    }
  };

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
                  <div className="server-name">访问保护一览</div>
                  <div className="server-meta">
                    {driftCount > 0
                      ? `有 ${driftCount} 条隧道云端和设置对不上，重新贴一次云端配置就好。`
                      : warnCount > 0
                        ? `有 ${warnCount} 条隧道还没设好保护，网址一泄露谁都能看。`
                        : `${tunnels.length} 条隧道本地设置都齐全。点每条下面的「检查云端」，云端真的生效了才算数。`}
                  </div>
                  {(pendingCount > 0 || driftCount > 0) && (
                    <div className="row-actions" style={{ marginTop: 8 }}>
                      {pendingCount > 0 && (
                        <span className="chip warn">{pendingCount} 条待核对</span>
                      )}
                      {driftCount > 0 && (
                        <span className="chip warn">
                          {driftCount} 条要重新贴云端配置
                        </span>
                      )}
                    </div>
                  )}
                </div>
              </div>
            </article>

            {tunnels.map((t) => {
              const ipOn = accessFlags(t.access).ip;
              const protection = t.protectionCheck;
              const driftShown = protection?.status === "drift" && !ignoredDrift[t.id];
              const auditState = audits[t.id];
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
                        onClick={() => setProtectForId(t.id)}
                      >
                        {actionFor(t)}
                      </button>
                      <button
                        className="btn btn-ghost btn-sm"
                        onClick={() => void toggleAudit(t)}
                      >
                        {auditFor === t.id ? "收起访问记录" : "查看访问记录"}
                      </button>
                    </span>
                  </div>
                  <ProtectionStatus tunnel={t} onOpenNginx={openNginxInTunnels} />

                  {driftShown && protection && (
                    <div className="drift-bar">
                      <p>
                        云端和设置对不上：
                        {(protection.driftFields ?? [])
                          .map((field) => DRIFT_FIELD_TEXT[field] ?? field)
                          .join("、") || "有些地方不一样"}
                        。重新贴一次云端配置。
                      </p>
                      <div className="drift-actions">
                        <button className="link-btn" onClick={() => setNginxForId(t.id)}>
                          打开云端配置
                        </button>
                        <button
                          className="link-btn"
                          onClick={() =>
                            setIgnoredDrift((m) => ({ ...m, [t.id]: true }))
                          }
                        >
                          暂时不管
                        </button>
                      </div>
                    </div>
                  )}

                  {protection?.status === "error" && (
                    <div className="drift-bar error">
                      <p>云端核不上：{protection.error ?? "没查到原因"}。点「检查云端」再试一次。</p>
                    </div>
                  )}

                  {protectionPending(t) && (
                    <p className="protect-result warn">
                      还没核对过云端，点「检查云端」才算数。
                    </p>
                  )}

                  {auditFor === t.id && (
                    <div className="audit-box">
                      <p className="audit-head">访问记录（次数是大概数）</p>
                      {auditState?.phase === "loading" && (
                        <p className="audit-note">正在读访问记录…</p>
                      )}
                      {auditState?.phase === "error" && (
                        <p className="audit-summary">
                          ⚠ 读不到访问记录：{auditState.message}
                        </p>
                      )}
                      {auditState?.phase === "done" && <AuditView data={auditState.data} />}
                    </div>
                  )}
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

      {protectForId && (
        <ProtectionHost
          id={protectForId}
          onClose={() => setProtectForId(null)}
          onSaved={() => {
            const savedId = protectForId;
            setProtectForId(null);
            // 保存成功后直接打开云端配置：贴到服务器面板里才算真的生效。
            setNginxForId(savedId);
            onRefresh();
          }}
        />
      )}
      {nginxForId && (
        <NginxHost id={nginxForId} onClose={() => setNginxForId(null)} />
      )}
    </div>
  );
}

// ---------- 弹层宿主：只存 id，渲染时取最新实体（D4 §1.1 铁律 1） ----------

function ProtectionHost({
  id,
  onClose,
  onSaved,
}: {
  id: string;
  onClose(): void;
  onSaved(): void;
}) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return (
    <ProtectionModal tunnel={tunnel} onClose={onClose} onSaved={onSaved} />
  );
}

function NginxHost({ id, onClose }: { id: string; onClose(): void }) {
  const tunnel = useTunnel(id);
  if (!tunnel) return null;
  return (
    <NginxModal id={id} name={tunnel.name} tunnel={tunnel} onClose={onClose} />
  );
}
