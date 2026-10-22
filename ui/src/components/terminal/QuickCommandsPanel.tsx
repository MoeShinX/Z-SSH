//! 管理快捷命令的侧面板（和代理、凭据面板同一种样式）：列表 + 选中后在下面编辑。

import { ArrowDown, ArrowUp, Pencil, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";

import { cn } from "../../lib/utils";
import {
  COMMANDS_MAX,
  groupsOf,
  moveQuickCommand,
  newQuickCommandId,
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
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
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
            显示在终端底部的命令栏里，顺序就是这里的顺序。单击发到当前窗格，Ctrl+单击发到这个标签的所有分屏。
          </p>
          <div className="space-y-1">
            {commands.length === 0 && (
              <p className="px-1 py-2 text-sm text-muted-foreground">
                还没有快捷命令，点右上角「新建命令」添加。
              </p>
            )}
            {commands.map((command, index) => {
              const selected = draft?.id === command.id;
              return (
                <div
                  key={command.id}
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
                    {command.group && <Badge variant="secondary">{command.group}</Badge>}
                  </button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-6 w-6"
                    title="上移"
                    disabled={index === 0}
                    onClick={() => onChange(moveQuickCommand(commands, command.id, -1))}
                  >
                    <ArrowUp size={12} />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-6 w-6"
                    title="下移"
                    disabled={index === commands.length - 1}
                    onClick={() => onChange(moveQuickCommand(commands, command.id, 1))}
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
              <Input
                value={draft.group}
                onChange={(event) => setDraft({ ...draft, group: event.target.value })}
                className="h-9"
                placeholder="例如：Debian、面板安装、测试脚本"
                list="quick-command-groups"
              />
              <datalist id="quick-command-groups">
                {groups.map((name) => (
                  <option key={name} value={name} />
                ))}
              </datalist>
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
