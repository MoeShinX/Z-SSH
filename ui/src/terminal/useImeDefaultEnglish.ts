// 设置「默认使用英文输入」打开时，让输入法控制器按文档焦点工作；关掉或卸载时立即恢复。
// 主窗口和文件窗口各调用一次（两边是各自独立的网页环境）。

import { useEffect } from "react";

import { useSettings } from "../stores/settings";
import { imeFocus } from "./imeFocus";

export function useImeDefaultEnglish(): void {
  // 设置还没读出来之前用的是默认值，别据此动输入法：关掉了这个开关的人启动时不该闪一下
  const loaded = useSettings((state) => state.loaded);
  const enabled = useSettings((state) => state.terminal_ime_english);
  useEffect(() => {
    if (!loaded || !enabled) return;
    return imeFocus.enable();
  }, [loaded, enabled]);
}
