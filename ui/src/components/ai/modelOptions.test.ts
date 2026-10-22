//! 模型下拉框：选中"默认"时要看得出实际是哪个模型；读不到就只写"默认模型"。

import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentModelCatalog } from "../../api/agent";
import {
  DEFAULT_MODEL_VALUE,
  buildModelOptions,
  cachedCatalog,
  catalogCacheKey,
  clearCatalogCache,
  defaultModelText,
  keepOrInitial,
  loadCatalog,
  modelForSend,
  selectedModelText,
} from "./modelOptions";

const install = {
  path: "C:\tools\opencode\opencode.exe",
  version: "1.19.1",
  source: "tools_dir" as const,
  needs_shell: false,
  auto: true,
  too_old: false,
  skipped_older: null,
};

describe("默认模型的显示", () => {
  it("知道实际模型时显示 默认（实际模型），不知道就是 默认模型", () => {
    expect(defaultModelText("anthropic/claude-sonnet-4")).toBe("默认（anthropic/claude-sonnet-4）");
    expect(defaultModelText(null)).toBe("默认模型");
    expect(defaultModelText("  ")).toBe("默认模型");
  });

  it("opencode 没配 model：补一个占位默认项，发送时不带模型", () => {
    const catalog: AgentModelCatalog = {
      models: [
        { id: "deepseek/flash", name: "DeepSeek Flash" },
        { id: "deepseek/pro", name: "DeepSeek Pro" },
      ],
      default: null,
      default_label: "DeepSeek Pro",
    };
    const { options, initial } = buildModelOptions(catalog);
    expect(initial).toBe(DEFAULT_MODEL_VALUE);
    expect(options[0]).toMatchObject({ label: "默认（DeepSeek Pro）", isDefault: true });
    expect(options).toHaveLength(3);
    expect(selectedModelText(initial, options)).toBe("默认（DeepSeek Pro）");
    expect(modelForSend(initial)).toBeNull();
    expect(modelForSend("deepseek/pro")).toBe("deepseek/pro");
    expect(modelForSend("")).toBeNull();
  });

  it("读不到实际模型：默认项只写 默认模型（不拿第一个模型去猜）", () => {
    const { options, initial } = buildModelOptions({
      models: [{ id: "a/m1", name: "M1" }],
      default: null,
    });
    expect(selectedModelText(initial, options)).toBe("默认模型");
  });

  it("codex：isDefault 那一项改名成 默认（…） 并放第一个，不重复列", () => {
    const { options, initial } = buildModelOptions({
      models: [
        { id: "gpt/low", name: "GPT · low" },
        { id: "gpt/high", name: "GPT · high" },
      ],
      default: "gpt/low",
      default_label: "GPT · low",
    });
    expect(initial).toBe("gpt/low");
    expect(options.map((o) => o.label)).toEqual(["默认（GPT · low）", "GPT · high"]);
    // 发出去的仍是那个 id（和以前一样）。
    expect(modelForSend(initial)).toBe("gpt/low");
  });

  it("Claude Code：默认项的名字不是模型名，没读到配置时只写 默认模型", () => {
    const models = [
      { id: "default", name: "默认（Claude Code 的设置）" },
      { id: "default/high", name: "默认 · high" },
      { id: "opus", name: "Opus" },
    ];
    const unknown = buildModelOptions({ models, default: "default", default_label: null });
    expect(unknown.options[0].label).toBe("默认模型");
    const known = buildModelOptions({ models, default: "default", default_label: "opus · high" });
    expect(known.options[0].label).toBe("默认（opus · high）");
    expect(known.options.map((o) => o.value)).toEqual(["default", "default/high", "opus"]);
  });

  it("配置的默认模型不在列表里：补一项，直接显示 id", () => {
    const { options, initial } = buildModelOptions({
      models: [{ id: "a/m1", name: "M1" }],
      default: "custom/my-model",
    });
    expect(initial).toBe("custom/my-model");
    expect(options[0].label).toBe("默认（custom/my-model）");
  });

  it("还没有目录时没有选项，触发器写 默认模型", () => {
    const { options, initial } = buildModelOptions(null);
    expect(options).toEqual([]);
    expect(initial).toBe("");
    expect(selectedModelText("", options)).toBe("默认模型");
  });

  it("换目录时保留用户选过、新目录里还有的模型", () => {
    const { options, initial } = buildModelOptions({
      models: [{ id: "a/m1", name: "M1" }],
      default: null,
    });
    expect(keepOrInitial("a/m1", options, initial)).toBe("a/m1");
    expect(keepOrInitial("gone/x", options, initial)).toBe(initial);
    expect(keepOrInitial("", options, initial)).toBe(initial);
  });
});

describe("模型目录缓存", () => {
  beforeEach(() => clearCatalogCache());

  it("键由智能体 + 安装路径 + 版本组成；没检测出安装时不取", () => {
    expect(catalogCacheKey("opencode", install)).toBe(
      "opencode|C:\tools\opencode\opencode.exe|1.19.1",
    );
    expect(catalogCacheKey("opencode", null)).toBeNull();
    expect(catalogCacheKey("opencode", undefined)).toBeNull();
    expect(catalogCacheKey("opencode", { ...install, version: "1.20.0" })).not.toBe(
      catalogCacheKey("opencode", install),
    );
  });

  it("同一个键只取一次（并发也只发一个请求）", async () => {
    const catalog: AgentModelCatalog = { models: [], default: null };
    const fetcher = vi.fn(async () => catalog);
    const [a, b] = await Promise.all([loadCatalog("k", fetcher), loadCatalog("k", fetcher)]);
    expect(a).toBe(catalog);
    expect(b).toBe(catalog);
    await loadCatalog("k", fetcher);
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(cachedCatalog("k")).toBe(catalog);
  });

  it("失败不缓存，下次再试", async () => {
    const fetcher = vi
      .fn<() => Promise<AgentModelCatalog>>()
      .mockRejectedValueOnce(new Error("还没起来"))
      .mockResolvedValueOnce({ models: [], default: null });
    await expect(loadCatalog("k", fetcher)).rejects.toThrow("还没起来");
    expect(cachedCatalog("k")).toBeNull();
    await expect(loadCatalog("k", fetcher)).resolves.toEqual({ models: [], default: null });
    expect(fetcher).toHaveBeenCalledTimes(2);
  });
});
