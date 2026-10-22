//! 启动时的提示：上次有文件的修改没有上传（程序崩溃或被强行关掉）。每一项可以
//! 重新上传、另存到本地、放弃（F05）。

import { useState } from "react";

import type { EditLeftover, EditLeftoverResult } from "../../api/sftp";
import {
  type SftpSessionInfo,
  draftNote,
  recoveryLine,
  recoveryTitle,
  reuploadLabel,
  reuploadState,
  sftpSessionFor,
  uncertainNote,
} from "../../state/editRecovery";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";

export type RecoveryAction =
  "discard" | "save_local" | "reupload" | "reupload_overwrite";

export interface EditRecoveryDialogProps {
  items: EditLeftover[];
  /** 现在在用的 SFTP 会话（标签、文件侧栏）：重新上传可以用这台主机上任何一个已连好的。 */
  sessions: ReadonlyArray<SftpSessionInfo>;
  /** 做一个动作；返回结果（界面据此显示冲突 / 失败原因）。 */
  onAction: (
    item: EditLeftover,
    action: RecoveryAction,
  ) => Promise<EditLeftoverResult>;
  onLater: () => void;
}

export function EditRecoveryDialog({
  items,
  sessions,
  onAction,
  onLater,
}: EditRecoveryDialogProps) {
  const [busy, setBusy] = useState<string | null>(null);
  const [notes, setNotes] = useState<Record<string, EditLeftoverResult>>({});
  const [confirmDiscard, setConfirmDiscard] = useState<string | null>(null);

  async function run(item: EditLeftover, action: RecoveryAction) {
    setBusy(item.id);
    try {
      const result = await onAction(item, action);
      setNotes((current) => ({ ...current, [item.id]: result }));
    } finally {
      setBusy(null);
      setConfirmDiscard(null);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onLater()}>
      <DialogContent className="sm:max-w-[640px]" data-section="edit-recovery">
        <DialogHeader>
          <DialogTitle>{recoveryTitle(items.length)}</DialogTitle>
          <DialogDescription>
            程序上次没有正常结束，这些文件在本地改过，但没来得及传到服务器。它们还保存着，选一种处理方式。
          </DialogDescription>
        </DialogHeader>
        <ul className="max-h-[50vh] space-y-3 overflow-auto">
          {items.map((item) => {
            const reupload = reuploadState(
              item,
              sftpSessionFor(sessions, item.host_id),
            );
            const note = notes[item.id];
            const disabled = busy !== null;
            return (
              <li
                key={item.id}
                className="rounded-md border p-3"
                data-recovery-item={item.id}
              >
                <p className="break-all text-sm font-medium">
                  {recoveryLine(item)}
                </p>
                {draftNote(item) && (
                  <p className="mt-1 text-xs text-muted-foreground" data-recovery-draft>
                    {draftNote(item)}
                  </p>
                )}
                {uncertainNote(item) && (
                  <p
                    className="mt-1 text-xs text-amber-500"
                    data-recovery-uncertain
                  >
                    {uncertainNote(item)}
                  </p>
                )}
                {note && note.state !== "done" && (
                  <p className="mt-1 text-xs text-destructive" role="alert">
                    {note.message}
                  </p>
                )}
                {!reupload.enabled && reupload.hint && (
                  <p className="mt-1 text-xs text-muted-foreground">
                    {reupload.hint}
                  </p>
                )}
                <div className="mt-2 flex flex-wrap gap-2">
                  <Button
                    size="sm"
                    disabled={disabled || !reupload.enabled}
                    onClick={() => void run(item, "reupload")}
                  >
                    {reuploadLabel(item)}
                  </Button>
                  {note?.state === "conflict" && (
                    <Button
                      size="sm"
                      variant="destructive"
                      disabled={disabled}
                      onClick={() => void run(item, "reupload_overwrite")}
                    >
                      覆盖服务器上的文件
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={disabled}
                    onClick={() => void run(item, "save_local")}
                  >
                    另存到本地
                  </Button>
                  {confirmDiscard === item.id ? (
                    <Button
                      size="sm"
                      variant="destructive"
                      disabled={disabled}
                      onClick={() => void run(item, "discard")}
                    >
                      确定放弃这份修改
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={disabled}
                      onClick={() => setConfirmDiscard(item.id)}
                    >
                      放弃
                    </Button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
        <DialogFooter>
          <Button variant="ghost" onClick={onLater}>
            稍后处理
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export default EditRecoveryDialog;
