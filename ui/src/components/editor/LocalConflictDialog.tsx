//! 本地文件保存冲突时的对话框（第 7 节）：重新载入 / 覆盖 / 另存为 / 取消。
//!
//! 和远端冲突（`SftpEditConflictDialog`，由后端的 `sftp_edit_event` 驱动）不同：本地文件保存是同步
//! 返回 `conflict`，对话框由编辑器标签自己弹；这些按钮都在这个标签里处理。

import { useState } from "react";

import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";
import { Input } from "../ui/input";

export interface LocalConflictDialogProps {
  /** 文件名（标题里显示）。 */
  fileName: string;
  /** 「另存为」输入框的默认名字。 */
  suggestedName: string;
  /** 上一次操作失败的原因（留在对话框里显示）。 */
  error: string | null;
  /** 重新载入（丢掉我的修改）。 */
  onReload: () => void;
  /** 覆盖磁盘上的文件（force=true）。 */
  onOverwrite: () => void;
  /** 另存为同目录下的新名字。 */
  onSaveAs: (name: string) => void;
  onCancel: () => void;
}

export function LocalConflictDialog({
  fileName,
  suggestedName,
  error,
  onReload,
  onOverwrite,
  onSaveAs,
  onCancel,
}: LocalConflictDialogProps) {
  const [saveAs, setSaveAs] = useState(suggestedName);
  const name = saveAs.trim();
  const nameOk =
    name !== "" && !name.includes("/") && !name.includes("\\") && name !== "." && name !== "..";
  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="sm:max-w-[520px]" data-section="edit-local-conflict">
        <DialogHeader>
          <DialogTitle>本地文件已经被别的地方改过</DialogTitle>
          <DialogDescription className="break-all">{fileName}</DialogDescription>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          这个文件在你打开之后被别的程序改过（或者被删除 / 移走了），所以这次保存没有写。要怎么办？
        </p>
        <div className="space-y-1">
          <label className="text-xs text-muted-foreground">另存为（同一目录下的新文件）</label>
          <Input
            value={saveAs}
            onChange={(event) => setSaveAs(event.target.value)}
            onFocus={(event) => event.target.select()}
          />
          {!nameOk && name !== "" && (
            <p className="text-xs text-destructive">名字不能为空，也不能含有路径分隔符</p>
          )}
        </div>
        {error && (
          <p className="text-xs text-destructive" data-testid="local-conflict-error">
            {error}
          </p>
        )}
        <DialogFooter className="flex-wrap gap-2 sm:justify-between">
          <Button variant="ghost" onClick={onCancel}>
            取消
          </Button>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={onReload}>
              重新载入（丢掉我的修改）
            </Button>
            <Button
              variant="outline"
              disabled={!nameOk}
              onClick={() => onSaveAs(name)}
            >
              另存为
            </Button>
            <Button variant="destructive" onClick={onOverwrite}>
              覆盖
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export default LocalConflictDialog;
