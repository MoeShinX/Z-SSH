//! 管理快捷命令的侧面板（和代理、凭据面板同一种样式）：列表 + 选中后在下面编辑。

import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, Pencil, Plus, Search, Trash2, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import { cn } from "../../lib/utils";
import {
  canMoveInGroup,
  COMMANDS_MAX,
  groupsOf,
  moveQuickCommandInGroup,
  newQuickCommandId,
  searchQuickCommands,
  sectionsOfQuickCommands,
  upsertQuickCommand,
  validateQuickCommand,
  type QuickCommand,
} from "../../state/quickCommands";
import {
  AsidePanel,
  AsidePanelContent,
  AsidePanelFooter,
  AsidePanelSection,
} from "../ui/aside-panel";
import { Button } from "../ui/button";
import { Combobox } from "../ui/combobox";
import { ConfirmDialog } from "../ui/confirm-dialog";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Switch } from "../ui/switch";
import { Textarea } from "../ui/textarea";

interface QuickCommandsPanelProps {
  commands: QuickCommand[];
  onChange: (commands: QuickCommand[]) => void;
  /** 打开时直接编辑这一条（栏上右键"编辑"进来的）；"new" = 直接新建。 */
  initialEditId?: string | null;
  onClose: () => void;
  onError: (message: string) => void;
}

interface Draft {
  id: string | null;
  name: string;
  group: string;
  command: string;
  enter: boolean;
}

const EMPTY: Draft = { id: null, name: "", group: "", command: "", enter: false };

function draftOf(command: QuickCommand): Draft {
  return {
    id: command.id,
    name: command.name,
    group: command.group ?? "",
    command: command.command,
    enter: command.enter !== false,
  };
}

