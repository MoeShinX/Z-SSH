//! 属性对话框（M3-5 第 4 节）：文件 / 文件夹 / 多选 / 当前目录的信息，文件夹可以递归「计算大小」，
//! 计算中随时能取消，关掉对话框也会取消。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { onSftpSize, sftpSizeCancel, sftpSizeStart } from "../../api/sftp";
import {
  applySizeEvent,
  bytesText,
  calcText,
  canCalculate,
  IDLE_CALC,
  infoRows,
  sumTotals,
  type CalcState,
  type PropertiesTarget,
} from "../../state/sftpProperties";
import { Button } from "../ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "../ui/dialog";

interface Props {
  sftpId: string;
  side: "local" | "remote";
  targets: PropertiesTarget[];
  onClose: () => void;
}

let nextSizeRequest = 0;

/**
 * 每点一次「计算大小」给每个文件夹生成一个关联 ID。进度事件在 `sftp_size_start`
 * 返回任务号之前就可能到，只靠任务号认领不了（同时算两个文件夹时会串号）。
 */
function nextSizeRequestId(): string {
  nextSizeRequest += 1;
  return `size-${nextSizeRequest}`;
}

export function SftpPropertiesDialog({ sftpId, side, targets, onClose }: Props) {
  const [calcs, setCalcs] = useState<Record<string, CalcState>>({});
  const calcsRef = useRef(calcs);
  calcsRef.current = calcs;
  const foldersToCount = useMemo(() => targets.filter(canCalculate), [targets]);

  useEffect(() => {
    let stop: (() => void) | undefined;
    let cancelled = false;
    void onSftpSize((id, event) => {
      if (id !== sftpId) return;
      setCalcs((current) => {
        const next: Record<string, CalcState> = {};
        for (const [path, state] of Object.entries(current)) {
          next[path] = applySizeEvent(state, event);
        }
        return next;
      });
    })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [sftpId]);

  // 关掉对话框 = 取消还在跑的计算。
  useEffect(
    () => () => {
      for (const state of Object.values(calcsRef.current)) {
        if (state.phase === "running" && state.calcId !== undefined) {
          void sftpSizeCancel(sftpId, state.calcId).catch(() => {});
        }
      }
    },
    [sftpId],
  );

  const start = useCallback(() => {
    for (const target of foldersToCount) {
      const requestId = nextSizeRequestId();
      setCalcs((current) => ({
        ...current,
        [target.path]: { phase: "running", requestId, totals: IDLE_CALC.totals },
      }));
      void sftpSizeStart(sftpId, side, target.path, requestId)
        .then((calcId) =>
          setCalcs((current) => {
            const state = current[target.path];
            // 事件可能比命令的返回值先到：已经有结果就别覆盖。也要认准自己这个关联 ID
            //（重新点过「计算大小」时，上一次请求的返回值不能改这次的状态）。
            if (!state || state.requestId !== requestId || state.calcId !== undefined) return current;
            return { ...current, [target.path]: { ...state, calcId } };
          }),
        )
        .catch((error) =>
          setCalcs((current) => {
            const state = current[target.path];
            if (!state || state.requestId !== requestId) return current;
            return {
              ...current,
              [target.path]: { phase: "failed", totals: IDLE_CALC.totals, error: String(error) },
            };
          }),
        );
    }
  }, [foldersToCount, sftpId, side]);

  const cancel = useCallback(() => {
    for (const state of Object.values(calcsRef.current)) {
      if (state.phase === "running" && state.calcId !== undefined) {
        void sftpSizeCancel(sftpId, state.calcId).catch(() => {});
      }
    }
  }, [sftpId]);

  const states = foldersToCount.map((target) => calcs[target.path] ?? IDLE_CALC);
  const running = states.some((state) => state.phase === "running");
  const aggregate: CalcState = {
    phase: running
      ? "running"
      : states.some((state) => state.phase === "failed")
        ? "failed"
        : states.some((state) => state.phase === "cancelled")
          ? "cancelled"
          : states.length > 0 && states.every((state) => state.phase === "done")
            ? "done"
            : "idle",
    totals: sumTotals(states.map((state) => state.totals)),
    error: states.find((state) => state.error)?.error,
  };

  const single = targets.length === 1 ? targets[0] : null;
  const fileBytes = targets
    .filter((target) => target.kind === "file")
    .reduce((sum, target) => sum + (target.size ?? 0), 0);

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-[480px]" data-section="sftp-properties">
        <DialogHeader>
          <DialogTitle>
            {single ? `${single.name} 属性` : `${targets.length} 项的属性`}
          </DialogTitle>
        </DialogHeader>
        <dl className="grid grid-cols-[88px_1fr] gap-x-3 gap-y-1.5 text-sm">
          {single ? (
            infoRows(single, side).map((row) => (
              <div key={row.label} className="contents">
                <dt className="text-muted-foreground">{row.label}</dt>
                <dd className="break-all" data-prop={row.label}>
                  {row.value}
                </dd>
              </div>
            ))
          ) : (
            <div className="contents">
              <dt className="text-muted-foreground">选中</dt>
              <dd data-prop="选中">
                {targets.length} 项（{targets.filter((target) => target.kind === "dir").length} 个文件夹，
                {targets.filter((target) => target.kind === "file").length} 个文件）
              </dd>
            </div>
          )}
          {!single && fileBytes > 0 && (
            <div className="contents">
              <dt className="text-muted-foreground">文件大小</dt>
              <dd data-prop="文件大小">{bytesText(fileBytes)}</dd>
            </div>
          )}
          {foldersToCount.length > 0 && (
            <div className="contents">
              <dt className="text-muted-foreground">{single ? "包含" : "文件夹合计"}</dt>
              <dd data-prop="计算结果" data-phase={aggregate.phase} role="status">
                {calcText(aggregate)}
              </dd>
            </div>
          )}
        </dl>
        <DialogFooter>
          {foldersToCount.length > 0 &&
            (running ? (
              <Button type="button" variant="outline" data-action="size-cancel" onClick={cancel}>
                取消计算
              </Button>
            ) : (
              <Button type="button" variant="outline" data-action="size-start" onClick={start}>
                {aggregate.phase === "idle" ? "计算大小" : "重新计算"}
              </Button>
            ))}
          <Button type="button" data-action="properties-close" onClick={onClose}>
            关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
