//! 文件窗口（像 Xftp）：整个程序最多一个，里面用标签放多个 SFTP。
//!
//! 标签从主窗口交过来（拖出主窗口、右键「移到文件窗口」，或者文件窗口开着时新开的 SFTP）。
//! SFTP 会话在后端，换窗口不用重连。关标签 / 关窗口 = 断开对应的 SFTP（有传输先问）；
//! 标签也可以拖回 / 右键移回主窗口。内置编辑器在主窗口，打开文件交给主窗口去开。

import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { FolderTree, X } from "lucide-react";

import {
  fileWindowOwned,
  fileWindowTake,
  onFileWindowPending,
  onSettingsChanged,
  reportFileSessions,
  requestOpenEditor,
  returnToMain,
} from "../../api/fileWindow";
import { sftpClose, sftpOpen } from "../../api/sftp";
import { useVault } from "../../stores/vault";
import SftpHostBar from "../sftp/SftpHostBar";
import { cn } from "../../lib/utils";
import {
  activeAfterClose,
  fileTabLabel,
  handoffOf,
  isOutsideWindow,
  sessionsOf,
  tabFromHandoff,
  type FileWindowTab,
} from "../../state/fileWindowTabs";
import { shouldMergeOnClose } from "../../state/closeBehavior";
import { useSettings } from "../../stores/settings";
import SftpConflictHost from "../sftp/SftpConflictHost";
import SftpTabView, { type SftpPaths } from "../sftp/SftpTabView";
import { WindowControls } from "../titlebar/TopTabs";
import { ConfirmDialog } from "../ui/confirm-dialog";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "../ui/context-menu";
import { toast } from "../ui/toast";

/** 窗口名（不带中文，和 Z-SSH 对应）。 */
const WINDOW_TITLE = "Z-SFTP";

let nextId = 0;
const makeId = () => `ftab-${Date.now().toString(36)}-${(nextId += 1)}`;

