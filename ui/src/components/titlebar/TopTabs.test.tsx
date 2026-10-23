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

const themeMock = vi.hoisted(() => ({
  terminalMode: "dark" as "light" | "dark",
  /** 测试里临时覆盖的设置值（主机树宽度、是否收起）。 */
  overrides: {} as Record<string, unknown>,
}));

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
      ...themeMock.overrides,
    }),
  useResolvedMode: () => "dark",
  useTerminalMode: () => themeMock.terminalMode,
  useTerminalITheme: () => ({ background: "#1F1F1F", foreground: "#CCCCCC" }),
}));

afterEach(() => {
  cleanup();
  themeMock.overrides = {};
});

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
    // "主机"（对齐主机树时是按钮，否则是标签）也不能拖
    const hostTab = container.querySelector(
      '[data-section="host-page-button"], [data-section="top-tabs-left"] > div',
    );
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

describe("双击标签复制标签页", () => {
  const base = { tree: { kind: "pane" as const, id: "p" }, focus: "p" };
  function renderTabs(extra: Record<string, unknown> = {}) {
    const handlers = {
      onDuplicateTab: vi.fn(),
      onShowVault: vi.fn(),
      onSelectTab: vi.fn(),
      onCloseTab: vi.fn(),
    };
    render(
      <TopTabs
        tabs={[
          { ...base, id: "t", title: "web-1" },
          { ...base, id: "s", title: "文件 web-1", sftp: { hostId: "h" } } as never,
          {
            ...base,
            id: "e",
            title: "notes.txt",
            editor: { editId: "x" },
          } as never,
        ]}
        activeTabId="t"
        panes={{ p: { phase: "connected" } as never }}
        view="terminal"
        onOpenSettings={() => {}}
        onNewTab={() => {}}
        aiOpen={false}
        onToggleAI={() => {}}
        sftpOpen={false}
        onToggleSftp={() => {}}
        {...handlers}
        {...extra}
      />,
    );
    return handlers;
  }

  it("双击终端标签：和右键「复制标签页」一样调 onDuplicateTab", () => {
    const handlers = renderTabs();
    fireEvent.doubleClick(screen.getByText("web-1"));
    expect(handlers.onDuplicateTab).toHaveBeenCalledTimes(1);
    expect(handlers.onDuplicateTab).toHaveBeenCalledWith("t");
    expect(handlers.onCloseTab).not.toHaveBeenCalled();
  });

  it("SFTP 标签、编辑器标签、「主机」标签双击什么都不做", () => {
    const handlers = renderTabs();
    fireEvent.doubleClick(screen.getByText("文件 web-1"));
    fireEvent.doubleClick(screen.getByText("notes.txt"));
    fireEvent.doubleClick(screen.getByText("主机"));
    expect(handlers.onDuplicateTab).not.toHaveBeenCalled();
    expect(handlers.onCloseTab).not.toHaveBeenCalled();
  });

  it("双击关闭按钮：不冒泡成复制", () => {
    const handlers = renderTabs();
    fireEvent.doubleClick(screen.getByLabelText("关闭 web-1"));
    expect(handlers.onDuplicateTab).not.toHaveBeenCalled();
  });

  it("没有提供复制能力时双击无效、不报错", () => {
    renderTabs({ onDuplicateTab: undefined });
    expect(() => fireEvent.doubleClick(screen.getByText("web-1"))).not.toThrow();
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

describe("顶栏左侧块和主机树对齐（UI-5）", () => {
  function renderWithView(view: "vault" | "terminal" | "settings") {
    return render(
      <TopTabs
        tabs={[]}
        activeTabId=""
        panes={{}}
        view={view}
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
  const left = (container: HTMLElement) =>
    container.querySelector<HTMLElement>('[data-section="top-tabs-left"]');

  it("主机树展开：左侧块宽度等于主机树宽度（主机页、终端界面都一样），默认 224", () => {
    for (const view of ["vault", "terminal"] as const) {
      const { container, unmount } = renderWithView(view);
      expect(left(container)?.dataset.aligned).toBe("true");
      expect(left(container)?.style.width).toBe("224px");
      unmount();
    }
  });

  it("宽度变化时左侧块跟着变；存的是脏数据回退 224", () => {
    themeMock.overrides = { "layout.host_tree_width": 300 };
    const first = renderWithView("terminal");
    expect(left(first.container)?.style.width).toBe("300px");
    first.unmount();

    themeMock.overrides = { "layout.host_tree_width": "wide" };
    const second = renderWithView("terminal");
    expect(left(second.container)?.style.width).toBe("224px");
  });

  it("对齐主机树时「主机」是撑满的按钮：主机页点亮，终端界面不点亮；左侧块带分隔线", () => {
    for (const [view, active] of [["vault", "true"], ["terminal", "false"]] as const) {
      const { container, unmount } = renderWithView(view);
      const button = container.querySelector<HTMLElement>('[data-section="host-page-button"]');
      expect(button?.dataset.active).toBe(active);
      expect(left(container)?.className).toContain("border-r");
      unmount();
    }
    // 收起主机树时还是原来的标签
    themeMock.overrides = { "layout.vault_sidebar_collapsed": true };
    const { container } = renderWithView("terminal");
    expect(container.querySelector('[data-section="host-page-button"]')).toBeNull();
    expect(left(container)?.className).not.toContain("border-r");
  });

  it("主机树收起：不设固定宽度，标签紧挨着排", () => {
    themeMock.overrides = { "layout.vault_sidebar_collapsed": true, "layout.host_tree_width": 300 };
    const { container } = renderWithView("terminal");
    expect(left(container)?.dataset.aligned).toBe("false");
    expect(left(container)?.style.width).toBe("");
  });

  it("设置页没有主机树：不设固定宽度", () => {
    const { container } = renderWithView("settings");
    expect(left(container)?.dataset.aligned).toBe("false");
    expect(left(container)?.style.width).toBe("");
  });

  it("logo、开关、「主机」标签都在左侧块里，顶栏仍是拖动区", () => {
    const { container } = renderWithView("vault");
    const block = left(container);
    expect(block?.querySelector("img")).not.toBeNull();
    expect(block?.querySelector('[data-action="toggle-host-sidebar"]')).not.toBeNull();
    expect(block?.textContent).toContain("主机");
    expect(
      container.querySelector('[data-section="top-tabs"]')?.getAttribute("data-tauri-drag-region"),
    ).toBe("deep");
  });
});
