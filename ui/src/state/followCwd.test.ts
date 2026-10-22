//! "跟随终端目录"的开关逻辑（M3-3 第 6.1 节）。

import { describe, expect, it } from "vitest";

import { followTarget } from "./followCwd";

describe("followTarget", () => {
  it("开着、终端目录变了：跳过去", () => {
    expect(followTarget(true, "/srv/app", "/home/u")).toBe("/srv/app");
  });

  it("终端目录和远端栏一样：不动", () => {
    expect(followTarget(true, "/srv/app", "/srv/app")).toBeNull();
  });

  it("用户手动换目录后开关关了：终端再怎么 cd 都不动", () => {
    expect(followTarget(false, "/srv/app", "/home/u")).toBeNull();
  });

  it("取不到终端目录：不动，安静地留在当前目录（家目录）", () => {
    expect(followTarget(true, null, "/home/u")).toBeNull();
    expect(followTarget(true, "", "/home/u")).toBeNull();
  });

  it("远端栏还没加载出目录：直接去终端的目录", () => {
    expect(followTarget(true, "/srv/app", null)).toBe("/srv/app");
  });
});
