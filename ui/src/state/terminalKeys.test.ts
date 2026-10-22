import { describe, expect, it } from "vitest";

import {
  multilinePaste,
  swallowsWithoutSelection,
  terminalClipboardAction,
  type KeyLike,
} from "./terminalKeys";

const key = (k: string, extra: Partial<KeyLike> = {}): KeyLike => ({
  type: "keydown",
  key: k,
  ctrlKey: true,
  shiftKey: false,
  altKey: false,
  metaKey: false,
  ...extra,
});

describe("终端复制粘贴快捷键", () => {
  it("选中了文字时 Ctrl+C 复制，没选中照常发 ^C", () => {
    expect(terminalClipboardAction(key("c"), true)).toBe("copy");
    expect(terminalClipboardAction(key("c"), false)).toBeNull();
  });

  it("Ctrl+V 粘贴，不管有没有选中", () => {
    expect(terminalClipboardAction(key("v"), false)).toBe("paste");
    expect(terminalClipboardAction(key("V", { shiftKey: true }), true)).toBe("paste");
  });

  it("Ctrl+Shift+C 同样复制；没选中时吞掉，不发中断", () => {
    expect(terminalClipboardAction(key("C", { shiftKey: true }), true)).toBe("copy");
    expect(terminalClipboardAction(key("C", { shiftKey: true }), false)).toBeNull();
    expect(swallowsWithoutSelection(key("C", { shiftKey: true }))).toBe(true);
    expect(swallowsWithoutSelection(key("c"))).toBe(false);
  });

  it("松开键、带 Alt、其他键都不管", () => {
    expect(terminalClipboardAction(key("c", { type: "keyup" }), true)).toBeNull();
    expect(terminalClipboardAction(key("v", { altKey: true }), false)).toBeNull();
    expect(terminalClipboardAction(key("v", { ctrlKey: false }), false)).toBeNull();
    expect(terminalClipboardAction(key("a"), true)).toBeNull();
  });
});

describe("多行粘贴确认", () => {
  it("单行不用确认，带换行（含末尾换行）要确认", () => {
    expect(multilinePaste("ls -la")).toBeNull();
    expect(multilinePaste("rm -rf /tmp/x\n")).toEqual({ lines: 1, preview: "rm -rf /tmp/x" });
    expect(multilinePaste("a\r\nb\rc")?.lines).toBe(3);
  });

  it("预览最多 5 行，多的写明还有几行", () => {
    const text = Array.from({ length: 8 }, (_, i) => `line${i}`).join("\n");
    const info = multilinePaste(text);
    expect(info?.lines).toBe(8);
    expect(info?.preview.split("\n")).toHaveLength(6);
    expect(info?.preview).toContain("还有 3 行");
  });
});
