/** @vitest-environment jsdom */
//! 应用内指针拖动（M3-3）：阈值、落点登记、跨栏落下、落在终端、Esc 取消、善后。

import { afterEach, describe, expect, it, vi } from "vitest";

import type { SftpEntry } from "../api/sftp";
import {
  beginPointerDrag,
  handleExternalDragEvent,
  registerDropTarget,
  registerExternalDropTarget,
  resetPointerDragForTests,
  useDragStore,
  type DragPayload,
} from "./pointerDrag";

const entry = (name: string): SftpEntry => ({
  name,
  path: `/srv/${name}`,
  kind: "file",
  name_is_utf8: true,
  hidden: false,
});

function payload(side: "local" | "remote"): DragPayload {
  return { kind: "sftp-entries", side, entries: [entry("a.txt"), entry("b.txt")] };
}

function pointer(type: string, x: number, y: number) {
  window.dispatchEvent(new MouseEvent(type, { clientX: x, clientY: y, bubbles: true }));
}

function stubPoint(element: Element | null) {
  document.elementFromPoint = () => element;
}

afterEach(() => {
  resetPointerDragForTests();
  document.body.innerHTML = "";
});

describe("beginPointerDrag", () => {
  it("没过阈值松手：不算拖动，不生成 payload，不触发落点", () => {
    const target = document.createElement("div");
    document.body.appendChild(target);
    const onDrop = vi.fn();
    registerDropTarget(target, { accepts: () => true, onDrop });
    const makePayload = vi.fn(() => payload("local"));
    stubPoint(target);
    beginPointerDrag({ clientX: 10, clientY: 10, button: 0 }, makePayload);
    pointer("pointermove", 12, 11);
    pointer("pointerup", 12, 11);
    expect(makePayload).not.toHaveBeenCalled();
    expect(onDrop).not.toHaveBeenCalled();
  });

  it("拖过阈值落在愿意接的落点上：onDrop 拿到 payload 和落点下面的元素", () => {
    const pane = document.createElement("div");
    const row = document.createElement("div");
    row.setAttribute("data-row", "3");
    pane.appendChild(row);
    document.body.appendChild(pane);
    const onDrop = vi.fn();
    registerDropTarget(pane, { accepts: (p) => p.side === "local", onDrop });
    const onStart = vi.fn();
    const onEnd = vi.fn();
    stubPoint(row);
    beginPointerDrag({ clientX: 10, clientY: 10, button: 0 }, () => payload("local"), {
      onStart,
      onEnd,
    });
    pointer("pointermove", 40, 40);
    expect(onStart).toHaveBeenCalledTimes(1);
    expect(useDragStore.getState().hover).toBe(pane);
    expect(document.querySelector('[data-section="drag-ghost"]')?.textContent).toContain("等 2 项");
    pointer("pointerup", 40, 40);
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onDrop.mock.calls[0][0].entries).toHaveLength(2);
    expect(onDrop.mock.calls[0][1].element).toBe(row);
    expect(onEnd).toHaveBeenCalledWith(true);
    // 善后：悬停状态清了、幽灵元素删了
    expect(useDragStore.getState().hover).toBeNull();
    expect(document.querySelector('[data-section="drag-ghost"]')).toBeNull();
  });

  it("落点不接这个 payload（本栏拖到本栏）：不触发", () => {
    const pane = document.createElement("div");
    document.body.appendChild(pane);
    const onDrop = vi.fn();
    registerDropTarget(pane, { accepts: (p) => p.side !== "remote", onDrop });
    stubPoint(pane);
    const onEnd = vi.fn();
    beginPointerDrag({ clientX: 0, clientY: 0, button: 0 }, () => payload("remote"), { onEnd });
    pointer("pointermove", 50, 50);
    expect(useDragStore.getState().hover).toBeNull();
    pointer("pointerup", 50, 50);
    expect(onDrop).not.toHaveBeenCalled();
    expect(onEnd).toHaveBeenCalledWith(false);
  });

  it("远端条目拖到终端区域：终端的落点收到远端条目（插路径用）", () => {
    const terminal = document.createElement("div");
    document.body.appendChild(terminal);
    const received: string[] = [];
    registerDropTarget(terminal, {
      accepts: (p) => p.side === "remote",
      onDrop: (p) => received.push(...p.entries.map((e) => e.path)),
    });
    stubPoint(terminal);
    beginPointerDrag({ clientX: 0, clientY: 0, button: 0 }, () => payload("remote"));
    pointer("pointermove", 30, 30);
    pointer("pointerup", 30, 30);
    expect(received).toEqual(["/srv/a.txt", "/srv/b.txt"]);
  });

  it("Esc 取消：松手也不落下，善后干净", () => {
    const pane = document.createElement("div");
    document.body.appendChild(pane);
    const onDrop = vi.fn();
    registerDropTarget(pane, { accepts: () => true, onDrop });
    stubPoint(pane);
    beginPointerDrag({ clientX: 0, clientY: 0, button: 0 }, () => payload("local"));
    pointer("pointermove", 30, 30);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    pointer("pointerup", 30, 30);
    expect(onDrop).not.toHaveBeenCalled();
    expect(document.querySelector('[data-section="drag-ghost"]')).toBeNull();
    expect(document.body.classList.contains("zz-panel-resizing")).toBe(false);
  });

  it("拖动结束后吞掉紧跟着的那个 click（不改选中状态）", () => {
    const pane = document.createElement("div");
    document.body.appendChild(pane);
    registerDropTarget(pane, { accepts: () => true, onDrop: () => {} });
    stubPoint(pane);
    const clicked = vi.fn();
    pane.addEventListener("click", clicked);
    beginPointerDrag({ clientX: 0, clientY: 0, button: 0 }, () => payload("local"));
    pointer("pointermove", 30, 30);
    pointer("pointerup", 30, 30);
    pane.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(clicked).not.toHaveBeenCalled();
  });

  it("右键不触发拖动", () => {
    const makePayload = vi.fn(() => payload("local"));
    beginPointerDrag({ clientX: 0, clientY: 0, button: 2 }, makePayload);
    pointer("pointermove", 80, 80);
    expect(makePayload).not.toHaveBeenCalled();
  });
});

