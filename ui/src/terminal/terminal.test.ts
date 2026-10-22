/** @vitest-environment jsdom */
//! 终端实例的链接接管：用真的 xterm 验证程序输出的 OSC 8 超链接走我们的 linkHandler
//! （和文本网址同一套回调），不走 xterm 默认的 confirm + window.open。
//!
//! xterm 的链接提供者是内部对象（没有公开 API 能不经鼠标取到某一行的链接），
//! 测试里从 `_core` 上取来直接调 `provideLinks`——这是 xterm 点击路径的上游，鼠标事件
//! 只是在它的结果里按位置挑一条。

import type { Terminal } from "@xterm/xterm";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HoveredLink, LinkRange, TerminalLinkHandlers } from "./links";
import { createTerminal } from "./terminal";

interface ProvidedLink {
  text: string;
  range: LinkRange;
  activate: (event: MouseEvent, text: string) => void;
  hover?: (event: MouseEvent, text: string) => void;
  leave?: (event: MouseEvent, text: string) => void;
}
interface LinkProvider {
  provideLinks: (y: number, callback: (links: ProvidedLink[] | undefined) => void) => void;
}

function providedLinks(terminal: Terminal, row: number): ProvidedLink[] {
  const core = terminal as unknown as {
    _core: { _linkProviderService: { linkProviders: LinkProvider[] } };
  };
  const found: ProvidedLink[] = [];
  for (const provider of core._core._linkProviderService.linkProviders) {
    provider.provideLinks(row, (links) => found.push(...(links ?? [])));
  }
  return found;
}

/** 程序输出一个 OSC 8 超链接（BEL 结尾）：显示文字是 text，目标是 uri。 */
function osc8(uri: string, text: string): string {
  return `\x1b]8;;${uri}\x07${text}\x1b]8;;\x07`;
}

function write(terminal: Terminal, data: string): Promise<void> {
  return new Promise((resolve) => terminal.write(data, resolve));
}

function setup() {
  const activated: { uri: string; detail: number; range?: LinkRange }[] = [];
  const hovered: (HoveredLink | null)[] = [];
  const handlers: TerminalLinkHandlers = {
    onActivate: (uri, event, range) => activated.push({ uri, detail: event.detail, range }),
    onHover: (link) => hovered.push(link),
  };
  const { terminal } = createTerminal({}, handlers);
  return { terminal, activated, hovered };
}

afterEach(() => vi.restoreAllMocks());

describe("OSC 8 超链接", () => {
  it("显示文字和目标不同：范围是显示文字，回调里的网址是目标", async () => {
    const { terminal, activated, hovered } = setup();
    await write(terminal, `ab ${osc8("https://target.example/p?q=1", "show me")} cd`);

    const links = providedLinks(terminal, 1).filter((l) => l.text === "https://target.example/p?q=1");
    expect(links).toHaveLength(1);
    // 显示文字 "show me" 在第 4～10 列（1 起始、含右端）
    expect(links[0].range).toEqual({ start: { x: 4, y: 1 }, end: { x: 10, y: 1 } });

    const left = new MouseEvent("mouseup", { button: 0, detail: 2 });
    links[0].hover?.(left, links[0].text);
    links[0].activate(left, links[0].text);
    links[0].leave?.(left, links[0].text);
    expect(hovered).toEqual([{ uri: "https://target.example/p?q=1", range: links[0].range }, null]);
    expect(activated).toEqual([
      { uri: "https://target.example/p?q=1", detail: 2, range: links[0].range },
    ]);
  });

  it("不弹 confirm、不 window.open；右键抬起不当作点击", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    const { terminal, activated } = setup();
    await write(terminal, osc8("https://target.example/", "xy"));

    const [link] = providedLinks(terminal, 1);
    link.activate(new MouseEvent("mouseup", { button: 2 }), link.text);
    expect(activated).toEqual([]);
    link.activate(new MouseEvent("mouseup", { button: 0 }), link.text);
    expect(activated).toHaveLength(1);
    expect(confirm).not.toHaveBeenCalled();
    expect(open).not.toHaveBeenCalled();
  });

  it("非 http / https 的目标不会成为链接（allowNonHttpProtocols 保持默认）", async () => {
    const { terminal } = setup();
    expect(terminal.options.linkHandler?.allowNonHttpProtocols).toBeUndefined();
    await write(
      terminal,
      `${osc8("file:///C:/x.exe", "run")} ${osc8("javascript:alert(1)", "js")}`,
    );
    expect(providedLinks(terminal, 1)).toEqual([]);
  });

  it("文本里的网址仍走同一套回调（带事件，不带范围）", async () => {
    const { terminal, activated } = setup();
    await write(terminal, "see https://plain.example/a ok");
    const link = providedLinks(terminal, 1).find((l) => l.text === "https://plain.example/a");
    expect(link).toBeDefined();
    link?.activate(new MouseEvent("mouseup", { button: 0, detail: 1 }), "https://plain.example/a");
    expect(activated).toEqual([{ uri: "https://plain.example/a", detail: 1, range: undefined }]);
  });
});
