// 云端配置弹层：给出要新增的 nginx 几行 + 简短示意，供用户复制到面板。

import { useEffect, useRef, useState } from "react";

import { getNginxSnippet, type NginxSnippet } from "../api";
import { copyText } from "../copy/clipboard";

interface Props {
  id: string;
  name: string;
  onClose: () => void;
}

/** 弹层里能复制的内容 */
type CopyTarget = "snippet" | "exempt" | "htpasswd";

export function NginxModal({ id, name, onClose }: Props) {
  const [data, setData] = useState<NginxSnippet | null>(null);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState<CopyTarget | null>(null);
  const [copyFailed, setCopyFailed] = useState(false);
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

  // 复制按钮上的字：复制成功后显示"已复制"，1.6 秒后恢复
  const label = (target: CopyTarget, idle: string) =>
    copied === target ? "已复制 ✓" : idle;

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" style={{ width: 720 }}>
        <div className="modal-head">
          <h2 className="modal-title">云端配置 · {name}</h2>
          <p className="modal-sub">
            把新增的几行加进你现有的站点配置里（不是替换整段）：灰色是原配置的样子，
            高亮是新增内容该放的位置。
          </p>
        </div>
        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}
          {data && (
            <>
              <ol className="steps-list">
                {data.steps.map((s, i) => (
                  <li key={i}>{s}</li>
                ))}
              </ol>
              {data.snippet && (
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    margin: "0 0 6px",
                  }}
                >
                  <span className="field-label" style={{ marginBottom: 0 }}>
                    要新增的几行
                  </span>
                  <button
                    className="btn btn-ghost btn-sm"
                    onClick={() => copy("snippet", data.snippet!)}
                  >
                    {label("snippet", "复制这段")}
                  </button>
                </div>
              )}
              <pre className="log-view" style={{ maxHeight: 340, padding: "8px 10px" }}>
                {data.preview?.length
                  ? data.preview.map((l, i) => (
                      <div
                        key={i}
                        style={{
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
                        {l.text === "" ? "\u00a0" : l.text}
                      </div>
                    ))
                  : data.snippet}
              </pre>
              {copyFailed && (
                <div className="error-line" style={{ marginTop: 8 }}>
                  没复制成功：请用鼠标选中上面的内容，按 Ctrl+C 复制。
                </div>
              )}
              {data.acmeExempt && (
                <div className="field" style={{ marginTop: 12 }}>
                  <label className="field-label">
                    证书自动续期用的一行（放进 location /.well-known/ 里）
                  </label>
                  <div
                    className="success-line"
                    style={{ justifyContent: "flex-start" }}
                  >
                    <span className="secret">{data.acmeExempt}</span>
                    <button
                      className="link-btn"
                      onClick={() => copy("exempt", data.acmeExempt!)}
                    >
                      {label("exempt", "复制")}
                    </button>
                  </div>
                </div>
              )}
              {data.htpasswd && (
                <div className="field" style={{ marginTop: 12 }}>
                  <label className="field-label">
                    密码文件内容（htpasswd）
                  </label>
                  <p className="field-hint">
                    这是别人打开网页时用的密码文件，不是「云端服务器」页里连服务器的登录密码。
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
