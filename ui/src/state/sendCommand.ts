//! 把一条命令发进终端标签的公共流程（快捷命令栏和撰写栏共用）：
//! 内容按「回车执行」做成终端输入，多行且开了「多行粘贴要确认」时先弹确认，
//! 一个连上的窗格都没有时给提示。纯函数，依赖都从参数传进来，单测直接调。

import { isMultiline, payloadOf, type QuickCommand } from "./quickCommands";

/** 多行命令发送前的确认框内容（App 里的 `ConfirmState` 同形）。 */
export interface SendConfirmRequest {
  title: string;
  message: string;
  confirmLabel: string;
  destructive: boolean;
  onConfirm: () => void;
}

export interface SendCommandDeps {
  /** 真正往终端写：`all` = 这个标签的所有分屏，否则只发焦点窗格；返回发出去几个。 */
  send: (data: string, all: boolean) => number;
  /** 设置里「多行粘贴要确认」开着没有。 */
  confirmMultiline: boolean;
  /** 弹确认框；用户点了「发送」才会调 `onConfirm`。 */
  askConfirm: (request: SendConfirmRequest) => void;
  /** 一个窗格都没发出去时的提示。 */
  notify: (message: string) => void;
}

/**
 * 发一条命令。`all` 由调用方算好（点了「发到所有分屏」或这个标签开着输入广播）。
 * `afterSend(sent)` 在真正发过（`sent` 可能是 0）之后调用；用户在确认框里取消时不会调用。
 */
export function sendCommandToTab(
  deps: SendCommandDeps,
  command: QuickCommand,
  all: boolean,
  afterSend?: (sent: number) => void,
): void {
  const run = () => {
    const sent = deps.send(payloadOf(command), all);
    if (sent === 0) {
      deps.notify(all ? "这个标签里还没有连上的窗格" : "当前窗格还没连上");
    }
    afterSend?.(sent);
  };
  if (isMultiline(command) && deps.confirmMultiline) {
    deps.askConfirm({
      title: `发送多行命令「${command.name}」？`,
      message: `${command.command.trim().split(/\r?\n/).length} 行，每一行都会回车执行${
        all ? "，发到这个标签的所有分屏" : ""
      }。`,
      confirmLabel: "发送",
      destructive: false,
      onConfirm: run,
    });
    return;
  }
  run();
}
