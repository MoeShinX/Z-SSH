//! 关于页里的更新下载进度条。以前下载时只写一句"正在下载更新…"，进度只在状态栏底边
//! 一条 2px 的细线上，用户注意不到；这里给一条明显的进度条，文字写百分比和大小。

import type { UpdateStatus } from "../../api/updater";
import { downloadProgress } from "../../state/updateModel";
import { cn } from "../../lib/utils";

export default function UpdateProgress({ status }: { status: UpdateStatus | null }) {
  const progress = downloadProgress(status);
  if (!progress) return null;
  const { percent, text } = progress;
  return (
    <div data-testid="update-progress" className="space-y-1.5 pb-1">
      <div
        role="progressbar"
        aria-label="更新下载进度"
        aria-valuemin={0}
        aria-valuemax={100}
        // 不知道总大小时不给 aria-valuenow，读屏按"进行中、进度未知"处理
        aria-valuenow={percent ?? undefined}
        aria-valuetext={text}
        className="h-2 w-full overflow-hidden rounded-full bg-muted"
      >
        <div
          data-indeterminate={percent === null ? "true" : undefined}
          className={cn(
            "h-full rounded-full bg-primary transition-[width] duration-200",
            percent === null && "animate-pulse opacity-60",
          )}
          style={{ width: `${percent ?? 100}%` }}
        />
      </div>
      <p className="text-xs tabular-nums text-muted-foreground">{text}</p>
    </div>
  );
}
