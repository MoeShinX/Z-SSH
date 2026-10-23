/** @vitest-environment jsdom */
//! 快捷命令栏：单击发到当前窗格、Ctrl+单击发到所有分屏；空的时候引导去添加；
//! 只有一个窗格时输入广播开关灰掉；上边框拖动条调高度。

import { cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const settingsSet = vi.hoisted(() => vi.fn(async () => {}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet,
}));

import { isPanelDragging, resetPanelDraggingForTests } from "../../state/panelWidths";
import { useSettings } from "../../stores/settings";
import QuickCommandBar, { type QuickCommandBarProps } from "./QuickCommandBar";
import {
  QUICK_BAR_DEFAULT_HEIGHT,
  quickBarMaxHeight,
  useLiveQuickBarHeight,
} from "./quickBarHeight";

afterEach(cleanup);

const props = (patch: Partial<QuickCommandBarProps> = {}): QuickCommandBarProps => ({
  commands: [
    { id: "a", name: "端口查看", command: "ss -tlnp", enter: true },
    { id: "b", name: "系统更新", command: "apt update", enter: true },
  ],
  groups: [],
  group: "",
  onGroupChange: vi.fn(),
  onRun: vi.fn(),
  onManage: vi.fn(),
  onCreate: vi.fn(),
  onEdit: vi.fn(),
  onDelete: vi.fn(),
  broadcast: false,
  paneCount: 2,
  onToggleBroadcast: vi.fn(),
  ...patch,
});

