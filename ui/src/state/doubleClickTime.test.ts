//! 系统双击时间的缓存：读一次、读不到用 500、脏数据不采信。

import { afterEach, describe, expect, it, vi } from "vitest";

const system = vi.hoisted(() => ({ systemDoubleClickTime: vi.fn() }));
vi.mock("../api/system", () => system);

import {
  DEFAULT_DOUBLE_CLICK_MS,
  doubleClickMs,
  loadDoubleClickTime,
  setDoubleClickMsForTests,
} from "./doubleClickTime";

afterEach(() => {
  setDoubleClickMsForTests(null);
  system.systemDoubleClickTime.mockReset();
});

describe("系统双击时间", () => {
  it("没读到之前是 500", () => {
    expect(doubleClickMs()).toBe(DEFAULT_DOUBLE_CLICK_MS);
    expect(DEFAULT_DOUBLE_CLICK_MS).toBe(500);
  });

  it("启动时读一次并缓存，重复调用不再读", async () => {
    system.systemDoubleClickTime.mockResolvedValue(650);
    expect(await loadDoubleClickTime()).toBe(650);
    expect(doubleClickMs()).toBe(650);
    await loadDoubleClickTime();
    expect(system.systemDoubleClickTime).toHaveBeenCalledTimes(1);
  });

  it("读失败、读到 0 / 负数 / 超过 5000 / 不是数：保持 500", async () => {
    for (const bad of [new Error("没有这个命令"), 0, -3, 5001, Number.NaN, "650"]) {
      setDoubleClickMsForTests(null);
      if (bad instanceof Error) system.systemDoubleClickTime.mockRejectedValue(bad);
      else system.systemDoubleClickTime.mockResolvedValue(bad);
      expect(await loadDoubleClickTime()).toBe(500);
      expect(doubleClickMs()).toBe(500);
    }
  });
});
