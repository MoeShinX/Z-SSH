//! 终端标签那一行：终端｜AI｜SFTP 从左到右固定（M3-3 第 1 节，用户 2026-10-17 决定）。
//!
//! 顺序只在这里定一次：谁开了谁渲染，没开的传 `null`，位置不会变。

import type { ReactNode } from "react";

export default function DockRow({
  main,
  ai,
  sftp,
  after = null,
}: {
  main: ReactNode;
  ai: ReactNode;
  sftp: ReactNode;
  /** 右边其他的侧栏（主机编辑、凭据、代理）：不属于三块同屏，排在最后。 */
  after?: ReactNode;
}) {
  return (
    <div data-section="dock-row" className="flex min-h-0 flex-1">
      {main}
      {ai}
      {sftp}
      {after}
    </div>
  );
}
