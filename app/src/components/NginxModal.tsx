// 云端配置弹层：生成 nginx 片段 + 密码文件内容，供用户复制到面板。

import { useEffect, useState } from "react";

import { getNginxSnippet, type NginxSnippet } from "../api";

interface Props {
  id: string;
  name: string;
  onClose: () => void;
}

export function NginxModal({ id, name, onClose }: Props) {
  const [data, setData] = useState<NginxSnippet | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    getNginxSnippet(id)
      .then(setData)
      .catch((e) => setError(String(e)));
  }, [id]);

  const copy = (text: string) => {
    void navigator.clipboard.writeText(text);
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" style={{ width: 720 }}>
        <div className="modal-head">
          <h2 className="modal-title">云端配置 · {name}</h2>
          <p className="modal-sub">
            复制下面的片段，粘到小皮面板里这个域名的站点配置；保存后重载 nginx。
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
              <pre className="log-view" style={{ maxHeight: 300 }}>
                {data.snippet}
              </pre>
              {data.htpasswd && (
                <div className="field" style={{ marginTop: 12 }}>
                  <label className="field-label">密码文件内容</label>
                  <div
                    className="success-line"
                    style={{ justifyContent: "flex-start" }}
                  >
                    <span className="secret">{data.htpasswd}</span>
                    <button
                      className="link-btn"
                      onClick={() => copy(data.htpasswd!)}
                    >
                      复制
                    </button>
                  </div>
                </div>
              )}
            </>
          )}
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            关闭
          </button>
          <span className="grow" />
          {data && (
            <button className="btn btn-primary" onClick={() => copy(data.snippet)}>
              复制片段
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
