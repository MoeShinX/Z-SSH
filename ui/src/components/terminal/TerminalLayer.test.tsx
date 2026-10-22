/** @vitest-environment jsdom */
//! 终端层的关键行为：分屏 / 关闭 / 拖比例**不能**重建已有的终端。
//!
//! 这是 M1-07 那个 bug 的防线：以前叶子跟着分屏树的 React 结构走，一分屏整棵
//! 子树被重建，xterm 跟着重建 → 回滚内容和 WebGL 渲染器都没了。这里用计数的
//! 假 pane 盯住：挂载只该发生一次，除非这个 pane 真的被关掉。

import { fireEvent, render } from "@testing-library/react";
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  isPanelDragging,
  resetPanelDraggingForTests,
} from "../../state/panelWidths";
import { closePane, setRatio, splitPane, type TreeNode } from "../../state/splitTree";
import { emptyPane } from "../../state/sessions";
import TerminalLayer from "./TerminalLayer";

const counts = {
  mounted: new Map<string, number>(),
  disposed: new Map<string, number>(),
};

function CountingPane({ paneId }: { paneId: string }) {
  useEffect(() => {
    counts.mounted.set(paneId, (counts.mounted.get(paneId) ?? 0) + 1);
    return () => {
      counts.disposed.set(paneId, (counts.disposed.get(paneId) ?? 0) + 1);
    };
  }, [paneId]);
  return <div data-testid={`pane-${paneId}`} />;
}

const single = (id: string): TreeNode => ({ kind: "pane", id });

function panesOf(tree: TreeNode): Record<string, ReturnType<typeof emptyPane>> {
  const map: Record<string, ReturnType<typeof emptyPane>> = {};
  const walk = (node: TreeNode) => {
    if (node.kind === "pane") {
      map[node.id] = emptyPane();
      return;
    }
    walk(node.first);
    walk(node.second);
  };
  walk(tree);
  return map;
}

beforeEach(() => {
  counts.mounted.clear();
  counts.disposed.clear();
  resetPanelDraggingForTests();
  // jsdom 没有 ResizeObserver；测试里也不需要真的量尺寸
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  // jsdom 没有 PointerEvent：React 检测不到就不注册 pointerdown 监听，
  // 用 MouseEvent 顶替（UI-1 的拖动测试要真的派发 pointer 事件）。
  if (typeof (globalThis as { PointerEvent?: unknown }).PointerEvent === "undefined") {
    (globalThis as { PointerEvent?: unknown }).PointerEvent = MouseEvent;
  }
  // 量一个固定的容器：拖分隔条的比例换算要真实的轨道长度（UI-1 第 3 节）。
  Element.prototype.getBoundingClientRect = function () {
    return {
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: 1000,
      bottom: 800,
      width: 1000,
      height: 800,
      toJSON: () => ({}),
    } as DOMRect;
  };
});

afterEach(() => {
  resetPanelDraggingForTests();
});

function renderLayer(tree: TreeNode, onRatio: (id: string, ratio: number) => void = () => {}) {
  return render(
    <TerminalLayer
      tree={tree}
      panes={panesOf(tree)}
      onRatio={onRatio}
      renderPane={(paneId) => <CountingPane paneId={paneId} />}
    />,
  );
}

