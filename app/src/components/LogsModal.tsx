// 日志弹层：source 区分看的是「发布日志」（隧道）还是「程序日志」（本机服务）。

import { useEffect, useState } from "react";

import { getLogs, getServiceLogs } from "../api";

interface Props {
  /** 要看的对象 id：隧道用隧道 id，本机服务用服务 id */
  id: string;
  name: string;
  /** tunnel = 发布日志（get_logs）；service = 程序日志（get_service_logs） */
  source: "tunnel" | "service";
  onClose: () => void;
}

export function LogsModal({ id, name, source, onClose }: Props) {
  const [text, setText] = useState("读取中…");

  const load = () => {
    const req = source === "tunnel" ? getLogs(id, 300) : getServiceLogs(id, 300);
    req.then(setText).catch((e) => setText(String(e)));
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, source]);

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true">
        <div className="modal-head">
          <h2 className="modal-title">
            {source === "tunnel" ? "发布日志" : "程序日志"} · {name}
          </h2>
          <p className="modal-sub">
            最近 300 行。
            {source === "tunnel"
              ? "这是这条隧道的连接记录，出问题时把这里的内容发出来就能定位。"
              : "这是程序自己打印的内容（还有看护记录），出问题时把这里的内容发出来就能定位。"}
          </p>
        </div>
        <div className="modal-body">
          <pre className="log-view">{text}</pre>
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={load}>
            刷新
          </button>
          <span className="grow" />
          <button className="btn btn-primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
