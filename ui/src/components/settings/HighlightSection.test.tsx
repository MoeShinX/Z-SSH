/** @vitest-environment jsdom */
//! 高亮设置：第一次打开给常用规则；删除、改颜色马上存；正则写错有提示；单条开关；补常用规则。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import HighlightSection from "./HighlightSection";
import { SAMPLE_RULES } from "../../terminal/highlightSamples";
import { useSettings } from "../../stores/settings";

afterEach(cleanup);

beforeEach(() => {
  useSettings.setState({ terminal_highlight_enabled: false, terminal_highlight_rules: "[]" });
});

const stored = () =>
  JSON.parse(useSettings.getState().terminal_highlight_rules) as { id: string; pattern: string; enabled?: boolean }[];

describe("高亮设置", () => {
  it("默认关，看不到规则列表；打开时没有规则就放进示例", async () => {
    render(<HighlightSection />);
    expect(screen.queryByTestId("highlight-rules")).toBeNull();
    await act(async () => screen.getByRole("switch", { name: "开启高亮" }).click());
    expect(useSettings.getState().terminal_highlight_enabled).toBe(true);
    expect(stored().map((rule) => rule.pattern)).toEqual(SAMPLE_RULES.map((rule) => rule.pattern));
    expect(screen.getAllByLabelText("高亮规则")).toHaveLength(SAMPLE_RULES.length);
    // 常用规则都有了：不显示"添加常用规则"
    expect(document.querySelector('[data-action="add-sample-rules"]')).toBeNull();
  });

  it("删除规则马上存；写错的正则有提示", async () => {
    useSettings.setState({
      terminal_highlight_enabled: true,
      terminal_highlight_rules: JSON.stringify(SAMPLE_RULES.slice(0, 2)),
    });
    render(<HighlightSection />);
    await act(async () => screen.getAllByTitle("删除")[0].click());
    expect(stored().map((rule) => rule.pattern)).toEqual([SAMPLE_RULES[1].pattern]);

    const input = screen.getByLabelText("高亮规则");
    fireEvent.change(input, { target: { value: "warn(" } });
    expect(screen.getByText(/这条先不生效/)).toBeTruthy();
  });

  it("单条规则可以关掉，马上存", async () => {
    useSettings.setState({
      terminal_highlight_enabled: true,
      terminal_highlight_rules: JSON.stringify(SAMPLE_RULES.slice(0, 2)),
    });
    render(<HighlightSection />);
    await act(async () => screen.getAllByRole("switch", { name: "关闭这条规则" })[1].click());
    expect(stored().map((rule) => rule.enabled)).toEqual([true, false]);
  });

  it("早先只有两条示例规则：添加常用规则只补没有的，不重复", async () => {
    const old = [
      { id: "sample-error", pattern: "error|fail(ed|ure)?|fatal|denied", regex: true, color: "#ef4444" },
      { id: "sample-warn", pattern: "warn(ing)?", regex: true, color: "#f59e0b" },
    ];
    useSettings.setState({ terminal_highlight_enabled: true, terminal_highlight_rules: JSON.stringify(old) });
    render(<HighlightSection />);
    await act(async () => (document.querySelector('[data-action="add-sample-rules"]') as HTMLElement).click());
    expect(stored().map((rule) => rule.id)).toEqual([
      "sample-error",
      "sample-warn",
      "sample-ok",
      "sample-ipv4",
      "sample-url",
    ]);
    // 用户原来的两条保持原样
    expect(stored()[0].pattern).toBe(old[0].pattern);
    expect(document.querySelector('[data-action="add-sample-rules"]')).toBeNull();
  });
});
