import { describe, expect, it } from "vitest";

import { createLinkDoubleClickGate, LINK_DOUBLE_CLICK_MS, linkSelection } from "./links";

describe("链接双击判定", () => {
  it("单击不放行，间隔内第二下放行", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    expect(gate("https://example.com")).toBe(false);
    t += 200;
    expect(gate("https://example.com")).toBe(true);
  });

  it("超过双击间隔：当作新的第一下", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    gate("https://example.com");
    t += LINK_DOUBLE_CLICK_MS + 1;
    expect(gate("https://example.com")).toBe(false);
    t += 100;
    expect(gate("https://example.com")).toBe(true);
  });

  it("两下点的不是同一个网址：不放行", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    gate("https://example.com/a");
    t += 100;
    expect(gate("https://example.com/b")).toBe(false);
  });

  it("放行之后记录清空：连点三下只打开一次", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    gate("https://example.com");
    t += 100;
    expect(gate("https://example.com")).toBe(true);
    t += 100;
    expect(gate("https://example.com")).toBe(false);
  });
});

describe("链接范围换选区", () => {
  it("同一行的链接：列从 0 起，长度含两端", () => {
    // 第 3 列到第 22 列（1 起始、含右端）共 20 格
    expect(linkSelection({ start: { x: 3, y: 10 }, end: { x: 22, y: 10 } }, 80)).toEqual({
      column: 2,
      row: 9,
      length: 20,
    });
  });

  it("折行的链接：按整行宽度累加", () => {
    // 第 1 行从第 70 列写到行尾（80），第 2 行写了 15 格：共 11 + 15 = 26 格
    expect(linkSelection({ start: { x: 70, y: 5 }, end: { x: 15, y: 6 } }, 80)).toEqual({
      column: 69,
      row: 4,
      length: 26,
    });
  });
});
