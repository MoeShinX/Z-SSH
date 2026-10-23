// 终端里默认英文：焦点进终端时切到英文，离开终端、进了别的可输入的地方时恢复用户自己的
// 中英文状态。请求排成一条队列发给后端。
//
// 两个区：
// - 英文区：只有终端（xterm 的输入框）；
// - 跟随区：其余所有可输入元素（AI 对话框、搜索框、路径框、表单、编辑器……），保持用户自己的状态。
// 不可输入的元素（按钮、空白处、标签……）不属于任何区：焦点落上去什么都不发，保持当前状态。
//
// 为什么要自己管：每次焦点变化各发一次 IPC，后端各自在线程池里跑，快速切换时
// 先后顺序不保证（"恢复"先到、"切英文"后到，最后停在英文）。这里保证：
// - 请求一个接一个发，上一个有结果才发下一个；
// - 焦点进英文区：立即发"进入"；已经在英文区（lastSent 为 true）就什么都不发，
//   所以两个终端之间来回切换不会重复发；
// - 焦点进跟随区：稍等一下再发"离开"（恢复用户原来的模式）。离开英文区时焦点若落在
//   不可输入的地方，Chromium 会把窗口的输入法上下文解除关联，这时写转换模式没有作用对象；
//   所以只有焦点真正落在跟随区的输入框、输入法重新关联好以后才恢复；
// - 等待期间焦点回了英文区、或去了不可输入的元素：取消，不发；
// - 窗口失去焦点（切到别的程序）时若焦点在英文区，仍照旧发一次离开，窗口回来再进入；
//   后端说"还没恢复成功"（窗口不在前台、写进去没生效）就一直算待恢复，下一次焦点进
//   跟随区时再发（窗口重新激活、焦点本来就停在跟随区也算，因为那时没有新的 focusin）。
//   窗口失焦不会把跟随区的输入框切成英文；
// - 另一个窗口被关掉、恢复没成功时，后端广播 ime_restore_pending：这个窗口此前没进过
//   英文区，自己不知道有一次恢复欠着，收到后同样算待恢复（markRestoreOwed），走同一条
//   队列和同一个延迟补发。没有这条通知，进 AI 输入框时不会发任何请求，恢复永远补不上。

import { imeTerminalFocus } from "../api/ime";

/**
 * 窗口失去焦点后等多久发离开（毫秒）。要比一次 React 渲染长，比人的反应短，
 * 窗口马上又回来（切换瞬间的抖动）就不用发。
 */
const LEAVE_DELAY_MS = 80;

/**
 * 焦点进入跟随区后等多久再恢复（毫秒）。Chromium 在焦点落到可输入元素之后，
 * 要经过一次渲染进程到浏览器进程的往返才把输入法上下文重新关联到窗口；立刻写
 * 转换模式会落在还没关联好的上下文上。40ms 超过一次往返（通常几毫秒），
 * 远低于人的察觉（约 100ms），也比"点输入框后马上敲字"的间隔短。
 * 写入没生效时后端会报失败，下一次进跟随区再补，所以这里不必留太大余量。
 */
const RESTORE_DELAY_MS = 40;

/** 跟随区的标记：加在输入框（或包着输入框的容器）上。 */
export const NATIVE_IME_ATTRIBUTE = "data-ime";
export const NATIVE_IME_VALUE = "native";
const NATIVE_SELECTOR = `[${NATIVE_IME_ATTRIBUTE}="${NATIVE_IME_VALUE}"]`;

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

export type ImeZone = "english" | "native";

/** TerminalView 打开 xterm 后，给它实际接收键盘输入的 textarea 打上这个标记；只有它算英文区。 */
export const TERMINAL_IME_ATTRIBUTE = "data-ime";
export const TERMINAL_IME_VALUE = "terminal";

/**
 * 元素属于哪个区；不可输入的元素（按钮、空白处）返回 null。
 * 英文区**只有终端**；其余可输入的地方（AI 对话框、搜索框、表单、编辑器……）都跟随用户自己的
 * 输入法状态（用户 2026-10-23 再次明确）。`data-ime="native"` 标记保留：显式标了的一律算跟随区。
 */
export function imeZoneOf(element: Element | null): ImeZone | null {
  if (!element || !isEditableElement(element)) return null;
  if (element.closest(NATIVE_SELECTOR)) return "native";
  return element instanceof HTMLTextAreaElement &&
    element.getAttribute(TERMINAL_IME_ATTRIBUTE) === TERMINAL_IME_VALUE
    ? "english"
    : "native";
}

export interface ImeFocusController {
  /**
   * 开始按文档焦点管理输入法（设置打开时调用）；返回的函数用来停止（设置关闭 / 卸载时）。
   * 停止时若还处在英文区的状态，立刻恢复用户原来的模式。
   */
  enable(): () => void;
  /**
   * 后端通知：有窗口被关掉、恢复没成功，输入法还停在英文。把"后端还欠恢复"标记置上，
   * 下一次焦点进跟随区时按现有延迟发一次离开（焦点已经在跟随区就马上开始等）；成功后清掉。
   * 没在管理输入法（开关没开过）时忽略：这个窗口没动过输入法。
   */
  markRestoreOwed(): void;
}

/**
 * `send` 是真正发给后端的函数，返回后端处理完以后是否仍认为处于英文区；
 * 抛错（比如非 Windows、IPC 失败）当作没成功处理。
 */
