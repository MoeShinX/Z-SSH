// 终端焦点 ↔ 输入法中英文：把"进终端切英文、离开恢复"的请求排成一条队列发给后端。
//
// 为什么要自己管：每次焦点变化各发一次 IPC，后端各自在线程池里跑，快速切换时
// 先后顺序不保证（"恢复"先到、"切英文"后到，最后停在英文）。这里保证：
// - 请求一个接一个发，上一个有结果才发下一个；
// - 离开终端先等一小会儿（分屏之间切换时，下一个终端马上拿到焦点，不用恢复又切回去）；
// - 焦点进了终端以外的输入元素（AI 输入框、表单、编辑器……）立刻补一次恢复，
//   不依赖终端自己的 blur 事件有没有发出来；
// - 后端说"还没恢复成功"（比如离开时窗口已经不在前台）就在下一次需要时再发一次离开。

import { imeTerminalFocus } from "../api/ime";

/** 离开终端后等多久才真正恢复（毫秒）。要比一次 React 渲染长，比人的反应短。 */
const LEAVE_DELAY_MS = 80;

/** 能打字的元素：焦点进去以后会弹输入法。 */
export function isEditableElement(element: Element | null): boolean {
  if (!(element instanceof HTMLElement)) return false;
  if (element.isContentEditable) return true;
  if (element instanceof HTMLTextAreaElement) return !element.readOnly && !element.disabled;
  if (element instanceof HTMLInputElement) {
    if (element.readOnly || element.disabled) return false;
    return !NON_TEXT_INPUT_TYPES.has(element.type);
  }
  return false;
}

const NON_TEXT_INPUT_TYPES = new Set([
  "button",
  "checkbox",
  "color",
  "file",
  "hidden",
  "image",
  "radio",
  "range",
  "reset",
  "submit",
]);

export interface ImeFocusController {
  /** 登记一个终端的输入框（xterm 的 textarea）；返回的函数用来取消登记。 */
  attach(textarea: HTMLElement): () => void;
}

/**
 * `send` 是真正发给后端的函数，返回后端处理完以后是否仍认为处于终端焦点；
 * 抛错（比如非 Windows、IPC 失败）当作没成功处理。
 */
export function createImeFocusController(
  send: (focused: boolean) => Promise<boolean>,
  leaveDelayMs = LEAVE_DELAY_MS,
): ImeFocusController {
  const terminals = new Set<HTMLElement>();
  let chain: Promise<void> = Promise.resolve();
  /** 最近一次发出（或排队）的请求；null = 还没发过，或进入没被后端接受。 */
  let lastSent: boolean | null = null;
  /** 后端上一次的回答：是否仍处于终端焦点（离开没恢复成功时为 true）。 */
  let backendInTerminal = false;
  let seq = 0;
  let leaveTimer: ReturnType<typeof setTimeout> | undefined;
  let documentListening = false;

  const request = (focused: boolean) => {
    const mine = ++seq;
    lastSent = focused;
    chain = chain
      .then(() => send(focused))
      .then(
        (inTerminal) => {
          backendInTerminal = inTerminal === true;
          // 进入没被接受（比如窗口还没拿到前台）：下次进入要重发
          if (focused && !backendInTerminal && mine === seq) lastSent = null;
        },
        () => {
          if (mine === seq) lastSent = null;
        },
      );
  };

  const cancelLeaveTimer = () => {
    if (leaveTimer !== undefined) {
      clearTimeout(leaveTimer);
      leaveTimer = undefined;
    }
  };

  const enter = () => {
    cancelLeaveTimer();
    // 已经是"进入"状态（分屏之间切换）：什么都不发
    if (lastSent === true) return;
    request(true);
  };

  const leaveNow = () => {
    cancelLeaveTimer();
    // 没进过终端、后端也没有等着恢复的东西：不发
    if (lastSent !== true && !backendInTerminal) return;
    request(false);
  };

  const scheduleLeave = () => {
    if (leaveTimer !== undefined) return;
    leaveTimer = setTimeout(() => {
      leaveTimer = undefined;
      leaveNow();
    }, leaveDelayMs);
  };

  const terminalHasFocus = () => {
    const active = document.activeElement;
    return active instanceof HTMLElement && terminals.has(active);
  };

  const onDocumentFocusIn = (event: FocusEvent) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    if (target instanceof HTMLElement && terminals.has(target)) return;
    // 焦点进了别的输入元素：不等终端的 blur，立刻恢复用户自己的输入法状态
    if (isEditableElement(target)) leaveNow();
  };

  const startDocumentListener = () => {
    if (documentListening) return;
    documentListening = true;
    document.addEventListener("focusin", onDocumentFocusIn, true);
  };
  const stopDocumentListener = () => {
    if (!documentListening) return;
    documentListening = false;
    document.removeEventListener("focusin", onDocumentFocusIn, true);
  };

  return {
    attach(textarea) {
      const onFocus = () => enter();
      const onBlur = (event: FocusEvent) => {
        const next = event.relatedTarget;
        // 焦点直接交给另一个终端：它自己的 focus 会取消等待，这里不用排恢复
        if (next instanceof HTMLElement && terminals.has(next)) return;
        scheduleLeave();
      };
      terminals.add(textarea);
      startDocumentListener();
      textarea.addEventListener("focus", onFocus);
      textarea.addEventListener("blur", onBlur);
      if (document.activeElement === textarea) enter();
      return () => {
        textarea.removeEventListener("focus", onFocus);
        textarea.removeEventListener("blur", onBlur);
        terminals.delete(textarea);
        // 关掉开关 / 终端被关掉时它正拿着焦点：立刻恢复，不等 blur
        if (lastSent === true && !terminalHasFocus()) leaveNow();
        if (terminals.size === 0) {
          cancelLeaveTimer();
          stopDocumentListener();
        }
      };
    },
  };
}

/** 全局唯一的控制器：所有终端共用一条队列。 */
export const imeFocus = createImeFocusController((focused) => imeTerminalFocus(focused));
