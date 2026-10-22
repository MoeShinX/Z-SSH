//! 关闭主窗口时的判断（UI-4）：缩到托盘，还是走退出流程。
//!
//! 纯函数，App.tsx 的 `onCloseRequested` 和托盘「退出」都用它们，所以点 × 和托盘「退出」
//! 走的是同一套确认代码。分工见 src-tauri/src/tray.rs 开头：关闭由前端决定，Rust 不碰主窗口的
//! CloseRequested。

/** 关闭主窗口时：`tray` 缩到托盘（默认），`quit` 退出程序。 */
export type CloseAction = "tray" | "quit";

export const DEFAULT_CLOSE_ACTION: CloseAction = "tray";

/** 库里读回来的值：只有明确是 `quit` 才退出，缺省或乱七八糟的值一律按默认（缩到托盘）。 */
export function parseCloseAction(raw: unknown): CloseAction {
  return raw === "quit" ? "quit" : DEFAULT_CLOSE_ACTION;
}

export interface CloseRequest {
  /** 设置里选的「关闭主窗口时」。 */
  action: CloseAction;
  /**
   * 这次关闭是「要退出程序」触发的：托盘「退出」，或者退出流程里用户已经确认、
   * 正在真的关窗口。这种情况下不管设置怎么选都不能再缩到托盘。
   */
  quitting: boolean;
  /** 第一次缩到托盘的提示已经出过了。 */
  hintShown: boolean;
}

export type CloseDecision =
  /** 缩到托盘；`showHint` = 这是第一次，要提示一下程序还在右下角。 */
  | { type: "hide"; showHint: boolean }
  /** 走退出流程（未保存修改 / 传输中的确认，没有要确认的就直接退出）。 */
  | { type: "quit" };

export function decideClose(request: CloseRequest): CloseDecision {
  if (request.quitting || request.action === "quit") return { type: "quit" };
  return { type: "hide", showHint: !request.hintShown };
}

/** 退出流程里下一步该做什么。 */
export type QuitVerdict =
  /** 没有要确认的了：放行，窗口关掉、程序退出。 */
  | { type: "go" }
  /** 有没保存的编辑器内容：先问保存还是放弃。 */
  | { type: "ask-unsaved" }
  /** 有进行中的传输 / 未保存的远程编辑：问是否放弃它们。 */
  | { type: "ask-busy" };

export interface QuitState {
  /** 编辑器里没保存的文档数。 */
  unsavedDocs: number;
  /** 进行中的 SFTP 传输数。 */
  transfers: number;
  /** SFTP 编辑里有改动没回传的文件数。 */
  dirtyEdits: number;
  /** 用户已经在「退出并取消 / 退出并放弃」上点过确认。 */
  confirmed: boolean;
}

/** 逻辑和 UI-4 之前 `onCloseRequested` 里一模一样：先看未保存文档，再看传输 / 远程编辑。 */
export function quitVerdict(state: QuitState): QuitVerdict {
  if (state.unsavedDocs > 0 && !state.confirmed) return { type: "ask-unsaved" };
  if ((state.transfers > 0 || state.dirtyEdits > 0) && !state.confirmed) {
    return { type: "ask-busy" };
  }
  return { type: "go" };
}

/** 关闭 Z-SFTP 文件窗口时：`close` 断开里面的 SFTP 再关窗口（默认），`merge` 把标签原样交回主窗口。 */
export type FileWindowCloseAction = "close" | "merge";

export const DEFAULT_FILE_WINDOW_CLOSE_ACTION: FileWindowCloseAction = "close";

/** 库里读回来的值：只有明确是 `merge` 才合并，缺省或乱七八糟的值一律按默认（断开并关闭）。 */
export function parseFileWindowCloseAction(raw: unknown): FileWindowCloseAction {
  return raw === "merge" ? "merge" : DEFAULT_FILE_WINDOW_CLOSE_ACTION;
}

/**
 * 关文件窗口时要不要先把标签合并回主窗口：设置选了合并、窗口里确实有标签、
 * 并且不是"合并已经做完、窗口正在真的关"那一次。
 */
export function shouldMergeOnClose(
  action: FileWindowCloseAction,
  tabCount: number,
  closing: boolean,
): boolean {
  return action === "merge" && tabCount > 0 && !closing;
}
