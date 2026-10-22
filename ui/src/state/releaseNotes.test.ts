import { describe, expect, it } from "vitest";

import { parseReleaseNotes, sectionTone } from "./releaseNotes";

describe("更新说明排版", () => {
  it("按标题分组、条目编号，去掉 Markdown 符号，加粗开头当小标题，续行接上", () => {
    const sections = parseReleaseNotes(`### 新增

- **主题和皮肤**：主题改成三档，
  浅色 7 套。
- 新图标：荧光绿 Z。

### 修复

- 安装后显示旧图标。`);
    expect(sections).toEqual([
      {
        heading: "新增",
        items: [
          { title: "主题和皮肤", text: "主题改成三档， 浅色 7 套。" },
          { title: "", text: "新图标：荧光绿 Z。" },
        ],
      },
      { heading: "修复", items: [{ title: "", text: "安装后显示旧图标。" }] },
    ]);
  });

  it("认不出结构就返回 null（界面显示原文）", () => {
    expect(parseReleaseNotes("加了新功能，修了几个问题")).toBeNull();
  });

  it("分组配色", () => {
    expect(sectionTone("新增")).toBe("added");
    expect(sectionTone("修复")).toBe("fixed");
    expect(sectionTone("优化")).toBe("improved");
    expect(sectionTone("安全")).toBe("security");
  });
});
