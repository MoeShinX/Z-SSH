//! 面板拖拽把手与宽度状态的前端测试（UI-1 4.2）：
//! 拖动改变宽度、松手才保存、双击恢复默认、重新挂载读到保存的宽度。
//!
//! 设置表用一个真的 zustand store 顶替（只实现 `usePanelWidth` 用到的那部分），
//! 这样 update 之后的重新渲染和真环境一致。

/** @vitest-environment jsdom */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

vi.mock("../../stores/settings", async () => {
  const { create } = await import("zustand");
  const store = create<Record<string, unknown>>(() => ({
    "layout.ai_panel_width": 400,
    "layout.aside_panel_width": 380,
    "layout.host_tree_width": 224,
    update: (key: string, value: unknown) => {
      store.setState({ [key]: value });
      return Promise.resolve();
    },
  }));
  return { useSettings: store };
});

import {
  hostTreeRightReserved,
  setHostTreeRightReserved,
  type PanelKind,
} from "../../state/panelWidths";
import { useSettings } from "../../stores/settings";
import {
  ResizeHandle,
  useHostTreeWidth,
  usePanelWidth,
  type PanelWidthKey,
} from "./resize-handle";

/** 假设置表里读一个键。 */
function stored(key: PanelWidthKey): unknown {
  return (useSettings as unknown as { getState: () => Record<string, unknown> }).getState()[
    key
  ];
}

/** 写入假设置表（模拟上次留下的值）。 */
function seed(key: PanelWidthKey, value: unknown): void {
  (useSettings as unknown as { setState: (s: Record<string, unknown>) => void }).setState({
    [key]: value,
  });
}

/** 用和真面板一样的接线搭一个测试壳。 */
function Harness({
  kind = "ai",
  storageKey = "layout.ai_panel_width",
  fallback = 400,
  side,
}: {
  kind?: PanelKind;
  storageKey?: PanelWidthKey;
  fallback?: number;
  side?: "left" | "right";
}) {
  const { width, bounds, onResize, onCommit, onReset } = usePanelWidth(
    kind,
    storageKey,
    fallback,
  );
  return (
    <div>
      <div data-testid="panel" style={{ width: `${width}px` }}>
        {width}
      </div>
      <ResizeHandle
        width={width}
        bounds={bounds}
        onResize={onResize}
        onCommit={onCommit}
        onReset={onReset}
        side={side}
      />
    </div>
  );
}

function handle(): HTMLElement {
  return screen.getByRole("separator");
}

afterEach(() => {
  cleanup();
  setHostTreeRightReserved(0);
  seed("layout.ai_panel_width", 400);
  seed("layout.aside_panel_width", 380);
  seed("layout.host_tree_width", 224);
});

