import type { ITheme } from "@xterm/xterm";
import { PlugZap, RotateCcw } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";

import {
  registerDropTarget,
  registerExternalDropTarget,
  useDragStore,
} from "../state/pointerDrag";
import { joinQuoted, quoteLocalPath, quoteRemotePath } from "../state/pathQuote";

import { termAck, termResize, termWrite } from "../api/session";
import type { Host } from "../api/vault";
import EmptyTabLauncher from "./terminal/EmptyTabLauncher";
import { cn } from "../lib/utils";
import type { PaneSession } from "../state/sessions";
import { Spinner } from "./ui/spinner";
import TerminalView, { type TerminalHandle } from "./TerminalView";

/** 累计渲染满这么多字节回一次 ack（设计文档 5.2）。 */
const ACK_THRESHOLD_BYTES = 64 * 1024;

interface PaneViewProps {
  paneId: string;
  session: PaneSession;
  /** 终端配色 */
  terminalTheme: ITheme;
  focused: boolean;
  onFocus: (paneId: string) => void;
  /** 终端挂载好、拿到第一份尺寸（开会话就用它）。 */
  onReady: (paneId: string, cols: number, rows: number) => void;
  /** fit 之后的新尺寸变化。 */
  onResize: (paneId: string, cols: number, rows: number) => void;
  onReconnect: (paneId: string) => void;
  /** 把"往这个 pane 写后端输出"的函数交给上层（开会话的 Channel 要用）。 */
  registerWriter: (paneId: string, writer: ((chunk: ArrayBuffer) => void) | null) => void;
  /** 键盘输入先给上层看一眼：返回 true 表示上层已经发出去了（输入广播），这里不再发。 */
  onInput?: (paneId: string, data: string) => boolean;
  /** 这个标签开着输入广播：边框和角标提醒"打的字会发到所有分屏"。 */
  broadcasting?: boolean;
  /** 这个标签有分屏：焦点窗格顶上画一道细线，告诉用户键盘输入去哪（单个窗格不画）。 */
  split?: boolean;
  /** 空白窗格里选了主机：就在这个窗格里连。 */
  onConnectHost?: (paneId: string, host: Host) => void;
  /** 空白窗格里点了某台主机的 SFTP：另开 SFTP 标签。 */
  onOpenSftp?: (host: Host) => void;
}

