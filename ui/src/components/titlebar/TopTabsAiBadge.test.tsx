/** @vitest-environment jsdom */
//! 标签上的 AI 活动提示（AI-SCOPE-1 第 4 节）：执行中是闪动的紫点，跑完没看过是静态紫点，
//! 和连接状态点是两个不同的元素。

import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import TopTabs from "./TopTabs";
import { emptyPane, type Tab } from "../../state/sessions";

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

afterEach(() => cleanup());

const tab = (id: string): Tab => ({
  id,
  title: id,
  tree: { kind: "pane", id: `p-${id}` },
  focus: `p-${id}`,
});

function renderBar(aiBadges: Record<string, "running" | "unseen">) {
  const tabs = [tab("a"), tab("b"), tab("c")];
  const panes = Object.fromEntries(tabs.map((t) => [`p-${t.id}`, emptyPane()]));
  return render(
    <TopTabs
      tabs={tabs}
      activeTabId="a"
      panes={panes}
      view="terminal"
      onShowVault={() => {}}
      onOpenSettings={() => {}}
      onSelectTab={() => {}}
      onCloseTab={() => {}}
      onNewTab={() => {}}
      aiBadges={aiBadges}
      aiOpen={false}
      onToggleAI={() => {}}
      sftpOpen={false}
      onToggleSftp={() => {}}
    />,
  );
}

describe("标签上的 AI 活动点", () => {
  it("running 闪动、unseen 静态，没有提示的标签没有点", () => {
    const { container } = renderBar({ b: "running", c: "unseen" });
    const badge = (id: string) =>
      container.querySelector<HTMLElement>(`[data-tab-id="${id}"] [data-ai-badge]`);
    expect(badge("a")).toBeNull();
    expect(badge("b")?.dataset.aiBadge).toBe("running");
    expect(badge("b")?.className).toContain("animate-pulse");
    expect(badge("c")?.dataset.aiBadge).toBe("unseen");
    expect(badge("c")?.className).not.toContain("animate-pulse");
  });

  it("和连接状态点是两个元素，颜色不同", () => {
    const { container } = renderBar({ b: "running" });
    const tabEl = container.querySelector<HTMLElement>('[data-tab-id="b"]');
    const dots = tabEl?.querySelectorAll(".rounded-full") ?? [];
    expect(dots.length).toBe(2);
    expect(tabEl?.querySelector("[data-ai-badge]")?.className).toContain("violet");
  });
});
