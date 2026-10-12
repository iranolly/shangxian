// 卡片上的云端检查提醒（D4 §3.5 CloudDriftBar）：
// - drift：云端配置和设置对不上 → 黄条 + 「打开云端配置 / 暂时不管」；
// - error：云端核不上 → 黄条 + 「再检查一次 / 暂时不管」；
// - HTTPS：读 protectionCheck.extra.https → 红/黄一行 + 「看怎么办」（红灯可「我暂时不管」）。
// 纪律：「暂时不管」只记在本次会话（模块级 Set），不写 ui.dismissed，
// 免得把「访问保护」页那份完整提醒也一起压掉。

import { useState } from "react";

import { actions } from "../../store/actions";
import { useTunnel } from "../../store/useStore";
import { httpsLight, readExtra } from "../ProtectionStatus";

/** 漂移字段 → 人话（说法与「访问保护」页完整版保持一致） */
const DRIFT_TEXT: Record<string, string> = {
  conf: "云端没找到这条隧道的站点配置",
  password: "密码开关不一样",
  ip: "IP 白名单开关不一样",
  allowIps: "白名单里的 IP 不一样",
  htpasswd: "服务器上还没有密码文件",
};

function driftText(fields: string[]): string {
  const list = fields.map((f) => DRIFT_TEXT[f] ?? f).join("、");
  return list || "云端和设置对不上";
}

/** 本次会话里点过「暂时不管」的提醒，键：`{tunnelId}:{drift|error|https}` */
const ignoredThisSession = new Set<string>();

interface Props {
  tunnelId: string;
  /** 打开这条隧道的「云端配置」弹层（漂移条 / HTTPS 行上的按钮） */
  onOpenNginx?: (id: string) => void;
}

export function CloudDriftBar({ tunnelId, onOpenNginx }: Props) {
  const tunnel = useTunnel(tunnelId);
  const [, setVersion] = useState(0);

  if (!tunnel) return null;

  const ignored = (kind: "drift" | "error" | "https") =>
    ignoredThisSession.has(`${tunnelId}:${kind}`);
  const ignore = (kind: "drift" | "error" | "https") => {
    ignoredThisSession.add(`${tunnelId}:${kind}`);
    setVersion((v) => v + 1);
  };

  const check = tunnel.protectionCheck;
  const { https } = readExtra(check);
  const light = httpsLight(tunnel, https);

  return (
    <>
      {check?.status === "drift" && !ignored("drift") ? (
        <div className="drift-bar">
          <span>
            云端和设置对不上：{driftText(check.driftFields ?? [])}。重新贴一次云端配置。
          </span>
          {onOpenNginx ? (
            <button className="link-btn" onClick={() => onOpenNginx(tunnelId)}>
              打开云端配置
            </button>
          ) : null}
          <button className="link-btn" onClick={() => ignore("drift")}>
            暂时不管
          </button>
        </div>
      ) : null}

      {check?.status === "error" && !ignored("error") ? (
        <div className="drift-bar">
          <span>云端核不上：{check.error ?? "不知道原因"}</span>
          <button
            className="link-btn"
            onClick={() => void actions.checkCloud(tunnelId)}
          >
            再检查一次
          </button>
          <button className="link-btn" onClick={() => ignore("error")}>
            暂时不管
          </button>
        </div>
      ) : null}

      {light && !ignored("https") ? (
        <div className={light.level === "red" ? "error-line" : "warn-line"}>
          {light.level === "red" ? "红灯： " : "注意： "}
          {light.text}
          {onOpenNginx ? (
            <>
              {" "}
              <button className="link-btn" onClick={() => onOpenNginx(tunnelId)}>
                看怎么办
              </button>
            </>
          ) : null}
          {light.level === "red" ? (
            <>
              {" "}
              <button className="link-btn" onClick={() => ignore("https")}>
                我暂时不管
              </button>
            </>
          ) : null}
        </div>
      ) : null}
    </>
  );
}
