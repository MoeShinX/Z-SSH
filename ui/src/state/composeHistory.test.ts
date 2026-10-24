//! 撰写栏的发送历史：最多 50 条、去重挪到最新、↑ ↓ 翻、多行内容里不抢光标。

import { describe, expect, it } from "vitest";

import {
  COMPOSE_HISTORY_MAX,
  IDLE_NAV,
  caretOnFirstLine,
  caretOnLastLine,
  pushHistory,
  stepHistory,
} from "./composeHistory";

describe("pushHistory", () => {
  it("按旧 → 新追加；空白不记", () => {
    let list: string[] = [];
    list = pushHistory(list, "ls");
    list = pushHistory(list, "pwd");
    list = pushHistory(list, "   ");
    list = pushHistory(list, "");
    expect(list).toEqual(["ls", "pwd"]);
  });

  it("重复的挪到最新，不留两份", () => {
    expect(pushHistory(["ls", "pwd", "df"], "ls")).toEqual(["pwd", "df", "ls"]);
  });

  it("最多 50 条，超出丢最旧的", () => {
    let list: string[] = [];
    for (let i = 0; i < COMPOSE_HISTORY_MAX + 7; i += 1) list = pushHistory(list, `cmd ${i}`);
    expect(list).toHaveLength(COMPOSE_HISTORY_MAX);
    expect(list[0]).toBe("cmd 7");
    expect(list.at(-1)).toBe(`cmd ${COMPOSE_HISTORY_MAX + 6}`);
  });

  it("不改传进来的数组", () => {
    const list = ["a"];
    pushHistory(list, "b");
    expect(list).toEqual(["a"]);
  });
});

describe("光标位置判断（多行内容里不抢光标）", () => {
  it("空输入框两个方向都响应", () => {
    expect(caretOnFirstLine("", 0, 0)).toBe(true);
    expect(caretOnLastLine("", 0, 0)).toBe(true);
  });

  it("单行内容：光标在哪都算第一行 / 最后一行", () => {
    expect(caretOnFirstLine("ls -la", 3, 3)).toBe(true);
    expect(caretOnLastLine("ls -la", 3, 3)).toBe(true);
  });

  it("多行：只有第一行能 ↑，只有最后一行能 ↓", () => {
    const text = "one\ntwo\nthree";
    expect(caretOnFirstLine(text, 2, 2)).toBe(true);
    expect(caretOnFirstLine(text, 5, 5)).toBe(false);
    expect(caretOnLastLine(text, 5, 5)).toBe(false);
    expect(caretOnLastLine(text, text.length, text.length)).toBe(true);
    // 第一行末尾（换行符之前）仍是第一行
    expect(caretOnFirstLine(text, 3, 3)).toBe(true);
    // 第二行开头已经不是
    expect(caretOnFirstLine(text, 4, 4)).toBe(false);
  });

  it("有选中的文字时不翻历史", () => {
    expect(caretOnFirstLine("abc", 0, 3)).toBe(false);
    expect(caretOnLastLine("abc", 0, 3)).toBe(false);
  });
});

describe("stepHistory", () => {
  const list = ["a", "b", "c"];

  it("没有历史：↑ 翻不动", () => {
    expect(stepHistory([], IDLE_NAV, "up", "draft")).toBeNull();
  });

  it("没在翻时 ↓ 翻不动", () => {
    expect(stepHistory(list, IDLE_NAV, "down", "draft")).toBeNull();
  });

  it("↑ 从最新一条开始，一路往旧，到最旧停住", () => {
    const first = stepHistory(list, IDLE_NAV, "up", "半截命令");
    expect(first?.value).toBe("c");
    const second = stepHistory(list, first!.nav, "up", "c");
    expect(second?.value).toBe("b");
    const third = stepHistory(list, second!.nav, "up", "b");
    expect(third?.value).toBe("a");
    expect(stepHistory(list, third!.nav, "up", "a")).toBeNull();
  });

  it("↓ 往新翻，翻过最新一条回到开始翻之前的草稿", () => {
    let step = stepHistory(list, IDLE_NAV, "up", "半截命令")!;
    step = stepHistory(list, step.nav, "up", step.value)!; // b
    const down1 = stepHistory(list, step.nav, "down", step.value)!;
    expect(down1.value).toBe("c");
    const down2 = stepHistory(list, down1.nav, "down", down1.value)!;
    expect(down2.value).toBe("半截命令");
    expect(down2.nav).toEqual(IDLE_NAV);
    expect(stepHistory(list, down2.nav, "down", down2.value)).toBeNull();
  });
});
