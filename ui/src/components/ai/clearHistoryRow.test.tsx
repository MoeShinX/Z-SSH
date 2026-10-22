/** @vitest-environment jsdom */
//! 设置页"清空 AI 历史"：先确认再删。

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ aiHistoryClear: vi.fn(async () => 3) }));

vi.mock("../../api/agent", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/agent")>();
  return { ...actual, aiHistoryClear: mocks.aiHistoryClear };
});

import { ClearAiHistoryRow } from "./ClearHistoryRow";

afterEach(() => cleanup());

describe("清空 AI 历史", () => {
  it("第一次点只进入确认，取消不删；确认才删", async () => {
    render(<ClearAiHistoryRow />);
    fireEvent.click(screen.getByText("清空 AI 历史"));
    expect(mocks.aiHistoryClear).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("取消"));
    expect(mocks.aiHistoryClear).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("清空 AI 历史"));
    fireEvent.click(screen.getByText("确认清空"));
    await waitFor(() => expect(mocks.aiHistoryClear).toHaveBeenCalledTimes(1));
    await screen.findByText("清空 AI 历史");
  });
});
