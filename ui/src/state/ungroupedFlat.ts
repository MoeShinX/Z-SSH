//! 「未分组」改名之后（UI-14）：用户把虚拟的「未分组」重命名成了真分组，树里不再有「未分组」
//! 这一行，不属于任何分组的主机直接排在根层、所有根分组的下面（像 Xshell 根目录下的会话）。
//!
//! 开关是同步设置 `vault_ungrouped_flat`（默认关，关着时一切照旧）；「重命名「未分组」」保存成功
//! 后设成开。这里只放读开关的小工具和各处文案，免得「未分组」「顶层」的叫法散在各个组件里。

import { useSettings } from "../stores/settings";

/** 界面里读开关（组件用）。 */
export function useUngroupedFlat(): boolean {
  return useSettings((state) => state.vault_ungrouped_flat);
}

/** 非组件代码（拖放、右键动作）读开关。 */
export function isUngroupedFlat(): boolean {
  return useSettings.getState().vault_ungrouped_flat;
}

/** 提示 / 列表里「没有分组」的叫法：开关关着是「未分组」，打开后是「顶层」。 */
export function ungroupedLabel(flat: boolean): string {
  return flat ? "顶层" : "未分组";
}

/** 主机详情里分组下拉的第一项（和分组对话框里「上级分组：（顶层）」一个叫法）。 */
export function ungroupedOptionLabel(flat: boolean): string {
  return flat ? "（顶层）" : "未分组";
}

/** 右键「移动到分组」子菜单的第一项。 */
export function ungroupedMoveLabel(flat: boolean): string {
  return flat ? "顶层（不分组）" : "未分组";
}

/** 删除分组的确认文案：分组下的主机去哪儿。 */
export function deleteGroupMessage(flat: boolean): string {
  return flat ? "分组下的主机会移到顶层，不会被删。" : "分组下的主机会变成未分组，不会被删。";
}
