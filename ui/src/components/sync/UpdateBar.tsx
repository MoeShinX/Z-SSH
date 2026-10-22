//! 自动更新的提示条和详情对话框。
//!
//! 自动检查的结果（后台下载、下好等退出时安装）**底部不提示**；只有手动「检查更新」引出的状态，
//! 以及免安装版（没法自动装）才在底部状态栏上方出现一条。点开看到版本号、更新说明和三个按钮
//! （立即更新 / 稍后 / 跳过这个版本）。手动流程下载时显示进度；下载完提示重启，重启前检查有没有
//! 正在进行的传输、终端里的 AI 命令、编辑器里没保存的修改，有就先问用户。
//! 后台已经自动下好时，手动打开的对话框说明"退出时会自动安装"，并保留"立即重启安装"。

import { useState } from "react";

import { updateBusy, updateInstall, type RestartBusy } from "../../api/updater";
import {
  mergeBusy,
  progressPercent,
  restartWarning,
  updateBarModel,
} from "../../state/updateModel";
import { useUpdater } from "../../stores/updater";
import { cn } from "../../lib/utils";
import { Button } from "../ui/button";
import { ConfirmDialog } from "../ui/confirm-dialog";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";
import { toast } from "../ui/toast";
import { parseReleaseNotes, sectionTone } from "../../state/releaseNotes";

const DOT: Record<string, string> = {
  info: "bg-sky-500",
  busy: "bg-sky-500 animate-pulse",
  ready: "bg-emerald-500",
  error: "bg-red-500",
};

export interface FrontendBusy {
  transfers: number;
  unsavedEdits: number;
}

/** 重启安装：先查有没有会被打断的事，有就问一次，再装。 */
export async function requestInstall(
  getFrontendBusy: () => FrontendBusy,
  ask: (message: string, proceed: () => void) => void,
): Promise<void> {
  let backend: RestartBusy = { transfers: 0, ai_commands: 0, unsaved_edits: 0 };
  try {
    backend = await updateBusy();
  } catch {
    // 查不了就按只有前端的数字判断
  }
  const warning = restartWarning(mergeBusy(getFrontendBusy(), backend));
  const proceed = () => {
    void updateInstall().catch((error: unknown) => toast.error(String(error)));
  };
  if (warning) ask(warning, proceed);
  else proceed();
}

