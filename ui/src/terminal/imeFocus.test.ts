/** @vitest-environment jsdom */
//! 输入法"默认英文、只有 data-ime="native" 的输入框跟随用户"的控制器：
//! 英文区 / 跟随区的判定、请求串行、只在进了跟随区的输入框以后（稍等一下）才恢复、
//! 恢复失败会在下一次进跟随区时补发、开关关闭的收尾。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createImeFocusController, imeZoneOf, isEditableElement } from "./imeFocus";

/** 显式传给控制器的延迟，避免测试跟着实现里的常量漂。 */
const RESTORE_MS = 40;
const LEAVE_MS = 80;

interface Pending {
  focused: boolean;
  resolve: (inEnglish: boolean) => void;
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

/** 英文区：终端的 textarea、搜索框、表单输入框…… */
const makeTextarea = (): HTMLTextAreaElement => {
  const element = document.createElement("textarea");
  document.body.appendChild(element);
  return element;
};

const makeInput = (type = "text"): HTMLInputElement => {
  const element = document.createElement("input");
  element.type = type;
  document.body.appendChild(element);
  return element;
};

/** 跟随区：AI 对话框。 */
const makeNative = (): HTMLTextAreaElement => {
  const element = makeTextarea();
  element.setAttribute("data-ime", "native");
  return element;
};

const makeButton = (): HTMLButtonElement => {
  const element = document.createElement("button");
  document.body.appendChild(element);
  return element;
};

/** CodeMirror 这类可编辑区域：jsdom 不实现 isContentEditable，手动补上。 */
const makeEditor = (): HTMLElement => {
  const element = document.createElement("div");
  element.tabIndex = 0;
  Object.defineProperty(element, "isContentEditable", { value: true });
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

describe("英文区：所有可输入的地方（跟随区除外）", () => {
  it.each([
    ["终端的 textarea", () => makeTextarea()],
    ["主机页搜索框 / 路径框（text input）", () => makeInput("text")],
    ["搜索类输入框", () => makeInput("search")],
    ["表单里的密码框", () => makeInput("password")],
    ["数字框", () => makeInput("number")],
    ["编辑器（可编辑区域）", () => makeEditor()],
  ])("焦点进 %s：立即发进入", async (_name, make) => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    make().focus();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true]);
  });

  it("从终端到搜索框：已在英文区，不再发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const search = makeInput();
    term.focus();
    search.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("英文区之间来回切换（终端、搜索框、编辑器、另一个终端）：一直只发一次进入", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const a = makeTextarea();
    const b = makeTextarea();
    const search = makeInput();
    const editor = makeEditor();
    a.focus();
    search.focus();
    editor.focus();
    b.focus();
    a.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("英文区 → 按钮 / 空白处 → 英文区：什么都不多发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const a = makeTextarea();
    const b = makeInput();
    a.focus();
    makeButton().focus();
    await vi.advanceTimersByTimeAsync(300);
    a.blur();
    await vi.advanceTimersByTimeAsync(300);
    b.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("按钮、复选框、只读的输入框不算英文区：焦点进去不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeButton().focus();
    makeInput("checkbox").focus();
    makeInput("radio").focus();
    const readOnly = makeTextarea();
    readOnly.readOnly = true;
    readOnly.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });
});

describe("跟随区：data-ime=native", () => {
  it("没进过英文区，直接进跟随区：不发任何请求", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });

  it("英文区 → 跟随区：延迟 40ms 之后才发离开", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 1);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]);
  });

  it("跟随区 → 英文区：立即发进入", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    makeInput().focus();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false, true]);
  });

  it("标记加在容器上也算：容器里的输入框跟随", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const wrapper = document.createElement("div");
    wrapper.setAttribute("data-ime", "native");
    const inner = document.createElement("input");
    wrapper.appendChild(inner);
    document.body.appendChild(wrapper);
    makeTextarea().focus();
    inner.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false]);
  });

  it("只认标记，不看类名 / 位置：长得像 AI 的输入框没有标记就是英文区", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const lookalike = makeTextarea();
    lookalike.className = "ai-chat-input native";
    lookalike.setAttribute("data-ime", "english");
    makeTextarea().focus();
    lookalike.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("跟随区 → 按钮 / 空白处：不发，保持当前状态", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const ai = makeNative();
    makeTextarea().focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    makeButton().focus();
    ai.blur();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });

  it("恢复成功之后再进别的跟随区输入框：不重复发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeTextarea().focus();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });
});