describe("面板拖拽把手", () => {
  it("拖动改变宽度，松手才保存（拖动中不写设置）", () => {
    render(<Harness />);
    expect(screen.getByTestId("panel").textContent).toBe("400");

    // 把手在左边缘：往左拖 100px → 宽度 +100。
    fireEvent.pointerDown(handle(), { clientX: 500, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientX: 400, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("500");
    expect(stored("layout.ai_panel_width")).toBe(400); // 拖动中不写

    fireEvent.pointerUp(handle(), { clientX: 400, pointerId: 1 });
    expect(stored("layout.ai_panel_width")).toBe(500); // 松手才写
  });

  it("拖动过程夹取在上下限内", () => {
    render(<Harness />);
    // 往右拖 1000px → 打到最小 320。
    fireEvent.pointerDown(handle(), { clientX: 100, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientX: 1100, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("320");
    // 往左拖到天上去 → 打到上限（jsdom 窗口宽 1024 → 60% = 614）。
    fireEvent.pointerMove(handle(), { clientX: -5000, pointerId: 1 });
    expect(Number(screen.getByTestId("panel").textContent)).toBe(Math.floor(1024 * 0.6));
    fireEvent.pointerUp(handle(), { clientX: -5000, pointerId: 1 });
  });

  it("双击恢复默认宽度并保存", () => {
    seed("layout.ai_panel_width", 560);
    render(<Harness />);
    expect(screen.getByTestId("panel").textContent).toBe("560");

    fireEvent.doubleClick(handle());
    expect(screen.getByTestId("panel").textContent).toBe("400");
    expect(stored("layout.ai_panel_width")).toBe(400);
  });

  it("重新挂载时读到保存的宽度（侧栏用 380 默认值）", () => {
    seed("layout.aside_panel_width", 480);
    const first = render(
      <Harness kind="aside" storageKey="layout.aside_panel_width" fallback={380} />,
    );
    expect(first.getByTestId("panel").textContent).toBe("480");
    first.unmount();

    // 再挂一次（模拟重启后重挂载）：还是 480。
    render(<Harness kind="aside" storageKey="layout.aside_panel_width" fallback={380} />);
    expect(screen.getByTestId("panel").textContent).toBe("480");
  });

  it("把手命中区域 8px、可见高亮条 2px（UI-1 0.2）", () => {
    render(<Harness />);
    expect(handle().className).toContain("w-2"); // 8px 命中区
    const strip = handle().querySelector("span") as HTMLElement;
    expect(strip.className).toContain("w-0.5"); // 2px 可见条
  });

  it("存的是脏数据时回退默认值", () => {
    seed("layout.ai_panel_width", -5);
    render(<Harness />);
    expect(screen.getByTestId("panel").textContent).toBe("400");
  });
});

describe("右边缘把手（主机树，UI-5）", () => {
  const tree = () => (
    <Harness kind="hostTree" storageKey="layout.host_tree_width" fallback={224} side="right" />
  );

  it("往右拖变宽、往左拖变窄，松手才保存", () => {
    render(tree());
    expect(screen.getByTestId("panel").textContent).toBe("224");

    fireEvent.pointerDown(handle(), { clientX: 224, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientX: 304, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("304");
    expect(stored("layout.host_tree_width")).toBe(224); // 拖动中不写

    fireEvent.pointerMove(handle(), { clientX: 204, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("204");

    fireEvent.pointerUp(handle(), { clientX: 204, pointerId: 1 });
    expect(stored("layout.host_tree_width")).toBe(204); // 松手才写
  });

  it("拖动夹取在 180 和上限之间（jsdom 窗口宽 1024 → 40% = 409）", () => {
    render(tree());
    fireEvent.pointerDown(handle(), { clientX: 224, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientX: -1000, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("180");
    fireEvent.pointerMove(handle(), { clientX: 5000, pointerId: 1 });
    expect(Number(screen.getByTestId("panel").textContent)).toBe(Math.floor(1024 * 0.4));
    fireEvent.pointerUp(handle(), { clientX: 5000, pointerId: 1 });
    expect(stored("layout.host_tree_width")).toBe(Math.floor(1024 * 0.4));
  });

  it("双击恢复默认 224 并保存", () => {
    seed("layout.host_tree_width", 300);
    render(tree());
    expect(screen.getByTestId("panel").textContent).toBe("300");
    fireEvent.doubleClick(handle());
    expect(screen.getByTestId("panel").textContent).toBe("224");
    expect(stored("layout.host_tree_width")).toBe(224);
  });

  it("存的是脏数据时回退 224", () => {
    seed("layout.host_tree_width", "wide");
    render(tree());
    expect(screen.getByTestId("panel").textContent).toBe("224");
  });

  it("把手骑在右边框上，并标明在右边", () => {
    render(tree());
    expect(handle().getAttribute("data-side")).toBe("right");
    expect(handle().className).toContain("-right-1");
    expect(handle().className).not.toContain("left-0");
  });

  it("左边缘把手的方向不受影响（往左拖变宽）", () => {
    render(<Harness />);
    expect(handle().getAttribute("data-side")).toBe("left");
    fireEvent.pointerDown(handle(), { clientX: 500, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientX: 450, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("450");
    fireEvent.pointerUp(handle(), { clientX: 450, pointerId: 1 });
  });
});

describe("主机树给右边开着的面板让位（SFTP-9）", () => {
  const tree = () => (
    <Harness kind="hostTree" storageKey="layout.host_tree_width" fallback={224} side="right" />
  );

  it("右边没有面板：和以前一样（jsdom 窗口宽 1024，上限 409）", () => {
    seed("layout.host_tree_width", 400);
    render(tree());
    expect(screen.getByTestId("panel").textContent).toBe("400");
  });

  it("右边开着一个面板：上限 = 窗口 - 360 - 320 = 344，已存的更宽值被收窄", () => {
    seed("layout.host_tree_width", 400);
    act(() => setHostTreeRightReserved(hostTreeRightReserved(false, true)));
    render(tree());
    expect(screen.getByTestId("panel").textContent).toBe("344");
  });

  it("AI 和 SFTP 都开：拖不到会把右边挤掉的宽度，下限仍是 180 不会被挤成 0", () => {
    act(() => setHostTreeRightReserved(hostTreeRightReserved(true, true)));
    render(tree());
    // 1024 - 360 - 640 < 180：上限抬到下限，主机树就是 180
    expect(screen.getByTestId("panel").textContent).toBe("180");
    fireEvent.pointerDown(handle(), { clientX: 180, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientX: 5000, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("180");
    fireEvent.pointerMove(handle(), { clientX: -5000, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("180");
    fireEvent.pointerUp(handle(), { clientX: 180, pointerId: 1 });
  });

  it("拖动时被右边面板的占用挡住：只能拖到 344", () => {
    act(() => setHostTreeRightReserved(hostTreeRightReserved(true, false)));
    render(tree());
    fireEvent.pointerDown(handle(), { clientX: 224, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientX: 5000, pointerId: 1 });
    expect(screen.getByTestId("panel").textContent).toBe("344");
    fireEvent.pointerUp(handle(), { clientX: 5000, pointerId: 1 });
    expect(stored("layout.host_tree_width")).toBe(344);
  });

  it("主机页、终端界面、顶栏三处读到同一个宽度，面板开关时一起变", () => {
    seed("layout.host_tree_width", 400);
    const seen: Record<string, number> = {};
    function Probe({ name }: { name: string }) {
      const { width } = useHostTreeWidth();
      seen[name] = width;
      return <span data-testid={name}>{width}</span>;
    }
    render(
      <>
        <Probe name="vault" />
        <Probe name="terminal" />
        <Probe name="titlebar" />
      </>,
    );
    expect([seen.vault, seen.terminal, seen.titlebar]).toEqual([400, 400, 400]);

    // 终端界面打开 SFTP 面板
    act(() => setHostTreeRightReserved(hostTreeRightReserved(false, true)));
    expect([seen.vault, seen.terminal, seen.titlebar]).toEqual([344, 344, 344]);

    // 再开 AI：180
    act(() => setHostTreeRightReserved(hostTreeRightReserved(true, true)));
    expect([seen.vault, seen.terminal, seen.titlebar]).toEqual([180, 180, 180]);

    // 回主机页（App 把占用清零）：恢复
    act(() => setHostTreeRightReserved(0));
    expect([seen.vault, seen.terminal, seen.titlebar]).toEqual([400, 400, 400]);
  });
});
