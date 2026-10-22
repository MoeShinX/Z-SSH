//! 离线备份：把主机、凭据、代理、可同步的设置导出成一个用密码加密的文件，换电脑或不用云同步时
//! 手动搬。导入可以合并（两边都留）或替换（本机变成和文件一样），导入前会自动做一份本地备份。

import { useState } from "react";

import { pickBackupFile, pickBackupSavePath } from "../../../api/dialog";
import { syncOfflineExport, syncOfflineImport } from "../../../api/sync";
import { refreshLocalData, useSync } from "../../../stores/sync";
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
import { Label } from "../../ui/label";
import { SectionCard, SettingRow } from "../settingsUi";

/** 和后端 `MIN_PASSWORD_CHARS` 一致。 */
const MIN_PASSWORD = 8;

type Mode = "merge" | "replace";

function defaultFileName(): string {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  return `zssh-${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}.zsshbak`;
}

export default function OfflineBackupCard() {
  const apply = useSync((state) => state.apply);
  const [dialog, setDialog] = useState<
    { kind: "export"; path: string } | { kind: "import"; path: string } | null
  >(null);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [mode, setMode] = useState<Mode>("merge");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const open = (next: NonNullable<typeof dialog>) => {
    setPassword("");
    setConfirm("");
    setMode("merge");
    setError(null);
    setDialog(next);
  };

  async function startExport() {
    setMessage(null);
    const path = await pickBackupSavePath(defaultFileName());
    if (path) open({ kind: "export", path });
  }

  async function startImport() {
    setMessage(null);
    const path = await pickBackupFile();
    if (path) open({ kind: "import", path });
  }

  async function submit() {
    if (!dialog) return;
    setError(null);
    if (dialog.kind === "export") {
      if (password.length < MIN_PASSWORD) {
        setError(`密码至少 ${MIN_PASSWORD} 个字符`);
        return;
      }
      if (password !== confirm) {
        setError("两次输入的密码不一样");
        return;
      }
    }
    setBusy(true);
    try {
      if (dialog.kind === "export") {
        setMessage(await syncOfflineExport(dialog.path, password));
      } else {
        const result = await syncOfflineImport(dialog.path, password, mode);
        apply(result.status);
        setMessage(result.summary);
        void refreshLocalData();
      }
      setDialog(null);
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
      setPassword("");
      setConfirm("");
    }
  }

  return (
    <SectionCard
      title="离线备份文件"
      description="把主机、分组、凭据（含密码和私钥）、代理、可同步的设置和已信任的主机指纹导出成一个用密码加密的文件，可以拷到别的电脑导入。端口转发规则不在文件里。"
    >
      <SettingRow title="导出" description="选保存位置，再设一个密码。忘了密码文件就打不开了。">
        <Button size="sm" variant="outline" disabled={busy} onClick={() => void startExport()}>
          导出…
        </Button>
      </SettingRow>
      <SettingRow title="导入" description="导入之前会自动给当前状态做一份本地备份，导错了可以恢复回去。">
        <Button size="sm" variant="outline" disabled={busy} onClick={() => void startImport()}>
          导入…
        </Button>
      </SettingRow>
      {message && (
        <div className="mt-2 text-xs text-emerald-500" data-testid="offline-message">
          {message}
        </div>
      )}

      <Dialog open={dialog !== null} onOpenChange={(next) => !next && !busy && setDialog(null)}>
        <DialogContent className="sm:max-w-[440px]">
          <DialogHeader>
            <DialogTitle>{dialog?.kind === "export" ? "导出离线备份" : "导入离线备份"}</DialogTitle>
            <DialogDescription className="break-all font-mono text-xs">{dialog?.path}</DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label>{dialog?.kind === "export" ? "设置密码" : "文件密码"}</Label>
              <Input
                type="password"
                autoComplete="new-password"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                className="h-9"
              />
            </div>
            {dialog?.kind === "export" && (
              <div className="space-y-1.5">
                <Label>再输一次</Label>
                <Input
                  type="password"
                  autoComplete="new-password"
                  value={confirm}
                  onChange={(event) => setConfirm(event.target.value)}
                  className="h-9"
                />
              </div>
            )}
            {dialog?.kind === "import" && (
              <div className="space-y-2" role="radiogroup" aria-label="导入方式">
                {(
                  [
                    ["merge", "合并", "两边都留。同一条记录两边都改过时取较新的，另一份进冲突页。"],
                    ["replace", "替换", "本机变成和文件一模一样，文件里没有的主机、凭据会被删掉。"],
                  ] as const
                ).map(([value, title, hint]) => (
                  <label
                    key={value}
                    className="flex cursor-pointer items-start gap-2 rounded-md border border-border/60 px-3 py-2 text-sm"
                  >
                    <input
                      type="radio"
                      name="offline-import-mode"
                      className="mt-1"
                      checked={mode === value}
                      onChange={() => setMode(value)}
                    />
                    <span>
                      <span className="block">{title}</span>
                      <span className="block text-xs text-muted-foreground">{hint}</span>
                    </span>
                  </label>
                ))}
              </div>
            )}
            {error && (
              <div className="text-xs text-red-500" role="alert">
                {error}
              </div>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDialog(null)} disabled={busy}>
              取消
            </Button>
            <Button
              variant={dialog?.kind === "import" && mode === "replace" ? "destructive" : "default"}
              onClick={() => void submit()}
              disabled={busy || password === ""}
            >
              {busy ? "处理中…" : dialog?.kind === "export" ? "导出" : mode === "replace" ? "替换导入" : "合并导入"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </SectionCard>
  );
}
