/** @vitest-environment jsdom */
//! WebView 自带的右键菜单（含「刷新」）和刷新快捷键：只在文本输入框、终端、选区本身上留着自带菜单，
//! 其他地方一律不弹；刷新快捷键只拦默认动作，事件照样往下传。
//! jsdom 不做排版，选区矩形（getClientRects）用替身注入。

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  allowNativeContextMenu,
  installNativeGuards,
  isReloadShortcut,
  pointInSelection,
  type RectsOfRange,
} from "./nativeGuard";

/** 替身：每个选区范围都是 (10,10)-(110,30) 这一个矩形。 */
const fakeRects: RectsOfRange = () => [{ left: 10, top: 10, right: 110, bottom: 30 }];
const INSIDE = { x: 50, y: 20 };
const OUTSIDE = { x: 300, y: 200 };

let uninstall: () => void = () => {};

beforeEach(() => {
  document.body.innerHTML = "";
  window.getSelection()?.removeAllRanges();
  uninstall = installNativeGuards(window, { rectsOf: fakeRects });
});

afterEach(() => uninstall());

function rightClick(target: Element, at: { x: number; y: number } = OUTSIDE): boolean {
  const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: at.x, clientY: at.y });
  target.dispatchEvent(event);
  return event.defaultPrevented;
}

