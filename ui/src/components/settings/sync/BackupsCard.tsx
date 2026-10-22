// Adapted from Netcatty components/cloud-sync/CloudSyncLocalBackupsPanel.tsx
//! 本地备份：每次从云端合并之前、每次恢复之前、版本升级后第一次启动时，会在本机留一份
//! 加密备份。这里列出来，可以恢复到任意一份（恢复前会再自动备份当前状态）。

import { useCallback, useEffect, useState } from "react";

import {
  syncBackupKeepSet,
  syncBackupRestore,
  syncBackupsList,
  type BackupDetail,
} from "../../../api/sync";
import { formatStamp } from "../../../state/syncModel";
import { refreshLocalData, useSync } from "../../../stores/sync";
import { Button } from "../../ui/button";
import { ConfirmDialog } from "../../ui/confirm-dialog";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../../ui/select";
import { SectionCard, SettingRow } from "../settingsUi";

/** 保留份数的可选值（后端接受 1～200）。 */
const KEEP_CHOICES = [5, 10, 20, 50, 100, 200];

export default function BackupsCard() {
  const apply = useSync((state) => state.apply);
  const lastAt = useSync((state) => state.status?.last?.at);
  const keep = useSync((state) => state.status?.backup_keep ?? 20);
  const [items, setItems] = useState<BackupDetail[] | null>(null);
  const [target, setTarget] = useState<BackupDetail | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setItems(await syncBackupsList());
    } catch (err: unknown) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload, lastAt]);

  async function changeKeep(value: number) {
    setError(null);
    try {
      apply(await syncBackupKeepSet(value));
    } catch (err: unknown) {
      setError(String(err));
    }
  }

  async function restore() {
    const backup = target;
    setTarget(null);
    if (!backup) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const result = await syncBackupRestore(backup.path);
      apply(result.status);
      setMessage(result.summary);
      void refreshLocalData();
      await reload();
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <SectionCard
      title="本地备份"
      description="用 Windows 账户加密保存在本机。每次从云端合并之前、每次恢复之前、版本升级后第一次启动时（库里有数据才备份）会自动留一份。恢复之前会先自动备份当前状态。"
    >
      <SettingRow
        title="最多保留份数"
        description="超过的最早的备份会被清理。调小之后，多出来的旧备份在下一次备份时才删，改这个设置本身不会删任何东西。"
      >
        <Select value={String(keep)} onValueChange={(value) => void changeKeep(Number(value))}>
          <SelectTrigger className="h-9 w-32" aria-label="备份保留份数">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {(KEEP_CHOICES.includes(keep) ? KEEP_CHOICES : [...KEEP_CHOICES, keep].sort((a, b) => a - b)).map(
              (count) => (
                <SelectItem key={count} value={String(count)}>
                  {count} 份
                </SelectItem>
              ),
            )}
          </SelectContent>
        </Select>
      </SettingRow>
      {items === null ? (
        <div className="py-2 text-xs text-muted-foreground">读取中…</div>
      ) : items.length === 0 ? (
        <div className="py-2 text-xs text-muted-foreground" data-testid="backups-empty">
          还没有备份。第一次从云端合并之前会自动做一份。
        </div>
      ) : (
        <ul className="divide-y divide-border/40" data-testid="backups-list">
          {items.map((item) => (
            <li
              key={item.path}
              className="flex items-center justify-between gap-3 py-2.5 text-[13px]"
              data-testid="backup-row"
            >
              <div className="min-w-0">
                <div>
                  {item.created_at !== null ? formatStamp(item.created_at) : item.stamp}
                </div>
                <div className="text-xs text-muted-foreground">
                  {item.readable
                    ? `应用版本 ${item.app_version ?? "未知"} · ${item.record_count ?? 0} 条记录（主机 ${
                        item.host_count ?? 0
                      } 台）`
                    : "这份备份读不了（可能是换了 Windows 用户保存的）"}
                </div>
              </div>
              <Button
                size="sm"
                variant="outline"
                disabled={busy || !item.readable}
                onClick={() => setTarget(item)}
              >
                恢复
              </Button>
            </li>
          ))}
        </ul>
      )}
      {message && (
        <div className="mt-2 text-xs text-emerald-500" data-testid="backup-restored">
          {message}
        </div>
      )}
      {error && (
        <div className="mt-2 text-xs text-red-500" role="alert" data-testid="backups-error">
          {error}
        </div>
      )}
      <ConfirmDialog
        open={target !== null}
        title="恢复到这份备份？"
        message={
          target
            ? `会把主机、凭据、代理和设置恢复到 ${
                target.created_at !== null ? formatStamp(target.created_at) : target.stamp
              } 的状态。恢复前会先自动备份当前状态。`
            : undefined
        }
        confirmLabel="恢复"
        destructive
        onOpenChange={(open) => !open && setTarget(null)}
        onConfirm={() => void restore()}
      />
    </SectionCard>
  );
}