export default function QuickCommandsPanel({
  commands,
  onChange,
  initialEditId = null,
  onClose,
  onError,
}: QuickCommandsPanelProps) {
  const [draft, setDraft] = useState<Draft | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<QuickCommand | null>(null);
  const [query, setQuery] = useState("");
  // 折起来的分组（"" = 未分组）；不持久化，每次打开面板都是全部展开
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());

  useEffect(() => {
    if (initialEditId === "new") setDraft({ ...EMPTY });
    else if (initialEditId) {
      const found = commands.find((item) => item.id === initialEditId);
      if (found) setDraft(draftOf(found));
    }
    // 只在打开（换了要编辑的那条）时设一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialEditId]);

  const groups = groupsOf(commands);
  const searching = query.trim() !== "";
  const sections = useMemo(() => sectionsOfQuickCommands(searchQuickCommands(commands, query)), [commands, query]);
  // 一个分组都没有时不画分组标题，和以前一样是一张平铺的列表
  const grouped = groups.length > 0;
  const toggleSection = (group: string) =>
    setCollapsed((current) => {
      const next = new Set(current);
      if (!next.delete(group)) next.add(group);
      return next;
    });

  const renderRow = (command: QuickCommand) => {
    const selected = draft?.id === command.id;
    // 上移 / 下移只在同一分组内动（跨分组没意义）；搜索时列表是过滤过的，不连续，一律禁用
    const [canUp, canDown] = canMoveInGroup(commands, command.id);
    return (
      <div
        key={command.id}
        data-quick-command-row={command.id}
        className={cn(
          "flex items-center gap-1 rounded-md border pr-1",
          selected ? "border-primary bg-primary/10" : "border-border/60 hover:bg-accent/40",
        )}
      >
        <button
          onClick={() => setDraft(draftOf(command))}
          className="flex min-w-0 flex-1 items-center gap-2 px-3 py-1.5 text-left"
        >
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm">{command.name}</span>
            <span className="block truncate font-mono text-xs text-muted-foreground">
              {command.command.split(/\r?\n/)[0]}
            </span>
          </span>
        </button>
        <Button
          variant="ghost"
          size="icon"
          className="h-6 w-6"
          title={searching ? "上移（搜索时不可用）" : "上移"}
          disabled={searching || !canUp}
          onClick={() => onChange(moveQuickCommandInGroup(commands, command.id, -1))}
        >
          <ArrowUp size={12} />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          className="h-6 w-6"
          title={searching ? "下移（搜索时不可用）" : "下移"}
          disabled={searching || !canDown}
          onClick={() => onChange(moveQuickCommandInGroup(commands, command.id, 1))}
        >
          <ArrowDown size={12} />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          className="h-6 w-6"
          title="编辑"
          onClick={() => setDraft(draftOf(command))}
        >
          <Pencil size={12} />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          className="h-6 w-6 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
          title="删除"
          onClick={() => setConfirmDelete(command)}
        >
          <Trash2 size={12} />
        </Button>
      </div>
    );
  };

  const save = () => {
    if (!draft) return;
    const problem = validateQuickCommand(draft);
    if (problem) {
      onError(problem);
      return;
    }
    if (!draft.id && commands.length >= COMMANDS_MAX) {
      onError(`快捷命令最多 ${COMMANDS_MAX} 条`);
      return;
    }
    const item: QuickCommand = {
      id: draft.id ?? newQuickCommandId(),
      name: draft.name.trim(),
      command: draft.command,
      group: draft.group.trim() || undefined,
      enter: draft.enter,
    };
    onChange(upsertQuickCommand(commands, item));
    setDraft(null);
  };

  return (
    <AsidePanel
      title="快捷命令"
      subtitle={`${commands.length} 条`}
      onClose={onClose}
      actions={
        <Button
          variant="outline"
          size="sm"
          className="h-7 gap-1"
          onClick={() => setDraft({ ...EMPTY })}
        >
          <Plus size={14} />
          新建命令
        </Button>
      }
    >
      <AsidePanelContent>
        <AsidePanelSection title="命令列表">
          <p className="text-xs text-muted-foreground">
            显示在终端底部的命令栏里，同一分组内的顺序就是命令栏里的顺序（上移 / 下移只在分组内移动）。单击发到当前窗格，Ctrl+单击发到这个标签的所有分屏。
          </p>
          {commands.length > 0 && (
            <div className="relative">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                data-section="quick-command-search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape" && query !== "") {
                    event.preventDefault();
                    event.stopPropagation();
                    setQuery("");
                  }
                }}
                className="h-8 pl-8 pr-8 text-[13px]"
                placeholder="搜索名称、命令或分组"
                aria-label="搜索快捷命令"
                spellCheck={false}
                autoComplete="off"
              />
              {query !== "" && (
                <button
                  type="button"
                  title="清空搜索"
                  aria-label="清空搜索"
                  onClick={() => setQuery("")}
                  className="absolute right-1.5 top-1/2 flex h-5 w-5 -translate-y-1/2 cursor-pointer items-center justify-center rounded text-muted-foreground hover:bg-foreground/10 hover:text-foreground"
                >
                  <X size={13} />
                </button>
              )}
            </div>
          )}
          <div className="space-y-1">
            {commands.length === 0 && (
              <p className="px-1 py-2 text-sm text-muted-foreground">
                还没有快捷命令，点右上角「新建命令」添加。
              </p>
            )}
            {commands.length > 0 && sections.length === 0 && (
              <p className="px-1 py-2 text-sm text-muted-foreground">没有匹配的命令。</p>
            )}
            {sections.map((section) => {
              if (!grouped) return section.items.map(renderRow);
              // 搜索时有匹配的分组自动展开（不动用户折起来的状态，清掉搜索后还是原样）
              const open = searching || !collapsed.has(section.group);
              return (
                <div key={section.group || "\u0000ungrouped"} data-quick-command-group={section.group || "none"} className="space-y-1">
                  <button
                    type="button"
                    aria-expanded={open}
                    onClick={() => toggleSection(section.group)}
                    className="flex w-full cursor-pointer items-center gap-1 rounded px-1 py-1 text-left text-xs font-medium text-muted-foreground hover:bg-accent/40 hover:text-foreground"
                  >
                    {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                    <span className="truncate">{section.group || "未分组"}</span>
                    <span className="ml-auto shrink-0 font-normal">{section.items.length}</span>
                  </button>
                  {open && section.items.map(renderRow)}
                </div>
              );
            })}
          </div>
        </AsidePanelSection>

        {draft && (
          <AsidePanelSection title={draft.id ? "编辑命令" : "新建命令"}>
            <div className="space-y-1.5">
              <Label>名称</Label>
              <Input
                value={draft.name}
                onChange={(event) => setDraft({ ...draft, name: event.target.value })}
                className="h-9"
                placeholder="例如：端口查看"
                autoFocus
              />
            </div>
            <div className="space-y-1.5">
              <Label>
                分组
                <span className="ml-2 text-xs font-normal text-muted-foreground">可选</span>
              </Label>
              <Combobox
                value={draft.group}
                onChange={(group) => setDraft({ ...draft, group })}
                options={groups}
                className="h-9"
                placeholder="例如：Debian、面板安装、测试脚本"
                aria-label="分组"
              />
            </div>
            <div className="space-y-1.5">
              <Label>命令</Label>
              <Textarea
                value={draft.command}
                onChange={(event) => setDraft({ ...draft, command: event.target.value })}
                className="min-h-[96px] font-mono text-xs"
                placeholder="ss -tlnp"
                spellCheck={false}
              />
              <p className="text-xs text-muted-foreground">可以写多行，每一行按回车执行一次。</p>
            </div>
            <div className="flex items-center justify-between gap-2">
              <div>
                <Label>发送后回车执行</Label>
                <p className="text-xs text-muted-foreground">关掉就只填进去，改完自己按回车。</p>
              </div>
              <Switch
                checked={draft.enter}
                onCheckedChange={(checked) => setDraft({ ...draft, enter: checked })}
              />
            </div>
          </AsidePanelSection>
        )}
      </AsidePanelContent>

      {draft && (
        <AsidePanelFooter className="flex items-center justify-end gap-2">
          <Button variant="outline" size="sm" onClick={() => setDraft(null)}>
            取消
          </Button>
          <Button size="sm" onClick={save}>
            保存
          </Button>
        </AsidePanelFooter>
      )}

      {confirmDelete && (
        <ConfirmDialog
          open
          title={`删除快捷命令「${confirmDelete.name}」？`}
          confirmLabel="删除"
          destructive
          onOpenChange={(open) => !open && setConfirmDelete(null)}
          onConfirm={() => {
            const target = confirmDelete;
            setConfirmDelete(null);
            if (draft?.id === target.id) setDraft(null);
            onChange(commands.filter((item) => item.id !== target.id));
          }}
        />
      )}
    </AsidePanel>
  );
}
