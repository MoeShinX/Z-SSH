/** @vitest-environment jsdom */
//! 终端焦点 ↔ 输入法的请求队列：串行、分屏不闪、进别的输入框立刻恢复、恢复失败会补发。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createImeFocusController, isEditableElement } from "./imeFocus";

interface Pending {
  focused: boolean;
  resolve: (inTerminal: boolean) => void;
}

/** 手动放行的 send：能看到"同一时刻最多一个在途请求"。 */
function manualSend() {
  const sent: boolean[] = [];
  const pending: Pending[] = [];
  const send = (focused: boolean) =>
    new Promise<boolean>((resolve) => {
      sent.push(focused);
      pending.push({ focused, resolve });
    });
  /** 放行最早的在途请求；默认按后端的正常语义回答。 */
  const release = async (answer?: boolean) => {
    const next = pending.shift();
    if (!next) throw new Error("没有在途请求");
    next.resolve(answer ?? next.focused);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  };
  return { sent, pending, send, release };
}

/** 自动放行的 send。 */
function autoSend(answer?: (focused: boolean) => boolean) {
  const sent: boolean[] = [];
  const send = async (focused: boolean) => {
    sent.push(focused);
    return answer ? answer(focused) : focused;
  };
  return { sent, send };
}

const makeTextarea = (): HTMLTextAreaElement => {
  const element = document.createElement("textarea");
  document.body.appendChild(element);
  return element;
};

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  document.body.innerHTML = "";
});

describe("请求串行发出", () => {
  it("上一个请求没返回，下一个不发", async () => {
    const m = manualSend();
    const controller = createImeFocusController(m.send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus(); // focusin 立刻要求恢复
    await vi.advanceTimersByTimeAsync(0);
    expect(m.sent).toEqual([true]);
    await m.release();
    await vi.advanceTimersByTimeAsync(0);
    expect(m.sent).toEqual([true, false]);
    await m.release();
    expect(m.pending).toHaveLength(0);
  });

  it("终端 → AI 输入框 → 终端 → AI 输入框：最后发出的是恢复", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, true, false]);
  });
});

describe("分屏之间切换", () => {
  it("焦点从一个终端直接交给另一个：不发恢复", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const a = makeTextarea();
    const b = makeTextarea();
    controller.attach(a);
    controller.attach(b);
    a.focus();
    b.focus();
    a.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("blur 的 relatedTarget 是空（点到空白处再聚焦另一个终端）：等待期内拿到焦点就不恢复", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const a = makeTextarea();
    const b = makeTextarea();
    controller.attach(a);
    controller.attach(b);
    a.focus();
    a.blur();
    await vi.advanceTimersByTimeAsync(20);
    b.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("终端失去焦点又没去别的终端：等一小会儿后恢复", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const a = makeTextarea();
    controller.attach(a);
    a.focus();
    a.blur();
    await vi.advanceTimersByTimeAsync(10);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });
});

describe("焦点进终端以外的输入元素", () => {
  it("blur 丢了也能补一次恢复：直接 focusin 到别的输入框", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    await vi.advanceTimersByTimeAsync(0);
    // 不经过终端的 blur，直接给别的输入框派 focusin
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
  });

  it("焦点进按钮 / 复选框不算：不会提前恢复（走 blur 的等待）", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    const box = document.createElement("input");
    box.type = "checkbox";
    document.body.appendChild(box);
    box.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    const button = document.createElement("button");
    document.body.appendChild(button);
    button.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true]);
  });

  it("从没进过终端：进输入框不发任何请求", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    controller.attach(makeTextarea());
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });
});

describe("后端没恢复成功", () => {
  it("离开时后端说还没恢复（窗口不在前台）：之后焦点进输入框再补一次", async () => {
    // 进入返回 true；第一次离开返回 true（失败），第二次离开返回 false
    let leaves = 0;
    const { sent, send } = autoSend((focused) => (focused ? true : ++leaves < 2));
    const controller = createImeFocusController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    term.blur(); // 窗口切走：只触发 blur
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    ai.focus(); // 窗口回来，焦点落在 AI 输入框
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false, false]);
    // 这次成功了：再进别的输入框不再发
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
  });

  it("进入没被后端接受（窗口还没拿到前台）：下次焦点进终端重发", async () => {
    let accept = false;
    const { sent, send } = autoSend((focused) => (focused ? accept : false));
    const controller = createImeFocusController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    await vi.advanceTimersByTimeAsync(0);
    term.blur();
    await vi.advanceTimersByTimeAsync(500);
    accept = true;
    term.focus();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent.filter((value) => value)).toHaveLength(2);
  });

  it("send 抛错不会卡住队列", async () => {
    const calls: boolean[] = [];
    let first = true;
    const controller = createImeFocusController(async (focused) => {
      calls.push(focused);
      if (first) {
        first = false;
        throw new Error("ipc 失败");
      }
      return focused;
    });
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    await vi.advanceTimersByTimeAsync(0);
    term.blur();
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(calls).toEqual([true, true]);
  });
});

describe("取消登记", () => {
  it("焦点正在终端时取消（关掉设置开关）：立即恢复", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const term = makeTextarea();
    const detach = controller.attach(term);
    term.focus();
    detach();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
  });

  it("取消的是没拿焦点的终端：不影响正在用的那个", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const a = makeTextarea();
    const b = makeTextarea();
    controller.attach(a);
    const detachB = controller.attach(b);
    a.focus();
    detachB();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("取消后终端不再响应焦点", async () => {
    const { sent, send } = autoSend();
    const controller = createImeFocusController(send);
    const term = makeTextarea();
    const detach = controller.attach(term);
    detach();
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });
});

describe("isEditableElement", () => {
  it("文本框、textarea、contenteditable 算；按钮、复选框、只读不算", () => {
    const make = (tag: string, props: Record<string, unknown> = {}) =>
      Object.assign(document.createElement(tag), props);
    expect(isEditableElement(make("input", { type: "text" }))).toBe(true);
    expect(isEditableElement(make("input", { type: "password" }))).toBe(true);
    expect(isEditableElement(make("textarea"))).toBe(true);
    expect(isEditableElement(make("textarea", { readOnly: true }))).toBe(false);
    expect(isEditableElement(make("input", { type: "checkbox" }))).toBe(false);
    expect(isEditableElement(make("input", { type: "text", disabled: true }))).toBe(false);
    expect(isEditableElement(make("button"))).toBe(false);
    expect(isEditableElement(null)).toBe(false);
  });
});