export default function UpdateBar({ getFrontendBusy }: { getFrontendBusy: () => FrontendBusy }) {
  const status = useUpdater((state) => state.status);
  const dismissed = useUpdater((state) => state.dismissedVersion);
  const dialogOpen = useUpdater((state) => state.dialogOpen);
  const openDialog = useUpdater((state) => state.openDialog);
  const dismiss = useUpdater((state) => state.dismiss);
  const skip = useUpdater((state) => state.skip);
  const download = useUpdater((state) => state.download);
  const portable = useUpdater((state) => state.portable);
  const auto = useUpdater((state) => state.auto);
  const autoInstall = useUpdater((state) => state.prefs?.auto_check ?? true);
  const openDownloadPage = useUpdater((state) => state.openDownloadPage);
  const [restartAsk, setRestartAsk] = useState<{ message: string; proceed: () => void } | null>(
    null,
  );
  const model = updateBarModel(status, dismissed, portable, auto);
  const openPage = () => {
    openDialog(false);
    void openDownloadPage().catch((error: unknown) => toast.error(String(error)));
  };

  const startInstall = () =>
    void requestInstall(getFrontendBusy, (message, proceed) => setRestartAsk({ message, proceed }));

  const onBarClick = () => {
    if (status?.phase === "ready") startInstall();
    else if (status?.phase === "available" || status?.phase === "failed") openDialog(true);
  };

  const available = status?.phase === "available" ? status : null;
  const ready = status?.phase === "ready" ? status : null;
  return (
    <>
      {model.visible && (
        <button
          type="button"
          data-testid="update-bar"
          data-tone={model.tone}
          title={model.text}
          onClick={onBarClick}
          className="relative flex h-6 w-full shrink-0 cursor-pointer items-center gap-2 border-t border-border/60 bg-background px-3 text-left text-xs text-foreground hover:bg-accent"
        >
          <span className={cn("inline-block size-2 shrink-0 rounded-full", DOT[model.tone])} />
          <span className="truncate">{model.text}</span>
          {status?.phase === "downloading" && (
            // 以前是 h-0.5 的细线，用户注意不到；不知道总大小时整条闪烁表示"进行中"
            <span
              data-testid="update-bar-progress"
              className={cn(
                "absolute bottom-0 left-0 h-1 bg-sky-500/80 transition-[width] duration-200",
                progressPercent(status.downloaded, status.total) === null && "animate-pulse",
              )}
              style={{
                width: `${progressPercent(status.downloaded, status.total) ?? 100}%`,
              }}
            />
          )}
        </button>
      )}

      <Dialog
        open={dialogOpen && (available !== null || ready !== null || status?.phase === "failed")}
        onOpenChange={openDialog}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {available
                ? `Z-SSH v${available.version.replace(/^v/, "")} 可以更新`
                : ready
                  ? `Z-SSH v${ready.version.replace(/^v/, "")} 已下载好`
                  : "更新没有成功"}
            </DialogTitle>
          </DialogHeader>
          {ready && (
            <p data-testid="update-ready-note" className="text-sm text-muted-foreground">
              {autoInstall
                ? "新版本已经在后台下载好并通过签名校验。现在重启安装会断开所有会话；手头有活没做完就选「稍后」，退出 Z-SSH 时会自动安装，下次打开就是新版。"
                : "新版本已经下载好并通过签名校验。「启动时自动检查」已关闭，退出 Z-SSH 时不会自动安装；需要的话可以现在重启安装。"}
            </p>
          )}
          {available && portable && (
            <p data-testid="update-portable-note" className="text-sm text-muted-foreground">
              这是免安装版，不能自动更新。请在发布页下载新的免安装 zip，解压覆盖原来的文件夹；
              主机、凭据和设置都在应用数据目录里，不受影响。
            </p>
          )}
          {available ? (
            <ReleaseNotes notes={available.notes ?? ""} />
          ) : ready ? (
            ready.notes ? (
              <ReleaseNotes notes={ready.notes} />
            ) : null
          ) : (
            <div className="text-sm text-red-500" role="alert">
              {status?.phase === "failed" ? status.message : ""}
            </div>
          )}
          <DialogFooter>
            {ready ? (
              <>
                <Button variant="outline" onClick={() => openDialog(false)}>
                  {autoInstall ? "稍后，退出时安装" : "稍后"}
                </Button>
                <Button
                  onClick={() => {
                    openDialog(false);
                    startInstall();
                  }}
                >
                  立即重启安装
                </Button>
              </>
            ) : available ? (
              <>
                <Button variant="ghost" onClick={() => void skip(available.version)}>
                  跳过这个版本
                </Button>
                <Button variant="outline" onClick={() => dismiss(available.version)}>
                  稍后
                </Button>
                {portable ? (
                  <Button onClick={openPage}>打开下载页</Button>
                ) : (
                  <Button
                    onClick={() => {
                      openDialog(false);
                      void download();
                    }}
                  >
                    立即更新
                  </Button>
                )}
              </>
            ) : (
              <>
                {portable && status?.phase === "failed" && status.stage === "download" && (
                  <Button onClick={openPage}>打开下载页</Button>
                )}
                {!portable && status?.phase === "failed" && status.stage === "download" && (
                  <Button
                    onClick={() => {
                      openDialog(false);
                      void download();
                    }}
                  >
                    重试下载
                  </Button>
                )}
                <Button variant="outline" onClick={() => openDialog(false)}>
                  关闭
                </Button>
              </>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {restartAsk && (
        <ConfirmDialog
          open
          title="现在重启安装更新？"
          message={restartAsk.message}
          confirmLabel="仍然重启"
          destructive
          onOpenChange={(open) => !open && setRestartAsk(null)}
          onConfirm={() => {
            const { proceed } = restartAsk;
            setRestartAsk(null);
            proceed();
          }}
        />
      )}
    </>
  );
}

const TONE_CLASS: Record<ReturnType<typeof sectionTone>, string> = {
  added: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  fixed: "bg-red-500/15 text-red-600 dark:text-red-400",
  improved: "bg-sky-500/15 text-sky-600 dark:text-sky-400",
  security: "bg-amber-500/15 text-amber-700 dark:text-amber-400",
  other: "bg-muted text-muted-foreground",
};

/** 更新说明：按"新增 / 改进 / 修复"分组、每组编号；认不出结构就显示原文。 */
function ReleaseNotes({ notes }: { notes: string }) {
  const text = notes.trim();
  const sections = text ? parseReleaseNotes(text) : null;
  return (
    <div
      data-testid="update-notes"
      className="max-h-[50vh] space-y-4 overflow-auto rounded-md border bg-muted/40 p-4 text-sm"
    >
      {!text ? (
        <p className="text-muted-foreground">这个版本没有写更新说明。</p>
      ) : !sections ? (
        <p className="whitespace-pre-wrap text-muted-foreground">{text}</p>
      ) : (
        sections.map((section, index) => (
          <section key={`${section.heading}-${index}`} className="space-y-2">
            {section.heading && (
              <span
                className={cn(
                  "inline-block rounded px-2 py-0.5 text-xs font-medium",
                  TONE_CLASS[sectionTone(section.heading)],
                )}
              >
                {section.heading}
              </span>
            )}
            <ol className="space-y-1.5">
              {section.items.map((item, itemIndex) => (
                <li key={itemIndex} className="flex gap-2 leading-relaxed">
                  <span className="w-5 shrink-0 text-right text-muted-foreground">{itemIndex + 1}.</span>
                  <span className="min-w-0">
                    {item.title && <span className="font-medium text-foreground">{item.title}：</span>}
                    <span className="text-muted-foreground">{item.text}</span>
                  </span>
                </li>
              ))}
            </ol>
          </section>
        ))
      )}
    </div>
  );
}
