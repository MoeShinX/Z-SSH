/** @vitest-environment jsdom */
//! 标题栏的拖动区（M1-09）：条本身必须带 `data-tauri-drag-region="deep"`，
//! 可点的东西必须显式 `={false}`。
//!
//! 为什么是 `"deep"` 而不是裸属性：Tauri 的 drag.js 里，裸属性（或 `"true"`）
//! 只认"鼠标正好点在带属性的那个元素本身"（`el === composedPath[0]`），点在子元素
//! 上判定失败。标签滚动容器、按钮之间的空隙都是子元素，所以必须 `deep` 让整棵
//! 子树都算拖动区；可点元素各自 `={false}` 挡回去，点击行为不受影响。
//!
//! 这个用例盯住属性本身，避免以后有人把 `deep` 改回裸属性（那样手测就又拖不动了）。
//! 端到端的"某个坐标点下去算不算拖动"由 `scripts/screenshots/m1-09.mjs` 验证。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import TopTabs from "./TopTabs";

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isMaximized: vi.fn(async () => false),
    minimize: vi.fn(),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(),
  }),
}));

const themeMock = vi.hoisted(() => ({ terminalMode: "dark" as "light" | "dark" }));

// 主题入口用的是 settings store 和 radix dropdown，这里不关心
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
  useTerminalMode: () => themeMock.terminalMode,
  useTerminalITheme: () => ({ background: "#1F1F1F", foreground: "#CCCCCC" }),
}));

afterEach(cleanup);

beforeEach(() => {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

function renderBar() {
  return render(
    <TopTabs
      tabs={[]}
      activeTabId=""
      panes={{}}
      view="vault"
      onShowVault={() => {}}
      onOpenSettings={() => {}}
      onSelectTab={() => {}}
      onCloseTab={() => {}}
      onNewTab={() => {}}
      aiOpen={false}
      onToggleAI={() => {}}
      sftpOpen={false}
      onToggleSftp={() => {}}
    />,
  );
}

describe("标题栏拖动区（M1-09）", () => {
  it("条本身是 deep 拖动区", () => {
    const { container } = renderBar();
    const bar = container.querySelector('[data-section="top-tabs"]');
    expect(bar).not.toBeNull();
    // 必须是 "deep"，裸属性/true 只认点在自己身上
    expect(bar?.getAttribute("data-tauri-drag-region")).toBe("deep");
  });

  it("可点的元素都显式挡掉拖动", () => {
    const { container } = renderBar();
    // 条上所有 button（+、主题、设置、窗口控制）都必须带 =false
    const buttons = Array.from(container.querySelectorAll('[data-section="top-tabs"] button'));
    expect(buttons.length).toBeGreaterThan(0);
    for (const button of buttons) {
      expect(button.getAttribute("data-tauri-drag-region")).toBe("false");
    }
    // "主机"标签也不能拖
    const hostTab = container.querySelector('[data-section="top-tabs"] > div');
    expect(hostTab?.getAttribute("data-tauri-drag-region")).toBe("false");
  });
});

describe("标签右键菜单", () => {
  const tab = (id: string, title: string) => ({
    id,
    title,
    tree: { kind: "pane" as const, id: `p-${id}` },
    focus: `p-${id}`,
  });

  it("终端标签：断开时能重新连接（连着是灰的）、复制、关闭其他 / 右侧 / 所有；最右边的标签不能关右侧", async () => {
    const handlers = {
      onCloseTab: vi.fn(),
      onReconnectTab: vi.fn(),
      onDuplicateTab: vi.fn(),
      onCloseTabs: vi.fn(),
    };
    render(
      <TopTabs
        tabs={[tab("a", "web-1"), tab("b", "db-1")]}
        activeTabId="a"
        panes={{
          "p-a": { phase: "disconnected" } as never,
          "p-b": { phase: "connected" } as never,
        }}
        view="terminal"
        onShowVault={() => {}}
        onOpenSettings={() => {}}
        onSelectTab={() => {}}
        onNewTab={() => {}}
        aiOpen={false}
        onToggleAI={() => {}}
        sftpOpen={false}
        onToggleSftp={() => {}}
        {...handlers}
      />,
    );
    fireEvent.contextMenu(screen.getByText("web-1"));
    fireEvent.click(await screen.findByText("重新连接"));
    expect(handlers.onReconnectTab).toHaveBeenCalledWith("a");

    fireEvent.contextMenu(screen.getByText("web-1"));
    fireEvent.click(await screen.findByText("关闭右侧标签页"));
    expect(handlers.onCloseTabs).toHaveBeenCalledWith("a", "right");

    fireEvent.contextMenu(screen.getByText("db-1"));
    const right = (await screen.findByText("关闭右侧标签页")).closest('[role="menuitem"]');
    expect(right?.getAttribute("data-disabled")).not.toBeNull();
    // 连着的标签：重新连接是灰的
    const again = screen.getByText("重新连接").closest('[role="menuitem"]');
    expect(again?.getAttribute("data-disabled")).not.toBeNull();
    fireEvent.click(screen.getByText("复制标签页"));
    expect(handlers.onDuplicateTab).toHaveBeenCalledWith("b");
  });
});

describe("当前终端标签和终端的衔接", () => {
  function renderActiveTerminal() {
    return render(
      <TopTabs
        tabs={[{ id: "a", title: "web-1", tree: { kind: "pane" as const, id: "p-a" }, focus: "p-a" }]}
        activeTabId="a"
        panes={{ "p-a": { phase: "connected" } as never }}
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
      />,
    );
  }

  it("界面和终端同为深色：借终端底色，主机名仍用界面文字色", () => {
    themeMock.terminalMode = "dark";
    const { container } = renderActiveTerminal();
    const tab = container.querySelector<HTMLElement>('[data-tab-id="a"]');
    expect(tab?.dataset.blend).toBe("terminal");
    expect(tab?.style.color).toBe("");
  });

  it("界面深色、终端浅色：不借终端底色", () => {
    themeMock.terminalMode = "light";
    const { container } = renderActiveTerminal();
    const tab = container.querySelector<HTMLElement>('[data-tab-id="a"]');
    expect(tab?.dataset.blend).toBeUndefined();
    expect(tab?.style.backgroundColor).toBe("");
    themeMock.terminalMode = "dark";
  });
});
