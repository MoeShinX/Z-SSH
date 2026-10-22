//! 设置页"下载 opencode"的完成处理（R11 用户反馈：下载完界面还是"版本过低"）。
//!
//! 抽成纯函数是为了能单测"成功 → 重新检测、失败 → 不检测只报错"这条规则，
//! 不用把整个设置页渲染起来。

import type { AgentInstall } from "../../api/agent";

export type DownloadOutcome =
  | { ok: true; install: AgentInstall }
  | { ok: false; error: string };

/** invoke 的拒绝值通常是字符串；万一是 Error 就取 message，避免显示成 "Error: …"。 */
export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * 下载，成功后调用 `recheck`（和"重新检测"按钮同一个入口）。
 *
 * 为什么下载返回了安装信息还要重新检测：界面上的"当前版本"来自发现流程
 *（设置里的手动路径优先、tools 目录 → PATH → 常见位置），下载结果不一定就是
 * 发现会选中的那一个；按同样的规则重跑一遍，显示的才是 AI 面板实际会用的。
 * 失败时不检测：旧状态仍然准确，只把错误交给调用方显示。
 */
export async function downloadAndRecheck(
  download: () => Promise<AgentInstall>,
  recheck: () => void,
): Promise<DownloadOutcome> {
  let install: AgentInstall;
  try {
    install = await download();
  } catch (error: unknown) {
    return { ok: false, error: errorText(error) };
  }
  recheck();
  return { ok: true, install };
}
