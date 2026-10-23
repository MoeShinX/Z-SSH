import { describe, expect, it } from "vitest";
import { languageFor, languageFromShebang } from "./editorLanguage";
import {
  defaultLocalSaveAsName,
  describeOpenError,
  encodingLabel,
  isSameFile,
  statusSegments,
  tabTooltip,
  unsavedPrompt,
} from "./editorModel";

describe("editorLanguage", () => {
  it("按扩展名和文件名识别", () => {
    expect(languageFor("/etc/app/config.yaml")).toBe("yaml");
    expect(languageFor("/srv/Dockerfile")).toBe("dockerfile");
    expect(languageFor("/srv/readme.md")).toBe("markdown");
    expect(languageFor("/srv/data.bin.unknown")).toBe("text");
  });

  it("nginx 目录下的 conf 识别为 nginx，其余 conf 为 ini 类", () => {
    expect(languageFor("/etc/nginx/conf.d/site.conf")).toBe("nginx");
    expect(languageFor("C:\\etc\\nginx\\site.conf")).toBe("nginx");
    expect(languageFor("/etc/other/site.conf")).not.toBe("nginx");
  });

  it("shebang", () => {
    expect(languageFromShebang("#!/usr/bin/env python3")).toBe("python");
    expect(languageFromShebang("#!/bin/bash")).toBe("shell");
    expect(languageFromShebang("hello")).toBeNull();
  });
});

describe("editorModel", () => {
  it("状态栏显示编码、换行和位置", () => {
    expect(encodingLabel(true)).toBe("UTF-8 (BOM)");
    const parts = statusSegments({
      bom: false,
      eol: "CRLF",
      line: 3,
      column: 7,
      language: "text",
      wrap: true,
      selected: 0,
    });
    expect(parts[0]).toBe("行 3，列 7");
    expect(parts).toContain("UTF-8");
    expect(parts).toContain("CRLF");
    expect(parts).toContain("自动换行");
  });

  it("同一个文件判定：远端按主机+路径，本地不分大小写和斜杠", () => {
    expect(
      isSameFile(
        { side: "remote", hostId: 1, path: "/a" },
        { side: "remote", hostId: 1, path: "/a" },
      ),
    ).toBe(true);
    expect(
      isSameFile(
        { side: "remote", hostId: 1, path: "/a" },
        { side: "remote", hostId: 2, path: "/a" },
      ),
    ).toBe(false);
    expect(
      isSameFile(
        { side: "local", hostId: 0, path: "C:/Work/A.txt" },
        { side: "local", hostId: 0, path: "c:\\work\\a.txt" },
      ),
    ).toBe(true);
  });

  it("被拒绝的文件用「不能用内置编辑器打开」作标题", () => {
    expect(describeOpenError({ code: "binary", message: "x" }, "a").title).toContain(
      "不能用内置编辑器打开",
    );
    expect(describeOpenError({ code: "remote", message: "x" }, "a").title).toContain("打不开");
  });

  it("本地文件另存为的默认名字", () => {
    expect(defaultLocalSaveAsName("a.txt")).toBe("a (副本).txt");
    expect(defaultLocalSaveAsName("Makefile")).toBe("Makefile (副本)");
    expect(defaultLocalSaveAsName(".env")).toBe(".env (副本)");
  });

  it("标签悬停：远端显示主机:路径，本地符号链接显示「链接 → 目标」", () => {
    expect(tabTooltip({ side: "remote", hostLabel: "web-1", path: "/etc/a.conf" })).toBe(
      "web-1:/etc/a.conf",
    );
    expect(tabTooltip({ side: "local", hostLabel: "本机", path: "C:\\work\\a.txt" })).toBe(
      "C:\\work\\a.txt",
    );
    expect(
      tabTooltip({
        side: "local",
        hostLabel: "本机",
        path: "C:\\work\\link.txt",
        linkTarget: "C:\\data\\real.txt",
      }),
    ).toBe("C:\\work\\link.txt → C:\\data\\real.txt");
  });

  it("关闭询问按数量改写文案", () => {
    const one = unsavedPrompt([{ editId: 1, fileName: "a.txt", path: "/a.txt" }], "close-tab");
    expect(one.title).toContain("a.txt");
    const many = unsavedPrompt(
      Array.from({ length: 8 }, (_, i) => ({ editId: i, fileName: `f${i}`, path: `/f${i}` })),
      "quit",
    );
    expect(many.title).toContain("8");
    expect(many.message).toContain("退出程序");
  });

  it("退出时带上其他后果：写在同一个询问里，单个文件和多个文件都一样", () => {
    const extra = "还有 2 个连接（2 个终端），退出会全部断开。";
    const one = unsavedPrompt([{ editId: 1, fileName: "a.txt", path: "/a.txt" }], "quit", extra);
    expect(one.message).toContain(extra);
    expect(one.message).toContain("继续退出");
    const many = unsavedPrompt(
      Array.from({ length: 3 }, (_, i) => ({ editId: i, fileName: `f${i}`, path: `/f${i}` })),
      "quit",
      extra,
    );
    expect(many.message).toContain(extra);
    // 没有其他后果：文案不变
    expect(unsavedPrompt([{ editId: 1, fileName: "a.txt", path: "/a.txt" }], "quit").message).not.toContain("继续退出");
  });
});
