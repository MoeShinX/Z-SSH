//! 「未分组」改名之后（UI-14）：用户把虚拟的「未分组」重命名成了真分组，树里不再有「未分组」
//! 这一行，不属于任何分组的主机直接排在根层、所有根分组的下面（像 Xshell 根目录下的会话）。
//!
//! 用户的明确选择是同步设置 `vault_ungrouped_flat`（「重命名「未分组」」保存成功后设成开，设置页
//! 也有开关，UI-16）；新用户的默认（UI-18）单独存在**仅本机**设置 `layout.vault_flat_default`，
//! 首次启动判断一次，不写进同步设置（不然几台电脑各写各的，合并时会冲突）。现在到底平不平铺，
//! 一律走 [`effectiveUngroupedFlat`]：同步键存过值就用它，没存过就看本机默认。这里还放各处文案，
//! 免得「未分组」「顶层」的叫法散在各个组件里。

import { useSettings } from "../stores/settings";
import { useVault } from "../stores/vault";

/**
 * 「现在是不是平铺」的有效值（唯一的算法）：同步键 `vault_ungrouped_flat` 在库里**存过值**
 * （用户明确选过、改过名，或者别的电脑同步过来的）就用它；没存过就看本机默认是不是 `flat`。
 * `stored` 为 null（读库出错、不知道）时保守地用同步键的值（没读到就是代码默认值：关）。
 */
export function effectiveFlat(
  stored: boolean | null,
  synced: boolean,
  localDefault: string,
): boolean {
  if (stored === false) return localDefault === "flat";
  return synced;
}

interface FlatSources {
  ungroupedFlatStored: boolean | null;
  vault_ungrouped_flat: boolean;
  "layout.vault_flat_default": string;
}

/** 从设置里算出有效值（组件的选择器和非组件代码共用）。 */
export function effectiveUngroupedFlat(state: FlatSources): boolean {
  return effectiveFlat(
    state.ungroupedFlatStored,
    state.vault_ungrouped_flat,
    state["layout.vault_flat_default"],
  );
}

/** 界面里读开关（组件用）。 */
export function useUngroupedFlat(): boolean {
  return useSettings(effectiveUngroupedFlat);
}

/** 非组件代码（拖放、右键动作）读开关。 */
export function isUngroupedFlat(): boolean {
  return effectiveUngroupedFlat(useSettings.getState());
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

/** 本机默认的取值：flat = 新用户默认平铺，classic = 照旧（有主机的老用户、或者同步键存过值）。 */
export type FlatDefault = "flat" | "classic";

/**
 * 新用户默认平铺的首次判断（UI-18），只决定写进本机的 `layout.vault_flat_default`：
 * - 已经判断过（`flat` / `classic`）：返回 null = 什么都不用写；
 * - 同步键 `vault_ungrouped_flat` 在库里存过值：写 `classic`（以那个值为准，本机默认不起作用）；
 * - 没存过：主机库是空的（新装）写 `flat`，已经有主机（老用户升级上来）写 `classic`；
 * - 读库出错（`stored` 是 null）：不决定，等下次启动。
 * 判断结果**不会**写同步键。
 */
export function defaultFlatFor(
  current: string,
  stored: boolean | null,
  hostCount: number,
): FlatDefault | null {
  if (current === "flat" || current === "classic") return null;
  if (stored === null) return null;
  if (stored) return "classic";
  return hostCount === 0 ? "flat" : "classic";
}

let flatDecided = false;

/** 测试用：重来一次「新用户默认」的判断。 */
export function resetFlatDefaultForTests(): void {
  flatDecided = false;
}

/**
 * 设置和主机库都读进来以后判断一次，把结果写进本机设置 `layout.vault_flat_default`（`classic` 也要写，
 * 不然以后清空了主机库，下次启动又会被当成新用户）。一次启动只判断一次。
 * 只写本机设置，不碰会同步的 `vault_ungrouped_flat`。
 */
export async function settleFlatDefault(): Promise<void> {
  if (flatDecided) return;
  const settings = useSettings.getState();
  const vault = useVault.getState();
  if (!settings.loaded || !vault.loaded) return;
  flatDecided = true;
  const decision = defaultFlatFor(
    settings["layout.vault_flat_default"],
    settings.ungroupedFlatStored,
    vault.hosts.length,
  );
  if (decision === null) return;
  // 失败时 update 自己会提示并回滚；这次启动不再重试，下次启动再判断
  await settings.update("layout.vault_flat_default", decision);
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
