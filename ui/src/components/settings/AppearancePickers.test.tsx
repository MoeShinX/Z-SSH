/** @vitest-environment jsdom */
//! 终端固定成和当前明暗相反的配色时给提示，点一下改回跟随皮肤；跟随皮肤或明暗一致时不提示。

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import { useSettings } from "../../stores/settings";
import { TerminalMismatchHint } from "./AppearancePickers";

afterEach(cleanup);
beforeEach(() => {
  useSettings.setState({ ui_theme_mode: "light", systemDark: false, terminal_theme: "graphite" });
});

describe("终端明暗不一致提示", () => {
  it("浅色主题配固定的深色终端：提示，点按钮改回跟随皮肤", async () => {
    render(<TerminalMismatchHint />);
    expect(screen.getByText(/终端配色固定为 石墨/)).toBeTruthy();
    await act(async () => screen.getByRole("button", { name: "改回跟随皮肤" }).click());
    expect(useSettings.getState().terminal_theme).toBe("skin");
    expect(document.querySelector('[data-section="terminal-mismatch"]')).toBeNull();
  });

  it("跟随皮肤、或明暗一致时不提示", () => {
    useSettings.setState({ terminal_theme: "skin" });
    const { rerender } = render(<TerminalMismatchHint />);
    expect(document.querySelector('[data-section="terminal-mismatch"]')).toBeNull();
    useSettings.setState({ terminal_theme: "github-light" });
    rerender(<TerminalMismatchHint />);
    expect(document.querySelector('[data-section="terminal-mismatch"]')).toBeNull();
  });
});
