//! AI 面板模型下拉框的选项（纯函数 + 按智能体缓存的模型目录）。
//!
//! "默认"那一项要让用户看出实际是哪个模型：后端能从智能体自己的配置 / 接口确定时
//! 给出 `default_label`（或默认项本身就在列表里），显示成"默认（实际模型）"；读不到
//! 就只显示"默认模型"，不猜。

import type { AgentId, AgentInstall, AgentModelCatalog } from "../../api/agent";

/** 智能体没有配置默认模型 id 时，"默认"项的值（发送时换成 null，让智能体自己决定）。 */
export const DEFAULT_MODEL_VALUE = "__zssh_default__";

/** 下拉框里的一项。 */
export interface ModelOption {
  value: string;
  label: string;
  unavailable: boolean;
  /** "默认"那一项。 */
  isDefault: boolean;
}

/** "默认"项的文字：知道实际模型就带上，不知道就只写"默认模型"。 */
export function defaultModelText(actual: string | null | undefined): string {
  const name = actual?.trim();
  return name ? `默认（${name}）` : "默认模型";
}

/**
 * 由模型目录算出下拉框选项和初始选中值。
 *
 * - 默认项总放第一个；
 * - 智能体给了默认 id 且在列表里（codex 的 isDefault、Claude Code 的 `default`、opencode
 *   配了 `model`）：把那一项改名成"默认（…）"，不重复列；
 * - 给了默认 id 但不在列表里：补一项，值就是那个 id（发出去和以前一样）；
 * - 没有默认 id：补一项 {@link DEFAULT_MODEL_VALUE}，发送时不带模型。
 */
export function buildModelOptions(catalog: AgentModelCatalog | null): {
  options: ModelOption[];
  initial: string;
} {
  if (!catalog) return { options: [], initial: "" };
  const defaultId = catalog.default ?? null;
  const listed = defaultId ? catalog.models.find((m) => m.id === defaultId) : undefined;
  // Claude Code 的默认项本身叫"默认（Claude Code 的设置）"，名字不是模型名，不能当实际模型。
  const listedName = listed && defaultId !== "default" ? listed.name : null;
  const actual = catalog.default_label ?? listedName ?? (listed ? null : defaultId);
  const head: ModelOption = {
    value: defaultId ?? DEFAULT_MODEL_VALUE,
    label: defaultModelText(actual),
    unavailable: listed?.unavailable === true,
    isDefault: true,
  };
  const rest = catalog.models
    .filter((m) => m.id !== defaultId)
    .map((m) => ({
      value: m.id,
      label: m.name,
      unavailable: m.unavailable === true,
      isDefault: false,
    }));
  return { options: [head, ...rest], initial: head.value };
}

/** 换了目录之后选中值还能不能留着（用户自己选过的模型尽量保留）。 */
export function keepOrInitial(current: string, options: ModelOption[], initial: string): string {
  return current && options.some((o) => o.value === current) ? current : initial;
}

/** 发送时带的模型：占位的默认值不带（智能体用自己的默认）。 */
export function modelForSend(value: string): string | null {
  return value && value !== DEFAULT_MODEL_VALUE ? value : null;
}

/** 触发器上显示的文字。 */
export function selectedModelText(value: string, options: ModelOption[]): string {
  return options.find((o) => o.value === value)?.label ?? "默认模型";
}

// ---------------------------------------------------------------------------
// 模型目录缓存：面板一打开就要显示默认模型，但不能每次渲染 / 每次挂载都去问智能体
// （opencode 冷启动可能要几十秒）。按"智能体 + 安装路径 + 版本"缓存：换了安装或者
// 升级了版本，键就变了，自然重新取。
// ---------------------------------------------------------------------------

const catalogCache = new Map<string, AgentModelCatalog>();
const inflight = new Map<string, Promise<AgentModelCatalog>>();

/** 缓存键；安装还没检测出来时为 null（不取）。 */
export function catalogCacheKey(
  agent: AgentId,
  install: AgentInstall | null | undefined,
): string | null {
  if (!install) return null;
  return `${agent}|${install.path}|${install.version}`;
}

/** 已经缓存的目录。 */
export function cachedCatalog(key: string | null): AgentModelCatalog | null {
  return key ? catalogCache.get(key) ?? null : null;
}

/** 取目录：有缓存用缓存，同一个键同时只发一次请求；失败不缓存（下次再试）。 */
export function loadCatalog(
  key: string,
  fetcher: () => Promise<AgentModelCatalog>,
): Promise<AgentModelCatalog> {
  const hit = catalogCache.get(key);
  if (hit) return Promise.resolve(hit);
  const pending = inflight.get(key);
  if (pending) return pending;
  const request = fetcher()
    .then((catalog) => {
      catalogCache.set(key, catalog);
      return catalog;
    })
    .finally(() => inflight.delete(key));
  inflight.set(key, request);
  return request;
}

/** 新对话拿到的最新目录顺手更新缓存。 */
export function storeCatalog(key: string | null, catalog: AgentModelCatalog): void {
  if (key) catalogCache.set(key, catalog);
}

/** 清空缓存（测试用）。 */
export function clearCatalogCache(): void {
  catalogCache.clear();
  inflight.clear();
}
