//! 关标签 / 关连接 / 退出时对未保存文件的三选一询问：保存 / 不保存 / 取消（M3-5 第 2 节）。

import { Button } from "../ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "../ui/dialog";

interface Props {
  title: string;
  message: string;
  busy: boolean;
  error: string | null;
  onSave: () => void;
  onDiscard: () => void;
  onCancel: () => void;
}

export function UnsavedDialog({ title, message, busy, error, onSave, onDiscard, onCancel }: Props) {
  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onCancel()}>
      <DialogContent className="max-w-[calc(100vw-2rem)] overflow-hidden sm:max-w-[420px]">
        <DialogHeader className="min-w-0 pr-6">
          <DialogTitle className="truncate">{title}</DialogTitle>
        </DialogHeader>
        <div className="min-w-0 whitespace-pre-wrap break-words text-sm text-muted-foreground [overflow-wrap:anywhere]">
          {message}
        </div>
        {error && (
          <div role="alert" className="text-sm text-rose-400">
            {error}
          </div>
        )}
        <DialogFooter>
          <Button type="button" variant="outline" data-action="unsaved-cancel" onClick={onCancel} disabled={busy}>
            取消
          </Button>
          <Button type="button" variant="destructive" data-action="unsaved-discard" onClick={onDiscard} disabled={busy}>
            不保存
          </Button>
          <Button type="button" data-action="unsaved-save" onClick={onSave} disabled={busy}>
            {busy ? "保存中…" : "保存"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