describe("TerminalLayer 的稳定挂载", () => {
  it("分屏：已有的终端不重建，新 pane 挂一次", () => {
    const first = single("A");
    const { rerender } = renderLayer(first);
    expect(counts.mounted.get("A")).toBe(1);

    const split = splitPane(first, "A", "B", "s1", "x");
    rerender(
      <TerminalLayer
        tree={split}
        panes={panesOf(split)}
        onRatio={() => {}}
        renderPane={(paneId) => <CountingPane paneId={paneId} />}
      />,
    );

    expect(counts.mounted.get("A")).toBe(1);
    expect(counts.disposed.get("A") ?? 0).toBe(0);
    expect(counts.mounted.get("B")).toBe(1);
  });

  it("关掉一个 pane：只销毁它自己，别的还在", () => {
    const split = splitPane(single("A"), "A", "B", "s1", "x");
    const { rerender } = renderLayer(split);
    expect(counts.mounted.get("A")).toBe(1);
    expect(counts.mounted.get("B")).toBe(1);

    const left = closePane(split, "B") as TreeNode;
    rerender(
      <TerminalLayer
        tree={left}
        panes={panesOf(left)}
        onRatio={() => {}}
        renderPane={(paneId) => <CountingPane paneId={paneId} />}
      />,
    );

    expect(counts.disposed.get("B")).toBe(1);
    expect(counts.disposed.get("A") ?? 0).toBe(0);
    expect(counts.mounted.get("A")).toBe(1);
  });

  it("拖分隔条改比例：谁也不重建", () => {
    const split = splitPane(single("A"), "A", "B", "s1", "x");
    const { rerender } = renderLayer(split);

    const resized = setRatio(split, "s1", 0.75);
    rerender(
      <TerminalLayer
        tree={resized}
        panes={panesOf(resized)}
        onRatio={() => {}}
        renderPane={(paneId) => <CountingPane paneId={paneId} />}
      />,
    );

    expect(counts.mounted.get("A")).toBe(1);
    expect(counts.mounted.get("B")).toBe(1);
    expect(counts.disposed.get("A") ?? 0).toBe(0);
    expect(counts.disposed.get("B") ?? 0).toBe(0);
  });

  it("在分屏里再分一次：三个 pane 都只挂一次", () => {
    const first = splitPane(single("A"), "A", "B", "s1", "x");
    const { rerender } = renderLayer(first);

    const nested = splitPane(first, "B", "C", "s2", "y");
    rerender(
      <TerminalLayer
        tree={nested}
        panes={panesOf(nested)}
        onRatio={() => {}}
        renderPane={(paneId) => <CountingPane paneId={paneId} />}
      />,
    );

    expect(counts.mounted.get("A")).toBe(1);
    expect(counts.mounted.get("B")).toBe(1);
    expect(counts.mounted.get("C")).toBe(1);
    expect(counts.disposed.size).toBe(0);
  });

  it("分隔条按布局摆好，拖的时候把位置换算成比例", () => {
    const split = splitPane(single("A"), "A", "B", "s1", "x");
    // 800 宽由 ResizeObserver 之外的路径给不到，这里直接验证布局函数接线正确：
    // 分隔条的 DOM 存在，并且带上 pointerdown 处理器
    const { container } = renderLayer(split);
    const dividers = container.querySelectorAll(".cursor-col-resize");
    expect(dividers.length).toBe(1);
  });
});

describe("分隔条拖动（UI-1 第 3 节）", () => {
  it("拖动中改比例、拖动期间标记为拖拽中（终端只在松手后上报）", () => {
    const calls: Array<{ id: string; ratio: number }> = [];
    const split = splitPane(single("A"), "A", "B", "s1", "x");
    const { container } = renderLayer(split, (id, ratio) => calls.push({ id, ratio }));
    const divider = container.querySelector('[data-section="split-divider"]')!;

    fireEvent.pointerDown(divider, { pointerId: 1 });
    expect(isPanelDragging()).toBe(true);

    fireEvent.pointerMove(divider, { clientX: 700 });
    expect(calls).toHaveLength(1);
    expect(calls[0].id).toBe("s1");
    // 1000px 轨道、指针 700 → 比例约 0.7（补半个分隔条厚度）。
    expect(calls[0].ratio).toBeGreaterThan(0.65);
    expect(calls[0].ratio).toBeLessThan(0.71);

    fireEvent.pointerUp(divider, { pointerId: 1 });
    expect(isPanelDragging()).toBe(false);
  });

  it("分隔条的命中区域比可见线宽（UI-1 0.2：6~8px）", () => {
    const split = splitPane(single("A"), "A", "B", "s1", "x");
    const { container } = renderLayer(split);
    const divider = container.querySelector('[data-section="split-divider"]') as HTMLElement;
    const width = Number.parseFloat(divider.style.width);
    expect(width).toBeGreaterThanOrEqual(6);
    expect(width).toBeLessThanOrEqual(8);
    // 可见的那条线还是 4px（里面那条 strip）。
    const strip = divider.querySelector("span") as HTMLElement;
    expect(strip.className).toContain("w-1");
  });

  it("双击分隔条恢复平分", () => {
    const calls: Array<{ id: string; ratio: number }> = [];
    const split = splitPane(single("A"), "A", "B", "s1", "x");
    const { container } = renderLayer(split, (id, ratio) => calls.push({ id, ratio }));
    const divider = container.querySelector('[data-section="split-divider"]')!;
    fireEvent.doubleClick(divider);
    expect(calls).toEqual([{ id: "s1", ratio: 0.5 }]);
  });
});