describe("经过不可输入的元素", () => {
  it("英文区 → 按钮 → 跟随区：只在最后一步发恢复，并且有延迟", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const button = makeButton();
    const ai = makeNative();
    term.focus();
    button.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]); // 等多久都不恢复，只是待恢复
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 1);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]);
  });

  it("英文区 → 空白处（什么都没聚焦）→ 跟随区：同样等到进输入框才恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    term.focus();
    term.blur();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false]);
  });

  it("英文区 → 按钮 → 回英文区：什么都不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const button = makeButton();
    term.focus();
    button.focus();
    await vi.advanceTimersByTimeAsync(300);
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("进跟随区后在延迟内又去了不可输入的元素：取消，等下一次进跟随区", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    const button = makeButton();
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

  it("进跟随区后在延迟内回到英文区：取消，什么都不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 10);
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("延迟内换到另一个跟随区输入框：从最后一次焦点变化重新等待", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    const ai2 = makeNative();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 5);
    ai2.focus();
    await vi.advanceTimersByTimeAsync(10);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 11);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]);
  });
});

describe("请求串行、快速切换不乱序", () => {
  it("上一个请求没返回，下一个不发", async () => {
    const m = manualSend();
    const controller = makeController(m.send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(m.sent).toEqual([true]);
    await m.release();
    await vi.advanceTimersByTimeAsync(0);
    expect(m.sent).toEqual([true, false]);
    await m.release();
    expect(m.pending).toHaveLength(0);
  });

  it("英文区 → AI → 英文区 → AI 快速切换：中途回英文区取消待发的恢复，最后只发一次恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    term.focus();
    ai.focus();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });

  it("进入还没返回时恢复已经到点、又回英文区：请求按顺序排队发出", async () => {
    const m = manualSend();
    const controller = makeController(m.send);
    controller.enable();
    const search = makeInput();
    const ai = makeNative();
    search.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    // 进入还没返回：恢复排在后面，不能先发出去
    expect(m.sent).toEqual([true]);
    search.focus(); // 又回到英文区：要再发进入，排在恢复后面
    await m.release();
    await vi.advanceTimersByTimeAsync(0);
    expect(m.sent).toEqual([true, false]);
    await m.release();
    await vi.advanceTimersByTimeAsync(0);
    expect(m.sent).toEqual([true, false, true]);
    await m.release();
    expect(m.pending).toHaveLength(0);
  });

  it("在 AI 输入框和搜索框之间反复快速切换：待满 40ms 才恢复，最后停在最后一次焦点对应的状态", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const search = makeInput();
    const ai = makeNative();
    for (let i = 0; i < 4; i++) {
      search.focus();
      await vi.advanceTimersByTimeAsync(10);
      ai.focus();
      await vi.advanceTimersByTimeAsync(10);
    }
    search.focus();
    await vi.advanceTimersByTimeAsync(500);
    // 每次在 AI 里都没待满 40ms，恢复一次都没发；最后回英文区也不用重发
    expect(sent).toEqual([true]);
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    search.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, true, false]);
  });
});

