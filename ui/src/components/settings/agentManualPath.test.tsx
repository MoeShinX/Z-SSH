/** @vitest-environment jsdom */
//! 设置 → AI：每个智能体的「自动 / 手动指定」开关（GitHub issue #3）。
//!
//! 以前手动模式只看「存没存过路径」：没存过时开关拨不过去（立刻弹回自动），
//! 填路径的输入框也就永远出不来。现在拨到手动先出输入框，检测通过才保存。

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  aiAgentDiscover: vi.fn(async () => ({
    path: "C:\\tools\\claude\\claude.exe",
    version: "2.1.300 (Claude Code)",
    source: "tools_dir" as const,
    needs_shell: false,
    auto: true,
    too_old: false,
    skipped_older: null,
  })),
  aiAgentLogin: vi.fn(async () => ({ logged_in: true, hint: null })),
  aiAgentGuard: vi.fn(async () => null),
  aiAgentProbePath: vi.fn(async () => ({
    path: "D:\\bin\\claude.exe",
    version: "2.1.301 (Claude Code)",
    source: "manual" as const,
    needs_shell: false,
    auto: false,
    too_old: false,
    skipped_older: null,
  })),
  settingsSet: vi.fn(async () => {}),
}));

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: mocks.settingsSet,
  dataDirInfo: vi.fn(async () => ({ data_dir: "C:\\data", known_hosts: "C:\\data\\known_hosts" })),
  openDataDir: vi.fn(async () => {}),
}));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return {
    ...actual,
    aiAgentDiscover: mocks.aiAgentDiscover,
    aiAgentLogin: mocks.aiAgentLogin,
    aiAgentGuard: mocks.aiAgentGuard,
    aiAgentProbePath: mocks.aiAgentProbePath,
    onDownloadProgress: vi.fn(async () => () => {}),
    onAgentEvent: vi.fn(async () => () => {}),
    onApprovalRequest: vi.fn(async () => () => {}),
  };
});

vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "0.1.0-test") }));

vi.mock("../../api/vault", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/vault")>();
  return {
    ...actual,
    defaultProxyGet: vi.fn(async () => ({ enabled: false, url: "" })),
    defaultProxySet: vi.fn(async () => {}),
  };
});

import SettingsPage from "./SettingsPage";
import { useSettings } from "../../stores/settings";

function stubBrowserApis() {
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
  Element.prototype.scrollIntoView = () => {};
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}

async function openAiSection() {
  render(<SettingsPage />);
  await waitFor(() => expect(document.querySelector('[data-section="settings"]')).not.toBeNull());
  const button = [...document.querySelectorAll('[data-section="settings"] button')].find(
    (node) => node.textContent?.trim() === "AI",
  ) as HTMLButtonElement | undefined;
  if (!button) throw new Error("设置页侧栏里没有 AI 按钮");
  await act(async () => button.click());
}

const toggle = () => screen.getByRole("switch", { name: "Claude Code 手动指定路径" });
const pathInput = () => screen.queryByPlaceholderText(/claude/i) as HTMLInputElement | null;

afterEach(cleanup);

beforeEach(() => {
  vi.clearAllMocks();
  stubBrowserApis();
  useSettings.setState({ ai_default_agent: "claude", ai_claude_path: "" });
});

describe("智能体路径：自动 / 手动指定开关", () => {
  it("没存过路径时拨到手动：开关留在手动、出现输入框，这时还不保存", async () => {
    await openAiSection();
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(pathInput()).toBeNull();
    await act(async () => toggle().click());
    expect(toggle().getAttribute("aria-checked")).toBe("true");
    expect(pathInput()).not.toBeNull();
    expect(mocks.settingsSet).not.toHaveBeenCalledWith("ai_claude_path", expect.anything());
  });

  it("填路径点「检测并保存」：检测通过才存下来，之后仍是手动", async () => {
    await openAiSection();
    await act(async () => toggle().click());
    await act(async () => fireEvent.change(pathInput()!, { target: { value: "D:\\bin\\claude.exe" } }));
    await act(async () => screen.getByText("检测并保存").click());
    await waitFor(() => expect(useSettings.getState().ai_claude_path).toBe("D:\\bin\\claude.exe"));
    expect(mocks.aiAgentProbePath).toHaveBeenCalledWith("claude", "D:\\bin\\claude.exe");
    expect(toggle().getAttribute("aria-checked")).toBe("true");
  });

  it("拨到手动又拨回自动、没保存过：输入框收起，不写设置", async () => {
    await openAiSection();
    await act(async () => toggle().click());
    await act(async () => toggle().click());
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(pathInput()).toBeNull();
    expect(mocks.settingsSet).not.toHaveBeenCalledWith("ai_claude_path", expect.anything());
  });

  it("已存了手动路径：显示手动和存着的路径；拨回自动清空路径", async () => {
    useSettings.setState({ ai_claude_path: "D:\\bin\\claude.exe" });
    await openAiSection();
    expect(toggle().getAttribute("aria-checked")).toBe("true");
    expect(pathInput()?.value).toBe("D:\\bin\\claude.exe");
    await act(async () => toggle().click());
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(useSettings.getState().ai_claude_path).toBe("");
  });
});
