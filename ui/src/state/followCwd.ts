//! "跟随终端目录"的判定（M3-3 第 3 节）：纯函数，方便单测。
//! 轮询终端目录的 hook 也放这里（PERF-2：目录没变就不更新状态）。

import { useEffect, useState } from "react";

/** 轮询终端目录的间隔：够快，又不会给服务器添负担（每次只是一条 exec 通道跑一下）。 */
export const FOLLOW_POLL_MS = 1500;

/**
 * 远端栏该不该跳到终端的目录。
 *
 * - 开关关着（用户手动换过目录）：不动；
 * - 取不到终端目录（`null`：不是 Linux、对不上号、会话没连上）：不动，保持在家目录或
 *   当前目录，不报错；
 * - 终端目录和远端栏当前目录一样：不动（不重复列目录）。
 */
export function followTarget(
  follow: boolean,
  terminalCwd: string | null,
  currentRemote: string | null,
): string | null {
  if (!follow || !terminalCwd) return null;
  if (terminalCwd === currentRemote) return null;
  return terminalCwd;
}

/** 最近一次查到的终端目录；`cwd` 为 null 表示这次没查到。 */
export interface TerminalCwd {
  sessionId: string;
  cwd: string | null;
}

/**
 * 把新查到的结果并进状态：会话和目录都和上次一样就返回原对象（React 看到同一个引用不再提交更新），
 * 成功结果和重复的 null（查询失败）都走这里。
 */
export function mergeTerminalCwd(
  prev: TerminalCwd | null,
  sessionId: string,
  cwd: string | null,
): TerminalCwd | null {
  if (prev && prev.sessionId === sessionId && prev.cwd === cwd) return prev;
  return { sessionId, cwd };
}

/**
 * 定期问一下 `sessionId` 的终端在哪个目录（`sessionId` 为 null = 不轮询）。
 * 首次立即查一次，之后每 `FOLLOW_POLL_MS` 一次；上一次没回来、页面隐藏时跳过；
 * 失败记成 null（取不到就是不跟随），下次成功自动恢复。
 */
export function useTerminalCwd(
  sessionId: string | null,
  query: (sessionId: string) => Promise<string | null>,
): TerminalCwd | null {
  const [terminalCwd, setTerminalCwd] = useState<TerminalCwd | null>(null);
  useEffect(() => {
    if (!sessionId) return;
    let stopped = false;
    let inFlight = false;
    const tick = () => {
      if (inFlight || document.hidden) return;
      inFlight = true;
      void query(sessionId)
        .then((cwd) => {
          if (!stopped) setTerminalCwd((prev) => mergeTerminalCwd(prev, sessionId, cwd));
        })
        .catch(() => {
          if (!stopped) setTerminalCwd((prev) => mergeTerminalCwd(prev, sessionId, null));
        })
        .finally(() => {
          inFlight = false;
        });
    };
    tick();
    const timer = window.setInterval(tick, FOLLOW_POLL_MS);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
    // query 是模块级函数，不进依赖，避免调用方每次渲染换引用就重启轮询。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);
  return terminalCwd;
}
