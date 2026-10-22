// Adapted from Netcatty components/cloud-sync/CloudSyncDialogs.tsx
//! 同步相关的对话框：防误删确认（继续 / 用云端覆盖本地 / 取消）、修改主密码。

import { Loader2 } from "lucide-react";
import { useState } from "react";

import { syncChangePassword, type PendingConfirmation } from "../../../api/sync";
import { deletionLines } from "../../../state/syncModel";
import { useSync } from "../../../stores/sync";
import { Button } from "../../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../../ui/dialog";
import { Input } from "../../ui/input";
import { StrengthBar } from "./StrengthBar";

/**
 * 防误删确认：写清楚会删掉多少、前几条是什么，三个选择。
 * 自动同步遇到这种情况时**不会**自己弹出来，也不会自己选，而是停在状态栏提示；
 * 用户打开同步页点“处理”才会看到这个对话框。
 */
export function DeletionDialog({
  pending,
  busy,
  onChoose,
  onClose,
}: {
  pending: PendingConfirmation | null;
  busy: boolean;
  onChoose: (decision: "continue" | "use_cloud" | "cancel") => void;
  onClose: () => void;
}) {
  const lines = pending ? deletionLines(pending) : null;
  return (
    <Dialog open={pending !== null} onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="sm:max-w-[460px]" data-testid="deletion-dialog">
        <DialogHeader>
          <DialogTitle>这次同步会删除很多记录</DialogTitle>
          <DialogDescription>
            {pending?.auto ? "自动同步在这里停下了，没有改动任何数据。" : "还没有改动任何数据。"}
          </DialogDescription>
        </DialogHeader>
        {lines && (
          <div className="space-y-2 text-sm">
            <p>{lines.headline}</p>
            <ul className="max-h-40 list-disc space-y-0.5 overflow-auto pl-5 text-xs text-muted-foreground">
              {lines.names.map((name) => (
                <li key={name}>{name}</li>
              ))}
              {lines.more > 0 && <li>……还有 {lines.more} 条</li>}
            </ul>
            <p className="text-xs text-muted-foreground">
              · 继续：按合并结果删除这些记录（删除会同步到云端）。
              <br />· 用云端版本覆盖本地：放弃本地的版本，以云端为准。
              <br />· 取消：本地和云端都不动。
            </p>
          </div>
        )}
        <DialogFooter className="gap-2 sm:justify-between">
          <Button variant="outline" disabled={busy} onClick={() => onChoose("cancel")}>
            取消
          </Button>
          <div className="flex gap-2">
            <Button variant="secondary" disabled={busy} onClick={() => onChoose("use_cloud")}>
              用云端版本覆盖本地
            </Button>
            <Button variant="destructive" disabled={busy} onClick={() => onChoose("continue")}>
              {busy && <Loader2 size={14} className="animate-spin" />}继续
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function ChangePasswordDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const apply = useSync((state) => state.apply);
  const [oldPassword, setOldPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const mismatch = confirm !== "" && confirm !== newPassword;
  const valid = oldPassword !== "" && newPassword.length >= 8 && newPassword === confirm;

  function close(next: boolean) {
    if (busy) return;
    if (!next) {
      setOldPassword("");
      setNewPassword("");
      setConfirm("");
      setError(null);
    }
    onOpenChange(next);
  }

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      apply(await syncChangePassword(oldPassword, newPassword));
      close(false);
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="sm:max-w-[420px]" data-testid="change-password-dialog">
        <DialogHeader>
          <DialogTitle>修改主密码</DialogTitle>
          <DialogDescription>
            会用新密码重新加密云端数据。忘记主密码就无法恢复云端数据。
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <Input
            type="password"
            value={oldPassword}
            onChange={(e) => setOldPassword(e.target.value)}
            placeholder="旧主密码"
            aria-label="旧主密码"
            autoComplete="current-password"
          />
          <Input
            type="password"
            value={newPassword}
            onChange={(e) => setNewPassword(e.target.value)}
            placeholder="新主密码（至少 8 个字符）"
            aria-label="新主密码"
            autoComplete="new-password"
          />
          <StrengthBar password={newPassword} />
          <Input
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            placeholder="再输入一次新主密码"
            aria-label="确认新主密码"
            autoComplete="new-password"
          />
          {mismatch && <div className="text-xs text-red-500">两次输入的新主密码不一致</div>}
          {error && (
            <div className="text-xs text-red-500" role="alert">
              {error}
            </div>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={() => close(false)}>
            取消
          </Button>
          <Button disabled={busy || !valid} onClick={() => void submit()}>
            {busy && <Loader2 size={14} className="animate-spin" />}修改并上传
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
