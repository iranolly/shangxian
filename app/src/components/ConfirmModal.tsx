// 通用确认弹窗（D4 §3.4）：替代 window.confirm。
// 由后续 wave 在删除 / 批量 / 向导 Esc / IP 白名单等调用点接入。

import { useEffect, useId, useRef } from "react";

import { useModalA11y } from "./useModalA11y";

export interface ConfirmModalProps {
  title: string;
  body?: string;
  bullets?: string[];
  confirmLabel: string;
  /** 默认 "取消" */
  cancelLabel?: string;
  /** 危险操作（删除等）：按红色按钮样式；初始焦点给「取消」防误触 */
  danger?: boolean;
  /** 可选勾选（如「把我的 IP 也加进去」，默认勾选由调用方传 checked=true） */
  ack?: { label: string; checked: boolean; onChange(next: boolean): void };
  onConfirm(): void;
  onClose(): void;
}

export function ConfirmModal({
  title,
  body,
  bullets,
  confirmLabel,
  cancelLabel = "取消",
  danger = false,
  ack,
  onConfirm,
  onClose,
}: ConfirmModalProps) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const titleId = useId();

  useModalA11y(dialogRef, onClose);

  // 危险操作：焦点先落在「取消」，避免一个回车就删掉
  useEffect(() => {
    if (danger) cancelRef.current?.focus();
  }, [danger]);

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
            {title}
          </h2>
        </div>
        <div className="modal-body">
          {body ? <p className="modal-text">{body}</p> : null}
          {bullets && bullets.length > 0 ? (
            <ul className="modal-bullets">
              {bullets.map((line, index) => (
                <li key={index}>{line}</li>
              ))}
            </ul>
          ) : null}
          {ack ? (
            <label className="switch modal-ack">
              <input
                type="checkbox"
                checked={ack.checked}
                onChange={(e) => ack.onChange(e.target.checked)}
              />
              {ack.label}
            </label>
          ) : null}
        </div>
        <div className="modal-foot">
          <span className="grow" />
          <button className="btn btn-ghost" ref={cancelRef} onClick={onClose}>
            {cancelLabel}
          </button>
          <button
            className={`btn ${danger ? "btn-danger" : "btn-primary"}`}
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