describe("资源管理器拖进来的文件（Tauri 的拖放事件）", () => {
  it("物理像素坐标按 devicePixelRatio 换成 CSS 像素再找落点；松手时把路径交给落点", () => {
    const pane = document.createElement("div");
    document.body.appendChild(pane);
    const onDrop = vi.fn();
    registerExternalDropTarget(pane, { onDrop });
    let probed: [number, number] | null = null;
    document.elementFromPoint = (x, y) => {
      probed = [x, y];
      return pane;
    };
    handleExternalDragEvent(
      { type: "over", position: { x: 300, y: 150 } },
      1.5,
    );
    expect(probed).toEqual([200, 100]);
    expect(useDragStore.getState().hover).toBe(pane);
    handleExternalDragEvent(
      { type: "drop", paths: ["C:\\a.txt", "C:\\dir"], position: { x: 300, y: 150 } },
      1.5,
    );
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect(onDrop.mock.calls[0][0]).toEqual(["C:\\a.txt", "C:\\dir"]);
    expect(useDragStore.getState().hover).toBeNull();
  });

  it("没有落点的地方松手：什么也不做；离开窗口：清掉高亮", () => {
    const pane = document.createElement("div");
    document.body.appendChild(pane);
    const onDrop = vi.fn();
    registerExternalDropTarget(pane, { onDrop });
    document.elementFromPoint = () => document.body;
    handleExternalDragEvent({ type: "over", position: { x: 5, y: 5 } }, 1);
    expect(useDragStore.getState().hover).toBeNull();
    handleExternalDragEvent({ type: "drop", paths: ["C:\\a"], position: { x: 5, y: 5 } }, 1);
    expect(onDrop).not.toHaveBeenCalled();
    document.elementFromPoint = () => pane;
    handleExternalDragEvent({ type: "over", position: { x: 5, y: 5 } }, 1);
    expect(useDragStore.getState().hover).toBe(pane);
    handleExternalDragEvent({ type: "leave" }, 1);
    expect(useDragStore.getState().hover).toBeNull();
  });
});