describe("窗口失去焦点（切到别的程序）", () => {
  it("英文区有焦点时窗口失焦：照旧发离开；后端说没恢复成功，回来后进跟随区再补", async () => {
    let leaves = 0;
    const { sent, send } = autoSend((focused) => (focused ? true : ++leaves < 2));
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    term.focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(LEAVE_MS - 1);
    expect(sent).toEqual([true]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false]); // 前台不是本进程，后端返回仍在英文区
    ai.focus(); // 窗口回来，焦点落在 AI 输入框
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false, false]);
    // 这次成功了：再进别的跟随区输入框不再发
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
  });

  it("窗口失焦后马上回来：不发离开", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    term.focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(LEAVE_MS - 20);
    window.dispatchEvent(new Event("focus")); // 窗口重新激活，焦点还在终端
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
  });

  it("窗口失焦发出离开后回来，焦点还在英文区：重新进入", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeInput().focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(LEAVE_MS + 10);
    expect(sent).toEqual([true, false]);
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false, true]);
  });

  it("跟随区有焦点时窗口失焦、回来：不会把 AI 输入框切成英文", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeTextarea().focus();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(500);
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });

  it("恢复没成功后只是重新激活原来的 AI 输入框（没有新的 focusin）：按延迟补发，直到成功", async () => {
    const answers = [true, true, false]; // 进入、第一次离开（没成功）、第二次离开（成功）
    const { sent, send } = autoSend(() => answers.shift() ?? false);
    const controller = makeController(send);
    controller.enable();
    makeTextarea().focus();
    const ai = makeNative();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]); // 后端说仍欠着
    // 窗口切走、再切回来：焦点一直在 AI 输入框上，不会有 focusin
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 1);
    expect(sent).toEqual([true, false]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([true, false, false]);
    // 这次成功了：再激活一次窗口不重复发
    window.dispatchEvent(new Event("blur"));
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
  });

  it("窗口重新激活补发也遵守延迟内换焦点：去了按钮取消、回英文区取消", async () => {
    const { sent, send } = autoSend(() => true);
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
    const button = makeButton();
    term.focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]); // 一直报仍欠着
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 10);
    button.focus(); // 延迟内去了不可输入的元素：取消
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    ai.focus(); // 重新进跟随区：按正常规则重新计时
    await vi.advanceTimersByTimeAsync(RESTORE_MS);
    expect(sent).toEqual([true, false, false]);
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(10);
    term.focus(); // 回英文区：取消待发的恢复，立即进入
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false, true]);
  });

  it("没有欠恢复时窗口重新激活：焦点在 AI 输入框上什么都不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(100);
    window.dispatchEvent(new Event("blur"));
    window.dispatchEvent(new Event("focus"));
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });

  it("后端广播欠恢复之后窗口重新激活：同样补发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const ai = makeNative();
    ai.focus();
    await vi.advanceTimersByTimeAsync(100);
    controller.markRestoreOwed();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([false]);
  });

  it("待恢复期间（焦点在按钮上）窗口失焦：不丢待恢复，回来进跟随区照样恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const ai = makeNative();
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

describe("后端没成功", () => {
  it("恢复没生效（后端返回仍在英文区）：下一次进跟随区重发，直到成功；进按钮不重发", async () => {
    let leaves = 0;
    const { sent, send } = autoSend((focused) => (focused ? true : ++leaves < 3));
    const controller = makeController(send);
    controller.enable();
    makeTextarea().focus();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    makeButton().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false, false]);
    makeNative().focus(); // 第三次成功了，不再发
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false, false]);
  });

  it("进入没被后端接受（窗口还没拿到前台）：下次焦点进英文区重发", async () => {
    let accept = false;
    const { sent, send } = autoSend((focused) => (focused ? accept : false));
    const controller = makeController(send);
    controller.enable();
    const term = makeTextarea();
    const search = makeInput();
    term.focus();
    await vi.advanceTimersByTimeAsync(0);
    accept = true;
    search.focus();
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
    controller.enable();
    const a = makeTextarea();
    const b = makeInput();
    a.focus();
    await vi.advanceTimersByTimeAsync(0);
    b.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(calls).toEqual([true, true]);
  });
});

describe("设置开关", () => {
  it("没打开（从没 enable）：任何焦点变化都不发", async () => {
    const { sent, send } = autoSend();
    makeController(send);
    makeTextarea().focus();
    makeNative().focus();
    makeInput().focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });

  it("打开时焦点已经在英文区：立即进入", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    makeInput().focus();
    controller.enable();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true]);
  });

  it("打开时焦点在跟随区：不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    makeNative().focus();
    controller.enable();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });

  it("焦点在英文区时关掉：立即恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const disable = controller.enable();
    makeTextarea().focus();
    disable();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
  });

  it("待恢复期间（焦点在按钮上）关掉：立即恢复，不等跟随区；之后什么都不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const disable = controller.enable();
    makeInput().focus();
    makeButton().focus();
    disable();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
    // 恢复成功后监听已摘掉：之后进哪里都不发
    makeTextarea().focus();
    makeNative().focus();
    window.dispatchEvent(new Event("blur"));
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });

  it("关掉时恢复没生效：监听保留，下次进输入框补发；这期间英文区不再进入", async () => {
    let leaves = 0;
    const { sent, send } = autoSend((focused) => (focused ? true : ++leaves < 2));
    const controller = makeController(send);
    const disable = controller.enable();
    makeTextarea().focus();
    makeButton().focus();
    disable();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
    // 开关关着：英文区的输入框也只负责补恢复，不会再切英文
    makeInput().focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    expect(sent).toEqual([true, false, false]);
    makeInput().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false, false]);
  });

  it("关掉时有待发的恢复定时器：被清掉，不会重复发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const disable = controller.enable();
    makeTextarea().focus();
    makeNative().focus(); // 排了延迟恢复
    disable(); // 立即恢复，同时清掉定时器
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true, false]);
  });

  it("关掉后焦点变化不再响应；再次打开重新生效", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const disable = controller.enable();
    disable();
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
    controller.enable();
    makeInput().focus();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true]);
  });

  it("重复调用关闭函数无害，多次 enable 要全部关掉才恢复", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    const disableA = controller.enable();
    const disableB = controller.enable();
    makeTextarea().focus();
    disableA();
    disableA();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true]);
    disableB();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
  });
});

