// 设置「终端里自动切到英文输入」打开时，让输入法控制器按文档焦点工作；关掉或卸载时立即恢复。
// 主窗口和文件窗口各调用一次（两边是各自独立的网页环境）。

import { useEffect } from "react";

import { onImeRestorePending } from "../api/ime";
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

  // 别的窗口被关掉时输入法没恢复成功：后端广播，这个窗口才知道还欠着一次恢复
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onImeRestorePending(() => imeFocus.markRestoreOwed())
      .then((stop) => {
        if (cancelled) stop();
        else unlisten = stop;
      })
      .catch((error: unknown) => console.warn("订阅输入法待恢复通知失败", error));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
