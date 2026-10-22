// 终端焦点 ↔ 输入法中英文：把"进终端切英文、离开恢复"的请求排成一条队列发给后端。
//
// 为什么要自己管：每次焦点变化各发一次 IPC，后端各自在线程池里跑，快速切换时
// 先后顺序不保证（"恢复"先到、"切英文"后到，最后停在英文）。这里保证：
// - 请求一个接一个发，上一个有结果才发下一个；
// - 只在焦点进了终端以外的**可输入元素**（AI 输入框、表单、编辑器……）以后才恢复，
//   而且稍等一下再发。离开终端时焦点若落在不可输入的地方（按钮、空白处、标签……），
//   Chromium 会把窗口的输入法上下文解除关联，这时写转换模式没有作用对象；
//   所以终端 blur 只是"待恢复"，什么都不发，等真正进了输入框、输入法重新关联好再恢复；
// - 待恢复期间回到终端：取消，什么都不发（后端本来就还处于终端焦点）；
// - 窗口失去焦点（切到别的程序）仍照旧发一次离开；后端说"还没恢复成功"
//   （窗口不在前台、写进去没生效）就一直算待恢复，下一次焦点进输入框时再发。

import { imeTerminalFocus } from "../api/ime";

/**
 * 窗口失去焦点后等多久发离开（毫秒）。要比一次 React 渲染长，比人的反应短，
 * 窗口马上又回来（切换瞬间的抖动）就不用发。
 */
const LEAVE_DELAY_MS = 80;

/**
 * 焦点进入输入元素后等多久再恢复（毫秒）。Chromium 在焦点落到可输入元素之后，
 * 要经过一次渲染进程到浏览器进程的往返才把输入法上下文重新关联到窗口；立刻写
 * 转换模式会落在还没关联好的上下文上。40ms 超过一次往返（通常几毫秒），
 * 远低于人的察觉（约 100ms），也比"点输入框后马上敲字"的间隔短。
 * 写入没生效时后端会报失败，下一次进输入框再补，所以这里不必留太大余量。
 */
const RESTORE_DELAY_MS = 40;

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

export const NON_TEXT_INPUT_TYPES = new Set([
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
  restoreDelayMs = RESTORE_DELAY_MS,
  leaveDelayMs = LEAVE_DELAY_MS,
): ImeFocusController {
  const terminals = new Set<HTMLElement>();
  let chain: Promise<void> = Promise.resolve();
  /** 最近一次发出（或排队）的请求；null = 还没发过，或进入没被后端接受。 */
  let lastSent: boolean | null = null;
  /** 后端上一次的回答：是否仍处于终端焦点（离开没恢复成功时为 true）。 */
  let backendInTerminal = false;
  let seq = 0;
  /** 已发出、还没有回答的请求数：回答之前不知道是否还欠着一次恢复。 */
  let inFlight = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let listening = false;

  /** 输入法还停在终端设定的状态，需要恢复（"待恢复"）。 */
  const needsRestore = () => lastSent === true || backendInTerminal;

  const cancelTimer = () => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  };

  const terminalHasFocus = () => {
    const active = document.activeElement;
    return active instanceof HTMLElement && terminals.has(active);
  };

  const onDocumentFocusIn = (event: FocusEvent) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    if (target instanceof HTMLElement && terminals.has(target)) return;
    if (!isEditableElement(target)) {
      // 去了不可输入的地方：输入法上下文还没关联，之前排的恢复作废，等进输入框再说
      cancelTimer();
      return;
    }
    if (needsRestore()) scheduleRestore();
  };

  // 只挂在冒泡阶段：元素的 blur 不冒泡，到这里的只有窗口自己的 blur
  const onWindowBlur = () => {
    // 终端正拿着焦点时切走窗口：照旧发一次离开（前台不是本进程时后端会说"仍在终端"）
    if (terminalHasFocus()) scheduleLeave();
  };

  const startListening = () => {
    if (listening) return;
    listening = true;
    document.addEventListener("focusin", onDocumentFocusIn, true);
    window.addEventListener("blur", onWindowBlur);
  };
  const stopListening = () => {
    if (!listening) return;
    listening = false;
    document.removeEventListener("focusin", onDocumentFocusIn, true);
    window.removeEventListener("blur", onWindowBlur);
  };
  /** 没有终端、也没有待恢复的东西时才摘掉监听；还欠着一次恢复就留着，等下一个输入框。 */
  const stopListeningIfIdle = () => {
    if (terminals.size === 0 && inFlight === 0 && !needsRestore()) {
      cancelTimer();
      stopListening();
    }
  };

  const request = (focused: boolean) => {
    const mine = ++seq;
    lastSent = focused;
    inFlight++;
    chain = chain
      .then(() => send(focused))
      .then(
        (inTerminal) => {
          inFlight--;
          backendInTerminal = inTerminal === true;
          // 进入没被后端接受（比如窗口还没拿到前台）：下次进入要重发
          if (focused && !backendInTerminal && mine === seq) lastSent = null;
          stopListeningIfIdle();
        },
        () => {
          inFlight--;
          if (mine === seq) lastSent = null;
          stopListeningIfIdle();
        },
      );
  };

  const enter = () => {
    cancelTimer();
    // 已经是"进入"状态（分屏之间切换、待恢复期间回到终端）：什么都不发
    if (lastSent === true) return;
    request(true);
  };

  const leaveNow = () => {
    cancelTimer();
    // 没进过终端、后端也没有等着恢复的东西：不发
    if (!needsRestore()) return;
    request(false);
  };

  const scheduleLeave = () => {
    if (timer !== undefined) return;
    timer = setTimeout(() => {
      timer = undefined;
      leaveNow();
    }, leaveDelayMs);
  };

  const scheduleRestore = () => {
    // 连续点进不同输入框时，每个新焦点都需要等自己的上下文关联好；不能沿用上一个的倒计时。
    cancelTimer();
    timer = setTimeout(() => {
      timer = undefined;
      // 等的这段时间里焦点回了终端、或又离开了输入框：不恢复，由之后的焦点变化接手
      if (terminalHasFocus() || !isEditableElement(document.activeElement)) return;
      leaveNow();
    }, restoreDelayMs);
  };

  return {
    attach(textarea) {
      const onFocus = () => enter();
      terminals.add(textarea);
      startListening();
      textarea.addEventListener("focus", onFocus);
      if (document.activeElement === textarea) enter();
      return () => {
        textarea.removeEventListener("focus", onFocus);
        terminals.delete(textarea);
        // 最后一个终端没了（关掉设置开关 / 终端卸载）：不能再等输入框，立刻恢复。
        // 此时焦点常在不可输入的地方，写入可能没生效，后端会报失败；
        // 监听留着，等下一次焦点进输入框再补（见 stopListeningIfIdle）。
        if (terminals.size === 0) leaveNow();
        stopListeningIfIdle();
      };
    },
  };
}

/** 全局唯一的控制器：所有终端共用一条队列。 */
export const imeFocus = createImeFocusController((focused) => imeTerminalFocus(focused));
