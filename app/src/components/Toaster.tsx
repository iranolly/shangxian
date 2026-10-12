// 右下角 toast 容器（D4 §1.6）。无 props：数据从 toastStore 读。
// error 条目用 role="alert"；hover / 聚焦时暂停倒计时。

import { useRef } from "react";

import {
  toast as toastApi,
  useToasts,
  type Toast,
  type ToastKind,
} from "../store/toastStore";

const ICON: Record<ToastKind, string> = {
  success: "✓",
  info: "i",
  warn: "!",
  error: "✕",
};

function ToastItem({
  item,
  onEnter,
  onLeave,
  onFocus,
  onBlur,
}: {
  item: Toast;
  onEnter: () => void;
  onLeave: () => void;
  onFocus: () => void;
  onBlur: () => void;
}) {
  const action = item.action;
  return (
    <div
      className={`toast ${item.kind}`}
      role={item.kind === "error" ? "alert" : undefined}
      onMouseEnter={onEnter}
      onMouseLeave={onLeave}
      onFocusCapture={onFocus}
      onBlurCapture={onBlur}
    >
      <span className="toast-icon" aria-hidden="true">
        {ICON[item.kind]}
      </span>
      <div className="toast-main">
        <div className="toast-text">
          {item.text}
          {item.count > 1 ? <span className="toast-count">×{item.count}</span> : null}
        </div>
        {item.detail ? <div className="toast-detail">{item.detail}</div> : null}
        {action ? (
          <div className="toast-actions">
            <button
              className="btn btn-ghost btn-sm"
              onClick={() => {
                action.run();
                if (!action.keepOpen) toastApi.dismiss(item.id);
              }}
            >
              {action.label}
            </button>
          </div>
        ) : null}
      </div>
      <button
        className="icon-btn toast-close"
        aria-label="关闭提示"
        onClick={() => toastApi.dismiss(item.id)}
      >
        ✕
      </button>
    </div>
  );
}

export function Toaster() {
  const items = useToasts();
  const hovered = useRef<string | null>(null);
  const focused = useRef<string | null>(null);

  const update = (id: string) => {
    if (hovered.current === id || focused.current === id) toastApi.pause(id);
    else toastApi.resume(id);
  };

  return (
    <div className="toaster" role="region" aria-label="操作提示" aria-live="polite">
      {items.map((item) => (
        <ToastItem
          key={item.id}
          item={item}
          onEnter={() => {
            hovered.current = item.id;
            update(item.id);
          }}
          onLeave={() => {
            if (hovered.current === item.id) hovered.current = null;
            update(item.id);
          }}
          onFocus={() => {
            focused.current = item.id;
            update(item.id);
          }}
          onBlur={() => {
            if (focused.current === item.id) focused.current = null;
            update(item.id);
          }}
        />
      ))}
    </div>
  );
}
