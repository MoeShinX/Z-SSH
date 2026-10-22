/** @vitest-environment jsdom */
//! AI 回复的 Markdown 排版（AI-UX-1 第 3 项）：渲染正确、不渲染 HTML、不加载图片、链接不跳转。

import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { AssistantMessage, Markdown } from "./Markdown";

const writeText = vi.fn(async (_text: string) => {});

beforeEach(() => {
  writeText.mockClear();
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
});

afterEach(cleanup);

const html = (text: string) => render(<Markdown text={text} />).container;

describe("基本排版", () => {
  it("标题、加粗、斜体、行内代码、引用、分隔线", () => {
    const root = html("## 硬件配置\n\n**主机** 和 *斜体* 以及 `df -h`\n\n> 引用一句\n\n---\n\n~~删掉~~");
    expect(root.querySelector("h2")?.textContent).toBe("硬件配置");
    expect(root.querySelector("strong")?.textContent).toBe("主机");
    expect(root.querySelector("em")?.textContent).toBe("斜体");
    expect(root.querySelector("p code")?.textContent).toBe("df -h");
    expect(root.querySelector("blockquote")?.textContent).toContain("引用一句");
    expect(root.querySelector("hr")).not.toBeNull();
    expect(root.querySelector("del")?.textContent).toBe("删掉");
    // 不能留下没解析的标记
    expect(root.textContent).not.toContain("##");
    expect(root.textContent).not.toContain("**");
  });

  it("有序 / 无序列表", () => {
    const root = html("- 甲\n- 乙\n\n1. 一\n2. 二");
    expect(root.querySelectorAll("ul > li")).toHaveLength(2);
    expect(root.querySelectorAll("ol > li")).toHaveLength(2);
  });

  it("表格：包在可横向滚动的容器里", () => {
    const root = html("| 项目 | 值 |\n| --- | --- |\n| CPU | 8 核 |\n| 内存 | 16G |");
    const wrap = root.querySelector('[data-section="md-table"]');
    expect(wrap?.className).toContain("overflow-x-auto");
    expect(wrap?.querySelectorAll("th")).toHaveLength(2);
    expect(wrap?.querySelectorAll("tbody tr")).toHaveLength(2);
    expect(wrap?.textContent).toContain("8 核");
  });

  it("代码块：等宽、横向滚动、有复制按钮，复制的是代码本身；没有执行按钮", async () => {
    const root = html("```bash\nls -la\ndf -h\n```");
    const block = root.querySelector('[data-section="md-code-block"]');
    expect(block).not.toBeNull();
    const pre = block?.querySelector("pre");
    expect(pre?.className).toContain("overflow-x-auto");
    expect(pre?.className).toContain("font-mono");
    expect(pre?.textContent).toContain("ls -la");
    // 代码块里的 code 不套行内代码的底色
    expect(pre?.querySelector("code")?.className ?? "").not.toContain("bg-muted");
    const buttons = block?.querySelectorAll("button") ?? [];
    expect(buttons).toHaveLength(1);
    expect(buttons[0].textContent).toContain("复制");
    await act(async () => {
      fireEvent.click(buttons[0]);
    });
    expect(writeText).toHaveBeenCalledWith("ls -la\ndf -h");
  });

  it("段落里的单个换行照样换行", () => {
    const root = html("第一行\n第二行");
    expect(root.querySelector("p")?.className).toContain("whitespace-pre-line");
    expect(root.querySelector("p")?.textContent).toBe("第一行\n第二行");
  });
});

describe("安全：AI 的输出可能夹带提示注入内容", () => {
  it("原始 HTML 不产生元素，也不执行", () => {
    const evil = (globalThis as unknown as { __pwned?: boolean });
    evil.__pwned = false;
    const root = html(
      '<img src=x onerror="globalThis.__pwned=true"> <script>globalThis.__pwned=true</script> <iframe src="http://x"></iframe> <b onclick="x()">粗</b>',
    );
    expect(root.querySelector("img, script, iframe, b")).toBeNull();
    expect(root.querySelector("[onerror], [onclick]")).toBeNull();
    expect(evil.__pwned).toBe(false);
  });

  it("Markdown 图片不产生 img，显示成文字，不发请求", () => {
    const requests: string[] = [];
    const original = globalThis.fetch;
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      requests.push(String(input));
      return new Response("");
    }) as typeof fetch;
    try {
      const root = html("看图 ![架构图](http://evil.example/a.png) 完");
      expect(root.querySelector("img")).toBeNull();
      expect(root.textContent).toContain("[图片：架构图]");
      expect(root.innerHTML).not.toContain("evil.example");
      expect(requests).toEqual([]);
    } finally {
      globalThis.fetch = original;
    }
  });

  it("链接不是 <a>：显示文字和完整网址，点击只复制，不导航", async () => {
    const root = html("见 [官网](https://example.com/path?a=1) 和 https://auto.example/x");
    expect(root.querySelector("a")).toBeNull();
    expect(root.querySelector("[href]")).toBeNull();
    expect(root.textContent).toContain("官网");
    expect(root.textContent).toContain("（https://example.com/path?a=1）");
    const hrefBefore = window.location.href;
    const buttons = root.querySelectorAll<HTMLButtonElement>('[data-section="md-link"] button');
    expect(buttons).toHaveLength(2);
    await act(async () => {
      fireEvent.click(buttons[0]);
    });
    expect(writeText).toHaveBeenCalledWith("https://example.com/path?a=1");
    expect(window.location.href).toBe(hrefBefore);
    // 文字本来就是网址：不重复写一遍
    expect(root.textContent?.match(/auto\.example\/x/g)).toHaveLength(1);
  });

  it("javascript: 链接被过滤，只剩文字、没有可点的东西", () => {
    const root = html("[点我](javascript:alert(1))");
    expect(root.querySelector("a, button")).toBeNull();
    expect(root.textContent).toContain("点我");
    expect(root.innerHTML).not.toContain("javascript:");
  });
});

describe("流式输出写到一半", () => {
  it("表格只写了一半：不报错，内容照常显示", () => {
    for (const partial of ["| 项目 | 值 |", "| 项目 | 值 |\n| ---", "| 项目 | 值 |\n| --- | --- |\n| CPU |", "```bash\nls", "**加粗没", "[链接](http://x"]) {
      const { container, unmount } = render(<Markdown text={partial} />);
      expect(container.textContent?.length ?? 0).toBeGreaterThan(0);
      unmount();
    }
  });

  it("没写完的代码块也显示成代码块", () => {
    const root = html("```bash\nls -la");
    expect(root.querySelector("pre")?.textContent).toContain("ls -la");
  });
});

describe("复制整条消息", () => {
  it("复制的是原始 Markdown 文本", async () => {
    const raw = "## 标题\n\n**重点**\n\n| a | b |\n| - | - |\n| 1 | 2 |";
    const { container } = render(<AssistantMessage text={raw} />);
    await act(async () => {
      fireEvent.click(container.querySelector('[data-action="copy-message"]') as HTMLElement);
    });
    expect(writeText).toHaveBeenCalledWith(raw);
  });
});