/** 选中一个元素里的全部内容。 */
function select(node: Node) {
  const range = document.createRange();
  range.selectNodeContents(node);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

describe("右键自带菜单", () => {
  it("空白处、按钮、普通文字上：不弹（默认动作被拦）", () => {
    const panel = document.createElement("div");
    const button = document.createElement("button");
    const text = document.createElement("p");
    text.textContent = "用自然语言让 AI 在当前标签的服务器上排查问题";
    panel.append(button, text);
    document.body.append(panel);
    expect(rightClick(panel)).toBe(true);
    expect(rightClick(button)).toBe(true);
    expect(rightClick(text)).toBe(true);
  });

  it("输入框、文本框、可编辑区域里：留着（要用剪切 / 复制 / 粘贴）", () => {
    const input = document.createElement("input");
    const textarea = document.createElement("textarea");
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    const inner = document.createElement("span");
    editable.append(inner);
    document.body.append(input, textarea, editable);
    expect(rightClick(input)).toBe(false);
    expect(rightClick(textarea)).toBe(false);
    expect(allowNativeContextMenu(inner, null, OUTSIDE, fakeRects)).toBe(true);
  });

  it("只读的文本框、文本域：留着（要能复制）；禁用的不弹", () => {
    const readOnly = document.createElement("input");
    readOnly.readOnly = true;
    const readOnlyArea = document.createElement("textarea");
    readOnlyArea.readOnly = true;
    const disabled = document.createElement("input");
    disabled.disabled = true;
    const disabledArea = document.createElement("textarea");
    disabledArea.disabled = true;
    document.body.append(readOnly, readOnlyArea, disabled, disabledArea);
    expect(rightClick(readOnly)).toBe(false);
    expect(rightClick(readOnlyArea)).toBe(false);
    expect(rightClick(disabled)).toBe(true);
    expect(rightClick(disabledArea)).toBe(true);
  });

  it("密码框：放行（剪切 / 复制由 Chromium 自己置灰，粘贴照常可用）", () => {
    const password = document.createElement("input");
    password.type = "password";
    document.body.append(password);
    expect(rightClick(password)).toBe(false);
  });

  it("勾选框、单选框、按钮类 input、文件框：不弹", () => {
    for (const type of ["checkbox", "radio", "button", "submit", "range", "file", "color"]) {
      const input = document.createElement("input");
      input.type = type;
      document.body.append(input);
      expect(rightClick(input), type).toBe(true);
    }
  });

  it("终端（xterm）：点到画布、行、隐藏输入框都留着，不会因为没有 DOM 选区被拦掉", () => {
    // xterm 的选区画在画布上；右键时它把隐藏输入框挪到鼠标下并选中终端选区文字，粘贴靠这个
    const terminal = document.createElement("div");
    terminal.className = "xterm";
    const screen = document.createElement("div");
    screen.className = "xterm-screen";
    const canvas = document.createElement("canvas");
    const helper = document.createElement("textarea");
    helper.className = "xterm-helper-textarea";
    screen.append(canvas);
    terminal.append(screen, helper);
    document.body.append(terminal);
    expect(window.getSelection()?.isCollapsed).toBe(true);
    expect(rightClick(canvas)).toBe(false);
    expect(rightClick(screen)).toBe(false);
    expect(rightClick(helper)).toBe(false);
  });

  it("终端：开了右键粘贴时终端自己 preventDefault，guard 不再插手也不放开", () => {
    const terminal = document.createElement("div");
    terminal.className = "xterm";
    const canvas = document.createElement("canvas");
    terminal.append(canvas);
    terminal.addEventListener("contextmenu", (event) => event.preventDefault());
    document.body.append(terminal);
    expect(rightClick(canvas)).toBe(true);
  });

  it("选中文字：右键落在选区矩形里才留着", () => {
    const text = document.createElement("p");
    text.textContent = "要复制的一段";
    const other = document.createElement("div");
    document.body.append(text, other);
    select(text);
    expect(rightClick(text, INSIDE)).toBe(false);
    // 有选区，但点在别处（空白、别的元素）：照旧不弹
    expect(rightClick(other, OUTSIDE)).toBe(true);
    expect(rightClick(text, OUTSIDE)).toBe(true);
  });

  it("只选中了空格也算选区（不 trim）", () => {
    const text = document.createElement("p");
    text.textContent = "   ";
    document.body.append(text);
    select(text);
    expect(rightClick(text, INSIDE)).toBe(false);
    expect(rightClick(text, OUTSIDE)).toBe(true);
  });

  it("没有选区：落在“矩形”里也不弹", () => {
    const text = document.createElement("p");
    text.textContent = "普通文字";
    document.body.append(text);
    expect(rightClick(text, INSIDE)).toBe(true);
  });

  it("pointInSelection：多个矩形任一个命中即可；边界算命中；没有选区信息返回 false", () => {
    const text = document.createElement("p");
    text.textContent = "两行的选区";
    document.body.append(text);
    select(text);
    const selection = window.getSelection();
    const twoLines: RectsOfRange = () => [
      { left: 0, top: 0, right: 100, bottom: 20 },
      { left: 0, top: 20, right: 40, bottom: 40 },
    ];
    expect(pointInSelection(selection, { x: 30, y: 30 }, twoLines)).toBe(true);
    expect(pointInSelection(selection, { x: 40, y: 40 }, twoLines)).toBe(true);
    expect(pointInSelection(selection, { x: 70, y: 30 }, twoLines)).toBe(false);
    expect(pointInSelection(null, { x: 30, y: 30 }, twoLines)).toBe(false);
    expect(allowNativeContextMenu(null, selection, { x: 30, y: 30 }, twoLines)).toBe(false);
  });

  it("页面里自己处理过的右键菜单（已 preventDefault）不再管", () => {
    const own = document.createElement("div");
    own.addEventListener("contextmenu", (event) => event.preventDefault());
    document.body.append(own);
    expect(rightClick(own)).toBe(true);
  });
});

describe("刷新快捷键", () => {
  it("F5、Ctrl+R、Ctrl+Shift+R 都算；别的键不算", () => {
    expect(isReloadShortcut({ key: "F5", ctrlKey: false, metaKey: false })).toBe(true);
    expect(isReloadShortcut({ key: "r", ctrlKey: true, metaKey: false })).toBe(true);
    expect(isReloadShortcut({ key: "R", ctrlKey: true, metaKey: false })).toBe(true);
    expect(isReloadShortcut({ key: "r", ctrlKey: false, metaKey: false })).toBe(false);
    expect(isReloadShortcut({ key: "c", ctrlKey: true, metaKey: false })).toBe(false);
  });

  it("只拦默认动作，事件照样传给终端 / 文件面板", () => {
    const terminal = document.createElement("textarea");
    document.body.append(terminal);
    let received = 0;
    terminal.addEventListener("keydown", () => received++);
    const event = new KeyboardEvent("keydown", { key: "r", ctrlKey: true, bubbles: true, cancelable: true });
    terminal.dispatchEvent(event);
    expect(received).toBe(1);
    expect(event.defaultPrevented).toBe(true);
    const plain = new KeyboardEvent("keydown", { key: "a", bubbles: true, cancelable: true });
    terminal.dispatchEvent(plain);
    expect(plain.defaultPrevented).toBe(false);
  });
});
