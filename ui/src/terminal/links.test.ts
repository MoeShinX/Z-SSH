import { describe, expect, it } from "vitest";

import {
  createLinkDoubleClickGate,
  isOpenableUrl,
  LINK_DOUBLE_CLICK_MS,
  linkSelection,
  sameLinkRange,
  type LinkRange,
} from "./links";

const URL_A = "https://example.com";
const RANGE_A: LinkRange = { start: { x: 5, y: 12 }, end: { x: 30, y: 12 } };
// 屏幕上另一处的同一个网址
const RANGE_B: LinkRange = { start: { x: 5, y: 20 }, end: { x: 30, y: 20 } };

describe("链接双击判定", () => {
  it("单击不放行，间隔内的第二下（点击计数 2）放行", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 1 })).toBe(false);
    t += 200;
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 2 })).toBe(true);
  });

  it("点击计数还是 1（两下各算第一下）：不放行，哪怕网址、范围都一样", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 1 });
    t += 100;
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 1 })).toBe(false);
  });

  it("超过双击间隔：不放行", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 1 });
    t += LINK_DOUBLE_CLICK_MS + 1;
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 2 })).toBe(false);
  });

  it("同一个网址在屏幕上两处，间隔 200ms 各点一下：不放行", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 1 })).toBe(false);
    t += 200;
    // 浏览器的点击计数不看位置能不能是 2，但范围不同就不是同一个链接实例
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_B, detail: 2 })).toBe(false);
    t += 200;
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_B, detail: 1 })).toBe(false);
  });

  it("两下点的不是同一个网址：不放行", () => {
    const gate = createLinkDoubleClickGate(() => 1000);
    gate.isDoubleClick({ uri: "https://example.com/a", range: RANGE_A, detail: 1 });
    expect(gate.isDoubleClick({ uri: "https://example.com/b", range: RANGE_A, detail: 2 })).toBe(false);
  });

  it("没有第一下的记录（点击计数 2 直接来）：不放行", () => {
    const gate = createLinkDoubleClickGate(() => 1000);
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 2 })).toBe(false);
  });

  it("离开链接 / 点到别处 / 拖选（reset）之后第二下不放行", () => {
    const gate = createLinkDoubleClickGate(() => 1000);
    gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 1 });
    gate.reset();
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 2 })).toBe(false);
  });

  it("放行之后记录清空：连点三下只打开一次", () => {
    let t = 1000;
    const gate = createLinkDoubleClickGate(() => t);
    gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 1 });
    t += 100;
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 2 })).toBe(true);
    t += 100;
    expect(gate.isDoubleClick({ uri: URL_A, range: RANGE_A, detail: 3 })).toBe(false);
  });
});

describe("链接范围比较和可打开判断", () => {
  it("范围四个端点都相同才算同一处", () => {
    expect(sameLinkRange(RANGE_A, { start: { x: 5, y: 12 }, end: { x: 30, y: 12 } })).toBe(true);
    expect(sameLinkRange(RANGE_A, RANGE_B)).toBe(false);
    expect(sameLinkRange(RANGE_A, { start: { x: 5, y: 12 }, end: { x: 31, y: 12 } })).toBe(false);
  });

  it("只有 http / https 能交给浏览器", () => {
    expect(isOpenableUrl("https://example.com/a?b=1")).toBe(true);
    expect(isOpenableUrl("http://example.com")).toBe(true);
    expect(isOpenableUrl("file:///C:/x.exe")).toBe(false);
    expect(isOpenableUrl("javascript:alert(1)")).toBe(false);
    expect(isOpenableUrl("ssh://host")).toBe(false);
    expect(isOpenableUrl("not a url")).toBe(false);
    expect(isOpenableUrl("")).toBe(false);
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
