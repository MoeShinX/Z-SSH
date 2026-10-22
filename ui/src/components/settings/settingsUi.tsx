// Adapted from Netcatty components/settings/settings-ui.tsx
//! 设置页共用的两个小积木：一组设置（标题 + 若干行）和一行设置（左说明右控件）。

import { cn } from "../../lib/utils";
import { Card } from "../ui/card";

/** 一组设置：标题 + 若干行。 */
export function SectionCard({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <Card className="p-4">
      <h3 className="text-sm font-semibold">{title}</h3>
      {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      <div className="mt-2">{children}</div>
    </Card>
  );
}

/** 一行设置：左边说明，右边控件。 */
export function SettingRow({
  title,
  description,
  children,
  stacked = false,
}: {
  title: string;
  description?: string;
  children: React.ReactNode;
  stacked?: boolean;
}) {
  return (
    <div
      className={cn(
        "gap-3 border-b border-border/40 py-3 last:border-0",
        stacked ? "flex flex-col" : "flex items-center justify-between gap-6",
      )}
    >
      <div className="min-w-0">
        <div className="text-[13px]">{title}</div>
        {description && (
          <div className="mt-0.5 text-xs text-muted-foreground">{description}</div>
        )}
      </div>
      <div className={cn("shrink-0", stacked && "w-full")}>{children}</div>
    </div>
  );
}