describe("快捷命令栏", () => {
  it("单击发到当前窗格，Ctrl+单击发到所有分屏", () => {
    const p = props();
    render(<QuickCommandBar {...p} />);
    fireEvent.click(screen.getByText("端口查看"));
    expect(p.onRun).toHaveBeenLastCalledWith(expect.objectContaining({ id: "a" }), false);
    fireEvent.click(screen.getByText("系统更新"), { ctrlKey: true });
    expect(p.onRun).toHaveBeenLastCalledWith(expect.objectContaining({ id: "b" }), true);
  });

  it("双击或右键空白处新建；点在命令按钮上不算", () => {
    const p = props();
    const { container } = render(<QuickCommandBar {...p} />);
    const list = container.querySelector('[data-section="quick-command-list"]') as HTMLElement;
    fireEvent.doubleClick(list);
    expect(p.onCreate).toHaveBeenCalledTimes(1);
    const menu = fireEvent.contextMenu(list);
    expect(p.onCreate).toHaveBeenCalledTimes(2);
    // 空白处右键不弹浏览器自己的菜单
    expect(menu).toBe(false);

    fireEvent.doubleClick(screen.getByText("端口查看"));
    fireEvent.contextMenu(screen.getByText("系统更新"));
    expect(p.onCreate).toHaveBeenCalledTimes(2);
  });

  it("没有命令时引导去添加（直接新建）；「+」照样在", () => {
    const p = props({ commands: [] });
    render(<QuickCommandBar {...p} />);
    fireEvent.click(screen.getByText(/还没有快捷命令/));
    expect(p.onCreate).toHaveBeenCalledTimes(1);
    expect(p.onManage).not.toHaveBeenCalled();
    expect(screen.getByTitle("新建快捷命令")).toBeTruthy();
  });

  it("管理按钮的图标和顶栏的 ⌘ 不一样；「+」在管理按钮旁边、命令按钮之前，点它新建", () => {
    const p = props();
    const { container } = render(<QuickCommandBar {...p} />);
    const manage = screen.getByTitle("管理快捷命令");
    expect(manage.querySelector("svg")?.getAttribute("class")).toContain("lucide-list-checks");
    expect(manage.querySelector("svg")?.getAttribute("class")).not.toContain("lucide-command");
    const create = screen.getByTitle("新建快捷命令");
    expect(create.querySelector("svg")?.getAttribute("class")).toContain("lucide-plus");
    // 固定在左边（不在会滚动 / 换行的命令列表里）
    const list = container.querySelector('[data-section="quick-command-list"]') as HTMLElement;
    expect(list.contains(create)).toBe(false);
    expect(manage.compareDocumentPosition(create) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(create.compareDocumentPosition(list) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    fireEvent.click(create);
    expect(p.onCreate).toHaveBeenCalledTimes(1);
    fireEvent.click(manage);
    expect(p.onManage).toHaveBeenCalledTimes(1);
  });

  it("面板开着时管理按钮显示成按下的样子", () => {
    const { rerender } = render(<QuickCommandBar {...props()} />);
    expect(screen.getByTitle("管理快捷命令").getAttribute("aria-pressed")).toBe("false");
    rerender(<QuickCommandBar {...props({ managing: true })} />);
    expect(screen.getByTitle("管理快捷命令").getAttribute("aria-pressed")).toBe("true");
  });

  it("只有一个窗格时广播开关灰掉；开着时显示广播中", () => {
    const { rerender } = render(<QuickCommandBar {...props({ paneCount: 1 })} />);
    const toggle = () =>
      document.querySelector<HTMLButtonElement>('[data-section="broadcast-toggle"]')!;
    expect(toggle().disabled).toBe(true);
    const p = props({ broadcast: true });
    rerender(<QuickCommandBar {...p} />);
    expect(toggle().textContent).toContain("广播中");
    fireEvent.click(toggle());
    expect(p.onToggleBroadcast).toHaveBeenCalled();
  });
});

describe("命令栏拖动条", () => {
  beforeEach(() => {
    settingsSet.mockClear();
    useSettings.setState({ "layout.quick_bar_height": 28 });
    resetPanelDraggingForTests();
    Element.prototype.setPointerCapture = () => {};
    Element.prototype.hasPointerCapture = () => false;
    Element.prototype.releasePointerCapture = () => {};
  });

  const handle = () =>
    document.querySelector<HTMLElement>('[data-section="quick-bar-resize-handle"]')!;
  const bar = () => document.querySelector<HTMLElement>('[data-section="quick-command-bar"]')!;
  const list = () => document.querySelector<HTMLElement>('[data-section="quick-command-list"]')!;

  it("默认是原来的一行高度，按钮不换行；拖动条光标是上下调整", () => {
    render(<QuickCommandBar {...props()} />);
    expect(bar().style.height).toBe(`${QUICK_BAR_DEFAULT_HEIGHT}px`);
    expect(handle().className).toContain("cursor-ns-resize");
    expect(list().className).not.toContain("flex-wrap");
    expect(list().className).toContain("overflow-x-auto");
  });

  it("往上拖变高、按钮换行并纵向滚动，松手写进设置；拖动期间终端不上报", async () => {
    render(<QuickCommandBar {...props()} />);
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    expect(isPanelDragging()).toBe(true);
    fireEvent.pointerMove(handle(), { clientY: 452, pointerId: 1 });
    expect(bar().style.height).toBe("76px");
    expect(list().className).toContain("flex-wrap");
    expect(list().className).toContain("overflow-y-auto");
    // 拖动中还没写设置
    expect(settingsSet).not.toHaveBeenCalled();
    fireEvent.pointerUp(handle(), { clientY: 452, pointerId: 1 });
    expect(isPanelDragging()).toBe(false);
    await waitFor(() => expect(settingsSet).toHaveBeenCalledWith("layout.quick_bar_height", "76"));
  });

  it("不能拖到比默认更矮，也不能超过上限", () => {
    render(<QuickCommandBar {...props()} />);
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientY: 900, pointerId: 1 });
    expect(bar().style.height).toBe(`${QUICK_BAR_DEFAULT_HEIGHT}px`);
    fireEvent.pointerMove(handle(), { clientY: -5000, pointerId: 1 });
    // jsdom 量不到终端区域，退到窗口高度（768 的 40% 大于 5 行），所以停在 5 行
    expect(bar().style.height).toBe(`${quickBarMaxHeight(window.innerHeight)}px`);
    expect(quickBarMaxHeight(window.innerHeight)).toBe(124);
    fireEvent.pointerUp(handle(), { clientY: -5000, pointerId: 1 });
  });

  it("双击拖动条恢复默认高度", async () => {
    useSettings.setState({ "layout.quick_bar_height": 100 });
    render(<QuickCommandBar {...props()} />);
    expect(bar().style.height).toBe("100px");
    fireEvent.doubleClick(handle());
    await waitFor(() =>
      expect(settingsSet).toHaveBeenCalledWith(
        "layout.quick_bar_height",
        String(QUICK_BAR_DEFAULT_HEIGHT),
      ),
    );
    expect(bar().style.height).toBe(`${QUICK_BAR_DEFAULT_HEIGHT}px`);
  });

  it("存的值非法时回默认；多个命令的容器允许换行", () => {
    useSettings.setState({ "layout.quick_bar_height": 3 });
    const { unmount } = render(<QuickCommandBar {...props()} />);
    expect(bar().style.height).toBe(`${QUICK_BAR_DEFAULT_HEIGHT}px`);
    unmount();
    useSettings.setState({ "layout.quick_bar_height": 100 });
    render(<QuickCommandBar {...props()} />);
    expect(bar().style.height).toBe("100px");
    expect(list().className).toContain("flex-wrap");
    expect(document.querySelectorAll("[data-quick-command]").length).toBe(2);
  });

  it("把实际高度上报给概览面板用，卸载后回默认", () => {
    const live = renderHook(() => useLiveQuickBarHeight());
    expect(live.result.current).toBe(QUICK_BAR_DEFAULT_HEIGHT);
    useSettings.setState({ "layout.quick_bar_height": 100 });
    const { unmount } = render(<QuickCommandBar {...props()} />);
    expect(live.result.current).toBe(100);
    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientY: 480, pointerId: 1 });
    expect(live.result.current).toBe(120);
    fireEvent.pointerUp(handle(), { clientY: 480, pointerId: 1 });
    unmount();
    expect(live.result.current).toBe(QUICK_BAR_DEFAULT_HEIGHT);
  });
});

describe("快捷命令按钮颜色", () => {
  const button = (id: string) => document.querySelector(`[data-quick-command="${id}"]`) as HTMLElement;

  it("有颜色的按钮带淡底色 + 边框 + 彩色图标，文字保持正常前景色", () => {
    render(
      <QuickCommandBar
        {...props({ commands: [{ id: "a", name: "危险", command: "rm -rf x", enter: true, color: "red" }] })}
      />,
    );
    const el = button("a");
    expect(el.className).toContain("bg-red-500/15");
    expect(el.className).toContain("ring-red-500/40");
    expect(el.querySelector("svg")?.getAttribute("class")).toContain("text-red-500");
    expect(el.className).not.toMatch(/text-red/);
  });

  it("没颜色的按钮样式和以前一样", () => {
    render(<QuickCommandBar {...props()} />);
    const el = button("a");
    expect(el.className).toBe(
      "my-0.5 flex max-w-40 shrink-0 items-center gap-1 rounded px-2 py-0.5 hover:bg-accent hover:text-accent-foreground",
    );
    expect(el.querySelector("svg")?.getAttribute("class")).toContain("text-muted-foreground");
    expect(el.querySelector("svg")?.getAttribute("class")).not.toMatch(/text-(red|green|blue)/);
  });
});
