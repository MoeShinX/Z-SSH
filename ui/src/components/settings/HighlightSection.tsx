//! 设置页"终端高亮"卡片：开关 + 规则列表（关键词或正则、颜色、涂背景）。
//!
//! 只标屏幕上看得见的行，不改输出流（见 terminal/highlight.ts）。默认关。

import { ListPlus, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";

import { useSettings } from "../../stores/settings";
import {
  HIGHLIGHT_COLORS,
  MAX_RULES,
  parseRules,
  ruleError,
  type HighlightRule,
} from "../../terminal/highlight";
import { missingSamples, SAMPLE_RULES } from "../../terminal/highlightSamples";
import { cn } from "../../lib/utils";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import { SectionCard, SettingRow } from "./settingsUi";

const SAMPLE_LABELS: Record<string, string> = {
  "sample-error": "错误",
  "sample-warn": "警告",
  "sample-ok": "成功",
  "sample-ipv4": "IP 地址",
  "sample-url": "网址",
};

function sampleLabel(rule: HighlightRule): string {
  return SAMPLE_LABELS[rule.id] ?? rule.pattern;
}

function newId(): string {
  return `h${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

export default function HighlightSection() {
  const enabled = useSettings((state) => state.terminal_highlight_enabled);
  const stored = useSettings((state) => state.terminal_highlight_rules);
  const update = useSettings((state) => state.update);
  // 本地草稿：正在输入的正则可能还没写完（编译不了），先在这里改，失焦或改选项时才存
  const [rules, setRules] = useState<HighlightRule[]>(() => parseRules(stored));
  useEffect(() => setRules(parseRules(stored)), [stored]);

  const save = (next: HighlightRule[]) => {
    setRules(next);
    void update("terminal_highlight_rules", JSON.stringify(next.filter((rule) => rule.pattern !== "")));
  };
  const patch = (id: string, change: Partial<HighlightRule>, persist: boolean) => {
    const next = rules.map((rule) => (rule.id === id ? { ...rule, ...change } : rule));
    if (persist) save(next);
    else setRules(next);
  };

  const toggle = (checked: boolean) => {
    void update("terminal_highlight_enabled", checked);
    if (checked && rules.length === 0) save(SAMPLE_RULES);
  };

  return (
    <SectionCard
      title="关键词高亮"
      description="把终端里看得见的关键词标上颜色（比如 error、warning）。只标屏幕上显示的那几十行，不改输出内容，大量输出时也不拖慢终端。vim、less 这类全屏程序里不标。"
    >
      <SettingRow title="开启高亮">
        <Switch checked={enabled} onCheckedChange={toggle} aria-label="开启高亮" />
      </SettingRow>
      {enabled && (
        <div className="space-y-2 pt-1" data-testid="highlight-rules">
          {rules.map((rule) => {
            const error = rule.pattern === "" ? null : ruleError(rule);
            return (
              <div key={rule.id} className="space-y-1" data-rule-id={rule.id}>
                <div className="flex items-center gap-2">
                  <Switch
                    checked={rule.enabled}
                    onCheckedChange={(checked) => patch(rule.id, { enabled: checked }, true)}
                    aria-label={rule.enabled ? "关闭这条规则" : "开启这条规则"}
                    title={rule.enabled ? "这条规则开着（点一下关闭）" : "这条规则关着（点一下开启）"}
                  />
                  <Input
                    value={rule.pattern}
                    placeholder={rule.regex ? "正则，例如 error|fail" : "关键词"}
                    aria-label="高亮规则"
                    onChange={(event) => patch(rule.id, { pattern: event.target.value }, false)}
                    onBlur={() => save(rules)}
                    className={cn("h-8 flex-1 font-mono text-xs", !rule.enabled && "opacity-50")}
                    style={{ color: rule.background ? undefined : rule.color }}
                  />
                  <Select value={rule.color} onValueChange={(color) => patch(rule.id, { color }, true)}>
                    <SelectTrigger className="h-8 w-20" aria-label="颜色">
                      <span className="inline-block h-3 w-3 rounded-full" style={{ background: rule.color }} />
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {HIGHLIGHT_COLORS.map((color) => (
                        <SelectItem key={color.value} value={color.value}>
                          {color.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <label className="flex items-center gap-1 text-xs" title="按正则表达式匹配">
                    <input
                      type="checkbox"
                      checked={rule.regex}
                      onChange={(event) => patch(rule.id, { regex: event.target.checked }, true)}
                    />
                    正则
                  </label>
                  <label className="flex items-center gap-1 text-xs" title="区分大小写">
                    <input
                      type="checkbox"
                      checked={rule.caseSensitive}
                      onChange={(event) => patch(rule.id, { caseSensitive: event.target.checked }, true)}
                    />
                    Aa
                  </label>
                  <label className="flex items-center gap-1 text-xs" title="涂背景色（不勾只改文字颜色）">
                    <input
                      type="checkbox"
                      checked={rule.background}
                      onChange={(event) => patch(rule.id, { background: event.target.checked }, true)}
                    />
                    背景
                  </label>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    title="删除"
                    onClick={() => save(rules.filter((item) => item.id !== rule.id))}
                  >
                    <Trash2 size={13} />
                  </Button>
                </div>
                {error && <p className="text-xs text-red-500">{error}（这条先不生效）</p>}
              </div>
            );
          })}
          <Button
            size="sm"
            variant="outline"
            className="gap-1"
            disabled={rules.length >= MAX_RULES}
            onClick={() =>
              setRules([
                ...rules,
                {
                  id: newId(),
                  pattern: "",
                  regex: false,
                  caseSensitive: false,
                  color: HIGHLIGHT_COLORS[rules.length % HIGHLIGHT_COLORS.length].value,
                  background: false,
                  enabled: true,
                },
              ])
            }
          >
            <Plus size={13} /> 添加规则
          </Button>
          {missingSamples(rules).length > 0 && (
            <Button
              size="sm"
              variant="ghost"
              className="ml-1 gap-1 text-muted-foreground"
              data-action="add-sample-rules"
              title={`补上还没有的常用规则：${missingSamples(rules).map(sampleLabel).join("、")}`}
              disabled={rules.length >= MAX_RULES}
              onClick={() => save([...rules, ...missingSamples(rules)].slice(0, MAX_RULES))}
            >
              <ListPlus size={13} /> 添加常用规则
            </Button>
          )}
        </div>
      )}
    </SectionCard>
  );
}
