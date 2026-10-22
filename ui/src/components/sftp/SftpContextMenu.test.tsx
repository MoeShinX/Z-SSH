/** @vitest-environment jsdom */
//! 右键菜单组件（M3-5 第 1 节）：禁用项的悬停原因、点击、键盘导航、子菜单、不超出屏幕。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SftpEntry } from "../../api/sftp";
import { buildMenu } from "../../state/sftpMenu";
import SftpContextMenu from "./SftpContextMenu";

const ALL_ON = { builtinEditor: true, clipboard: true, properties: true };

function file(name: string): SftpEntry {
  return {
    name,
    path: `/srv/${name}`,
    kind: "file",
    name_is_utf8: true,
    hidden: false,
    size: 10,
  };
}

function nodesFor(entries: SftpEntry[], target: "items" | "blank" = "items") {
  return buildMenu({
    side: "remote",
    target,
    entries,
    currentDir: "/srv",
    clipboard: null,
    features: ALL_ON,
  });
}

/** jsdom 量不出尺寸：让菜单量出来是 220×480，屏幕 1000×700。 */
function fakeLayout(menu = { width: 220, height: 480 }) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    value: 1000,
  });
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    value: 700,
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      const isMenu = this.getAttribute("role") === "menu";
      return {
        width: isMenu ? menu.width : 200,
        height: isMenu ? menu.height : 28,
        left: 0,
        top: 0,
        right: isMenu ? menu.width : 200,
        bottom: isMenu ? menu.height : 28,
        x: 0,
        y: 0,
        toJSON() {},
      } as DOMRect;
    },
  );
}

beforeEach(() => fakeLayout());
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const item = (id: string) =>
  document.querySelector(`[data-menu-id="${id}"]`) as HTMLElement;

function renderMenu(
  nodes = nodesFor([file("a.conf")]),
  overrides: Partial<React.ComponentProps<typeof SftpContextMenu>> = {},
) {
  const props = {
    nodes,
    x: 50,
    y: 60,
    onSelect: vi.fn(),
    onClose: vi.fn(),
    ...overrides,
  };
  render(<SftpContextMenu {...props} />);
  return props;
}

describe("SftpContextMenu", () => {
  it("点一个可用项：回调拿到它的动作名；点禁用项：什么也不发生", () => {
    const props = renderMenu();
    fireEvent.click(item("rename"));
    expect(props.onSelect).toHaveBeenLastCalledWith("rename");
    // 剪贴板是空的：粘贴禁用。
    fireEvent.click(item("paste"));
    expect(props.onSelect).toHaveBeenCalledTimes(1);
  });

  it("禁用项悬停显示原因（title），可用项没有", () => {
    renderMenu(nodesFor([file("a.conf"), file("b.conf")]));
    expect(item("rename").getAttribute("title")).toBe("只能对单个条目操作");
    expect(item("rename").getAttribute("aria-disabled")).toBe("true");
    expect(item("paste").getAttribute("title")).toBe("剪贴板是空的");
    expect(item("delete").getAttribute("title")).toBeNull();
  });

  it("空白处的菜单只有 粘贴、新建、刷新、属性", () => {
    renderMenu(nodesFor([], "blank"));
    const ids = [...document.querySelectorAll("[data-menu-id]")].map((node) =>
      node.getAttribute("data-menu-id"),
    );
    expect(ids).toEqual(["paste", "new", "refresh", "properties"]);
  });

  it("快捷键写在菜单项右边", () => {
    renderMenu();
    expect(item("copy").textContent).toContain("Ctrl+C");
    expect(item("properties").textContent).toContain("Alt+Enter");
    expect(item("delete").textContent).toContain("Del");
  });

  it("「新建」是子菜单：悬停展开，选「文件夹」得到 mkdir", () => {
    const props = renderMenu();
    expect(item("mkdir")).toBeNull();
    fireEvent.mouseEnter(item("new"));
    expect(item("mkdir")).not.toBeNull();
    fireEvent.click(item("mkdir"));
    expect(props.onSelect).toHaveBeenLastCalledWith("mkdir");
  });

  it("键盘：向下移到第一个可用项，回车执行", () => {
    const props = renderMenu();
    const menu = screen.getByTestId("sftp-context-menu");
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    fireEvent.keyDown(menu, { key: "Enter" });
    expect(props.onSelect).toHaveBeenLastCalledWith("open");
  });

  it("键盘：向上从头回绕到最后一个可用项（属性）", () => {
    const props = renderMenu();
    const menu = screen.getByTestId("sftp-context-menu");
    fireEvent.keyDown(menu, { key: "ArrowUp" });
    fireEvent.keyDown(menu, { key: "Enter" });
    expect(props.onSelect).toHaveBeenLastCalledWith("properties");
  });

  it("键盘：子菜单可达——移到「新建」，右键展开，下键、回车选「文件」，Esc 先关子菜单", () => {
    const props = renderMenu();
    const menu = screen.getByTestId("sftp-context-menu");
    // 逐项往下走，直到停在「新建」上。
    for (
      let step = 0;
      step < 30 && item("new").getAttribute("data-active") !== "true";
      step += 1
    ) {
      fireEvent.keyDown(menu, { key: "ArrowDown" });
    }
    expect(item("new").getAttribute("data-active")).toBe("true");
    fireEvent.keyDown(menu, { key: "ArrowRight" });
    const sub = screen.getByTestId("sftp-context-submenu");
    expect(item("mkdir")).not.toBeNull();
    fireEvent.keyDown(sub, { key: "ArrowDown" });
    fireEvent.keyDown(sub, { key: "Enter" });
    expect(props.onSelect).toHaveBeenLastCalledWith("create");

    // Esc：先关子菜单，菜单还在；再按一次才关菜单。
    fireEvent.keyDown(sub, { key: "Escape" });
    expect(screen.queryByTestId("sftp-context-submenu")).toBeNull();
    expect(props.onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it("点菜单外面（遮罩）关闭菜单", () => {
    const props = renderMenu();
    fireEvent.click(screen.getByTestId("menu-backdrop"));
    expect(props.onClose).toHaveBeenCalledTimes(1);
  });

  it("点在屏幕右下角：菜单完整落在屏幕里（右边往左挪，下边翻到点击点上方）", () => {
    renderMenu(nodesFor([file("a")]), { x: 995, y: 695 });
    const menu = screen.getByTestId("sftp-context-menu");
    const left = parseFloat(menu.style.left);
    const top = parseFloat(menu.style.top);
    expect(left + 220).toBeLessThanOrEqual(1000);
    expect(top + 480).toBeLessThanOrEqual(700);
    expect(left).toBeGreaterThanOrEqual(0);
    expect(top).toBeGreaterThanOrEqual(0);
  });

  it("菜单比屏幕还高：限制高度，内部滚动", () => {
    vi.restoreAllMocks();
    fakeLayout({ width: 220, height: 900 });
    renderMenu(nodesFor([file("a")]), { x: 100, y: 100 });
    const menu = screen.getByTestId("sftp-context-menu");
    expect(parseFloat(menu.style.maxHeight)).toBeLessThanOrEqual(700);
    expect(
      parseFloat(menu.style.top) + parseFloat(menu.style.maxHeight),
    ).toBeLessThanOrEqual(700);
  });
});
