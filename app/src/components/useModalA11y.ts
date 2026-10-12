// 弹层 / 菜单的键盘与焦点处理（公共 hook）。
// 弹层：Esc 关闭、Tab 焦点圈在弹层里、打开时焦点进弹层、关闭后焦点还给打开它的控件；
// 菜单：点到菜单外面、或按 Esc 时关闭。
// 多个弹层叠着时（成功屏再开「云端配置」），只有最上面那个响应 Esc。

import { useEffect, useRef, type RefObject } from "react";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** 弹层里现在能聚焦的元素（跳过被隐藏的） */
function focusablesIn(node: HTMLElement): HTMLElement[] {
  return Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => el.offsetParent !== null,
  );
}

/** 当前打开的弹层：最后一个是叠在最上面的那个 */
const dialogStack: HTMLElement[] = [];

let modalTitleSeq = 0;

/**
 * 弹层没有 aria-labelledby 时，找到里面的 .modal-title（没有 id 就补一个），
 * 让 aria-labelledby 指过去。返回这次补上的 id（用于卸载时还原）。
 */
function labelModal(node: HTMLElement): string | null {
  if (node.getAttribute("aria-labelledby")) return null;
  const title = node.querySelector<HTMLElement>(".modal-title");
  if (!title) return null;
  if (!title.id) title.id = `modal-title-${++modalTitleSeq}`;
  node.setAttribute("aria-labelledby", title.id);
  return title.id;
}

/**
 * 弹层通用的无障碍行为：Esc 关闭、Tab 焦点圈定、焦点进出弹层、aria-labelledby 指向标题。
 * ref 指向弹层根节点（.modal 那个 div）。
 */
export function useModalA11y<T extends HTMLElement>(
  ref: RefObject<T | null>,
  onClose: () => void,
) {
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  });

  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    // 打开弹层前焦点在哪：关掉后要还回去
    const opener =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    dialogStack.push(node);
    const labelledById = labelModal(node);

    // 焦点先放进弹层：没有能聚焦的控件时让弹层自身可聚焦（tabindex=-1）
    if (!node.contains(document.activeElement)) {
      const first = focusablesIn(node)[0];
      if (first) {
        first.focus();
      } else {
        if (!node.hasAttribute("tabindex")) node.tabIndex = -1;
        node.focus();
      }
    }

    const onKeyDown = (e: KeyboardEvent) => {
      // 只有最上面的弹层响应键盘
      if (dialogStack[dialogStack.length - 1] !== node) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCloseRef.current();
        return;
      }
      if (e.key !== "Tab") return;
      const list = focusablesIn(node);
      if (list.length === 0) {
        e.preventDefault();
        return;
      }
      const first = list[0];
      const last = list[list.length - 1];
      const active = document.activeElement;
      if (e.shiftKey) {
        if (active === first || !node.contains(active)) {
          e.preventDefault();
          last.focus();
        }
      } else if (active === last || !node.contains(active)) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      const i = dialogStack.indexOf(node);
      if (i >= 0) dialogStack.splice(i, 1);
      // 只回收自己加的 aria-labelledby；调用方本来就写好的不动
      if (labelledById && node.getAttribute("aria-labelledby") === labelledById) {
        node.removeAttribute("aria-labelledby");
      }
      if (opener && document.body.contains(opener)) opener.focus();
    };
  }, [ref]);
}

/**
 * 卡片上「⋯」菜单的关闭行为：点击菜单外面、或按 Esc 就关。
 * ref 指向把触发按钮和菜单包在一起的那个锚点元素。
 */
export function useDismissOnOutside<T extends HTMLElement>(
  ref: RefObject<T | null>,
  open: boolean,
  onClose: () => void,
) {
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  });

  useEffect(() => {
    if (!open) return;
    const node = ref.current;
    if (!node) return;
    const onPointerDown = (e: PointerEvent) => {
      if (!node.contains(e.target as Node)) onCloseRef.current();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onCloseRef.current();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open, ref]);
}