export function createImeFocusController(
  send: (focused: boolean) => Promise<boolean>,
  restoreDelayMs = RESTORE_DELAY_MS,
  leaveDelayMs = LEAVE_DELAY_MS,
): ImeFocusController {
  let enabledCount = 0;
  let chain: Promise<void> = Promise.resolve();
  /** 最近一次发出（或排队）的请求；null = 还没发过，或进入没被后端接受。 */
  let lastSent: boolean | null = null;
  /** 后端上一次的回答：是否仍处于英文区（离开没恢复成功时为 true）。 */
  let backendInEnglish = false;
  let seq = 0;
  /** 已发出、还没有回答的请求数：回答之前不知道是否还欠着一次恢复。 */
  let inFlight = 0;
  /** 收到过几次「后端欠恢复」通知：请求发出之后才到的通知，不能被这次请求的（较旧的）回答抹掉。 */
  let owedNotices = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let listening = false;

  const isEnabled = () => enabledCount > 0;

  /** 输入法还停在英文区设定的状态，需要恢复（"待恢复"）。 */
  const needsRestore = () => lastSent === true || backendInEnglish;

  const cancelTimer = () => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  };

  /** 这个区的焦点是否该处于英文：开关关着时没有英文区，所有可输入元素都只做补恢复。 */
  const isEnglishZone = (zone: ImeZone | null) => zone === "english" && isEnabled();

  const onDocumentFocusIn = (event: FocusEvent) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const zone = imeZoneOf(target);
    if (zone === null) {
      // 去了不可输入的地方：输入法上下文还没关联，之前排的恢复作废，等进输入框再说
      cancelTimer();
      return;
    }
    if (isEnglishZone(zone)) {
      enter();
      return;
    }
    if (needsRestore()) scheduleRestore();
  };

  // 只挂在冒泡阶段：元素的 blur 不冒泡，到这里的只有窗口自己的 blur
  const onWindowBlur = () => {
    // 英文区正拿着焦点时切走窗口：照旧发一次离开（前台不是本进程时后端会说"仍在英文区"）
    if (isEnglishZone(imeZoneOf(document.activeElement))) scheduleLeave();
  };

  // 窗口回来：焦点还在英文区的话重新进入（不依赖浏览器是否补发 focusin）；
  // 焦点还在跟随区、而后台还欠着一次恢复（之前那次没成功）的话，按现有延迟补一次：
  // 窗口只是重新激活，焦点本来就在那个输入框里，不会再有新的 focusin
  const onWindowFocus = () => {
    const zone = imeZoneOf(document.activeElement);
    if (isEnglishZone(zone)) {
      enter();
      return;
    }
    if (zone !== null && needsRestore()) scheduleRestore();
  };

  const startListening = () => {
    if (listening) return;
    listening = true;
    document.addEventListener("focusin", onDocumentFocusIn, true);
    window.addEventListener("blur", onWindowBlur);
    window.addEventListener("focus", onWindowFocus);
  };
  const stopListening = () => {
    if (!listening) return;
    listening = false;
    document.removeEventListener("focusin", onDocumentFocusIn, true);
    window.removeEventListener("blur", onWindowBlur);
    window.removeEventListener("focus", onWindowFocus);
  };
  /** 关着开关、也没有待恢复的东西时才摘掉监听；还欠着一次恢复就留着，等下一个跟随区输入框。 */
  const stopListeningIfIdle = () => {
    if (!isEnabled() && inFlight === 0 && !needsRestore()) {
      cancelTimer();
      stopListening();
    }
  };

  const request = (focused: boolean) => {
    const mine = ++seq;
    const noticesAtSend = owedNotices;
    lastSent = focused;
    inFlight++;
    chain = chain
      .then(() => send(focused))
      .then(
        (inEnglish) => {
          inFlight--;
          // 这次请求发出之后又收到了欠恢复的通知：后端的回答比通知旧，仍按欠着算
          backendInEnglish = inEnglish === true || owedNotices > noticesAtSend;
          // 进入没被后端接受（比如窗口还没拿到前台）：下次进入要重发
          if (focused && !backendInEnglish && mine === seq) lastSent = null;
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
    // 已经是"进入"状态（英文区之间切换、待恢复期间回到英文区）：什么都不发
    if (lastSent === true) return;
    request(true);
  };

  const leaveNow = () => {
    cancelTimer();
    // 没进过英文区、后端也没有等着恢复的东西：不发
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
      // 等的这段时间里焦点回了英文区、或又离开了输入框：不恢复，由之后的焦点变化接手
      const zone = imeZoneOf(document.activeElement);
      if (zone === null || isEnglishZone(zone)) return;
      leaveNow();
    }, restoreDelayMs);
  };

  return {
    markRestoreOwed() {
      if (!listening) return;
      owedNotices++;
      backendInEnglish = true;
      // 用户此刻就停在 AI 输入框里：不会再有焦点变化，现在就开始等
      const zone = imeZoneOf(document.activeElement);
      if (zone !== null && !isEnglishZone(zone)) scheduleRestore();
    },
    enable() {
      enabledCount++;
      startListening();
      // 开关刚打开时焦点已经在英文区（比如设置页里点开关后焦点回到输入框）：直接进入
      if (isEnglishZone(imeZoneOf(document.activeElement))) enter();
      let active = true;
      return () => {
        if (!active) return;
        active = false;
        enabledCount--;
        if (enabledCount > 0) return;
        // 关掉开关（或界面卸载）：不能再等跟随区，立刻恢复。
        // 此时焦点常在不可输入的地方，写入可能没生效，后端会报失败；
        // 监听留着，等下一次焦点进输入框再补（见 stopListeningIfIdle）。
        leaveNow();
        stopListeningIfIdle();
      };
    },
  };
}

/** 全局唯一的控制器：整个窗口共用一条队列。 */
export const imeFocus = createImeFocusController((focused) => imeTerminalFocus(focused));
