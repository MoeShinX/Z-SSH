//! 设置页的进出（R-11）：齿轮是开关，关掉设置要回到进来之前的那一页。
//!
//! 为什么单独抽出来：以前齿轮只会 `setView("settings")`，设置页里又没有关闭按钮，
//! 用户只能点左上角「主机」出去——从终端进来的人也被扔回主机页。进出的规则
//! （回哪一页、Esc 什么时候算关设置）放成纯函数，单测能直接盯住。

export type MainView = "vault" | "terminal" | "settings";
/** 离开设置页能回去的那两页。 */
export type ReturnView = Exclude<MainView, "settings">;

export interface SettingsNav {
  view: MainView;
  /** 进设置之前在哪一页；不在设置页时没有意义。 */
  returnTo: ReturnView;
}

/** 打开设置：记住从哪来。已经在设置页就不动（别把 returnTo 改成 settings）。 */
export function openSettings(nav: SettingsNav): SettingsNav {
  if (nav.view === "settings") return nav;
  return { view: "settings", returnTo: nav.view };
}

/**
 * 关掉设置：回进来之前那一页。
 *
 * 进来时在终端、待在设置页期间标签全关了（标题栏还能关标签），这时回终端
 * 只会看到一片空白，退回主机页。
 */
export function closeSettings(nav: SettingsNav, hasActiveTab: boolean): SettingsNav {
  if (nav.view !== "settings") return nav;
  const back: ReturnView = nav.returnTo === "terminal" && !hasActiveTab ? "vault" : nav.returnTo;
  return { view: back, returnTo: back };
}

/** 齿轮按钮：在设置页就关，不在就开。 */
export function toggleSettings(nav: SettingsNav, hasActiveTab: boolean): SettingsNav {
  return nav.view === "settings" ? closeSettings(nav, hasActiveTab) : openSettings(nav);
}

/**
 * 页面上是否还开着弹层（下拉框、对话框、菜单）。
 *
 * Radix 的弹层自己在 document 捕获阶段处理 Esc 并 `preventDefault`，按理看
 * `defaultPrevented` 就够了；这里再查一遍 DOM 兜底：弹层关闭要等 React 提交，
 * 同一次按键冒泡到 window 时它还在 DOM 里，看到它就说明这次 Esc 是给它的。
 */
export function hasOpenLayer(doc: Document): boolean {
  return (
    doc.querySelector(
      [
        '[role="dialog"][data-state="open"]',
        '[role="alertdialog"][data-state="open"]',
        '[role="listbox"][data-state="open"]',
        '[role="menu"][data-state="open"]',
      ].join(", "),
    ) !== null
  );
}

/** 这次按键要不要关掉设置页：只认没被别人处理过的 Esc，输入法组字时的 Esc 不算。 */
export function escClosesSettings(
  event: Pick<KeyboardEvent, "key" | "defaultPrevented" | "isComposing">,
  view: MainView,
  layerOpen: boolean,
): boolean {
  return (
    view === "settings" &&
    event.key === "Escape" &&
    !event.defaultPrevented &&
    !event.isComposing &&
    !layerOpen
  );
}
