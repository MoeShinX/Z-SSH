/** @vitest-environment jsdom */
//! 面板一打开就显示默认模型（不用先点"+"）。

import { cleanup, render, waitFor } from "@testing-library/react";
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

function modelTrigger(): HTMLElement {
  const el = document.querySelector<HTMLElement>('[data-section="model-select"]');
  if (!el) throw new Error("没有模型下拉框");
  return el;
}

beforeEach(() => {
  clearCatalogCache();
  mocks.aiAgentCatalog.mockClear();
  mocks.settingsSet.mockClear();
  useSettings.setState({ ai_default_agent: "opencode" });
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.releasePointerCapture = () => {};
});

afterEach(() => cleanup());

describe("模型目录预加载", () => {
  it("打开面板就显示 默认（实际模型），不用点 +；重新打开不重复取", async () => {
    const first = mount();
    await waitFor(() => expect(modelTrigger().textContent).toContain("默认（DeepSeek Pro）"));
    expect(mocks.aiAgentStart).not.toHaveBeenCalled();
    expect(mocks.aiAgentCatalog).toHaveBeenCalledWith("opencode", "C:\tools\opencode\opencode.exe");
    first.unmount();

    mount();
    await waitFor(() => expect(modelTrigger().textContent).toContain("默认（DeepSeek Pro）"));
    expect(mocks.aiAgentCatalog).toHaveBeenCalledTimes(1);
  });

  it("取不到目录时保持只显示 默认模型", async () => {
    mocks.aiAgentCatalog.mockRejectedValueOnce(new Error("起不来"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    mount();
    await waitFor(() => expect(mocks.aiAgentCatalog).toHaveBeenCalled());
    await waitFor(() => expect(warn).toHaveBeenCalled());
    expect(modelTrigger().textContent).toContain("默认模型");
    warn.mockRestore();
  });
});
