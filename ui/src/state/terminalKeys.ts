//! 终端里的复制 / 粘贴快捷键（和 Windows Terminal 一样）：
//! - Ctrl+C：**选中了文字**才复制；没选中照常把 ^C 发给远端（中断命令）
//! - Ctrl+V：把剪贴板内容粘贴进 shell
//! - Ctrl+Shift+C / Ctrl+Shift+V：Linux 终端的习惯，同上（Ctrl+Shift+C 没选中时什么都不做）

export interface KeyLike {
  type: string;
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

export type TerminalClipboardAction = "copy" | "paste" | null;

export function terminalClipboardAction(
  event: KeyLike,
  hasSelection: boolean,
): TerminalClipboardAction {
  if (event.type !== "keydown" || !event.ctrlKey || event.altKey || event.metaKey) return null;
  const key = event.key.toLowerCase();
  if (key === "c" && hasSelection) return "copy";
  if (key === "v") return "paste";
  return null;
}

/** Ctrl+Shift+C 没选中：不复制，也不能把 ^C 发出去（用户按的是"复制"不是"中断"）。 */
export function swallowsWithoutSelection(event: KeyLike): boolean {
  return (
    event.type === "keydown" &&
    event.ctrlKey &&
    event.shiftKey &&
    !event.altKey &&
    !event.metaKey &&
    event.key.toLowerCase() === "c"
  );
}

/** 多行粘贴提示里最多预览几行、每行最多几个字。 */
const PREVIEW_LINES = 5;
const PREVIEW_WIDTH = 120;

/**
 * 粘贴的内容带换行时要先确认（和 Windows Terminal 一样）：对方 shell 没开括号粘贴模式
 * （老系统、sh）时，换行会让命令直接执行，末尾一个换行也一样。返回行数和预览；不用确认返回 null。
 */
export function multilinePaste(text: string): { lines: number; preview: string } | null {
  if (!/[\r\n]/.test(text)) return null;
  const lines = text.split(/\r\n|\r|\n/);
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  const shown = lines
    .slice(0, PREVIEW_LINES)
    .map((line) => (line.length > PREVIEW_WIDTH ? `${line.slice(0, PREVIEW_WIDTH)}…` : line));
  if (lines.length > PREVIEW_LINES) shown.push(`…（还有 ${lines.length - PREVIEW_LINES} 行）`);
  return { lines: lines.length, preview: shown.join("\n") };
}
