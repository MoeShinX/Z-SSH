import { describe, expect, it } from "vitest";

import {
  decideClose,
  DEFAULT_CLOSE_ACTION,
  parseCloseAction,
  parseFileWindowCloseAction,
  quitVerdict,
  shouldMergeOnClose,
} from "./closeBehavior";

describe("parseCloseAction", () => {
  it("缺省或读不懂的值按默认：缩到托盘", () => {
    expect(DEFAULT_CLOSE_ACTION).toBe("tray");
    for (const raw of [undefined, null, "", "Quit", 1, true, "tray", "close", {}]) {
      expect(parseCloseAction(raw)).toBe("tray");
    }
  });

  it("明确选了退出才退出", () => {
    expect(parseCloseAction("quit")).toBe("quit");
  });
});

describe("decideClose", () => {
  it("缩到托盘模式：隐藏，不走任何确认", () => {
    expect(decideClose({ action: "tray", quitting: false, hintShown: true })).toEqual({
      type: "hide",
      showHint: false,
    });
  });

  it("第一次缩到托盘才要提示，之后不再提示", () => {
    expect(decideClose({ action: "tray", quitting: false, hintShown: false })).toEqual({
      type: "hide",
      showHint: true,
    });
    // 提示过之后（调用方把 hintShown 记成 true）
    expect(decideClose({ action: "tray", quitting: false, hintShown: true })).toEqual({
      type: "hide",
      showHint: false,
    });
  });

  it("退出模式：走原来的退出流程，不提示", () => {
    expect(decideClose({ action: "quit", quitting: false, hintShown: false })).toEqual({
      type: "quit",
    });
  });

  it("托盘退出 / 已确认退出时，即使设置是缩到托盘也不能再隐藏", () => {
    expect(decideClose({ action: "tray", quitting: true, hintShown: false })).toEqual({
      type: "quit",
    });
  });
});

describe("quitVerdict（和 UI-4 之前 onCloseRequested 的判断一致）", () => {
  const idle = { unsavedDocs: 0, transfers: 0, dirtyEdits: 0, confirmed: false };

  it("什么都没有：放行", () => {
    expect(quitVerdict(idle)).toEqual({ type: "go" });
  });

  it("先问未保存的文档", () => {
    expect(quitVerdict({ ...idle, unsavedDocs: 2, transfers: 1 })).toEqual({
      type: "ask-unsaved",
    });
  });

  it("没有未保存文档，但有传输或远程编辑未回传：问放弃", () => {
    expect(quitVerdict({ ...idle, transfers: 1 })).toEqual({ type: "ask-busy" });
    expect(quitVerdict({ ...idle, dirtyEdits: 3 })).toEqual({ type: "ask-busy" });
  });

  it("已经确认过：放行", () => {
    expect(quitVerdict({ unsavedDocs: 1, transfers: 1, dirtyEdits: 1, confirmed: true })).toEqual({
      type: "go",
    });
  });
});

describe("关闭 Z-SFTP 文件窗口", () => {
  it("缺省和读不懂的值都按「断开并关闭」，只有 merge 才合并", () => {
    expect(parseFileWindowCloseAction(undefined)).toBe("close");
    expect(parseFileWindowCloseAction("")).toBe("close");
    expect(parseFileWindowCloseAction("something-else")).toBe("close");
    expect(parseFileWindowCloseAction("close")).toBe("close");
    expect(parseFileWindowCloseAction("merge")).toBe("merge");
  });

  it("合并只在：选了合并、窗口里有标签、不是正在真的关", () => {
    expect(shouldMergeOnClose("merge", 2, false)).toBe(true);
    expect(shouldMergeOnClose("close", 2, false)).toBe(false);
    expect(shouldMergeOnClose("merge", 0, false)).toBe(false);
    expect(shouldMergeOnClose("merge", 2, true)).toBe(false);
  });
});
