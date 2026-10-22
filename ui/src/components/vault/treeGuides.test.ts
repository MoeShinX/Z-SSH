//! 主机树虚线连接线的几何：竖线对齐父分组的展开箭头中心，└ 只画到行中间。

import { describe, expect, it } from "vitest";

import { TREE_LINE_COLOR, guideAttrs, guideX, treeGuideStyle } from "./treeGuides";

/** 按顶层逗号拆（渐变函数里面也有逗号）。 */
function layers(value: string | number | undefined): string[] {
  const out: string[] = [];
  let depth = 0;
  let current = "";
  for (const ch of String(value ?? "")) {
    if (ch === "(") depth++;
    if (ch === ")") depth--;
    if (ch === "," && depth === 0) {
      out.push(current.trim());
      current = "";
    } else {
      current += ch;
    }
  }
  if (current.trim()) out.push(current.trim());
  return out;
}

describe("树连接线", () => {
  it("竖线在父分组展开箭头的中心（行内边距 4 + 箭头框一半 8，每层缩进 14）", () => {
    expect(guideX(0)).toBe(12);
    expect(guideX(1)).toBe(26);
  });

  it("├：竖线整行高 + 一小段横线连到主机图标前", () => {
    const style = treeGuideStyle(1, { trail: [], last: false, kind: "host" });
    // 竖线 + 横线两层
    expect(layers(style.backgroundImage)).toHaveLength(2);
    expect(layers(style.backgroundSize)).toEqual(["1px 100%", "20px 1px"]);
    expect(layers(style.backgroundPosition)).toEqual(["12px 0", "12px 13px"]);
    expect(style.backgroundRepeat).toBe("no-repeat");
    expect(String(style.backgroundImage)).toContain(TREE_LINE_COLOR);
  });

  it("└：最后一个子项的竖线只画到行中间，不往下延伸", () => {
    const style = treeGuideStyle(1, { trail: [], last: true, kind: "host" });
    expect(layers(style.backgroundSize)[0]).toBe("1px 14px");
  });

  it("子分组的横线只连到它的展开箭头前", () => {
    const style = treeGuideStyle(2, { trail: [true], last: false, kind: "group" });
    // 祖先竖线 + 自己的竖线 + 横线
    expect(layers(style.backgroundSize)).toEqual(["1px 100%", "1px 100%", "6px 1px"]);
    expect(layers(style.backgroundPosition)).toEqual(["12px 0", "26px 0", "26px 13px"]);
  });

  it("祖先已经是最后一个子项时，它的竖线不再穿过这一行", () => {
    const style = treeGuideStyle(2, { trail: [false], last: true, kind: "host" });
    expect(layers(style.backgroundSize)).toEqual(["1px 14px", "20px 1px"]);
    expect(layers(style.backgroundPosition)[0]).toBe("26px 0");
  });

  it("颜色用现有主题令牌，不写死色值", () => {
    expect(TREE_LINE_COLOR).toMatch(/^hsl\(var\(--muted-foreground\)/);
  });

  it("标记：顶层没有；有的写出形状和穿过的祖先层", () => {
    expect(guideAttrs(undefined)).toEqual({});
    expect(guideAttrs({ trail: [true, false], last: true, kind: "host" })).toEqual({
      "data-tree-line": "last",
      "data-tree-trail": "10",
    });
  });
});
