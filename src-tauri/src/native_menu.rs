//! WebView2 自带右键菜单的过滤（NATIVE-1）。
//!
//! 前端的 `nativeGuard` 只能决定「弹不弹」；弹出来的是 WebView2 完整的自带菜单，里面有
//! 「刷新 / 后退 / 另存为 / 打印 / 检查」等，点「刷新」会重载整个界面、断开所有连接。
//! 这里在 WebView2 层挂 `ContextMenuRequested`，把菜单里不属于编辑操作的项删掉；
//! 一项都不剩就标记为已处理，不弹菜单。这是主防线，前端那层是第一道筛。
//!
//! 开发构建也启用：这里只删危险项，不影响调试；需要检查元素用开发者工具的快捷键。

#[cfg(not(windows))]
use tauri::{Runtime, WebviewWindow};

/// 菜单项种类（对应 `COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND`）。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemKind {
    Command,
    CheckBox,
    Radio,
    Separator,
    Submenu,
}

/// 一个菜单项里过滤要看的部分。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct MenuEntry {
    /// WebView2 的 `Name`：菜单项英文标签的小驼峰，与界面语言无关。
    pub name: String,
    pub kind: ItemKind,
}

/// 保留的编辑类菜单项（`Name`）。名字出自 WebView2 文档：英文标签的小驼峰
/// （“Paste as plain text” → `pasteAsPlainText`）。不在表里的一律删。
#[cfg_attr(not(windows), allow(dead_code))]
const KEEP_NAMES: &[&str] = &[
    "undo",
    "redo",
    "cut",
    "copy",
    "paste",
    "pasteAndMatchStyle",
    "pasteAsPlainText",
    "selectAll",
    // 拼写检查的建议项 / “添加到字典”：文档说拼写检查类的项统一叫 spellCheck。
    // 只留命令类（子菜单、勾选框如“检查拼写”开关、语言设置都不是命令）。
    "spellCheck",
];

#[cfg_attr(not(windows), allow(dead_code))]
fn keeps(entry: &MenuEntry) -> bool {
    entry.kind == ItemKind::Command && KEEP_NAMES.contains(&entry.name.as_str())
}

/// 过滤后要留下的下标（升序，含整理过的分隔线）。
///
/// - 只留 [`keeps`] 认的项；种类读不出来的调用方按 `Submenu` 传，名字读不出来的传空串，都会被删。
/// - 分隔线只在两个留下的项之间保留一条：不会出现在开头、结尾，也不会连续。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn keep_indices(items: &[MenuEntry]) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::new();
    let mut pending_separator: Option<usize> = None;
    let mut has_item = false;
    for (index, entry) in items.iter().enumerate() {
        if entry.kind == ItemKind::Separator {
            if has_item && pending_separator.is_none() {
                pending_separator = Some(index);
            }
        } else if keeps(entry) {
            if let Some(separator) = pending_separator.take() {
                kept.push(separator);
            }
            kept.push(index);
            has_item = true;
        }
    }
    kept
}

/// 给窗口的 WebView 挂上右键菜单过滤。挂不上只记 warn，不影响窗口。
#[cfg(windows)]
pub fn install<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    let label = window.label().to_string();
    let result = window.with_webview({
        let label = label.clone();
        move |webview| {
            // SAFETY: with_webview 的回调在 WebView 所在的 UI 线程上执行，controller 是有效的 COM 接口。
            if let Err(err) = unsafe { windows_impl::attach(&webview.controller()) } {
                tracing::warn!(window = %label, %err, "挂 WebView2 右键菜单过滤失败");
            } else {
                tracing::debug!(window = %label, "WebView2 右键菜单过滤已挂上");
            }
        }
    });
    if let Err(err) = result {
        tracing::warn!(window = %label, %err, "挂 WebView2 右键菜单过滤失败（拿不到 WebView）");
    }
}

#[cfg(not(windows))]
pub fn install<R: Runtime>(_window: &WebviewWindow<R>) {}

