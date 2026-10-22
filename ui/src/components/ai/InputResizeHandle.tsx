//! AI 面板输入框上沿的拖动条：上下拖改输入框高度，双击恢复默认。
//!
//! 和面板宽度的把手一样：拖动中只改视觉高度，松手才写回设置表；用 pointer capture，
//! 鼠标快速划出面板也不会断。

import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

import { useSettings } from "../../stores/settings";
import {
  AI_INPUT_DEFAULT_HEIGHT,
  clampInputHeight,
  dragInputHeight,
  sanitizeStoredInputHeight,
} from "./inputHeight";

/** 面板的实际高度（上限要用）；量不到时退到窗口高度。 */
function usePanelHeight(panelRef: RefObject<HTMLElement | null>): number {
  const [height, setHeight] = useState(() =>
    typeof window === "undefined" ? 800 : window.innerHeight,
  );
  useEffect(() => {
    const element = panelRef.current;
    if (!element) return;
    const measure = () => {
      const h = element.getBoundingClientRect().height;
      setHeight(h > 0 ? h : window.innerHeight);
    };
    measure();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", measure);
      return () => window.removeEventListener("resize", measure);
    }
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [panelRef]);
  return height;
}

/** 输入框高度：设置表里读（非法值回默认）、按面板高度夹取、拖动时本地覆盖。 */
export function useAiInputHeight(panelRef: RefObject<HTMLElement | null>) {
  const stored = useSettings((state) => state["layout.ai_input_height"]);
  const update = useSettings((state) => state.update);
  const panelHeight = usePanelHeight(panelRef);
  const [dragHeight, setDragHeight] = useState<number | null>(null);
  const height = dragHeight ?? clampInputHeight(sanitizeStoredInputHeight(stored), panelHeight);
  const onCommit = useCallback(
    (next: number) => {
      setDragHeight(null);
      void update("layout.ai_input_height", next);
    },
    [update],
  );
  const onReset = useCallback(() => {
    setDragHeight(null);
    void update("layout.ai_input_height", AI_INPUT_DEFAULT_HEIGHT);
  }, [update]);
  return { height, panelHeight, onResize: setDragHeight, onCommit, onReset };
}

export function InputResizeHandle({
  height,
  panelHeight,
  onResize,
  onCommit,
  onReset,
}: {
  height: number;
  panelHeight: number;
  onResize: (height: number) => void;
  onCommit: (height: number) => void;
  onReset: () => void;
}) {
  const start = useRef<{ y: number; height: number } | null>(null);
  const latest = useRef(height);
  const heightRef = useRef(height);
  const panelRef = useRef(panelHeight);
  heightRef.current = height;
  panelRef.current = panelHeight;

  const finish = useCallback(() => {
    if (!start.current) return;
    start.current = null;
    document.body.style.userSelect = "";
    onCommit(latest.current);
  }, [onCommit]);

  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      title="拖动调整输入框高度（双击恢复默认）"
      data-section="input-resize-handle"
      className="group absolute inset-x-0 top-0 z-10 h-2 -translate-y-1/2 cursor-ns-resize"
      onPointerDown={(event) => {
        event.preventDefault();
        start.current = { y: event.clientY, height: heightRef.current };
        latest.current = heightRef.current;
        // 拖动中不选中文字（光标靠 pointer capture 保持在拖动条上的 ns-resize）。
        document.body.style.userSelect = "none";
        try {
          (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        } catch {
          // jsdom 里没有 pointer capture：不影响逻辑。
        }
      }}
      onPointerMove={(event) => {
        const origin = start.current;
        if (!origin) return;
        const next = dragInputHeight(origin.height, origin.y, event.clientY, panelRef.current);
        if (next !== latest.current) {
          latest.current = next;
          onResize(next);
        }
      }}
      onPointerUp={finish}
      onPointerCancel={finish}
      onDoubleClick={() => {
        latest.current = AI_INPUT_DEFAULT_HEIGHT;
        onReset();
      }}
    >
      <span
        aria-hidden
        className="pointer-events-none absolute inset-x-0 top-1/2 h-0.5 -translate-y-1/2 bg-transparent transition-colors group-hover:bg-primary/60 group-active:bg-primary/80"
      />
    </div>
  );
}
