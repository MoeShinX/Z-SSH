//! 关标签 / 退出时的确认文字（M3-3 第 5 节：还有没上传的修改要提醒）。

import { describe, expect, it } from "vitest";

import { editConfirmMessage } from "./editDirty";

describe("editConfirmMessage", () => {
  it("只有传输：沿用 M3-2 的说法", () => {
    expect(editConfirmMessage(2, 0, "退出程序")).toEqual({
      title: "还有 2 个传输未完成",
      message: "退出程序会取消这些传输。",
    });
  });

  it("只有没上传的修改：提醒会丢掉", () => {
    expect(editConfirmMessage(0, 1, "关闭这个标签")).toEqual({
      title: "还有 1 个文件的修改还没上传",
      message: "关闭这个标签会丢掉这些没上传的修改。",
    });
  });

  it("两样都有：一起说", () => {
    const text = editConfirmMessage(3, 2, "退出程序");
    expect(text.title).toBe("还有 3 个传输未完成、2 个文件的修改还没上传");
    expect(text.message).toBe("退出程序会取消这些传输，并丢掉这些没上传的修改。");
  });
});
