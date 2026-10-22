import { describe, expect, it } from "vitest";

import type { AgentModelCatalog } from "../../api/agent";
import { DEFAULT_MODEL_VALUE } from "./modelOptions";
import {
  buildModelPicker,
  effortChoices,
  effortLabel,
  joinSelection,
  pickEffort,
  selectionAfterCatalog,
  splitSelection,
} from "./modelPicker";

const m = (id: string, name: string) => ({ id, name, unavailable: false });

const claude: AgentModelCatalog = {
  default: "default",
  default_label: "Sonnet 5.5",
  models: [
    m("default", "默认（Claude Code 的设置）"),
    m("default/low", "默认 · low"),
    m("default/high", "默认 · high"),
    m("opus", "Opus 5.5"),
    m("opus/low", "Opus 5.5 · low"),
    m("opus/high", "Opus 5.5 · high"),
    m("opus/max", "Opus 5.5 · max"),
    m("claude-haiku-4-5", "Haiku 4.5"),
  ],
};

const codex: AgentModelCatalog = {
  default: "gpt-sol/low",
  default_label: null,
  models: [
    m("gpt-sol/low", "GPT-Sol · low"),
    m("gpt-sol/medium", "GPT-Sol · medium"),
    m("gpt-sol/high", "GPT-Sol · high"),
    m("gpt-astra/medium", "GPT-Astra · medium"),
    m("gpt-astra/high", "GPT-Astra · high"),
  ],
};

const opencode: AgentModelCatalog = {
  default: null,
  default_label: "DeepSeek Pro",
  models: [m("deepseek/flash", "DeepSeek Flash"), m("deepseek/pro", "DeepSeek Pro")],
};

describe("模型 / 思考强度分开选", () => {
  it("Claude Code：每个模型一项，强度单独列；默认项显示实际模型", () => {
    const picker = buildModelPicker(claude);
    expect(picker.groups.map((g) => g.label)).toEqual([
      "默认（Sonnet 5.5）",
      "Opus 5.5",
      "Haiku 4.5",
    ]);
    const [def, opus, haiku] = picker.groups;
    expect(effortChoices(def)).toEqual([null, "low", "high"]);
    expect(effortChoices(opus)).toEqual([null, "low", "high", "max"]);
    expect(effortChoices(haiku)).toEqual([null]);
    expect(picker.initial).toBe("default");
  });

  it("codex：模型总要带强度，默认那条放第一个并标成默认", () => {
    const picker = buildModelPicker(codex);
    expect(picker.groups.map((g) => g.label)).toEqual(["默认（GPT-Sol）", "GPT-Astra"]);
    expect(effortChoices(picker.groups[0])).toEqual(["low", "medium", "high"]);
    expect(splitSelection(picker.initial, picker.groups)).toEqual({
      group: picker.groups[0],
      effort: "low",
    });
  });

  it("opencode：provider/model 不当成强度，没有强度框", () => {
    const picker = buildModelPicker(opencode);
    expect(picker.groups.map((g) => g.value)).toEqual([
      DEFAULT_MODEL_VALUE,
      "deepseek/flash",
      "deepseek/pro",
    ]);
    expect(picker.groups.every((g) => g.efforts.length === 0)).toBe(true);
  });

  it("opencode 带思考等级：服务商/模型/等级归到同一个模型下，可以不指定等级", () => {
    const picker = buildModelPicker({
      default: "deepseek/flash",
      default_label: null,
      models: [
        m("deepseek/flash", "DeepSeek Flash"),
        m("deepseek/flash/low", "DeepSeek Flash · low"),
        m("deepseek/flash/high", "DeepSeek Flash · high"),
        m("openrouter/anthropic/claude", "OpenRouter Claude"),
      ],
    });
    expect(picker.groups.map((g) => g.value)).toEqual(["deepseek/flash", "openrouter/anthropic/claude"]);
    expect(effortChoices(picker.groups[0])).toEqual([null, "low", "high"]);
    expect(effortChoices(picker.groups[1])).toEqual([null]);
    expect(joinSelection(picker.groups[0], "high")).toBe("deepseek/flash/high");
    expect(splitSelection("deepseek/flash/low", picker.groups)?.effort).toBe("low");
  });

  it("平铺 id 和（模型，强度）来回换", () => {
    const { groups } = buildModelPicker(claude);
    const opus = groups[1];
    expect(joinSelection(opus, "high")).toBe("opus/high");
    expect(joinSelection(opus, null)).toBe("opus");
    expect(splitSelection("opus/max", groups)).toEqual({ group: opus, effort: "max" });
    expect(splitSelection("opus/xhigh", groups)).toBeNull();
  });

  it("换模型时强度能保持就保持，不支持就退到默认 / medium / 第一个", () => {
    const { groups } = buildModelPicker(claude);
    expect(pickEffort(groups[1], "high")).toBe("high");
    expect(pickEffort(groups[2], "high")).toBeNull(); // Haiku 4.5 不支持强度
    const astra = buildModelPicker(codex).groups[1];
    expect(pickEffort(astra, "high")).toBe("high");
    expect(pickEffort(astra, "low")).toBe("medium");
    expect(pickEffort(astra, null)).toBe("medium");
  });

  it("换了目录：原选择还在就留着，否则用默认模型并尽量保持强度", () => {
    const picker = buildModelPicker(claude);
    expect(selectionAfterCatalog("opus/max", picker, "max")).toBe("opus/max");
    expect(selectionAfterCatalog("", picker, "high")).toBe("default/high");
    expect(selectionAfterCatalog("gone", picker, "max")).toBe("default");
    const codexPicker = buildModelPicker(codex);
    expect(selectionAfterCatalog("", codexPicker, "high")).toBe("gpt-sol/high");
    expect(selectionAfterCatalog("", codexPicker, null)).toBe("gpt-sol/low");
  });

  it("强度的显示文字", () => {
    expect(effortLabel(null)).toBe("默认强度");
    expect(effortLabel("high")).toBe("High");
    expect(effortLabel("xhigh")).toBe("XHigh");
  });
});