/** 拖多远才算开始拖（避免点一下就当成拖）。 */
const DRAG_THRESHOLD = 6;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export default function FileWindowApp() {
  const [tabs, setTabsState] = useState<FileWindowTab[]>([]);
  const [activeId, setActiveId] = useState("");
  const [confirm, setConfirm] = useState<{
    title: string;
    message: string;
    confirmLabel: string;
    onConfirm: () => void;
  } | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);
  // 关窗口时要读到最新的标签，回调里不能用闭包里的旧值：每次改标签同时更新这个引用。
  const tabsRef = useRef<FileWindowTab[]>([]);
  const setTabs = useCallback((update: (tabs: FileWindowTab[]) => FileWindowTab[]) => {
    tabsRef.current = update(tabsRef.current);
    setTabsState(tabsRef.current);
  }, []);
  /** 每个会话还在进行的传输数（关标签 / 关窗口前要问）。 */
  const activeTransfers = useRef(new Map<string, number>());
  /** 每个会话两栏现在的目录（移回主窗口时带过去）。 */
  const pathsRef = useRef(new Map<string, SftpPaths>());
  /** 用户已经确认要关窗口：再收到关闭请求就直接关。 */
  const closingRef = useRef(false);

  useEffect(() => {
    // 外观设置（主题在这里应用）；主窗口改了设置会通知这边重读，主题跟着变
    void useSettings.getState().load();
    // 标签顶上的主机下拉要主机库
    void useVault.getState().load().catch(() => {});
    let stop: (() => void) | null = null;
    let cancelled = false;
    void onSettingsChanged(() => void useSettings.getState().load())
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);

  const connect = useCallback(
    (tab: FileWindowTab) => {
      // 连回来时标签已经换了主机（连点了两次），这条就关掉，不让旧结果盖掉新的
      const stillWanted = (item: FileWindowTab) =>
        item.id === tab.id && item.hostId === tab.hostId && !item.sftpId;
      void sftpOpen(tab.hostId)
        .then((sftpId) => {
          if (!tabsRef.current.some(stillWanted)) {
            void sftpClose(sftpId).catch(() => {});
            return;
          }
          setTabs((current) =>
            current.map((item) =>
              stillWanted(item) ? { ...item, sftpId, phase: "ready", status: "" } : item,
            ),
          );
        })
        .catch((error: unknown) =>
          setTabs((current) =>
            current.map((item) =>
              stillWanted(item) ? { ...item, phase: "failed", status: errorText(error) } : item,
            ),
          ),
        );
    },
    [setTabs],
  );

  /** 标签顶上换主机：断开原来的 SFTP，在同一个标签里连新的。有传输在跑时先不换。 */
  const switchHost = useCallback(
    (tabId: string, host: { id: number; name: string }) => {
      const tab = tabsRef.current.find((item) => item.id === tabId);
      if (!tab || tab.hostId === host.id) return;
      const busy = tab.sftpId ? activeTransfers.current.get(tab.sftpId) ?? 0 : 0;
      if (busy > 0) {
        toast.info(`还有 ${busy} 个传输在进行，等它们结束再换主机`);
        return;
      }
      if (tab.sftpId) {
        activeTransfers.current.delete(tab.sftpId);
        void sftpClose(tab.sftpId).catch(() => {});
      }
      const next: FileWindowTab = {
        ...tab,
        hostId: host.id,
        title: `${host.name} (SFTP)`,
        sftpId: "",
        phase: "connecting",
        status: "连接中…",
      };
      setTabs((current) => current.map((item) => (item.id === tabId ? next : item)));
      connect(next);
    },
    [connect, setTabs],
  );

  /** 把待接管的标签都取过来。 */
  const adopt = useCallback(async () => {
    let incoming;
    try {
      incoming = await fileWindowTake();
    } catch (error) {
      console.warn("取待接管的 SFTP 失败", error);
      return;
    }
    for (const handoff of incoming) {
      const tab = tabFromHandoff(handoff, makeId());
      setTabs((current) => [...current, tab]);
      setActiveId(tab.id);
      if (tab.phase === "connecting") connect(tab);
    }
  }, [connect, setTabs]);

  useEffect(() => {
    let stop: (() => void) | null = null;
    let cancelled = false;
    void adopt();
    void onFileWindowPending(() => void adopt())
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [adopt]);

  // 会话清单变了：告诉后端（兜底关）和主窗口（编辑器保存时找会话）。
  useEffect(() => {
    const sessions = sessionsOf(tabs);
    void fileWindowOwned(sessions.map((s) => s.sftp_id)).catch(() => {});
    void reportFileSessions(sessions).catch(() => {});
    document.title = WINDOW_TITLE;
  }, [tabs]);

  /** 把标签从这个窗口拿掉。`disconnect` = 同时断开 SFTP（移回主窗口时不断）。 */
  const removeTab = useCallback(
    (tabId: string, disconnect: boolean) => {
      const tab = tabsRef.current.find((item) => item.id === tabId);
      if (!tab) return;
      if (tab.sftpId) {
        activeTransfers.current.delete(tab.sftpId);
        if (disconnect) void sftpClose(tab.sftpId).catch(() => {});
      }
      const before = tabsRef.current;
      setTabs((current) => current.filter((item) => item.id !== tabId));
      setActiveId((active) => activeAfterClose(before, tabId, active));
      // 最后一个标签走了，窗口也没用了
      if (tabsRef.current.length === 0) {
        closingRef.current = true;
        void getCurrentWindow().close();
      }
    },
    [setTabs],
  );

  const closeTab = useCallback(
    (tabId: string) => {
      const tab = tabsRef.current.find((item) => item.id === tabId);
      if (!tab) return;
      const busy = tab.sftpId ? activeTransfers.current.get(tab.sftpId) ?? 0 : 0;
      if (busy > 0) {
        setConfirm({
          title: "关闭这个标签？",
          message: `还有 ${busy} 个传输在进行，关闭会取消它们。`,
          confirmLabel: "关闭并取消",
          onConfirm: () => removeTab(tabId, true),
        });
        return;
      }
      removeTab(tabId, true);
    },
    [removeTab],
  );

  /** 移回主窗口：会话交过去，这边不断开。 */
  const moveToMain = useCallback(
    (tabId: string) => {
      const tab = tabsRef.current.find((item) => item.id === tabId);
      if (!tab) return;
      const handoff = handoffOf(tab, pathsRef.current.get(tab.sftpId));
      void returnToMain(handoff)
        .then(() => removeTab(tabId, false))
        .catch((error: unknown) => toast.error(`移回主窗口失败：${errorText(error)}`));
    },
    [removeTab],
  );

  /**
   * 设置选了「合并回主窗口」时关窗口走这里：每个连好的标签原样交给主窗口（连接、传输都不断），
   * 没连好的标签没有会话可交，直接收掉；最后一个标签走了，窗口自己会关。
   * 某个标签交不过去就停在这里、窗口留着（报错已经弹了），不会把还没交的标签断开。
   */
  const mergeAllToMain = useCallback(async () => {
    for (const tab of [...tabsRef.current]) {
      if (tab.phase === "ready" && tab.sftpId) {
        try {
          await returnToMain(handoffOf(tab, pathsRef.current.get(tab.sftpId)));
        } catch (error: unknown) {
          toast.error(`移回主窗口失败：${errorText(error)}`);
          return;
        }
        removeTab(tab.id, false);
      } else {
        removeTab(tab.id, true);
      }
    }
  }, [removeTab]);
  const mergeAllRef = useRef(mergeAllToMain);
  mergeAllRef.current = mergeAllToMain;

  // 关窗口：设置选了合并就把标签交回主窗口；否则有传输先问，然后断开这个窗口里的全部 SFTP。
  useEffect(() => {
    const win = getCurrentWindow();
    let stop: (() => void) | null = null;
    let cancelled = false;
    const closeAll = async () => {
      const ids = tabsRef.current.map((tab) => tab.sftpId).filter(Boolean);
      await Promise.all(ids.map((id) => sftpClose(id).catch(() => {})));
      await fileWindowOwned([]).catch(() => {});
    };
    void win
      .onCloseRequested(async (event) => {
        if (closingRef.current) {
          await closeAll();
          return;
        }
        if (!useSettings.getState().loaded) {
          // 设置还没读回来（刚打开几秒内）：先读，免得拿默认值盖掉用户选的「合并」。
          await useSettings.getState().load().catch(() => {});
        }
        if (
          shouldMergeOnClose(
            useSettings.getState().file_window_close,
            tabsRef.current.length,
            closingRef.current,
          )
        ) {
          event.preventDefault();
          await mergeAllRef.current();
          return;
        }
        const busy = [...activeTransfers.current.values()].reduce((sum, n) => sum + n, 0);
        if (busy > 0) {
          event.preventDefault();
          setConfirm({
            title: "关闭文件窗口？",
            message: `还有 ${busy} 个传输在进行，关闭会取消它们，并断开这个窗口里的 SFTP。`,
            confirmLabel: "关闭并取消",
            onConfirm: () => {
              closingRef.current = true;
              void win.close();
            },
          });
          return;
        }
        await closeAll();
      })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);

  /** 标签拖出这个窗口 = 移回主窗口。 */
  const startDrag = useCallback(
    (tabId: string, event: React.PointerEvent<HTMLDivElement>) => {
      if (event.button !== 0 || (event.target as HTMLElement).closest("button")) return;
      const start = { x: event.clientX, y: event.clientY };
      const element = event.currentTarget;
      element.setPointerCapture(event.pointerId);
      let active = false;
      const move = (e: PointerEvent) => {
        if (!active && Math.hypot(e.clientX - start.x, e.clientY - start.y) >= DRAG_THRESHOLD) {
          active = true;
          setDragging(tabId);
        }
      };
      const up = (e: PointerEvent) => {
        element.removeEventListener("pointermove", move);
        element.removeEventListener("pointerup", up);
        element.removeEventListener("pointercancel", up);
        setDragging(null);
        if (
          active &&
          e.type === "pointerup" &&
          isOutsideWindow(e.clientX, e.clientY, window.innerWidth, window.innerHeight)
        ) {
          moveToMain(tabId);
        }
      };
      element.addEventListener("pointermove", move);
      element.addEventListener("pointerup", up);
      element.addEventListener("pointercancel", up);
    },
    [moveToMain],
  );

  return (
    <div className="flex h-full flex-col bg-background text-foreground">
      {/* 自绘标题栏：空白处拖动窗口、双击最大化；标签和按钮各自挡掉拖动 */}
      <div
        data-section="file-window-titlebar"
        data-tauri-drag-region="deep"
        className="relative flex h-10 shrink-0 items-end gap-0.5 bg-background pl-3"
      >
        {/* 和主窗口的标签栏一样：同一个底色，底部一条细线，当前标签盖住它下面那段 */}
        <div aria-hidden className="pointer-events-none absolute inset-x-0 bottom-0 h-px bg-border" />
        <span className="mb-2 mr-2 flex shrink-0 items-center gap-1.5 text-xs font-semibold text-muted-foreground">
          <FolderTree size={14} className="text-sky-400" />
          {WINDOW_TITLE}
        </span>
        <div className="flex min-w-0 flex-1 items-end gap-0.5 overflow-x-auto">
        {tabs.map((tab) => (
          <ContextMenu key={tab.id}>
            <ContextMenuTrigger asChild>
              <div
                data-file-tab={tab.id}
                data-tauri-drag-region={false}
                onClick={() => setActiveId(tab.id)}
                onPointerDown={(event) => startDrag(tab.id, event)}
                title={`${tab.title}（拖出窗口或右键可移回主窗口）`}
                className={cn(
                  "group relative flex h-8 max-w-56 shrink-0 cursor-pointer items-center gap-2 rounded-t-md border-x border-t px-3 text-[13px] transition-colors",
                  tab.id === activeId
                    ? "z-10 border-border bg-background text-foreground"
                    : "border-transparent text-muted-foreground hover:bg-foreground/5",
                  dragging === tab.id && "opacity-50",
                )}
              >
                <FolderTree size={14} className="shrink-0 text-sky-400" />
                <span className="truncate">{fileTabLabel(tab.title)}</span>
                <button
                  title="关闭（断开这个 SFTP）"
                  onClick={(event) => {
                    event.stopPropagation();
                    closeTab(tab.id);
                  }}
                  className="rounded p-0.5 text-muted-foreground opacity-60 hover:bg-accent hover:opacity-100"
                >
                  <X size={12} />
                </button>
              </div>
            </ContextMenuTrigger>
            <ContextMenuContent>
              <ContextMenuItem onSelect={() => moveToMain(tab.id)}>移回主窗口</ContextMenuItem>
              <ContextMenuItem onSelect={() => closeTab(tab.id)}>关闭</ContextMenuItem>
            </ContextMenuContent>
          </ContextMenu>
        ))}
        </div>
        <WindowControls />
      </div>

      <div className="relative min-h-0 flex-1">
        {tabs.length === 0 && (
          <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
            正在打开…
          </div>
        )}
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={cn("absolute inset-0 flex flex-col", tab.id !== activeId && "hidden")}
          >
            <SftpHostBar
              hostId={tab.hostId}
              busy={tab.phase === "connecting"}
              onPick={(host) => switchHost(tab.id, host)}
            />
            <div className="relative flex min-h-0 flex-1 flex-col">
            {tab.phase === "ready" ? (
              <SftpTabView
                key={tab.sftpId}
                sftpId={tab.sftpId}
                hostId={tab.hostId}
                active={tab.id === activeId}
                initialPaths={tab.paths}
                onPathsChange={(paths) => pathsRef.current.set(tab.sftpId, paths)}
                onActiveCountChange={(count) => activeTransfers.current.set(tab.sftpId, count)}
                openInEditor={{
                  remote: (path) =>
                    void requestOpenEditor({
                      kind: "remote",
                      sftp_id: tab.sftpId,
                      host_id: tab.hostId,
                      path,
                    }).catch((error: unknown) => toast.error(errorText(error))),
                  local: (path) =>
                    void requestOpenEditor({ kind: "local", sftp_id: "", host_id: null, path }).catch(
                      (error: unknown) => toast.error(errorText(error)),
                    ),
                }}
              />
            ) : (
              <div className="flex h-full items-center justify-center p-6 text-sm text-muted-foreground">
                {tab.phase === "failed" ? `SFTP 打开失败：${tab.status}` : tab.status}
              </div>
            )}
            </div>
          </div>
        ))}
      </div>

      {/* 同名文件对话框：这个窗口里所有标签的提问都在这里排队弹 */}
      <SftpConflictHost sessionIds={tabs.map((tab) => tab.sftpId).filter(Boolean)} />

      {confirm && (
        <ConfirmDialog
          open
          title={confirm.title}
          message={confirm.message}
          confirmLabel={confirm.confirmLabel}
          destructive
          onOpenChange={(open) => !open && setConfirm(null)}
          onConfirm={() => {
            const run = confirm.onConfirm;
            setConfirm(null);
            run();
          }}
        />
      )}
    </div>
  );
}
