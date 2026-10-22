// Adapted from Netcatty infrastructure/config/uiThemes.ts
//! 界面配色的机制：一组 HSL 三元组（"220 28% 10%" 这种）挂到 <html> 的 CSS 变量上，
//! Tailwind 的 @theme 再把它们接成颜色类（bg-background 之类）。具体有哪些配色见 skins.ts。

export type UiThemeTokens = {
  background: string;
  foreground: string;
  card: string;
  cardForeground: string;
  popover: string;
  popoverForeground: string;
  primary: string;
  primaryForeground: string;
  secondary: string;
  secondaryForeground: string;
  muted: string;
  mutedForeground: string;
  accent: string;
  accentForeground: string;
  destructive: string;
  destructiveForeground: string;
  border: string;
  input: string;
  ring: string;
};

export type UiThemeMode = "light" | "dark";

/** token → CSS 变量，和 Netcatty 的 buildAppThemeCssVars 一致。 */
export function uiThemeCssVars(tokens: UiThemeTokens): Record<string, string> {
  return {
    "--background": tokens.background,
    "--foreground": tokens.foreground,
    "--card": tokens.card,
    "--card-foreground": tokens.cardForeground,
    "--popover": tokens.popover,
    "--popover-foreground": tokens.popoverForeground,
    "--primary": tokens.primary,
    "--primary-foreground": tokens.primaryForeground,
    "--secondary": tokens.secondary,
    "--secondary-foreground": tokens.secondaryForeground,
    "--muted": tokens.muted,
    "--muted-foreground": tokens.mutedForeground,
    "--accent": tokens.accent,
    "--accent-foreground": tokens.accentForeground,
    "--destructive": tokens.destructive,
    "--destructive-foreground": tokens.destructiveForeground,
    "--border": tokens.border,
    "--input": tokens.input,
    "--ring": tokens.ring,
  };
}

/** 把一套界面配色应用到 <html>：换 light/dark class + 写 CSS 变量。 */
export function applyUiTokens(mode: UiThemeMode, tokens: UiThemeTokens): void {
  const root = window.document.documentElement;
  root.classList.remove("light", "dark");
  root.classList.add(mode);
  root.style.colorScheme = mode;
  for (const [property, value] of Object.entries(uiThemeCssVars(tokens))) {
    root.style.setProperty(property, value);
  }
}
