// 自绘标题栏：拖拽移动、双击最大化、右上角三个窗口按钮。
// 最大化按钮的命中测试在 Rust 侧（Windows 11 贴靠布局要用），
// 所以它的悬停高亮由 Rust 事件回传。

import { useEffect, useRef, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

const inTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function reportMaxRect(el: HTMLElement | null) {
  if (!el || !inTauri) return;
  const r = el.getBoundingClientRect();
  void invoke("set_maximize_rect", {
    x: r.left,
    y: r.top,
    w: r.width,
    h: r.height,
  });
}

export function TitleBar() {
  const maxRef = useRef<HTMLButtonElement | null>(null);
  const [maximized, setMaximized] = useState(false);
  const [maxHover, setMaxHover] = useState(false);

  useEffect(() => {
    if (!inTauri) return;
    const win = getCurrentWindow();
    void win.isMaximized().then(setMaximized);

    const unlistenResized = win.onResized(() => {
      void win.isMaximized().then(setMaximized);
      reportMaxRect(maxRef.current);
    });
    const unlistenHover = listen<boolean>("titlebar-max-hover", (e) =>
      setMaxHover(e.payload),
    );

    return () => {
      void unlistenResized.then((f) => f());
      void unlistenHover.then((f) => f());
    };
  }, []);

  useEffect(() => {
    reportMaxRect(maxRef.current);
    const onResize = () => reportMaxRect(maxRef.current);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const win = getCurrentWindow();

  return (
    <div className="titlebar" data-tauri-drag-region>
      <span className="titlebar-spacer" data-tauri-drag-region />
      <div className="titlebar-btns">
        <button
          className="tb-btn"
          aria-label="最小化"
          onClick={() => void win.minimize()}
        >
          <svg viewBox="0 0 10 10" aria-hidden="true">
            <line x1="1" y1="5.5" x2="9" y2="5.5" />
          </svg>
        </button>
        <button
          ref={maxRef}
          className={`tb-btn ${maxHover ? "hover" : ""}`}
          aria-label={maximized ? "还原" : "最大化"}
          onClick={() => void win.toggleMaximize()}
        >
          {maximized ? (
            <svg viewBox="0 0 10 10" aria-hidden="true">
              <rect x="1" y="3" width="6" height="6" />
              <path d="M3.5 3V1h5.5v5.5H7" />
            </svg>
          ) : (
            <svg viewBox="0 0 10 10" aria-hidden="true">
              <rect x="1.2" y="1.2" width="7.6" height="7.6" />
            </svg>
          )}
        </button>
        <button
          className="tb-btn tb-close"
          aria-label="关闭"
          onClick={() => void win.close()}
        >
          <svg viewBox="0 0 10 10" aria-hidden="true">
            <path d="M1.2 1.2l7.6 7.6M8.8 1.2L1.2 8.8" />
          </svg>
        </button>
      </div>
    </div>
  );
}
