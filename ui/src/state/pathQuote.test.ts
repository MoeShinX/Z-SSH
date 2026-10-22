//! 往终端插路径的引号规则（M3-3 第 6.1 节；TERM-1 T01）。

import { describe, expect, it } from "vitest";

import {
  hasControlChars,
  joinQuoted,
  quoteLocalPath,
  quoteRemotePath,
} from "./pathQuote";

/**
 * 按 POSIX shell 的规则解析引用结果这一个词（够用的子集）：单引号段原样，`\'` 是一个单引号，
 * 裸的只认安全字符；遇到其他会被 shell 解释的字符（`$`、反引号、双引号、空白、`;()` 等）就抛错，
 * 这样引用漏掉东西时测试直接失败，而不是悄悄解析成别的字符串。
 */
function reparse(quoted: string): string {
  let out = "";
  let i = 0;
  while (i < quoted.length) {
    const ch = quoted[i];
    if (ch === "'") {
      const end = quoted.indexOf("'", i + 1);
      if (end < 0) throw new Error(`单引号没有闭合：${quoted}`);
      out += quoted.slice(i + 1, end);
      i = end + 1;
    } else if (ch === "\\" && quoted[i + 1] === "'") {
      out += "'";
      i += 2;
    } else if (/[A-Za-z0-9_/.,:@%+=~-]/.test(ch)) {
      out += ch;
      i += 1;
    } else {
      throw new Error(`引用之外有 shell 会解释的字符 ${JSON.stringify(ch)}：${quoted}`);
    }
  }
  return out;
}

const SAMPLES: Record<string, string> = {
  命令替换: String.raw`C:\Users\<用户>\note-$(printf PROBE).txt`,
  反引号: String.raw`C:\Users\<用户>\note-` + "`id`" + ".txt",
  环境变量: String.raw`C:\Users\<用户>\$HOME\${PATH}.txt`,
  单引号: String.raw`C:\Users\<用户>\it's a 'test'.txt`,
  双引号: String.raw`C:\Users\<用户>\say "hi".txt`,
  反斜杠: String.raw`C:\a\b\\c\n\t.txt`,
  空格: String.raw`C:\Program Files\my dir\a b.txt`,
  中文: String.raw`D:\资料\说明 文档.txt`,
  分号和管道: String.raw`C:\x\a;b|c&d>e<f.txt`,
  只有单引号: "'''",
  空串: "",
};

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

  it("路径里有单引号：写成反斜杠加单引号的转义，不会提前闭合", () => {
    // 必须是 反斜杠 + 单引号 共四个字符的 '\''：用模板字面量写会把 \' 吃掉，这里曾经就写错过
    expect(quoteRemotePath("/srv/it's.txt")).toBe(String.raw`'/srv/it'\''s.txt'`);
    expect(reparse(quoteRemotePath("/srv/it's.txt"))).toBe("/srv/it's.txt");
  });

  it("$、反引号、分号这类 shell 元字符不会被执行：都在单引号里", () => {
    expect(quoteRemotePath("/srv/$(rm -rf x);`id`")).toBe("'/srv/$(rm -rf x);`id`'");
  });

  for (const [name, path] of Object.entries(SAMPLES)) {
    it(`${name}：解析出的参数和原路径逐字一致`, () => {
      expect(reparse(quoteRemotePath(path))).toBe(path);
    });
  }
});

describe("quoteLocalPath（T01：按接收端的 POSIX shell 引用）", () => {
  it("Windows 路径加单引号，反斜杠原样保留", () => {
    expect(quoteLocalPath(String.raw`C:\zssh shots\a.txt`)).toBe(String.raw`'C:\zssh shots\a.txt'`);
  });

  it("不再用双引号包路径", () => {
    expect(quoteLocalPath(String.raw`C:\a`)).not.toContain('"');
    expect(quoteLocalPath(String.raw`C:\say "hi".txt`)).toBe(String.raw`'C:\say "hi".txt'`);
  });

  it("命令替换、反引号、变量在单引号里原样，不会被展开", () => {
    expect(quoteLocalPath("C:\\n-$(id).txt")).toBe(String.raw`'C:\n-$(id).txt'`);
    expect(quoteLocalPath("a`id`b$HOME")).toBe("'a`id`b$HOME'");
  });

  it("单引号写成反斜杠加单引号的转义", () => {
    expect(quoteLocalPath("it's")).toBe(String.raw`'it'\''s'`);
  });

  for (const [name, path] of Object.entries(SAMPLES)) {
    it(`${name}：交给 POSIX shell 解析出的参数和原路径逐字一致`, () => {
      expect(reparse(quoteLocalPath(path))).toBe(path);
    });
  }
});

describe("hasControlChars", () => {
  it("换行、回车、ESC、NUL、制表、DEL、Ctrl+C 都算", () => {
    for (const ch of ["\n", "\r", "\x1b", "\x00", "\t", "\x7f", "\x03"]) {
      expect(hasControlChars(`a${ch}b`)).toBe(true);
    }
  });

  it("普通文件名（空格、引号、中文、emoji、$）不算", () => {
    for (const path of ["/srv/my dir/it's.txt", String.raw`D:\资料\说明.txt`, "/a/b🙂.txt", "$(id)"]) {
      expect(hasControlChars(path)).toBe(false);
    }
  });
});

describe("joinQuoted", () => {
  it("多个路径用空格隔开，不带回车和结尾空格", () => {
    const text = joinQuoted(["/a", "/b c"], quoteRemotePath);
    expect(text).toBe("/a '/b c'");
    expect(text.endsWith(" ")).toBe(false);
    expect(text.includes("\n") || text.includes("\r")).toBe(false);
  });

  it("本地路径也一样", () => {
    expect(joinQuoted([String.raw`C:\a b`, String.raw`C:\c`], quoteLocalPath)).toBe(
      String.raw`'C:\a b' 'C:\c'`,
    );
  });
});
