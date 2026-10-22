// AI 执行模式的三档（后端 ExecMode：manual / readonly / auto）。面板和设置页共用这份文案。

import type { ExecMode } from "../stores/settings";

export const EXEC_MODE_OPTIONS: { value: ExecMode; label: string; hint: string }[] = [
  { value: "manual", label: "逐条确认", hint: "每条命令都要你点确认。" },
  {
    value: "readonly",
    label: "只读自动",
    hint: "查看类命令（ls、cat、systemctl status 等）直接执行，会改动系统的命令和写文件要确认。",
  },
  { value: "auto", label: "全部放行", hint: "所有命令直接执行，不再确认（危险命令也一样）。" },
];

/** 设置里存的值 → 三档之一；不认识的按逐条确认。 */
export function toExecMode(value: string): ExecMode {
  return value === "auto" || value === "readonly" ? value : "manual";
}
