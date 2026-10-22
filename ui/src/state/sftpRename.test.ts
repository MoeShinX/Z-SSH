import { describe, expect, it, vi } from "vitest";

import { isAlreadyExists, renameRemoteSafely } from "./sftpRename";

describe("远端重命名", () => {
  it("目标不存在：只试一次，不覆盖，也不问", async () => {
    const rename = vi.fn(async () => {});
    const confirmOverwrite = vi.fn(async () => true);
    expect(await renameRemoteSafely({ rename, confirmOverwrite })).toBe("renamed");
    expect(rename).toHaveBeenCalledTimes(1);
    expect(rename).toHaveBeenCalledWith(false);
    expect(confirmOverwrite).not.toHaveBeenCalled();
  });

  it("服务器说已经存在：先弹确认，取消后服务器上的文件不变（不再调用覆盖）", async () => {
    const rename = vi.fn(async (overwrite: boolean) => {
      if (!overwrite) throw "已经存在：/srv/b.txt";
    });
    const confirmOverwrite = vi.fn(async () => false);
    expect(await renameRemoteSafely({ rename, confirmOverwrite })).toBe("cancelled");
    expect(confirmOverwrite).toHaveBeenCalledTimes(1);
    expect(rename).toHaveBeenCalledTimes(1);
    expect(rename).not.toHaveBeenCalledWith(true);
  });

  it("用户确认之后才用覆盖重试", async () => {
    const rename = vi.fn(async (overwrite: boolean) => {
      if (!overwrite) throw new Error("已经存在：/srv/b.txt");
    });
    const confirmOverwrite = vi.fn(async () => true);
    expect(await renameRemoteSafely({ rename, confirmOverwrite })).toBe("renamed");
    expect(rename.mock.calls.map((call) => call[0])).toEqual([false, true]);
  });

  it("别的错误原样抛出，不问覆盖", async () => {
    const rename = vi.fn(async () => {
      throw "没有权限：/srv/b.txt";
    });
    const confirmOverwrite = vi.fn(async () => true);
    await expect(renameRemoteSafely({ rename, confirmOverwrite })).rejects.toBe(
      "没有权限：/srv/b.txt",
    );
    expect(confirmOverwrite).not.toHaveBeenCalled();
  });

  it("认得出已存在的错误文字", () => {
    expect(isAlreadyExists("已经存在：/a")).toBe(true);
    expect(isAlreadyExists(new Error("已经存在：/a"))).toBe(true);
    expect(isAlreadyExists("没有权限：/a")).toBe(false);
  });
});
