// 云端配置弹层：给出要新增的 nginx 几行，供用户复制到面板。
//
// 极简版（2026-10-12）：一句话说明 + 一个复制块。
// - 要加的行合并成一块（访问密码 / 白名单 / 预设 / 证书续期豁免，注释里写清放哪儿）；
// - 不再有预览高亮、分段复制按钮和重复的「证书续期」块；
// - 只保留真正要报警的 ⚠ 提示和检查结果（红灯/黄灯、密码文件缺失等）。

import { useEffect, useRef, useState } from "react";

import {
  checkTunnelAccess,
  checkTunnelCloud,
  getNginxSnippet,
  type AccessCheck,
  type CloudCheckResult,
  type NginxSnippet,
  type TunnelView,
} from "../api";
import { copyText } from "../copy/clipboard";
import { errorText } from "../store/commands";
import {
  cloudCheck,
  cloudLabel,
  httpsLight,
  readExtra,
} from "./ProtectionStatus";
import { useModalA11y } from "./useModalA11y";

interface Props {
  id: string;
  name: string;
  /** 传进来就能把云端实际配置和本机设置对比；不传只报「云端找到没找到」 */
  tunnel?: TunnelView;
  onClose(): void;
}

/** 弹层里能复制的内容 */
type CopyTarget = "lines" | "htpasswd";

const EMPTY_ACCESS: AccessCheck = {
  confFound: false,
  passwordOn: false,
  ipOn: false,
  allowIps: [],
  htpasswdExists: false,
};

/** 同一个隧道上次检查的结果：弹层关了再开还能看到「上次检查」 */
const lastChecks = new Map<
  string,
  { actual: AccessCheck; cloud: CloudCheckResult | null; at: number }
>();

type CheckState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "done"; actual: AccessCheck; cloud: CloudCheckResult | null; at: number }
  | { phase: "error"; message: string };

function timeText(at: number): string {
  return new Date(at).toLocaleTimeString("zh-CN", { hour12: false });
}

