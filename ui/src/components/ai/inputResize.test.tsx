/** @vitest-environment jsdom */
//! 输入框上沿的拖动条：上下拖调高度，松手记住，双击恢复默认。

import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  settingsSet: vi.fn(async () => {}),
  aiAgentDiscover: vi.fn(async (agent: string) =>
    agent === "opencode"
      ? {
          path: "C:\tools\opencode\opencode.exe",
          version: "1.19.1",
          source: "tools_dir" as const,
          needs_shell: false,
          auto: true,
          too_old: false,
          skipped_older: null,
        }
      : null,
  ),
  aiAgentLogin: vi.fn(async () => ({ logged_in: true, hint: null })),
  aiAgentCatalog: vi.fn(async () => ({
    models: [
      { id: "deepseek/flash", name: "DeepSeek Flash" },
      { id: "deepseek/pro", name: "DeepSeek Pro" },
    ],
    default: null,
    default_label: "DeepSeek Pro",
  })),
  aiAgentStart: vi.fn(async () => {
    throw new Error("这个用例不该开对话");
  }),
  onAgentEvent: vi.fn(async () => () => {}),
  onApprovalRequest: vi.fn(async () => () => {}),
}));

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: mocks.settingsSet,
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return {
    ...actual,
    aiAgentDiscover: mocks.aiAgentDiscover,
    aiAgentLogin: mocks.aiAgentLogin,
    aiAgentCatalog: mocks.aiAgentCatalog,
    aiAgentStart: mocks.aiAgentStart,
    onAgentEvent: mocks.onAgentEvent,
    onApprovalRequest: mocks.onApprovalRequest,
  };
});

import AIChatPanel from "./AIChatPanel";
import { clearCatalogCache } from "./modelOptions";
import { AI_INPUT_DEFAULT_HEIGHT } from "./inputHeight";
import { useSettings } from "../../stores/settings";

function mount() {
  return render(
    <AIChatPanel
      tabId="tab-1"
      sessions={[]}
      hostStates={{}}
      firstUse={false}
      onMarkUsed={() => {}}
      onOpenSettings={() => {}}
      onClose={() => {}}
    />,
  );
}

beforeEach(() => {
  clearCatalogCache();
  mocks.aiAgentCatalog.mockClear();
  mocks.settingsSet.mockClear();
  useSettings.setState({ ai_default_agent: "opencode", "layout.ai_input_height": 50 });
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => cleanup());

describe("输入框拖动条", () => {
  function handle(): HTMLElement {
    const el = document.querySelector<HTMLElement>('[data-section="input-resize-handle"]');
    if (!el) throw new Error("没有拖动条");
    return el;
  }
  function textarea(): HTMLTextAreaElement {
    const el = document.querySelector("textarea");
    if (!el) throw new Error("没有输入框");
    return el;
  }

  it("往上拖变高、松手写进设置；双击恢复默认", async () => {
    mount();
    expect(handle().className).toContain("cursor-ns-resize");
    // 存的是旧版默认 50：按新默认 75 显示，往上拖 100 就是 175
    expect(textarea().style.height).toBe(`${AI_INPUT_DEFAULT_HEIGHT}px`);

    fireEvent.pointerDown(handle(), { clientY: 500, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientY: 400, pointerId: 1 });
    expect(textarea().style.height).toBe("175px");
    fireEvent.pointerUp(handle(), { clientY: 400, pointerId: 1 });
    await waitFor(() =>
      expect(mocks.settingsSet).toHaveBeenCalledWith("layout.ai_input_height", "175"),
    );

    fireEvent.doubleClick(handle());
    await waitFor(() =>
      expect(mocks.settingsSet).toHaveBeenCalledWith(
        "layout.ai_input_height",
        String(AI_INPUT_DEFAULT_HEIGHT),
      ),
    );
  });

  it("拖得再高也不超过面板高度的一半", () => {
    mount();
    fireEvent.pointerDown(handle(), { clientY: 900, pointerId: 1 });
    fireEvent.pointerMove(handle(), { clientY: -5000, pointerId: 1 });
    // jsdom 量不到面板高度，退到窗口高度。
    expect(textarea().style.height).toBe(`${Math.floor(window.innerHeight / 2)}px`);
    fireEvent.pointerUp(handle(), { clientY: -5000, pointerId: 1 });
  });
});
