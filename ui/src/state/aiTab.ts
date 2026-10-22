//! AI 面板挂在哪个标签上：只跟着 shell 标签。当前是 SFTP 标签时沿用最近那个 shell 标签
//! （面板藏起来但不卸载，切回去对话还在）；那个标签已经关了就没有。

export interface TabLike {
  id: string;
  sftp?: unknown;
}

export function aiTabFor<T extends TabLike>(
  tabs: T[],
  active: T | null,
  lastShellTabId: string | null,
): T | null {
  if (active && !active.sftp) return active;
  return tabs.find((tab) => tab.id === lastShellTabId && !tab.sftp) ?? null;
}
