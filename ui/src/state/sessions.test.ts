import { describe, expect, it } from "vitest";

import { insertTabAfter } from "./sessions";

describe("新标签放在哪（复制标签页）", () => {
  const tabs = [{ id: "a" }, { id: "b" }, { id: "c" }];

  it("紧挨在原标签右边，不跑到最右边", () => {
    expect(insertTabAfter(tabs, { id: "n" }, "a").map((t) => t.id)).toEqual(["a", "n", "b", "c"]);
    expect(insertTabAfter(tabs, { id: "n" }, "c").map((t) => t.id)).toEqual(["a", "b", "c", "n"]);
  });

  it("没给位置、或者原标签已经不在了：加在最后", () => {
    expect(insertTabAfter(tabs, { id: "n" }).map((t) => t.id)).toEqual(["a", "b", "c", "n"]);
    expect(insertTabAfter(tabs, { id: "n" }, "gone").map((t) => t.id)).toEqual([
      "a",
      "b",
      "c",
      "n",
    ]);
  });

  it("不改原数组", () => {
    insertTabAfter(tabs, { id: "n" }, "a");
    expect(tabs.map((t) => t.id)).toEqual(["a", "b", "c"]);
  });
});
