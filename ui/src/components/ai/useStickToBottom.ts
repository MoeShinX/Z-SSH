//! 消息区跟着输出往下滚：停在底部附近时，内容变高（流式输出、展开工具结果）就滚到底；
//! 用户往上翻离开底部后不再打扰，回到底部（或点"回到最新"）后恢复跟随。

import { useCallback, useEffect, useRef, useState } from "react";

/** 离底部多近算"在底部"：留一点余量，滚轮停在差几像素的位置也算。 */
export const STICK_THRESHOLD_PX = 40;

export function isNearBottom(
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
  threshold = STICK_THRESHOLD_PX,
): boolean {
  return scrollHeight - scrollTop - clientHeight <= threshold;
}

/**
 * `rootRef` 挂在 ScrollArea 上（真正滚动的是 Radix 的 viewport，在它里面），
 * `contentRef` 挂在消息列表的容器上。返回是否在跟随，以及"滚到底并恢复跟随"。
 */
export function useStickToBottom() {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const stickRef = useRef(true);
  const [sticking, setSticking] = useState(true);

  const viewport = useCallback(
    () =>
      rootRef.current?.querySelector<HTMLElement>("[data-radix-scroll-area-viewport]") ?? null,
    [],
  );

  const scrollToBottom = useCallback(() => {
    stickRef.current = true;
    setSticking(true);
    const el = viewport();
    if (el) el.scrollTop = el.scrollHeight;
  }, [viewport]);

  useEffect(() => {
    const el = viewport();
    if (!el) return;
    const onScroll = () => {
      const near = isNearBottom(el.scrollTop, el.clientHeight, el.scrollHeight);
      stickRef.current = near;
      setSticking(near);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [viewport]);

  useEffect(() => {
    const content = contentRef.current;
    // jsdom 里没有 ResizeObserver
    if (!content || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (!stickRef.current) return;
      const el = viewport();
      if (el) el.scrollTop = el.scrollHeight;
    });
    observer.observe(content);
    return () => observer.disconnect();
  }, [viewport]);

  return { rootRef, contentRef, sticking, scrollToBottom };
}
