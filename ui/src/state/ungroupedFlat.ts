//! 「未分组」改名之后（UI-14）：用户把虚拟的「未分组」重命名成了真分组，树里不再有「未分组」
//! 这一行，不属于任何分组的主机直接排在根层、所有根分组的下面（像 Xshell 根目录下的会话）。
//!
//! 开关是同步设置 `vault_ungrouped_flat`（代码里的默认值是关，关着时一切照旧）；「重命名「未分组」」
//! 保存成功后设成开，设置页也有开关（UI-16）。这里放读开关的小工具和各处文案，免得「未分组」
//! 「顶层」的叫法散在各个组件里；还有「新用户默认平铺」的判断（UI-16）。

import { settingsSet } from "../api/settings";
import { useSettings } from "../stores/settings";
import { useVault } from "../stores/vault";

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

const FLAT_KEY = "vault_ungrouped_flat";

/**
 * 新用户默认平铺（UI-16）：这项设置**没存过**时，主机库是空的（新装）就定成开，已经有主机
 * （老用户升级上来）就定成关，保持原样。存过值（不管开关）一律不动：返回 null = 什么都不用写；
 * 读库出错（`stored` 是 null）时也不决定，等下次启动。
 */
export function defaultFlatFor(stored: boolean | null, hostCount: number): boolean | null {
  if (stored !== false) return null;
  return hostCount === 0;
}

let flatDecided = false;

/** 测试用：重来一次「新用户默认」的判断。 */
export function resetFlatDefaultForTests(): void {
  flatDecided = false;
}

/**
 * 设置和主机库都读进来以后判断一次，并把结果写进库里（关也要写，不然以后清空了主机库，
 * 下次启动又会被当成新用户）。一次启动只判断一次。
 */
export async function settleFlatDefault(): Promise<void> {
  if (flatDecided) return;
  const settings = useSettings.getState();
  const vault = useVault.getState();
  if (!settings.loaded || !vault.loaded) return;
  flatDecided = true;
  const decision = defaultFlatFor(settings.ungroupedFlatStored, vault.hosts.length);
  if (decision === null) return;
  try {
    if (decision) {
      // 和代码默认值（关）不同，走正常的更新（立刻生效、写库、失败回滚）
      if (!(await settings.update(FLAT_KEY, true))) return;
    } else {
      // 和代码默认值一样，update 会当成「没变」而不写库：直接写，让库里有这一项
      await settingsSet(FLAT_KEY, JSON.stringify(false));
    }
    useSettings.setState({ ungroupedFlatStored: true });
  } catch (error) {
    console.warn("写入「未分组」平铺的默认值失败", error);
  }
}

/** 启动时挂上：设置和主机库都读完就判断一次。返回取消订阅的函数。 */
export function watchFlatDefault(): () => void {
  const check = () => void settleFlatDefault();
  const stopSettings = useSettings.subscribe(check);
  const stopVault = useVault.subscribe(check);
  check();
  return () => {
    stopSettings();
    stopVault();
  };
}
