//! 外观选择：主题（系统 / 浅色 / 深色）、浅色和深色皮肤卡片、终端配色。设置页和标题栏的
//! 主题按钮共用。

import { Check, Monitor, Moon, Sun } from "lucide-react";

import { cn } from "../../lib/utils";
import {
  DARK_SKINS,
  LIGHT_SKINS,
  SKINS,
  TERMINAL_FOLLOW_SKIN,
  type Skin,
} from "../../state/skins";
import { activeSkin, normalizeTerminalTheme, useSettings, type ThemeMode } from "../../stores/settings";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";

export const THEME_MODES: { value: ThemeMode; label: string; icon: typeof Monitor }[] = [
  { value: "system", label: "系统", icon: Monitor },
  { value: "light", label: "浅色", icon: Sun },
  { value: "dark", label: "深色", icon: Moon },
];

/** 一张皮肤卡片：小窗口示意（侧栏 + 标题 + 文字行 + 三个点的配色）。 */
export function SkinCard({
  skin,
  selected,
  onSelect,
  compact = false,
}: {
  skin: Skin;
  selected: boolean;
  onSelect: () => void;
  compact?: boolean;
}) {
  const { ui, terminal } = skin;
  return (
    <button
      type="button"
      data-skin={skin.id}
      aria-pressed={selected}
      onClick={onSelect}
      className={cn(
        "group flex flex-col gap-1.5 rounded-lg border p-1.5 text-left transition-colors",
        selected ? "border-primary bg-accent" : "border-border hover:border-ring/60",
      )}
    >
      <div
        className={cn("flex w-full overflow-hidden rounded-md border", compact ? "h-10" : "h-14")}
        style={{ background: ui.background, borderColor: ui.border }}
      >
        <div className="flex w-1/5 flex-col gap-1 p-1.5" style={{ background: ui.surface }}>
          <span className="h-1 w-full rounded-full" style={{ background: ui.accent }} />
          <span className="h-1 w-3/4 rounded-full" style={{ background: ui.border }} />
          <span className="h-1 w-3/4 rounded-full" style={{ background: ui.border }} />
        </div>
        <div className="flex flex-1 flex-col gap-1 p-1.5">
          <span className="h-1 w-3/4 rounded-full" style={{ background: ui.text }} />
          <span className="h-1 w-full rounded-full" style={{ background: ui.textMuted, opacity: 0.6 }} />
          <span className="mt-auto flex gap-1">
            {[terminal.ansi[4], terminal.ansi[2], terminal.ansi[3]].map((color) => (
              <span key={color} className="h-1.5 w-1.5 rounded-full" style={{ background: color }} />
            ))}
          </span>
        </div>
      </div>
      <span className="flex items-center justify-between px-0.5 text-xs">
        <span className="truncate">{skin.name}</span>
        {selected && <Check size={12} className="shrink-0 text-primary" />}
      </span>
    </button>
  );
}

/** 浅色或深色的皮肤网格。 */
export function SkinGrid({ mode, compact = false }: { mode: "light" | "dark"; compact?: boolean }) {
  const selectedId = useSettings((state) => (mode === "dark" ? state.ui_skin_dark : state.ui_skin_light));
  const update = useSettings((state) => state.update);
  const skins = mode === "dark" ? DARK_SKINS : LIGHT_SKINS;
  const selected = skins.find((skin) => skin.id === selectedId)?.id ?? skins[0].id;
  return (
    <div
      className={cn("grid gap-2", compact ? "grid-cols-2" : "grid-cols-[repeat(auto-fill,minmax(150px,1fr))]")}
      data-section={`skins-${mode}`}
    >
      {skins.map((skin) => (
        <SkinCard
          key={skin.id}
          skin={skin}
          compact={compact}
          selected={skin.id === selected}
          onSelect={() => void update(mode === "dark" ? "ui_skin_dark" : "ui_skin_light", skin.id)}
        />
      ))}
    </div>
  );
}

/** 主题模式三选一（按钮组）。 */
export function ThemeModeButtons() {
  const mode = useSettings((state) => state.ui_theme_mode);
  const update = useSettings((state) => state.update);
  return (
    <div className="inline-flex rounded-md border border-border p-0.5" role="radiogroup" aria-label="主题">
      {THEME_MODES.map((item) => {
        const Icon = item.icon;
        const active = item.value === mode;
        return (
          <button
            key={item.value}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => void update("ui_theme_mode", item.value)}
            className={cn(
              "flex items-center gap-1.5 rounded px-2.5 py-1 text-xs transition-colors",
              active ? "bg-primary text-primary-foreground" : "text-muted-foreground hover:bg-accent hover:text-foreground",
            )}
          >
            <Icon size={13} />
            {item.label}
          </button>
        );
      })}
    </div>
  );
}

/** 终端固定成了和当前明暗相反的配色时的提示（浅色主题配深色终端之类），带一键改回跟随皮肤。 */
export function TerminalMismatchHint() {
  const value = useSettings((state) => normalizeTerminalTheme(state.terminal_theme));
  const mode = useSettings((state) => activeSkin(state).mode);
  const update = useSettings((state) => state.update);
  const fixed = SKINS.find((skin) => skin.id === value);
  if (!fixed || fixed.mode === mode) return null;
  return (
    <p className="flex flex-wrap items-center gap-2 pb-2 text-xs text-amber-600" data-section="terminal-mismatch">
      终端配色固定为 {fixed.name}（{fixed.mode === "dark" ? "深色" : "浅色"}），现在是
      {mode === "dark" ? "深色" : "浅色"}主题，所以终端和界面明暗不一样。
      <button
        type="button"
        className="rounded border border-amber-500/50 px-2 py-0.5 hover:bg-amber-500/10"
        onClick={() => void update("terminal_theme", TERMINAL_FOLLOW_SKIN)}
      >
        改回跟随皮肤
      </button>
    </p>
  );
}

/** 终端配色：跟随皮肤，或者固定用某一套。 */
export function TerminalThemeSelect({ className }: { className?: string }) {
  const value = useSettings((state) => normalizeTerminalTheme(state.terminal_theme));
  const update = useSettings((state) => state.update);
  const following = useSettings((state) => activeSkin(state).name);
  return (
    <Select value={value} onValueChange={(next) => void update("terminal_theme", next)}>
      <SelectTrigger className={cn("h-9 w-56", className)} aria-label="终端配色">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={TERMINAL_FOLLOW_SKIN}>跟随皮肤（现在是 {following}）</SelectItem>
        {SKINS.map((skin) => (
          <SelectItem key={skin.id} value={skin.id}>
            {skin.name}（{skin.mode === "dark" ? "深色" : "浅色"}）
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