export function NginxModal({ id, name, tunnel, onClose }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const [data, setData] = useState<NginxSnippet | null>(null);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState<CopyTarget | null>(null);
  const [copyFailed, setCopyFailed] = useState(false);
  const [checkState, setCheckState] = useState<CheckState>(() => {
    const cached = lastChecks.get(id);
    return cached
      ? {
          phase: "done",
          actual: cached.actual,
          cloud: cached.cloud,
          at: cached.at,
        }
      : { phase: "idle" };
  });
  const timer = useRef<number | null>(null);

  useEffect(() => {
    getNginxSnippet(id)
      .then(setData)
      .catch((e) => setError(errorText(e)));
  }, [id]);

  // 弹层关掉时清掉计时器：别把"已复制"的提示带到别的地方
  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);

  const copy = async (target: CopyTarget, text: string) => {
    const ok = await copyText(text);
    setCopyFailed(!ok);
    setCopied(ok ? target : null);
    if (timer.current !== null) window.clearTimeout(timer.current);
    if (ok) {
      timer.current = window.setTimeout(() => setCopied(null), 1600);
    }
  };

  /** 去云端读一遍 nginx 实际配置，看贴好没有 */
  const runCheck = async () => {
    setCheckState({ phase: "loading" });
    try {
      const cloud = await checkTunnelCloud(id, "full");
      const actual =
        cloud.actual ?? cloud.check.cloud ?? EMPTY_ACCESS;
      const at = Date.now();
      lastChecks.set(id, { actual, cloud, at });
      setCheckState({ phase: "done", actual, cloud, at });
    } catch {
      // 新命令没上线：退回旧 check_tunnel_access
      try {
        const actual = await checkTunnelAccess(id);
        const at = Date.now();
        lastChecks.set(id, { actual, cloud: null, at });
        setCheckState({ phase: "done", actual, cloud: null, at });
      } catch (e) {
        setCheckState({ phase: "error", message: errorText(e) });
      }
    }
  };

  // 有本机设置就对比；没有就只报云端状态
  const checkResult =
    checkState.phase === "done" && tunnel
      ? cloudCheck(tunnel, checkState.actual)
      : null;
  const checkTone = (() => {
    if (checkState.phase === "error") return "warn";
    if (checkState.phase !== "done") return "";
    if (checkResult) return checkResult.tone;
    return checkState.actual.confFound ? "ok" : "warn";
  })();
  const statusText = (() => {
    if (checkState.phase === "idle") return "尚未检查云端。";
    if (checkState.phase === "loading") return "连接服务器检查中…";
    if (checkState.phase === "error") return `检查失败：${checkState.message}`;
    const { actual, at } = checkState;
    const head = `上次检查 ${timeText(at)}：`;
    if (checkResult) return `${head}${checkResult.text}`;
    if (!actual.confFound) {
      return `${head}云端尚未找到站点配置（可能未保存或未重载）。`;
    }
    return `${head}云端已找到站点配置，当前生效：${cloudLabel(actual)}。`;
  })();

  // 检查结果里的额外提示（D3 §3/§4/§8/§9）
  const cloud = checkState.phase === "done" ? checkState.cloud : null;
  const freshCheck = cloud?.check ?? null;
  const { https } = readExtra(freshCheck);
  const light = tunnel ? httpsLight(tunnel, https) : null;
  const portExposed = cloud?.port?.bound === "exposed";

  // 复制按钮上的字：复制成功后显示"已复制"，1.6 秒后恢复
  const label = (target: CopyTarget, idle: string) =>
    copied === target ? "已复制 ✓" : idle;

  // 要加的行合成一块：配置片段 + 证书续期豁免。每段自带注释写清放哪儿，
  // 用户复制一次、按注释分别贴进对应位置。
  const linesToAdd = [data?.snippet, data?.acmeExempt]
    .filter((s): s is string => !!s && s.trim() !== "")
    .join("\n\n");

  // 生成器给的 ⚠ 警告照常显示（域名不对、白名单为空、没有访问保护之类）；
  // 长篇操作步骤不再占地方。
  const alertSteps = (data?.steps ?? []).filter((s) =>
    s.trimStart().startsWith("⚠"),
  );

  return (
    <div className="modal-backdrop">
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        ref={dialogRef}
        style={{ width: 720 }}
      >
        <div className="modal-head">
          <h2 className="modal-title">云端配置 · {name}</h2>
          <p className="modal-sub">将以下配置行加入站点配置，保存后重载 nginx。</p>
        </div>
        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}
          <div className={`nginx-check ${checkTone}`}>
            <span
              className="nginx-check-text"
              title="粘贴配置、保存并重载 nginx 后，点击「检查云端」查看是否生效。"
            >
              {statusText}
            </span>
            <button
              className="link-btn"
              disabled={checkState.phase === "loading"}
              onClick={() => void runCheck()}
            >
              {checkState.phase === "loading" ? "检查中…" : "检查云端"}
            </button>
          </div>
          {checkResult?.htpasswdMissing && (
            <div className="warn-line">
              ⚠ 服务器上还没有密码文件，密码可能无法使用。
            </div>
          )}
          {light && (
            <div
              className={light.level === "red" ? "error-line" : "warn-line"}
              title={light.hint}
            >
              {light.level === "red" ? "红灯： " : "注意： "}
              {light.text}
            </div>
          )}
          {portExposed && (
            <div
              className="warn-line"
              title="发布仅绑定 127.0.0.1（仅服务器自身可连接的地址），说明该端口被其他程序占用，请检查或关闭防火墙。"
            >
              云端端口对所有网卡开放，已被其他程序占用。
            </div>
          )}
          {cloud?.clientIp && (
            <div
              className="field-hint hint"
              title="更换网络后可能变化；运营商共享 IP 时，加入白名单可能不生效。"
            >
              服务器看到的访问 IP 是 {cloud.clientIp}。
            </div>
          )}

          {data && (
            <>
              {linesToAdd !== "" && (
                <>
                  <div className="optional-head">
                    <span className="field-label" style={{ marginBottom: 0 }}>
                      要添加的配置行
                    </span>
                    <button
                      className="link-btn"
                      onClick={() => copy("lines", linesToAdd)}
                    >
                      {label("lines", "复制")}
                    </button>
                  </div>
                  <pre
                    className="log-view"
                    style={{ maxHeight: 340, padding: "8px 10px" }}
                    title="每段注释标明位置：① 放在 server_name 下方；② 放入已有的 location /.well-known/ { } 中（已有相同行则跳过）。"
                  >
                    {linesToAdd}
                  </pre>
                </>
              )}

              {alertSteps.map((s, i) => (
                <div className="warn-line" key={i}>
                  {s}
                </div>
              ))}

              {data.htpasswd && (
                <div className="field" style={{ marginTop: 12 }}>
                  <div className="optional-head">
                    <span className="field-label" style={{ marginBottom: 0 }}>
                      密码文件内容
                    </span>
                    <button
                      className="link-btn"
                      onClick={() => copy("htpasswd", data.htpasswd!)}
                    >
                      {label("htpasswd", "复制")}
                    </button>
                  </div>
                  <div
                    className="success-line"
                    style={{ justifyContent: "flex-start" }}
                  >
                    <span className="secret">{data.htpasswd}</span>
                  </div>
                  <p
                    className="field-hint hint"
                    title="网页访问使用的密码文件，不是登录密码；路径即上述配置中 auth_basic_user_file 指定的路径。"
                  >
                    保存到服务器配置中指定的 .htpasswd 路径（没有则新建）。
                  </p>
                </div>
              )}

              {copyFailed && (
                <div className="error-line" style={{ marginTop: 8 }}>
                  复制失败：请选中内容后按 Ctrl+C。
                </div>
              )}
            </>
          )}
        </div>
        <div className="modal-foot">
          <span className="grow" />
          <button className="btn btn-primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
