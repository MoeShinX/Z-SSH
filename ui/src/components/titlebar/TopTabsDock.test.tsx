/** @vitest-environment jsdom */
//! SFTP 标签拖到终端标签上停靠（M3-3 第 2 节）：指针拖动，悬停提示，松手停靠，Esc 取消。

import { act, fireEvent, render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { emptyPane, type PaneSession, type Tab } from "../../state/sessions";
import TopTabs from "./TopTabs";

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isMaximized: vi.fn(async () => false),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(),
  }),
}));

vi.mock("../../stores/settings", () => ({
  useSettings: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      ui_theme_mode: "dark",
      ui_skin_light: "paper",
      ui_skin_dark: "graphite",
      terminal_theme: "skin",
      quick_commands_bar: false,
      update: () => {},
    }),
  useResolvedMode: () => "dark",
  useTerminalMode: () => "dark",
  useTerminalITheme: () => ({ background: "#1F1F1F", foreground: "#CCCCCC" }),
}));

beforeEach(() => {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

const tabs: Tab[] = [
  { id: "term-a", title: "主机A", tree: { kind: "pane", id: "pa" }, focus: "pa" },
  { id: "term-b", title: "主机B", tree: { kind: "pane", id: "pb" }, focus: "pb" },
  {
    id: "sftp-a",
    title: "主机A（文件）",
    tree: { kind: "pane", id: "ps" },
    focus: "ps",
    sftp: { sftpId: "s1", hostId: 1, phase: "ready", status: "" },
  },
];
const panes: Record<string, PaneSession> = {
  pa: emptyPane({ kind: "host", hostId: 1, label: "主机A" }),
  pb: emptyPane({ kind: "host", hostId: 2, label: "主机B" }),
  ps: emptyPane(),
};

function setup() {
  const onDock = vi.fn();
  const onHover = vi.fn();
  const verdict = vi.fn((_sftp: string, target: string) =>
    target === "term-a" ? ("ok" as const) : target === "term-b" ? ("other-host" as const) : ("none" as const),
  );
  const view = render(
    <TopTabs
      tabs={tabs}
      activeTabId="sftp-a"
      panes={panes}
      view="terminal"
      onShowVault={() => {}}
      onOpenSettings={() => {}}
      onSelectTab={() => {}}
      onCloseTab={() => {}}
      onNewTab={() => {}}
      aiOpen={false}
      onToggleAI={() => {}}
      sftpOpen={false}
      onToggleSftp={() => {}}
      dockVerdict={verdict}
      onDockSftpTab={onDock}
      onSftpDragHover={onHover}
    />,
  );
  const tabEl = (id: string) => view.container.querySelector(`[data-tab-id="${id}"]`) as HTMLElement;
  /** 指针下面是哪个标签（jsdom 没有布局，直接桩掉）。 */
  const pointOver = (id: string | null) => {
    document.elementFromPoint = () => (id ? tabEl(id) : null);
  };
  return { ...view, onDock, onHover, verdict, tabEl, pointOver };
}

function move(x: number, y = 10) {
  act(() => {
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: x, clientY: y, bubbles: true }));
  });
}

describe("SFTP 标签拖动停靠", () => {
  it("拖到同一台主机的终端标签上松手：触发停靠，带上两个标签的 id", () => {
    const { tabEl, pointOver, onDock, onHover } = setup();
    fireEvent.pointerDown(tabEl("sftp-a"), { button: 0, clientX: 300, clientY: 10 });
    pointOver("term-a");
    move(200);
    expect(onHover).toHaveBeenLastCalledWith({
      sftpTabId: "sftp-a",
      overTabId: "term-a",
      verdict: "ok",
    });
    expect(tabEl("term-a").getAttribute("data-dock-target")).toBe("ok");
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerup", { bubbles: true }));
    });
    expect(onDock).toHaveBeenCalledWith("sftp-a", "term-a");
    expect(onHover).toHaveBeenLastCalledWith(null);
  });

  it("拖到别的主机的终端标签上：没有停靠高亮，松手什么也不做，但通知上层给提示", () => {
    const { tabEl, pointOver, onDock, onHover } = setup();
    fireEvent.pointerDown(tabEl("sftp-a"), { button: 0, clientX: 300, clientY: 10 });
    pointOver("term-b");
    move(250);
    expect(onHover).toHaveBeenLastCalledWith({
      sftpTabId: "sftp-a",
      overTabId: "term-b",
      verdict: "other-host",
    });
    expect(tabEl("term-b").getAttribute("data-dock-target")).toBeNull();
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerup", { bubbles: true }));
    });
    expect(onDock).not.toHaveBeenCalled();
  });

  it("没拖出阈值就松手：算点击，不停靠", () => {
    const { tabEl, pointOver, onDock } = setup();
    fireEvent.pointerDown(tabEl("sftp-a"), { button: 0, clientX: 300, clientY: 10 });
    pointOver("term-a");
    move(302);
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerup", { bubbles: true }));
    });
    expect(onDock).not.toHaveBeenCalled();
  });

  it("按 Esc 取消拖动：松手也不停靠", () => {
    const { tabEl, pointOver, onDock, onHover } = setup();
    fireEvent.pointerDown(tabEl("sftp-a"), { button: 0, clientX: 300, clientY: 10 });
    pointOver("term-a");
    move(200);
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(onHover).toHaveBeenLastCalledWith(null);
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerup", { bubbles: true }));
    });
    expect(onDock).not.toHaveBeenCalled();
  });

  it("终端标签不能被拖（只有 SFTP 标签能停靠）", () => {
    const { tabEl, pointOver, onDock, onHover } = setup();
    fireEvent.pointerDown(tabEl("term-a"), { button: 0, clientX: 100, clientY: 10 });
    pointOver("term-b");
    move(300);
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerup", { bubbles: true }));
    });
    expect(onDock).not.toHaveBeenCalled();
    expect(onHover).not.toHaveBeenCalled();
  });
});
