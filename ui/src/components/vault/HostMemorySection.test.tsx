/** @vitest-environment jsdom */
//! 主机详情里的 AI 记忆：读出来显示、改了能存、能清空。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const memory = vi.hoisted(() => ({ content: "服务：nginx", updated_at: 1_760_000_000 as number | null }));

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../../api/vault", () => ({
  hostMemoryGet: vi.fn(async () =>
    memory.updated_at === null ? null : { content: memory.content, updated_at: memory.updated_at },
  ),
  hostMemorySet: vi.fn(async (_id: number, content: string) => {
    memory.content = content;
    memory.updated_at = content ? 1_760_000_100 : null;
  }),
}));

import HostMemorySection from "./HostMemorySection";

afterEach(cleanup);
beforeEach(() => {
  memory.content = "服务：nginx";
  memory.updated_at = 1_760_000_000;
});

describe("AI 记忆", () => {
  it("显示已有记忆；改了以后保存，清空后变成空", async () => {
    const api = await import("../../api/vault");
    render(<HostMemorySection hostId={3} onError={() => {}} />);
    await act(async () => {});
    const box = screen.getByLabelText("AI 记忆") as HTMLTextAreaElement;
    expect(box.value).toBe("服务：nginx");
    expect((screen.getByRole("button", { name: "保存记忆" }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(box, { target: { value: "服务：nginx、redis" } });
    await act(async () => screen.getByRole("button", { name: "保存记忆" }).click());
    expect(api.hostMemorySet).toHaveBeenCalledWith(3, "服务：nginx、redis");

    await act(async () => screen.getByRole("button", { name: "清空" }).click());
    expect(api.hostMemorySet).toHaveBeenLastCalledWith(3, "");
    expect((screen.getByLabelText("AI 记忆") as HTMLTextAreaElement).value).toBe("");
  });
});