#[cfg(windows)]
mod windows_impl {
    use super::{keep_indices, ItemKind, MenuEntry};
    use tracing::{debug, warn};
    use webview2_com::take_pwstr;
    use webview2_com::ContextMenuRequestedEventHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2ContextMenuItem, ICoreWebView2ContextMenuRequestedEventArgs,
        ICoreWebView2Controller, ICoreWebView2_11, COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND,
        COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_CHECK_BOX, COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
        COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_RADIO, COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_SEPARATOR,
    };
    use windows::core::Interface;

    /// 查询接口或注册失败（比如 WebView2 运行时太旧，没有 `ICoreWebView2_11`）时返回错误。
    ///
    /// # Safety
    /// `controller` 必须是有效的 WebView2 控制器，并且在它所属的 UI 线程上调用。
    pub(super) unsafe fn attach(controller: &ICoreWebView2Controller) -> windows::core::Result<()> {
        let core = controller.CoreWebView2()?;
        let core11: ICoreWebView2_11 = core.cast()?;
        let handler = ContextMenuRequestedEventHandler::create(Box::new(|_sender, args| {
            if let Some(args) = args {
                // 出错就当作“不确定”：收起菜单，不能把完整的自带菜单放出去
                if let Err(err) = filter(&args) {
                    warn!(%err, "过滤右键菜单出错，改为不弹菜单");
                    let _ = args.SetHandled(true);
                }
            }
            Ok(())
        }));
        let mut token = 0i64;
        // 不用取消注册：事件跟着这个 WebView 一起销毁
        core11.add_ContextMenuRequested(&handler, &mut token)?;
        Ok(())
    }

    fn kind_of(kind: COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND) -> ItemKind {
        match kind {
            COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND => ItemKind::Command,
            COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_CHECK_BOX => ItemKind::CheckBox,
            COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_RADIO => ItemKind::Radio,
            COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_SEPARATOR => ItemKind::Separator,
            // 子菜单和以后新增的种类都当子菜单：一律不留
            _ => ItemKind::Submenu,
        }
    }

    fn entry_of(item: &ICoreWebView2ContextMenuItem) -> MenuEntry {
        // 读不出来的按“不认识”处理，会被删掉
        let mut raw_name = windows::core::PWSTR::null();
        // SAFETY: 事件回调内同步调用，item 有效；返回的字符串由 take_pwstr 释放。
        let name = match unsafe { item.Name(&mut raw_name) } {
            Ok(()) => take_pwstr(raw_name),
            Err(_) => String::new(),
        };
        let mut raw_kind = COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND(-1);
        // SAFETY: 同上。
        let kind = match unsafe { item.Kind(&mut raw_kind) } {
            Ok(()) => kind_of(raw_kind),
            Err(_) => ItemKind::Submenu,
        };
        MenuEntry { name, kind }
    }

    fn filter(args: &ICoreWebView2ContextMenuRequestedEventArgs) -> windows::core::Result<()> {
        // SAFETY: 这些都是事件参数上的 COM 调用，在事件回调内同步完成。
        unsafe {
            let collection = args.MenuItems()?;
            let mut count = 0u32;
            collection.Count(&mut count)?;
            let mut entries = Vec::with_capacity(count as usize);
            for index in 0..count {
                entries.push(entry_of(&collection.GetValueAtIndex(index)?));
            }
            let keep = keep_indices(&entries);

            // 从后往前删，下标才不会错位
            for index in (0..count as usize).rev() {
                if !keep.contains(&index) {
                    collection.RemoveValueAtIndex(index as u32)?;
                }
            }
            let kept_names: Vec<&str> = keep
                .iter()
                .map(|&index| entries[index].name.as_str())
                .collect();
            let removed_names: Vec<&str> = (0..entries.len())
                .filter(|index| !keep.contains(index))
                .map(|index| entries[index].name.as_str())
                .collect();
            // 只记菜单项的名字，不记选中的文字内容
            debug!(
                total = entries.len(),
                kept = ?kept_names,
                removed = ?removed_names,
                "WebView2 右键菜单过滤"
            );
            if keep.is_empty() {
                args.SetHandled(true)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str) -> MenuEntry {
        MenuEntry {
            name: name.into(),
            kind: ItemKind::Command,
        }
    }
    fn sep() -> MenuEntry {
        MenuEntry {
            name: String::new(),
            kind: ItemKind::Separator,
        }
    }
    fn kept(items: &[MenuEntry]) -> Vec<&str> {
        keep_indices(items)
            .into_iter()
            .map(|index| {
                if items[index].kind == ItemKind::Separator {
                    "-"
                } else {
                    items[index].name.as_str()
                }
            })
            .collect()
    }

    #[test]
    fn edit_box_menu_keeps_only_edit_actions() {
        // 可编辑框里的完整自带菜单：编辑项、全选，后面跟着页面级的项
        let items = [
            cmd("undo"),
            cmd("redo"),
            sep(),
            cmd("cut"),
            cmd("copy"),
            cmd("paste"),
            cmd("pasteAsPlainText"),
            cmd("selectAll"),
            sep(),
            cmd("emoji"),
            sep(),
            cmd("back"),
            cmd("forward"),
            cmd("reload"),
            sep(),
            cmd("saveAs"),
            cmd("print"),
            cmd("inspectElement"),
        ];
        assert_eq!(
            kept(&items),
            [
                "undo",
                "redo",
                "-",
                "cut",
                "copy",
                "paste",
                "pasteAsPlainText",
                "selectAll"
            ]
        );
    }

    #[test]
    fn selection_menu_drops_search_share_and_reload() {
        let items = [
            cmd("copy"),
            sep(),
            cmd("webSelect"),
            cmd("share"),
            cmd("readAloud"),
            sep(),
            cmd("print"),
            cmd("inspectElement"),
        ];
        assert_eq!(kept(&items), ["copy"]);
    }

    #[test]
    fn blank_page_menu_is_removed_entirely() {
        let items = [
            cmd("back"),
            cmd("forward"),
            cmd("reload"),
            sep(),
            cmd("saveAs"),
            cmd("print"),
            cmd("createQrCode"),
            sep(),
            cmd("inspectElement"),
        ];
        assert!(keep_indices(&items).is_empty());
        assert!(keep_indices(&[]).is_empty());
    }

    #[test]
    fn separators_are_tidied() {
        // 开头、结尾、连续的分隔线都不留；两个留下的项之间只留一条
        let items = [
            sep(),
            sep(),
            cmd("cut"),
            sep(),
            sep(),
            cmd("reload"),
            sep(),
            cmd("paste"),
            sep(),
            cmd("inspectElement"),
            sep(),
        ];
        assert_eq!(kept(&items), ["cut", "-", "paste"]);
        let result = keep_indices(&items);
        assert_eq!(
            result.first().map(|&i| items[i].kind),
            Some(ItemKind::Command)
        );
        assert_eq!(
            result.last().map(|&i| items[i].kind),
            Some(ItemKind::Command)
        );
    }

    #[test]
    fn unknown_or_unreadable_names_are_removed() {
        let items = [
            cmd("copy"),
            cmd("somethingNew"),
            cmd(""),
            cmd("Copy"), // 大小写必须一致
            cmd("extension"),
            cmd("custom"),
            cmd("openLinkInNewWindow"),
            cmd("copyLinkLocation"),
            cmd("copyImage"),
            cmd("saveImageAs"),
        ];
        assert_eq!(kept(&items), ["copy"]);
    }

    #[test]
    fn only_command_kind_counts() {
        // 同名但不是命令（子菜单 / 勾选框 / 单选项）不留；读不出种类的按子菜单传，同样被删
        let items = [
            MenuEntry {
                name: "spellCheck".into(),
                kind: ItemKind::Submenu,
            },
            MenuEntry {
                name: "spellCheck".into(),
                kind: ItemKind::CheckBox,
            },
            MenuEntry {
                name: "copy".into(),
                kind: ItemKind::Radio,
            },
            cmd("spellCheck"),
        ];
        assert_eq!(kept(&items), ["spellCheck"]);
        assert_eq!(keep_indices(&items), vec![3]);
    }

    #[test]
    fn whitelist_is_exactly_the_edit_actions() {
        for name in [
            "undo",
            "redo",
            "cut",
            "copy",
            "paste",
            "pasteAndMatchStyle",
            "pasteAsPlainText",
            "selectAll",
        ] {
            assert!(keeps(&cmd(name)), "{name}");
        }
        for name in [
            "back",
            "forward",
            "reload",
            "saveAs",
            "print",
            "createQrCode",
            "inspectElement",
            "emoji",
            "share",
            "readAloud",
            "openLinkInNewTab",
            "copyLinkLocation",
            "copyImage",
        ] {
            assert!(!keeps(&cmd(name)), "{name}");
        }
    }
}
