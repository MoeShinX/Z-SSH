//! SFTP 的应用内剪贴板（M3-5 第 3 节）：所有 SFTP 标签 / 面板共用一份，所以能从一台主机复制、
//! 到另一台主机的标签里粘贴。不碰系统剪贴板。

import { create } from "zustand";
import type { ClipboardContent } from "../state/sftpPaste";

interface ClipboardState {
  content: ClipboardContent | null;
  set: (content: ClipboardContent) => void;
  clear: () => void;
}

export const useSftpClipboard = create<ClipboardState>((set) => ({
  content: null,
  set: (content) => set({ content }),
  clear: () => set({ content: null }),
}));
