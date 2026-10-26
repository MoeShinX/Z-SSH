// Adapted from Netcatty components/ui/confirm-dialog.tsx
import type { ReactNode } from "react";

import { Button } from "./button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "./dialog";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message?: ReactNode;
  confirmLabel: string;
  busy?: boolean;
  destructive?: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
  /**
   * 弹窗关掉之后焦点去哪：不给就按 Radix 的默认（实测常常落到 body 上）。要把焦点还给
   * 某个输入区（比如终端）的，在这里 `preventDefault()` 再自己 focus——弹窗还开着时 focus
   * 外面的元素会被焦点陷阱拉回来，所以不能在 onConfirm 里做。
   */
  onCloseAutoFocus?: (event: Event) => void;
}

/** 二次确认弹窗（删除主机、删凭据这类操作用）。 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel,
  busy = false,
  destructive = false,
  onOpenChange,
  onConfirm,
  onCloseAutoFocus,
}: ConfirmDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-w-[calc(100vw-2rem)] overflow-hidden sm:max-w-[380px]"
        onCloseAutoFocus={onCloseAutoFocus}
      >
        <DialogHeader className="min-w-0 pr-6">
          <DialogTitle className="truncate">{title}</DialogTitle>
        </DialogHeader>

        {message ? (
          <div className="min-w-0 whitespace-pre-wrap break-words text-sm text-muted-foreground [overflow-wrap:anywhere]">
            {message}
          </div>
        ) : null}

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={busy}
          >
            取消
          </Button>
          <Button
            type="button"
            variant={destructive ? "destructive" : "default"}
            onClick={onConfirm}
            disabled={busy}
          >
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
