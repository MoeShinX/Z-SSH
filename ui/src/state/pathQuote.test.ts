//! 往终端插路径的引号规则（M3-3 第 6.1 节）。

import { describe, expect, it } from "vitest";

import { joinQuoted, quoteLocalPath, quoteRemotePath } from "./pathQuote";

describe("quoteRemotePath", () => {
  it("普通路径原样", () => {
    expect(quoteRemotePath("/var/log/syslog")).toBe("/var/log/syslog");
    expect(quoteRemotePath("/srv/app-1/a_b.tar.gz")).toBe("/srv/app-1/a_b.tar.gz");
  });

  it("带空格、中文、括号：单引号包起来", () => {
    expect(quoteRemotePath("/srv/my dir/a.txt")).toBe("'/srv/my dir/a.txt'");
    expect(quoteRemotePath("/srv/资料/说明.txt")).toBe("'/srv/资料/说明.txt'");
    expect(quoteRemotePath("/srv/a(1).txt")).toBe("'/srv/a(1).txt'");
  });

  it("路径里有单引号：转义，不会提前闭合", () => {
    expect(quoteRemotePath("/srv/it's.txt")).toBe(`'/srv/it'\''s.txt'`);
  });

  it("$、反引号、分号这类 shell 元字符不会被执行：都在单引号里", () => {
    expect(quoteRemotePath("/srv/$(rm -rf x);`id`")).toBe("'/srv/$(rm -rf x);`id`'");
  });
});

describe("quoteLocalPath / joinQuoted", () => {
  it("本地路径加双引号", () => {
    expect(quoteLocalPath("C:\zssh shots\a.txt")).toBe('"C:\zssh shots\a.txt"');
  });

  it("多个路径用空格隔开，不带回车和结尾空格", () => {
    const text = joinQuoted(["/a", "/b c"], quoteRemotePath);
    expect(text).toBe("/a '/b c'");
    expect(text.endsWith(" ")).toBe(false);
    expect(text.includes("\n") || text.includes("\r")).toBe(false);
  });
});
