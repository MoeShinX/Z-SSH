import { describe, expect, it } from "vitest";

import { aiTabFor } from "./aiTab";

const shellA = { id: "a" };
const shellB = { id: "b" };
const files = { id: "f", sftp: { hostId: 1 } };

describe("AI 面板挂在哪个标签上", () => {
  it("当前是 shell 标签就用它", () => {
    expect(aiTabFor([shellA, shellB, files], shellB, "a")).toBe(shellB);
  });

  it("当前是 SFTP 标签：沿用最近的 shell 标签", () => {
    expect(aiTabFor([shellA, shellB, files], files, "a")).toBe(shellA);
  });

  it("最近的 shell 标签已经关了，或者从没开过 shell：没有", () => {
    expect(aiTabFor([shellB, files], files, "a")).toBeNull();
    expect(aiTabFor([files], files, null)).toBeNull();
  });
});