describe("imeZoneOf / isEditableElement", () => {
  it("可输入元素分英文区和跟随区，不可输入的是 null", () => {
    const input = makeInput();
    const native = makeNative();
    expect(imeZoneOf(input)).toBe("english");
    expect(imeZoneOf(native)).toBe("native");
    expect(imeZoneOf(makeButton())).toBeNull();
    expect(imeZoneOf(null)).toBeNull();
    // 标记在不可输入的元素自己身上：它不是输入框，仍是 null
    const button = makeButton();
    button.setAttribute("data-ime", "native");
    expect(imeZoneOf(button)).toBeNull();
  });

  it("文本框、textarea、contenteditable 算；按钮、复选框、只读不算", () => {
    const make = (tag: string, props: Record<string, unknown> = {}) =>
      Object.assign(document.createElement(tag), props);
    expect(isEditableElement(make("input", { type: "text" }))).toBe(true);
    expect(isEditableElement(make("input", { type: "password" }))).toBe(true);
    expect(isEditableElement(make("textarea"))).toBe(true);
    expect(isEditableElement(makeEditor())).toBe(true);
    expect(isEditableElement(make("textarea", { readOnly: true }))).toBe(false);
    expect(isEditableElement(make("input", { type: "checkbox" }))).toBe(false);
    expect(isEditableElement(make("input", { type: "text", disabled: true }))).toBe(false);
    expect(isEditableElement(make("button"))).toBe(false);
    expect(isEditableElement(null)).toBe(false);
  });
});

describe("别的窗口被关掉、恢复没成功：markRestoreOwed", () => {
  it("没进过英文区，收到通知后进跟随区：延迟 40ms 后补发一次离开", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeButton().focus();
    controller.markRestoreOwed();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 1);
    expect(sent).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([false]);
  });

  it("没有欠恢复：进跟随区什么都不发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });

  it("补发成功后清掉：再进跟随区不重复发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    controller.markRestoreOwed();
    const ai = makeNative();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([false]);
    makeButton().focus();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([false]);
  });

  it("补发没成功（后端说仍欠着）：下一次进跟随区再补，直到成功", async () => {
    const answers = [true, false];
    const { sent, send } = autoSend(() => answers.shift() ?? false);
    const controller = makeController(send);
    controller.enable();
    controller.markRestoreOwed();
    const ai = makeNative();
    const button = makeButton();
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([false]);
    button.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([false]);
    ai.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([false, false]);
  });

  it("焦点已经在跟随区时收到通知：马上开始等 40ms 再补发", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(100);
    controller.markRestoreOwed();
    await vi.advanceTimersByTimeAsync(RESTORE_MS - 1);
    expect(sent).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(sent).toEqual([false]);
  });

  it("补发也遵守快速切换重新计时：延迟内换输入框从最后一次焦点重新等", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    controller.markRestoreOwed();
    const first = makeNative();
    const second = makeNative();
    first.focus();
    await vi.advanceTimersByTimeAsync(30);
    second.focus();
    await vi.advanceTimersByTimeAsync(30);
    expect(sent).toEqual([]);
    await vi.advanceTimersByTimeAsync(10);
    expect(sent).toEqual([false]);
  });

  it("补发等待期间回了英文区：取消；欠着的恢复由进入后的离开一并处理", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.enable();
    controller.markRestoreOwed();
    const ai = makeNative();
    const term = makeTextarea();
    ai.focus();
    await vi.advanceTimersByTimeAsync(20);
    term.focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([true]);
    ai.focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS);
    expect(sent).toEqual([true, false]);
  });

  it("走同一条队列：上一个请求没返回，补发不会并发地发出去", async () => {
    const { sent, pending, send, release } = manualSend();
    const controller = makeController(send);
    controller.enable();
    makeTextarea().focus();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true]);
    controller.markRestoreOwed();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(RESTORE_MS + 10);
    // 进入还没返回：离开排在它后面
    expect(sent).toEqual([true]);
    expect(pending).toHaveLength(1);
    await release();
    await vi.advanceTimersByTimeAsync(0);
    expect(sent).toEqual([true, false]);
  });

  it("没在管理输入法（从没 enable）：忽略通知", async () => {
    const { sent, send } = autoSend();
    const controller = makeController(send);
    controller.markRestoreOwed();
    makeNative().focus();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent).toEqual([]);
  });
});
