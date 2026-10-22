// Adapted from Netcatty components/cloud-sync/CloudSyncControls.tsx (StatusDot)
//! 底部的同步状态条：连接了存储才出现。失败原因只在这里和同步页显示，不弹窗；
//! 点一下打开设置页的“同步”一节。

import { statusBarModel } from "../../state/syncModel";
import { useSync } from "../../stores/sync";
import { cn } from "../../lib/utils";

const DOT: Record<string, string> = {
  idle: "bg-muted-foreground/60",
  ok: "bg-emerald-500",
  busy: "bg-sky-500 animate-pulse",
  warn: "bg-amber-500",
  error: "bg-red-500",
};

export default function SyncStatusBar({ onOpen }: { onOpen: () => void }) {
  const status = useSync((state) => state.status);
  const requestSection = useSync((state) => state.requestSettingsSection);
  const model = statusBarModel(status);
  if (!model.visible) return null;
  return (
    <button
      type="button"
      data-testid="sync-status-bar"
      data-tone={model.tone}
      title={model.text}
      onClick={() => {
        requestSection(true);
        onOpen();
      }}
      className="flex h-6 w-full shrink-0 cursor-pointer items-center gap-2 border-t border-border/60 bg-background px-3 text-left text-xs text-muted-foreground hover:bg-accent"
    >
      <span className={cn("inline-block size-2 shrink-0 rounded-full", DOT[model.tone])} />
      <span className="truncate">{model.text}</span>
    </button>
  );
}
