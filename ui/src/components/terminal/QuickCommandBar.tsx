//! 终端底部的快捷命令栏（像 Xshell 的快速命令）：一排按钮，点一下把命令发进终端。
//!
//! - 单击：发到当前焦点的窗格；Ctrl+单击（或这个标签开着输入广播）：发到这个标签的所有分屏
//! - 右键按钮：编辑 / 删除 / 发到所有分屏
//! - 双击或右键按钮之间 / 后面的空白处：新建一条快捷命令
//! - 最左边：管理快捷命令；分组切换；输入广播开关
//! - 上边框可以拖：默认一行（放不下横向滚动），拖高后按钮自动换行、放不下纵向滚动；
//!   双击拖动条恢复一行

import { Command, Radio, SquareTerminal } from "lucide-react";
import { useRef } from "react";

import { cn } from "../../lib/utils";
import type { QuickCommand } from "../../state/quickCommands";
import { Button } from "../ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "../ui/context-menu";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { QuickBarResizeHandle, useQuickBarHeight } from "./QuickBarResizeHandle";
import { QUICK_BAR_DEFAULT_HEIGHT } from "./quickBarHeight";

/** 分组下拉框里"全部"的值（Radix 的 Select 不收空串）。 */
const ALL_GROUPS = "__all__";

export interface QuickCommandBarProps {
  commands: QuickCommand[];
  groups: string[];
  group: string;
  onGroupChange: (group: string) => void;
  /** `all` = 发到这个标签的所有分屏。 */
  onRun: (command: QuickCommand, all: boolean) => void;
  onManage: () => void;
  /** 在空白处双击 / 右键：直接新建一条。 */
  onCreate: () => void;
  onEdit: (command: QuickCommand) => void;
  onDelete: (command: QuickCommand) => void;
  /** 这个标签开着输入广播（键盘输入同时发到所有分屏）。 */
  broadcast: boolean;
  /** 这个标签有几个分屏（只有一个时广播没意义，开关灰掉）。 */
  paneCount: number;
  onToggleBroadcast: () => void;
}

export default function QuickCommandBar({
  commands,
  groups,
  group,
  onGroupChange,
  onRun,
  onManage,
  onCreate,
  onEdit,
  onDelete,
  broadcast,
  paneCount,
  onToggleBroadcast,
}: QuickCommandBarProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const { height, areaHeight, onResize, onCommit, onReset } = useQuickBarHeight(barRef);
  // 比默认一行高：按钮换行排、纵向滚动；正好是默认高度时保持原来的单行横向滚动
  const tall = height > QUICK_BAR_DEFAULT_HEIGHT;

  return (
    <div
      ref={barRef}
      data-section="quick-command-bar"
      style={{ height }}
      className={cn(
        "relative flex shrink-0 gap-1 border-t border-border/60 px-1.5 text-xs",
        tall ? "items-start pt-0.5" : "items-center",
      )}
    >
      <QuickBarResizeHandle
        height={height}
        areaHeight={areaHeight}
        onResize={onResize}
        onCommit={onCommit}
        onReset={onReset}
      />
      <Button
        variant="ghost"
        size="icon"
        className="h-6 w-6 shrink-0"
        title="管理快捷命令"
        onClick={onManage}
      >
        <Command size={13} />
      </Button>
      {groups.length > 0 && (
        <Select
          value={group || ALL_GROUPS}
          onValueChange={(value) => onGroupChange(value === ALL_GROUPS ? "" : value)}
        >
          <SelectTrigger className="h-6 w-auto max-w-32 shrink-0 gap-1 px-2 text-xs" title="分组">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL_GROUPS} className="text-xs">
              全部
            </SelectItem>
            {groups.map((name) => (
              <SelectItem key={name} value={name} className="text-xs">
                {name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}

      {/* 默认一行排开，放不下就横向滚动（鼠标滚轮也能横着滚）；拖高后换行排、纵向滚动 */}
      <div
        ref={scrollRef}
        data-section="quick-command-list"
        className={cn(
          "flex min-w-0 flex-1 gap-x-0.5",
          tall
            ? "min-h-0 flex-wrap content-start self-stretch overflow-y-auto"
            : "items-center overflow-x-auto [scrollbar-width:none]",
        )}
        // 只认点在列表自己身上（按钮之间、后面的空白）：命令按钮上的双击 / 右键照旧
        onDoubleClick={(event) => {
          if (event.target === event.currentTarget) onCreate();
        }}
        onContextMenu={(event) => {
          if (event.target !== event.currentTarget) return;
          event.preventDefault();
          onCreate();
        }}
        title={commands.length > 0 ? "双击或右键空白处新建快捷命令" : undefined}
        onWheel={(event) => {
          const element = scrollRef.current;
          if (!tall && element && event.deltaY !== 0) element.scrollLeft += event.deltaY;
        }}
      >
        {commands.length === 0 ? (
          <button
            onClick={onManage}
            className="my-0.5 truncate rounded px-2 py-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            还没有快捷命令，点这里添加常用命令（例如查看端口、系统更新）
          </button>
        ) : (
          commands.map((command) => (
            <ContextMenu key={command.id}>
              <ContextMenuTrigger asChild>
                <button
                  data-quick-command={command.id}
                  onClick={(event) => onRun(command, event.ctrlKey || event.metaKey)}
                  title={`${command.command}${
                    command.enter === false ? "\n（只填入，不回车）" : ""
                  }\nCtrl+单击：发到所有分屏`}
                  className="my-0.5 flex max-w-40 shrink-0 items-center gap-1 rounded px-2 py-0.5 hover:bg-accent hover:text-accent-foreground"
                >
                  <SquareTerminal size={12} className="shrink-0 text-muted-foreground" />
                  <span className="truncate">{command.name}</span>
                </button>
              </ContextMenuTrigger>
              <ContextMenuContent className="w-44">
                <ContextMenuItem onSelect={() => onRun(command, false)}>发送</ContextMenuItem>
                <ContextMenuItem onSelect={() => onRun(command, true)}>
                  发到这个标签的所有分屏
                </ContextMenuItem>
                <ContextMenuSeparator />
                <ContextMenuItem onSelect={() => onEdit(command)}>编辑</ContextMenuItem>
                <ContextMenuItem
                  className="text-destructive focus:text-destructive-foreground"
                  onSelect={() => onDelete(command)}
                >
                  删除
                </ContextMenuItem>
              </ContextMenuContent>
            </ContextMenu>
          ))
        )}
      </div>

      <button
        data-section="broadcast-toggle"
        onClick={onToggleBroadcast}
        disabled={paneCount < 2 && !broadcast}
        title={
          paneCount < 2 && !broadcast
            ? "输入广播：先分屏（Ctrl+Shift+D / E）再开，键盘输入会同时发到所有分屏"
            : broadcast
              ? "正在广播：键盘输入同时发到这个标签的所有分屏（点一下关闭，Ctrl+Shift+B）"
              : "输入广播：键盘输入同时发到这个标签的所有分屏（Ctrl+Shift+B）"
        }
        className={cn(
          "flex shrink-0 items-center gap-1 rounded px-2 py-0.5 disabled:opacity-40",
          tall && "mt-1",
          broadcast
            ? "bg-amber-500/20 text-amber-600 hover:bg-amber-500/30"
            : "text-muted-foreground hover:bg-accent hover:text-foreground",
        )}
      >
        <Radio size={12} />
        {broadcast ? "广播中" : "广播"}
      </button>
    </div>
  );
}
