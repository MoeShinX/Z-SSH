//! 主密码强度条（只是提醒，不是门槛）。

import { passwordStrength } from "../../../state/syncModel";
import { cn } from "../../../lib/utils";

const STRENGTH_COLOR = ["", "bg-red-500", "bg-orange-500", "bg-yellow-500", "bg-emerald-500"];

export function StrengthBar({ password }: { password: string }) {
  const strength = passwordStrength(password);
  if (strength.score === 0) return null;
  return (
    <div className="flex items-center gap-2" data-testid="password-strength">
      <div className="flex h-1.5 w-32 gap-0.5">
        {[1, 2, 3, 4].map((step) => (
          <div
            key={step}
            className={cn(
              "h-full flex-1 rounded-full",
              step <= strength.score ? STRENGTH_COLOR[strength.score] : "bg-muted",
            )}
          />
        ))}
      </div>
      <span className="text-xs text-muted-foreground">{strength.label}</span>
    </div>
  );
}
