// Adapted from Netcatty lib/utils.ts
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** 条件类名 + Tailwind 冲突合并（shadcn 系组件的标准做法）。 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
