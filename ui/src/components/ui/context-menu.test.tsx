/** @vitest-environment jsdom */
//! 右键菜单的统一样式（UI-6 第 3 节）：12px 字、紧凑行高、14px 图标、危险项只变红不反白。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Trash2 } from "lucide-react";
import { afterEach, describe, expect, it } from "vitest";

import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "./context-menu";

afterEach(cleanup);

function openMenu() {
  render(
    <ContextMenu>
      <ContextMenuTrigger>
        <div>区域</div>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <ContextMenuItem>普通项</ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem destructive>
          <Trash2 /> 删除
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>,
  );
  fireEvent.contextMenu(screen.getByText("区域"));
}

describe("共享右键菜单样式", () => {
  it("菜单项：text-xs、py-1，没有 text-sm / py-1.5；菜单整体 text-xs", () => {
    openMenu();
    const item = screen.getByText("普通项");
    expect(item.className).toContain("text-xs");
    expect(item.className).toContain("py-1");
    expect(item.className).not.toContain("text-sm");
    expect(item.className).not.toContain("py-1.5");
    expect(screen.getByRole("menu").className).toContain("text-xs");
  });

  it("图标统一 14px（菜单项给内部 svg 设 size-3.5，不依赖各处自己写）", () => {
    openMenu();
    expect(screen.getByText("删除").className).toContain("[&_svg]:size-3.5");
  });

  it("危险项：红字，选中时是淡红底而不是反白", () => {
    openMenu();
    const danger = screen.getByText("删除").className;
    expect(danger).toContain("text-destructive");
    expect(danger).toContain("focus:bg-destructive/10");
    expect(danger).toContain("focus:text-destructive");
    expect(danger).not.toContain("destructive-foreground");
  });
});
