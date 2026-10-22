// Adapted from Netcatty components/cloud-sync/CloudSyncDashboardTabs.tsx (status tab)
//! 同步：立即同步、上次结果、自动同步（变化后 30 秒 / 启动时 / 每 N 分钟，可关闭）、
//! 防误删待确认、基线损坏时的“重建同步基线”。失败只显示在这里和状态栏，不弹窗。

import { Loader2, RefreshCw } from "lucide-react";
import { useState } from "react";

import {
  syncAutoSet,
  syncNow,
  syncResetBaseline,
  type Decision,
  type SyncStatusView,
} from "../../../api/sync";
import { lastResultText } from "../../../state/syncModel";
import { useSync } from "../../../stores/sync";
import { useVault } from "../../../stores/vault";
import { Button } from "../../ui/button";
import { ConfirmDialog } from "../../ui/confirm-dialog";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../../ui/select";
import { Switch } from "../../ui/switch";
import { SectionCard, SettingRow } from "../settingsUi";
import { DeletionDialog } from "./SyncDialogs";

const INTERVALS = [5, 15, 30, 60];

export default function SyncCard({ status }: { status: SyncStatusView }) {
  const apply = useSync((state) => state.apply);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [resetting, setResetting] = useState(false);

  const ready = status.ready && status.password_in_memory;
  const running = busy || status.running;

  async function run(decision?: Decision) {
    setBusy(true);
    setError(null);
    try {
      const result = await syncNow(decision);
      apply(result.status_view);
      // 需要确认：弹对话框让用户选；其余结果显示在“上次同步”那一行。
      setDialogOpen(result.pending !== null);
      void useVault.getState().load();
    } catch (err: unknown) {
      setError(String(err));
      setDialogOpen(false);
    } finally {
      setBusy(false);
    }
  }

  async function choose(decision: Decision) {
    if (decision === "cancel") {
      setDialogOpen(false);
    }
    await run(decision);
  }

  async function setAuto(enabled: boolean, interval: number) {
    setError(null);
    try {
      apply(await syncAutoSet(enabled, interval));
    } catch (err: unknown) {
      setError(String(err));
    }
  }

  async function rebuild() {
    setResetting(false);
    setError(null);
    try {
      apply(await syncResetBaseline());
    } catch (err: unknown) {
      setError(String(err));
    }
  }

  const intervals = INTERVALS.includes(status.auto.interval_minutes)
    ? INTERVALS
    : [...INTERVALS, status.auto.interval_minutes].sort((a, b) => a - b);

  return (
    <SectionCard title="同步" description="手动同步，或者让它在后台自动做。">
      <SettingRow
        title="立即同步"
        description={lastResultText(status)}
      >
        <Button
          size="sm"
          onClick={() => void run()}
          disabled={running || !ready || status.baseline_broken}
          data-testid="sync-now"
        >
          {running ? (
            <Loader2 size={14} className="animate-spin" />
          ) : (
            <RefreshCw size={14} />
          )}
          {running ? "同步中…" : "立即同步"}
        </Button>
      </SettingRow>

      {status.last?.status === "error" && (
        <div className="py-1 text-xs text-red-500" role="alert" data-testid="sync-last-error">
          上次同步失败：{status.last.summary}
        </div>
      )}

      {status.pending && (
        <div
          className="my-2 flex items-center justify-between gap-3 rounded-md border border-amber-500/50 bg-amber-500/10 p-3 text-xs"
          data-testid="sync-pending"
        >
          <span>
            {status.pending.auto ? "自动同步已暂停：" : ""}
            {status.pending.summary}。需要你来决定。
          </span>
          <Button size="sm" variant="outline" onClick={() => setDialogOpen(true)}>
            处理
          </Button>
        </div>
      )}

      {status.baseline_broken && (
        <div
          className="my-2 space-y-2 rounded-md border border-red-500/50 bg-red-500/10 p-3 text-xs"
          role="alert"
          data-testid="sync-baseline-broken"
        >
          <div className="font-medium text-red-500">同步基线损坏，暂停同步</div>
          <div>
            基线是“上次同步成功时的状态”，用来判断哪边改过什么。重建后下一次同步按首次同步处理：
            <strong>两边的数据都会保留</strong>，同名的不会去重（可能出现重复的主机，之后手动删就行）；
            重建之前在另一边删掉的记录可能会回来。
          </div>
          <Button size="sm" variant="outline" onClick={() => setResetting(true)}>
            重建同步基线…
          </Button>
        </div>
      )}

      <SettingRow
        title="自动同步"
        description="配置变化 30 秒后、应用启动时、以及每隔一段时间自动同步一次。遇到需要确认的情况会停下来等你。"
      >
        <Switch
          checked={status.auto.enabled}
          onCheckedChange={(enabled) => void setAuto(enabled, status.auto.interval_minutes)}
          aria-label="自动同步"
        />
      </SettingRow>
      <SettingRow title="定时同步间隔" description="只在开启自动同步时有效。">
        <Select
          value={String(status.auto.interval_minutes)}
          onValueChange={(value) => void setAuto(status.auto.enabled, Number(value))}
          disabled={!status.auto.enabled}
        >
          <SelectTrigger className="h-9 w-40" aria-label="定时同步间隔">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {intervals.map((minutes) => (
              <SelectItem key={minutes} value={String(minutes)}>
                每 {minutes} 分钟
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingRow>

      {error && (
        <div className="mt-2 text-xs text-red-500" role="alert" data-testid="sync-error">
          {error}
        </div>
      )}

      <DeletionDialog
        pending={dialogOpen ? status.pending : null}
        busy={busy}
        onChoose={(decision) => void choose(decision)}
        onClose={() => setDialogOpen(false)}
      />
      <ConfirmDialog
        open={resetting}
        title="重建同步基线？"
        message={
          "下一次同步会按首次同步处理：本机和云端的数据都会保留并合并，同名的记录不会去重。\n" +
          "重建之前在另一边删掉的记录可能会回来。"
        }
        confirmLabel="重建"
        onOpenChange={setResetting}
        onConfirm={() => void rebuild()}
      />
    </SectionCard>
  );
}
