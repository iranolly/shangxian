// 卡片上的「⋯」菜单：无障碍版（role=menu / menuitem，方向键 + Home/End + Esc 回焦触发按钮）。
// D4 §3.3 CardMenu / §6.3。隧道卡片与「本机服务」卡片共用。

import { Fragment, useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";

export interface MenuItemSpec {
  label: string;
  run(): void;
  danger?: boolean;
  disabled?: boolean;
  title?: string;
  /** 这一项之前画一条分隔线 */
  dividerBefore?: boolean;
}

interface Props {
  items: MenuItemSpec[];
  label?: string;
  /** 触发按钮整体禁用（同步操作进行中时用） */
  disabled?: boolean;
}

export function MenuButton({ items, label = "更多操作", disabled }: Props) {
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLSpanElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // 点击菜单外面 / Esc → 关闭；Esc 还要把焦点还给触发按钮
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (!anchorRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      setOpen(false);
      triggerRef.current?.focus();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open]);

  // 打开后焦点进第一个可用项
  useEffect(() => {
    if (!open) return;
    menuRef.current?.querySelector<HTMLButtonElement>('button[role="menuitem"]:not([disabled])')?.focus();
  }, [open]);

  const onMenuKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const list = Array.from(
      menuRef.current?.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]:not([disabled])') ?? [],
    );
    if (list.length === 0) return;
    const current = document.activeElement;
    const index = list.findIndex((el) => el === current);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      list[(index + 1 + list.length) % list.length].focus();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      list[(index - 1 + list.length) % list.length].focus();
    } else if (e.key === "Home") {
      e.preventDefault();
      list[0].focus();
    } else if (e.key === "End") {
      e.preventDefault();
      list[list.length - 1].focus();
    }
  };

  return (
    <span className="menu-anchor" ref={anchorRef}>
      <button
        ref={triggerRef}
        className="icon-btn"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
      >
        ⋯
      </button>
      {open ? (
        <div className="menu" role="menu" ref={menuRef} onKeyDown={onMenuKeyDown}>
          {items.map((item) => (
            <Fragment key={item.label}>
              {item.dividerBefore ? <div className="menu-divider" role="separator" /> : null}
              <button
                role="menuitem"
                className={`menu-item${item.danger ? " danger" : ""}`}
                disabled={item.disabled}
                title={item.title}
                onClick={() => {
                  setOpen(false);
                  item.run();
                }}
              >
                {item.label}
              </button>
            </Fragment>
          ))}
        </div>
      ) : null}
    </span>
  );
}
