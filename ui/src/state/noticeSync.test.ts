import { describe, expect, it } from "vitest";

import bundled from "../assets/NOTICE.txt?raw";
import root from "../../../NOTICE?raw";

describe("关于页里的第三方声明", () => {
  it("和仓库根目录的 NOTICE 内容一致（第三方署名不能漏）", () => {
    expect(bundled.replace(/\r\n/g, "\n")).toBe(root.replace(/\r\n/g, "\n"));
  });
});
