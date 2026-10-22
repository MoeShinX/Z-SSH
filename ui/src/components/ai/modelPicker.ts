//! AI 面板顶部三个框里的后两个：模型、思考强度（参考 Netcatty 的模型 / 强度分开选）。
//!
//! 后端的模型目录是平铺的："模型"一项，加上"模型/强度"若干项（Claude Code、codex 都是
//! 这个约定，名字是"模型名 · 强度"）。这里按模型分组：中间框选模型，右边框选这个模型
//! 支持的强度。发给后端的仍是平铺的那个 id（`模型` 或 `模型/强度`）。

import type { AgentModelCatalog } from "../../api/agent";
import { DEFAULT_MODEL_VALUE, defaultModelText } from "./modelOptions";

export interface ModelGroup {
  /** 模型本身的 id（不带强度）。 */
  value: string;
  label: string;
  unavailable: boolean;
  isDefault: boolean;
  /** 能不能不指定强度（= 用智能体自己的默认强度）。codex 的模型总要带强度。 */
  plain: boolean;
  /** 这个模型支持的强度，按目录里的顺序。 */
  efforts: string[];
}

export interface ModelPicker {
  groups: ModelGroup[];
  /** 初始选中的平铺 id。 */
  initial: string;
}

/** `模型/强度` + `模型名 · 强度` 才算强度变体（opencode 的 `provider/model` 不是）。 */
function splitEffort(id: string, name: string): { base: string; label: string; effort: string } | null {
  const slash = id.lastIndexOf("/");
  const dot = name.lastIndexOf(" · ");
  if (slash <= 0 || dot <= 0) return null;
  const effort = id.slice(slash + 1);
  if (!effort || name.slice(dot + 3) !== effort) return null;
  return { base: id.slice(0, slash), label: name.slice(0, dot), effort };
}

export function buildModelPicker(catalog: AgentModelCatalog | null): ModelPicker {
  if (!catalog) return { groups: [], initial: "" };
  const groups = new Map<string, ModelGroup>();
  const groupOf = (value: string, label: string) => {
    let group = groups.get(value);
    if (!group) {
      group = { value, label, unavailable: false, isDefault: false, plain: false, efforts: [] };
      groups.set(value, group);
    }
    return group;
  };
  for (const model of catalog.models) {
    const variant = splitEffort(model.id, model.name);
    if (variant) {
      const group = groupOf(variant.base, variant.label);
      group.efforts.push(variant.effort);
      if (model.unavailable) group.unavailable = true;
    } else {
      const group = groupOf(model.id, model.name);
      group.plain = true;
      group.label = model.name;
      if (model.unavailable) group.unavailable = true;
    }
  }

  const defaultId = catalog.default ?? null;
  let head: ModelGroup;
  if (defaultId === null) {
    head = {
      value: DEFAULT_MODEL_VALUE,
      label: defaultModelText(catalog.default_label),
      unavailable: false,
      isDefault: true,
      plain: true,
      efforts: [],
    };
  } else {
    const variant = [...groups.values()].find((g) =>
      g.efforts.some((e) => `${g.value}/${e}` === defaultId),
    );
    const own = groups.get(defaultId) ?? variant;
    if (own) {
      groups.delete(own.value);
      // Claude Code 的 `default` 名字是"默认（Claude Code 的设置）"，不是模型名
      const actual = catalog.default_label ?? (own.value === "default" ? null : own.label);
      head = { ...own, isDefault: true, label: defaultModelText(actual) };
    } else {
      head = {
        value: defaultId,
        label: defaultModelText(catalog.default_label ?? defaultId),
        unavailable: false,
        isDefault: true,
        plain: true,
        efforts: [],
      };
    }
  }
  return { groups: [head, ...groups.values()], initial: defaultId ?? DEFAULT_MODEL_VALUE };
}

/** 平铺 id → 选中的模型和强度（null = 不指定强度）。找不到返回 null。 */
export function splitSelection(
  value: string,
  groups: ModelGroup[],
): { group: ModelGroup; effort: string | null } | null {
  for (const group of groups) {
    if (group.plain && group.value === value) return { group, effort: null };
    for (const effort of group.efforts) {
      if (`${group.value}/${effort}` === value) return { group, effort };
    }
  }
  return null;
}

/** 选中的模型和强度 → 平铺 id。 */
export function joinSelection(group: ModelGroup, effort: string | null): string {
  return effort === null ? group.value : `${group.value}/${effort}`;
}

/** 这个模型右边框里能选的强度（null = "默认"）。 */
export function effortChoices(group: ModelGroup): (string | null)[] {
  return group.plain ? [null, ...group.efforts] : [...group.efforts];
}

/**
 * 换了模型（或者换了智能体）之后用哪个强度：想要的强度这个模型支持就保持不变；
 * 不支持时能不指定就不指定（用智能体自己的默认），否则优先 medium，再不行取第一个。
 */
export function pickEffort(group: ModelGroup, wanted: string | null): string | null {
  const choices = effortChoices(group);
  if (choices.includes(wanted)) return wanted;
  if (group.plain) return null;
  if (group.efforts.includes("medium")) return "medium";
  return group.efforts[0] ?? null;
}

/** 强度的显示文字。 */
export function effortLabel(effort: string | null): string {
  if (effort === null) return "默认强度";
  if (effort === "xhigh") return "XHigh";
  return effort.charAt(0).toUpperCase() + effort.slice(1);
}

/**
 * 换了目录（新对话、换智能体）后的选中值：原来选的还在就留着；不在就用初始模型，
 * 强度按 {@link pickEffort} 尽量保持用户之前选的。
 */
export function selectionAfterCatalog(
  current: string,
  picker: ModelPicker,
  wantedEffort: string | null,
): string {
  if (current && splitSelection(current, picker.groups)) return current;
  const initial = splitSelection(picker.initial, picker.groups);
  const group = initial?.group ?? picker.groups[0];
  if (!group) return picker.initial;
  return joinSelection(group, pickEffort(group, wantedEffort ?? initial?.effort ?? null));
}
