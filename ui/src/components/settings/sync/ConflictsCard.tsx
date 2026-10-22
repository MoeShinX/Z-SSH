// Adapted from Netcatty components/cloud-sync/CloudSyncDialogs.tsx (ConflictModal)
//! 冲突列表：两边都改了同一条记录、或一边删了另一边改了时，没被采用的那份留在这里。
//! 可以“改用另一份”（删除类的就是“恢复这条记录”）、或者“我知道了”清掉；
//! 超过 90 天的冲突会自动清理。

import { useCallback, useEffect, useState } from "react";

import {
  syncConflictDismiss,
  syncConflictsList,
  syncConflictUseOther,
  type ConflictView,
  type SyncStatusView,
} from "../../../api/sync";
import { entityLabel, formatStamp, keptText, otherText } from "../../../state/syncModel";
import { refreshLocalData, useSync } from "../../../stores/sync";
import { Button } from "../../ui/button";
import { SectionCard } from "../settingsUi";

export default function ConflictsCard({ status }: { status: SyncStatusView }) {
  const apply = useSync((state) => state.apply);
  const [items, setItems] = useState<ConflictView[] | null>(null);
  const [busyId, setBusyId] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setItems(await syncConflictsList());
    } catch (err: unknown) {
      setError(String(err));
    }
  }, []);

  // 条数变了（同步产生了新冲突 / 处理掉了一条）就重新读。
  useEffect(() => {
    void reload();
  }, [reload, status.conflict_count, status.last?.at]);

  async function act(id: number, action: "use" | "dismiss") {
    setBusyId(id);
    setError(null);
    try {
      apply(
        action === "use" ? await syncConflictUseOther(id) : await syncConflictDismiss(id),
      );
      if (action === "use") void refreshLocalData();
      await reload();
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <SectionCard
      title="冲突"
      description="两台电脑改了同一条记录时，默认采用修改时间较新的那份；一边删除、另一边修改时，删除生效。没被采用的那份留在这里，保留 90 天后自动清理。"
    >
      {items === null ? (
        <div className="py-2 text-xs text-muted-foreground">读取中…</div>
      ) : items.length === 0 ? (
        <div className="py-2 text-xs text-muted-foreground" data-testid="conflicts-empty">
          没有冲突。
        </div>
      ) : (
        <ul className="divide-y divide-border/40" data-testid="conflicts-list">
          {items.map((item) => (
            <li key={item.id} className="space-y-1.5 py-3 text-[13px]" data-testid="conflict-row">
              <div className="flex items-baseline justify-between gap-3">
                <div className="min-w-0">
                  <span className="mr-2 rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
                    {entityLabel(item.entity)}
                  </span>
                  <span className="break-all font-medium">{item.name}</span>
                </div>
                <span className="shrink-0 text-xs text-muted-foreground">
                  {formatStamp(item.at)}
                </span>
              </div>
              <div className="text-xs text-muted-foreground">
                <div>保留：{keptText(item)}</div>
                <div>另一份：{otherText(item)}</div>
              </div>
              {/* 不能一键改用另一份时（例如主机指纹）说明原因，免得用户以为按钮丢了。 */}
              {!item.can_use_other && item.message && (
                <div className="text-xs text-muted-foreground" data-testid="conflict-note">
                  {item.message}
                </div>
              )}
              <div className="flex gap-2">
                {item.can_use_other && (
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={busyId === item.id}
                    onClick={() => void act(item.id, "use")}
                  >
                    {item.deletion ? "恢复这条记录" : "改用另一份"}
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busyId === item.id}
                  onClick={() => void act(item.id, "dismiss")}
                >
                  我知道了
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
      {error && (
        <div className="mt-2 text-xs text-red-500" role="alert" data-testid="conflicts-error">
          {error}
        </div>
      )}
    </SectionCard>
  );
}