export default function PaneView({
  paneId,
  session,
  terminalTheme,
  focused,
  onFocus,
  onReady,
  onResize,
  onReconnect,
  registerWriter,
  onInput,
  broadcasting = false,
  split = false,
  onConnectHost,
  onOpenSftp,
}: PaneViewProps) {
  const onInputRef = useRef(onInput);
  onInputRef.current = onInput;
  const terminalRef = useRef<TerminalHandle>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  // 已经渲染、但还没报给后端的字节数。
  const pendingAck = useRef(0);
  // 回调里要读会话的最新状态（sessionId 什么时候出现、什么时候没了）。
  const sessionRef = useRef(session);
  sessionRef.current = session;
  /** 最近一次量到的行列数（容器没尺寸时量不到，所以可能一直是 null）。 */
  const latestSize = useRef<{ cols: number; rows: number } | null>(null);
  /**
   * 开会话时用的尺寸：进入 connecting 的那一刻量到的值。
   *
   * 连接期间量到的新尺寸发不出去（`sessionId` 还没回来），连上之后拿它跟最新
   * 尺寸比一次，不同就补发 term_resize——否则远端 PTY 会一直停在初始尺寸。
   */
  const openedSize = useRef<{ cols: number; rows: number } | null>(null);

  const flushAck = useCallback(() => {
    const sessionId = sessionRef.current.sessionId;
    const bytes = pendingAck.current;
    if (!sessionId || bytes === 0) return;
    pendingAck.current = 0;
    void termAck(sessionId, bytes).catch((error: unknown) => {
      console.error("term_ack 失败", error);
    });
  }, []);

  const writeOutput = useCallback(
    (chunk: ArrayBuffer) => {
      // 必须在 xterm 处理完（write 的 callback）之后才报 ack，
      // 收到数据就报等于没流控（设计文档 5.2）。
      terminalRef.current?.write(new Uint8Array(chunk), () => {
        pendingAck.current += chunk.byteLength;
        if (pendingAck.current >= ACK_THRESHOLD_BYTES) flushAck();
      });
    },
    [flushAck],
  );

  useEffect(() => {
    registerWriter(paneId, writeOutput);
    return () => registerWriter(paneId, null);
  }, [paneId, registerWriter, writeOutput]);

  // 断开之后攒着的 ack 没意义了（后端会话已经没了），直接丢掉。
  useEffect(() => {
    if (session.phase === "disconnected") pendingAck.current = 0;
  }, [session.phase]);

  // 焦点转到这个 pane 时把键盘交给终端。
  useEffect(() => {
    if (focused && session.phase !== "disconnected") {
      terminalRef.current?.focus();
    }
  }, [focused, session.phase]);

  const handleData = useCallback((data: string) => {
    if (onInputRef.current?.(paneId, data)) return;
    const sessionId = sessionRef.current.sessionId;
    if (!sessionId) return;
    void termWrite(sessionId, data).catch((error: unknown) => {
      console.error("term_write 失败", error);
    });
  }, [paneId]);

  const handleReady = useCallback(
    (cols: number, rows: number) => {
      latestSize.current = { cols, rows };
      onReady(paneId, cols, rows);
    },
    [onReady, paneId],
  );

  const handleResize = useCallback(
    (cols: number, rows: number) => {
      latestSize.current = { cols, rows };
      onResize(paneId, cols, rows);
      const sessionId = sessionRef.current.sessionId;
      // 连接中还没有 sessionId，这次 resize 补不了；连上之后的 effect 会补发。
      if (!sessionId) return;
      void termResize(sessionId, cols, rows).catch((error: unknown) => {
        console.error("term_resize 失败", error);
      });
    },
    [onResize, paneId],
  );

  // 会话开始连接：记住开会话用的尺寸。
  useEffect(() => {
    if (session.phase !== "connecting" && session.phase !== "authenticating") return;
    const size = latestSize.current;
    if (size) openedSize.current = size;
  }, [session.phase]);

  // 连上（含重连、分屏 duplicate）：补发连接期间丢掉的 resize。
  useEffect(() => {
    if (session.phase !== "connected") return;
    const sessionId = session.sessionId;
    const size = latestSize.current;
    const opened = openedSize.current;
    if (!sessionId || !size || !opened) return;
    if (size.cols === opened.cols && size.rows === opened.rows) return;
    void termResize(sessionId, size.cols, size.rows).catch((error: unknown) => {
      console.error("term_resize 失败", error);
    });
  }, [session.phase, session.sessionId]);

  // 落点：从 SFTP 面板远端栏拖来的文件，把远端路径插到输入行（不执行、不加回车，M3-3 第 3 节）。
  useEffect(() => {
    const element = rootRef.current;
    if (!element) return;
    return registerDropTarget(element, {
      accepts: (payload) =>
        payload.side === "remote" && sessionRef.current.phase === "connected",
      onDrop: (payload) => {
        const text = joinQuoted(
          payload.entries.map((entry) => entry.path),
          quoteRemotePath,
        );
        terminalRef.current?.paste(text);
        terminalRef.current?.focus();
      },
    });
  }, []);
  // 资源管理器拖来的文件落在终端上：插入本地路径（加引号），不执行（M3-3 第 4 节）。
  useEffect(() => {
    const element = rootRef.current;
    if (!element) return;
    return registerExternalDropTarget(element, {
      onDrop: (paths) => {
        if (sessionRef.current.phase !== "connected") return;
        terminalRef.current?.paste(joinQuoted(paths, quoteLocalPath));
        terminalRef.current?.focus();
      },
    });
  }, []);
  const dropHover = useDragStore((state) => state.hover !== null && state.hover === rootRef.current);

  const connecting =
    session.phase === "connecting" || session.phase === "authenticating";
  const showEmptyHint = session.origin.kind === "empty" && session.phase !== "connected";

  return (
    <div
      ref={rootRef}
      data-drop-target="terminal"
      className={cn(
        // 不画外框：留白和终端同一个背景色，和标签栏连成一片（以前焦点时有一圈蓝框）
        "workspace-pane group relative h-full w-full overflow-hidden",
        dropHover && "ring-2 ring-inset ring-primary",
        broadcasting && "ring-2 ring-inset ring-amber-500/70",
      )}
      style={{ backgroundColor: terminalTheme.background }}
      data-pane-id={paneId}
      data-focused={focused ? "true" : undefined}
      // mousedown 而不是 click：先在终端抢走焦点之前把焦点记下来
      onMouseDown={() => onFocus(paneId)}
    >
      {/* 8px 内边距：文字和光标不贴边，焦点边框也不压在字上；
          fit 量的是这一层里面的尺寸，行列数自动扣掉内边距 */}
      <div className="absolute inset-0 p-2">
        <TerminalView
          ref={terminalRef}
          theme={terminalTheme}
          onData={handleData}
          onResize={handleResize}
          onReady={handleReady}
        />
      </div>

      {split && focused && !broadcasting && (
        <span
          aria-hidden
          data-section="pane-focus-line"
          className="pointer-events-none absolute inset-x-0 top-0 z-10 h-0.5 bg-primary/70"
        />
      )}
      {broadcasting && (
        <span
          data-section="broadcast-badge"
          className="pointer-events-none absolute right-2 top-1 z-10 rounded bg-amber-500/90 px-1.5 text-[10px] font-medium text-black"
        >
          广播
        </span>
      )}

      {/* 连接中：把终端整个盖住，只显示居中的进度（不出现那个空光标块） */}
      {connecting && (
        <div
          className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 text-sm"
          style={{ backgroundColor: terminalTheme.background ?? "hsl(var(--background))" }}
        >
          <Spinner size={18} className="text-muted-foreground" />
          <span className="text-muted-foreground">{session.status || "连接中…"}</span>
        </div>
      )}

      {/* 不再在终端右上角放悬停的"关闭窗格"按钮（用户 2026-10-21 要求去掉，它压住终端内容还容易误点）：
          关窗格用 Ctrl+Shift+W，或者标签的关闭按钮 / 右键菜单 */}

      {/* 空 pane：选一台主机连（不依赖左边的主机树，侧栏收起时也能选） */}
      {showEmptyHint &&
        (onConnectHost ? (
          <EmptyTabLauncher
            onConnect={(host) => onConnectHost(paneId, host)}
            onOpenSftp={(host) => onOpenSftp?.(host)}
          />
        ) : (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center text-sm text-muted-foreground">
            双击左侧主机开始连接
          </div>
        ))}

      {/* 断线遮罩：保留回滚内容，点这里或者按 R 重连（设计文档 4.5） */}
      {session.phase === "disconnected" && (
        <div
          // 用终端自己的底色和文字色：界面和终端一明一暗时，遮罩和上面的标签仍是一块
          className={cn(
            "absolute inset-0 z-10 flex cursor-pointer flex-col items-center justify-center gap-1.5 text-sm backdrop-blur-[1px]",
            !terminalTheme.background && "bg-background/85",
          )}
          style={
            terminalTheme.background
              ? {
                  backgroundColor: `color-mix(in srgb, ${terminalTheme.background} 88%, transparent)`,
                  color: terminalTheme.foreground,
                }
              : undefined
          }
          onClick={() => onReconnect(paneId)}
        >
          <div className="flex items-center gap-2 font-medium text-destructive">
            <PlugZap size={16} />
            已断开
          </div>
          <div className="max-w-[80%] truncate text-xs opacity-70">
            {session.reason || "未知原因"}
          </div>
          <div className="mt-1 flex items-center gap-1 text-xs opacity-80">
            <RotateCcw size={12} /> 按 R 重连，或者点这里
          </div>
        </div>
      )}
    </div>
  );
}
