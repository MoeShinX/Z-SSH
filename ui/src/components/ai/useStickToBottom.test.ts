import { describe, expect, it } from "vitest";

import { isNearBottom, STICK_THRESHOLD_PX } from "./useStickToBottom";

describe("消息区是否在底部", () => {
  it("贴底和差几像素都算在底部，往上翻了就不算", () => {
    expect(isNearBottom(600, 400, 1000)).toBe(true);
    expect(isNearBottom(600 - STICK_THRESHOLD_PX, 400, 1000)).toBe(true);
    expect(isNearBottom(600 - STICK_THRESHOLD_PX - 1, 400, 1000)).toBe(false);
    expect(isNearBottom(0, 400, 1000)).toBe(false);
  });

  it("内容不满一屏时一直算在底部", () => {
    expect(isNearBottom(0, 400, 300)).toBe(true);
  });
});
