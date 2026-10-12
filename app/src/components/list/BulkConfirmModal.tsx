// 批量操作确认框（D4 §3.3 BulkConfirmModal）。
// 启动 / 暂停是非破坏性动作（D4 说不用确认），这个弹层实际只用在批量删除上。
//
// 删除的后果用 .danger-line 红块醒目提示（复验 R-B 低：原来的确认文案太不显眼）。
// 这里不复用 ConfirmModal 是因为它只收纯字符串 body，给不了警示块样式。
// 其余行为和 ConfirmModal 对齐：Esc 关闭、Tab 焦点圈定、焦点归还，
// 危险操作初始焦点落在「取消」防误触。

import { useEffect, useId, useRef } from "react";

import { ConfirmModal } from "../ConfirmModal";
import { useModalA11y } from "../useModalA11y";

interface Props {
  action: "start" | "pause" | "remove";
  names: string[];
  /** 删除对象的说法：隧道 / 本机服务 */
  unit: string;
  onConfirm(): void;
  onClose(): void;
}

/** 批量删除确认：红色警示块 + 待删名单 + 取消/删除按钮 */
function BulkRemoveConfirm({
  names,
  unit,
  onConfirm,
  onClose,
}: {
  names: string[];
  unit: string;
  onConfirm(): void;
  onClose(): void;
}) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const titleId = useId();
  useModalA11y(dialogRef, onClose);

  // 危险操作：焦点先落在「取消」，避免一个回车就删掉
  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  const n = names.length;
  const bullets = names.slice(0, 5);
  if (n > 5) bullets.push(`…等共 ${n} 条`);
  const warning =
    unit === "隧道" ? "删除后外部（访客）将立即无法访问" : "如果仍有隧道使用它，程序会阻止该操作";
  const overflow = n > 20 ? "超过 20 条的部分不能撤销。" : "";

  return (
    <div className="modal-backdrop">
      <div
        className="modal confirm-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        ref={dialogRef}
      >
        <div className="modal-head">
          <h2 className="modal-title" id={titleId}>
            删除这 {n} 条{unit}？
          </h2>
        </div>
        <div className="modal-body">
          <div className="danger-line">
            ⚠ {warning}；5 秒内可以撤销。{overflow}
          </div>
          <ul className="modal-bullets">
            {bullets.map((line, index) => (
              <li key={index}>{line}</li>
            ))}
          </ul>
        </div>
        <div className="modal-foot">
          <span className="grow" />
          <button className="btn btn-ghost" ref={cancelRef} onClick={onClose}>
            取消
          </button>
          <button className="btn btn-danger" onClick={onConfirm}>
            删除 {n} 条
          </button>
        </div>
      </div>
    </div>
  );
}

export function BulkConfirmModal({ action, names, unit, onConfirm, onClose }: Props) {
  const n = names.length;
  if (action === "remove") {
    return (
      <BulkRemoveConfirm
        names={names}
        unit={unit}
        onConfirm={onConfirm}
        onClose={onClose}
      />
    );
  }

  return (
    <ConfirmModal
      title={action === "start" ? `开始发布这 ${n} 条？` : `暂停这 ${n} 条？`}
      body={
        action === "start"
          ? "这些隧道会立刻开始连接云端。"
          : "暂停后外部（访客）将无法访问。"
      }
      confirmLabel={action === "start" ? "开始发布" : "暂停发布"}
      onConfirm={onConfirm}
      onClose={onClose}
    />
  );
}
