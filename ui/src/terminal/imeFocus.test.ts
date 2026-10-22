/** @vitest-environment jsdom */
//! 终端焦点 ↔ 输入法的请求队列：串行、分屏不闪、只在进了可输入元素以后（稍等一下）才恢复、
//! 恢复失败会在下一次进输入框时补发。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createImeFocusController, isEditableElement } from "./imeFocus";

/** 显式传给控制器的延迟，避免测试跟着实现里的常量漂。 */
const RESTORE_MS = 40;
const LEAVE_MS = 80;

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

const makeController = (send: (focused: boolean) => Promise<boolean>) =>
  createImeFocusController(send, RESTORE_MS, LEAVE_MS);

const makeTextarea = (): HTMLTextAreaElement => {
  const element = document.createElement("textarea");
  document.body.appendChild(element);
  return element;
};

const makeButton = (): HTMLButtonElement => {
  const element = document.createElement("button");
  document.body.appendChild(element);
  return element;
};

const makeInput = (): HTMLInputElement => {
  const element = document.createElement("input");
  element.type = "text";
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
    const controller = makeController(m.send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus(); // 稍等一下后要求恢复
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(m.sent).toEqual([true]);
    await m.release();
    await vi.advanceTimersByTimeAsync(0);
    expect(m.sent).toEqual([true, false]);
    await m.release();
    expect(m.pending).toHaveLength(0);
  });

  it("终端 → AI 输入框 → 终端 → AI 输入框：中途回终端取消待发的恢复，最后只发一次恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });

  it("恢复发出后又回终端再进输入框：每一轮都是 进入、恢复 成对", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, true, false]);
  });
});

describe("分屏之间切换", () => {
  it("焦点从一个终端直接交给另一个：不发恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
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

  it("中间点到空白处再聚焦另一个终端：不恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
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
});

describe("离开终端后只在进了可输入元素时才恢复", () => {
  it("终端 → 不可输入的元素 → 可输入元素：只在最后一步发恢复，并且有延迟", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const button = makeButton();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    button.focus(); // 终端 blur，焦点落在不可输入的按钮上
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]); // 等多久都不恢复，只是待恢复
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 1);
    expect(sent).toEqual([true]); // 还在延迟里
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]);
  });

  it("终端 → 空白处（什么都没聚焦）→ 可输入元素：同样等到进输入框才恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    term.blur();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false]);
  });

  it("终端 → 不可输入的元素 → 回终端：什么都不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const button = makeButton();
    controller.attach(term);
    term.focus();
    button.focus();
    await vi.advanceTimersByTimeAsync(300);
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("终端 → 可输入元素：延迟之后才发恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 1);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]);
  });

  it("延迟内换到另一个输入框：从最后一次焦点变化重新等待", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    const search = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 5);
    search.focus();
    await vi.advanceTimersByTimeAsync(10);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 11);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]);
  });

  it("进输入框后在延迟内回到终端：取消，什么都不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 10);
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("进输入框后在延迟内又去了不可输入的元素：取消，等下一次进输入框", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    const button = makeButton();
    controller.attach(term);
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 10);
    button.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false]);
  });

  it("焦点进按钮 / 复选框不算可输入：不会恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    const box = document.createElement("input");
    box.type = "checkbox";
    document.body.appendChild(box);
    box.focus();
    makeButton().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("直接进别的文本输入框也一样（不经按钮）：延迟后恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    await vi.advanceTimersByTimeAsync(0);
    makeInput().focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false]);
  });

  it("从没进过终端：进输入框不发任何请求", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.attach(makeTextarea());
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });

  it("恢复成功后再进别的输入框：不重复发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    makeInput().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });
});

describe("窗口失去焦点（切到别的程序）", () => {
  it("终端有焦点时窗口失焦：照旧发离开；后端说没恢复成功，回来后进输入框再补", async () => {
    let leaves = 0;
    const { sent, send } = autoSend((focused) => (focused ? true : ++leaves < 2));
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(LEAVE_MS - 1);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]); // 前台不是本进程，后端返回仍在终端
    ai.focus(); // 窗口回来，焦点落在 AI 输入框
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false, false]);
    // 这次成功了：再进别的输入框不再发
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
  });

  it("窗口失焦后马上回来：不发离开", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(LEAVE_MS - 20);
    term.dispatchEvent(new FocusEvent("focus")); // 窗口重新激活，焦点回到终端
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("待恢复期间（焦点在按钮上）窗口失焦：不丢待恢复，回来进输入框照样恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const ai = makeTextarea();
    controller.attach(term);
    term.focus();
    makeButton().focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false]);
  });
});

describe("后端没恢复成功", () => {
  it("恢复没生效（后端返回仍在终端）：下一次进输入框重发，直到成功", async () => {
    let leaves = 0;
    const { sent, send } = autoSend((focused) => (focused ? true : ++leaves < 3));
    const controller = makeController(send);
    const term = makeTextarea();
    controller.attach(term);
    term.focus();
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    makeButton().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]); // 不可输入的地方不重发
    makeInput().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false, false]);
    makeTextarea().focus(); // 第三次成功了，不再发
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false, false]);
  });

  it("进入没被后端接受（窗口还没拿到前台）：下次焦点进终端重发", async () => {
    let accept = false;
    const { sent, send } = autoSend((focused) => (focused ? accept : false));
    const controller = makeController(send);
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
    const controller = makeController(async (focused) => {
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
    const controller = makeController(send);
    const term = makeTextarea();
    const detach = controller.attach(term);
    term.focus();
    detach();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
  });

  it("待恢复期间（焦点在按钮上）取消：立即恢复，不等输入框", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const detach = controller.attach(term);
    term.focus();
    makeButton().focus();
    detach();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
    // 恢复成功后监听已摘掉：之后进输入框什么都不发
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });

  it("取消时恢复没生效：监听保留，下次进输入框补发", async () => {
    let leaves = 0;
    const { sent, send } = autoSend((focused) => (focused ? true : ++leaves < 2));
    const controller = makeController(send);
    const term = makeTextarea();
    const detach = controller.attach(term);
    term.focus();
    makeButton().focus();
    detach();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false, false]);
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
  });

  it("取消的是没拿焦点的终端：不影响正在用的那个", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const a = makeTextarea();
    const b = makeTextarea();
    controller.attach(a);
    const detachB = controller.attach(b);
    a.focus();
    detachB();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("还有别的终端在：取消其中一个不触发恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const a = makeTextarea();
    const b = makeTextarea();
    const detachA = controller.attach(a);
    controller.attach(b);
    a.focus();
    makeButton().focus(); // 待恢复
    detachA();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("取消后终端不再响应焦点", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const detach = controller.attach(term);
    detach();
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });

  it("取消时有待发的恢复定时器：被清掉，不会重复发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const term = makeTextarea();
    const detach = controller.attach(term);
    term.focus();
    makeTextarea().focus(); // 排了延迟恢复
    detach(); // 立即恢复，同时清掉定时器
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
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
