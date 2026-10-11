// 云端配置弹层：给出要新增的 nginx 几行 + 简短示意，供用户复制到面板。

import { useEffect, useRef, useState } from "react";

import {
  checkTunnelAccess,
  getNginxSnippet,
  type AccessCheck,
  type NginxSnippet,
  type TunnelView,
} from "../api";
import { copyText } from "../copy/clipboard";
import { cloudCheck, cloudLabel } from "./ProtectionStatus";
import { useModalA11y } from "./useModalA11y";

interface Props {
  id: string;
  name: string;
  /** 传进来就能把云端实际配置和本机设置对比；不传只报「云端找到没找到」 */
  tunnel?: TunnelView;
  onClose: () => void;
}

/** 弹层里能复制的内容 */
type CopyTarget = "htpasswd" | `block-${number}`;

/** 同一个隧道上次检查的结果：弹层关了再开还能看到「上次检查」 */
const lastChecks = new Map<string, { check: AccessCheck; at: number }>();

type CheckState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "done"; check: AccessCheck; at: number }
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
      ? { phase: "done", check: cached.check, at: cached.at }
      : { phase: "idle" };
  });
  const timer = useRef<number | null>(null);

  useEffect(() => {
    getNginxSnippet(id)
      .then(setData)
      .catch((e) => setError(String(e)));
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
      const check = await checkTunnelAccess(id);
      const at = Date.now();
      lastChecks.set(id, { check, at });
      setCheckState({ phase: "done", check, at });
    } catch (e) {
      setCheckState({ phase: "error", message: String(e) });
    }
  };

  // 有本机设置就对比；没有就只报云端状态
  const checkResult =
    checkState.phase === "done" && tunnel
      ? cloudCheck(tunnel, checkState.check)
      : null;
  const checkTone = (() => {
    if (checkState.phase === "error") return "warn";
    if (checkState.phase !== "done") return "";
    if (checkResult) return checkResult.tone;
    return checkState.check.confFound ? "ok" : "warn";
  })();
  const statusText = (() => {
    if (checkState.phase === "idle") {
      return "还没检查过云端：贴完上面的配置、保存并重载 nginx 后，点右边「检查云端」看生效没有。";
    }
    if (checkState.phase === "loading") return "正在连服务器检查…";
    if (checkState.phase === "error") return `检查失败：${checkState.message}`;
    const { check, at } = checkState;
    const head = `上次检查 ${timeText(at)}：`;
    if (checkResult) return `${head}${checkResult.text}`;
    if (!check.confFound) {
      return `${head}云端还没找到这条隧道的站点配置（可能站点还没建，或者改了没保存、没重载）。`;
    }
    return `${head}云端已找到站点配置，当前生效：${cloudLabel(check)}。`;
  })();

  // 复制按钮上的字：复制成功后显示"已复制"，1.6 秒后恢复
  const label = (target: CopyTarget, idle: string) =>
    copied === target ? "已复制 ✓" : idle;

  // 把连续的高亮行合成一段：段里至少有一行真正的配置（不是 # 注释）才配小复制按钮；
  // 按钮放在该段首行行尾，复制的是整段原文（含 # 注释行）。
  const blockCopies = new Map<number, { target: CopyTarget; text: string }>();
  {
    const lines = data?.preview ?? [];
    let i = 0;
    let n = 0;
    while (i < lines.length) {
      if (!lines[i].isNew) {
        i += 1;
        continue;
      }
      let j = i;
      while (j < lines.length && lines[j].isNew) j += 1;
      const seg = lines.slice(i, j);
      const hasConfig = seg.some(
        (l) => l.text.trim() !== "" && !l.text.trimStart().startsWith("#"),
      );
      if (hasConfig) {
        blockCopies.set(i, {
          target: `block-${n}`,
          text: seg.map((l) => l.text).join("\n"),
        });
        n += 1;
      }
      i = j;
    }
  }

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
          <p className="modal-sub">
            灰色是原配置的样子，高亮是要加的行：只加高亮的这几行，不要替换整段。
          </p>
        </div>
        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}
          <div className={`nginx-check ${checkTone}`}>
            <span className="nginx-check-text">{statusText}</span>
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
              ⚠ 服务器上还没有密码文件（htpasswd），密码可能进不去。
            </div>
          )}
          {data && (
            <>
              <ol className="steps-list">
                {data.steps.map((s, i) => (
                  <li key={i}>{s}</li>
                ))}
              </ol>
              {data.snippet && (
                <span
                  className="field-label"
                  style={{ display: "inline-block", marginBottom: 6 }}
                >
                  要新增的几行
                </span>
              )}
              <pre className="log-view" style={{ maxHeight: 340, padding: "8px 10px" }}>
                {data.preview?.length
                  ? data.preview.map((l, i) => {
                      const entry = blockCopies.get(i);
                      return (
                        <div
                          key={i}
                          style={{
                            display: "flex",
                            alignItems: "baseline",
                            gap: 8,
                            padding: "0 6px",
                            margin: "0 -6px",
                            borderRadius: 4,
                            color: l.isNew ? "var(--text)" : "var(--text-2)",
                            background: l.isNew
                              ? "var(--accent-soft)"
                              : "transparent",
                            boxShadow: l.isNew
                              ? "inset 2px 0 0 var(--accent)"
                              : undefined,
                          }}
                        >
                          <span style={{ flex: 1, minWidth: 0 }}>
                            {l.text === "" ? "\u00a0" : l.text}
                          </span>
                          {entry && (
                            <button
                              className="link-btn"
                              style={{ flexShrink: 0, padding: "0 4px" }}
                              onClick={() => copy(entry.target, entry.text)}
                            >
                              {label(entry.target, "复制")}
                            </button>
                          )}
                        </div>
                      );
                    })
                  : data.snippet}
              </pre>
              {copyFailed && (
                <div className="error-line" style={{ marginTop: 8 }}>
                  没复制成功：请用鼠标选中上面的内容，按 Ctrl+C 复制。
                </div>
              )}
              {data.htpasswd && (
                <div className="field" style={{ marginTop: 12 }}>
                  <label className="field-label">
                    密码文件内容（htpasswd）
                  </label>
                  <p className="field-hint">
                    别人打开网页时用的密码文件，不是连服务器的登录密码。
                  </p>
                  <div
                    className="success-line"
                    style={{ justifyContent: "flex-start" }}
                  >
                    <span className="secret">{data.htpasswd}</span>
                    <button
                      className="link-btn"
                      onClick={() => copy("htpasswd", data.htpasswd!)}
                    >
                      {label("htpasswd", "复制")}
                    </button>
                  </div>
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
