/** @vitest-environment jsdom */
//! 快捷命令管理面板（UI-10）：分组输入框是可输入的下拉、列表可搜索、按分组折叠、
//! 上移 / 下移只在分组内动、搜索时禁用。

import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import type { QuickCommand } from "../../state/quickCommands";
import QuickCommandsPanel from "./QuickCommandsPanel";

afterEach(cleanup);

beforeEach(() => {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  // Radix Popover 在 jsdom 里需要的
  Element.prototype.scrollIntoView = () => {};
});

const cmd = (id: string, group?: string, name = id, command = `run ${id}`): QuickCommand => ({
  id,
  name,
  command,
  group,
  enter: true,
});

// 全局顺序：a(甲) x(乙) b(甲) y(未分组) c(甲)
const LIST = [cmd("a", "甲"), cmd("x", "乙"), cmd("b", "甲"), cmd("y"), cmd("c", "甲")];

function setup(commands: QuickCommand[] = LIST, props: { initialEditId?: string | null } = {}) {
  const onChange = vi.fn();
  const view = render(
    <QuickCommandsPanel commands={commands} onChange={onChange} onClose={vi.fn()} onError={vi.fn()} {...props} />,
  );
  return { onChange, view };
}

const rows = () => [...document.querySelectorAll("[data-quick-command-row]")].map((el) => el.getAttribute("data-quick-command-row"));
const row = (id: string) => document.querySelector(`[data-quick-command-row="${id}"]`) as HTMLElement;
const group = (name: string) => document.querySelector(`[data-quick-command-group="${name}"]`) as HTMLElement;
const search = () => screen.getByLabelText("搜索快捷命令");
const idsOf = (list: QuickCommand[]) => list.map((item) => item.id);

describe("命令列表按分组折叠", () => {
  it("每个分组一个标题（带条数），组内保持命令栏里的顺序，未分组放最后", () => {
    setup();
    const headers = [...document.querySelectorAll("[data-quick-command-group] > button")].map((el) => el.textContent);
    expect(headers).toEqual(["甲3", "乙1", "未分组1"]);
    expect(rows()).toEqual(["a", "b", "c", "x", "y"]);
  });

  it("点分组标题折叠 / 展开，互不影响", () => {
    setup();
    const header = group("甲").querySelector("button") as HTMLElement;
    expect(header.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(header);
    expect(header.getAttribute("aria-expanded")).toBe("false");
    expect(rows()).toEqual(["x", "y"]);
    fireEvent.click(header);
    expect(rows()).toEqual(["a", "b", "c", "x", "y"]);
    fireEvent.click(group("none").querySelector("button") as HTMLElement);
    expect(rows()).toEqual(["a", "b", "c", "x"]);
  });

  it("一个分组都没有时不画分组标题，是平铺的列表", () => {
    setup([cmd("p"), cmd("q")]);
    expect(document.querySelectorAll("[data-quick-command-group]").length).toBe(0);
    expect(rows()).toEqual(["p", "q"]);
  });
});

describe("搜索", () => {
  it("按名字、命令内容、分组过滤，不分大小写；没有匹配时提示", () => {
    setup([
      cmd("a", "Debian", "端口查看", "ss -TLNP"),
      cmd("b", "Debian", "系统更新", "apt update"),
      cmd("c", undefined, "磁盘", "df -h"),
    ]);
    fireEvent.change(search(), { target: { value: "tlnp" } });
    expect(rows()).toEqual(["a"]);
    fireEvent.change(search(), { target: { value: "更新" } });
    expect(rows()).toEqual(["b"]);
    fireEvent.change(search(), { target: { value: "DEBIAN" } });
    expect(rows()).toEqual(["a", "b"]);
    fireEvent.change(search(), { target: { value: "没有这个" } });
    expect(rows()).toEqual([]);
    expect(screen.getByText("没有匹配的命令。")).toBeTruthy();
  });

  it("搜索时有匹配的分组自动展开，清掉搜索后恢复原来的折叠状态", () => {
    setup();
    fireEvent.click(group("甲").querySelector("button") as HTMLElement);
    expect(rows()).toEqual(["x", "y"]);
    fireEvent.change(search(), { target: { value: "run a" } });
    expect(rows()).toEqual(["a"]);
    expect(group("甲").querySelector("button")?.getAttribute("aria-expanded")).toBe("true");
    // 没有匹配的分组不显示
    expect(group("乙")).toBeNull();
    fireEvent.click(screen.getByLabelText("清空搜索"));
    expect(rows()).toEqual(["x", "y"]);
  });
});

describe("上移 / 下移只在分组内", () => {
  const disabled = (id: string, title: string) =>
    (within(row(id)).getByTitle(title) as HTMLButtonElement).disabled;

  it("组内第一条的上移、最后一条的下移置灰；组内只有一条两个都灰", () => {
    setup();
    expect(disabled("a", "上移")).toBe(true);
    expect(disabled("a", "下移")).toBe(false);
    expect(disabled("b", "上移")).toBe(false);
    expect(disabled("b", "下移")).toBe(false);
    expect(disabled("c", "上移")).toBe(false);
    expect(disabled("c", "下移")).toBe(true);
    // 乙、未分组都只有一条：跨分组不算
    expect(disabled("x", "上移")).toBe(true);
    expect(disabled("x", "下移")).toBe(true);
    expect(disabled("y", "上移")).toBe(true);
    expect(disabled("y", "下移")).toBe(true);
  });

  it("下移 / 上移：和同组相邻的一条交换，别的分组的命令原地不动", () => {
    const { onChange } = setup();
    fireEvent.click(within(row("a")).getByTitle("下移"));
    expect(idsOf(onChange.mock.calls[0][0])).toEqual(["b", "x", "a", "y", "c"]);
    fireEvent.click(within(row("c")).getByTitle("上移"));
    expect(idsOf(onChange.mock.calls[1][0])).toEqual(["a", "x", "c", "y", "b"]);
  });

  it("搜索框里有字时上移 / 下移全部禁用", () => {
    setup();
    fireEvent.change(search(), { target: { value: "run" } });
    for (const id of ["a", "b", "c"]) {
      expect(disabled(id, "上移（搜索时不可用）")).toBe(true);
      expect(disabled(id, "下移（搜索时不可用）")).toBe(true);
    }
  });
});

describe("分组输入框（可输入的下拉）", () => {
  const groupInput = () => screen.getByRole("combobox", { name: "分组" }) as HTMLInputElement;
  const options = () => [...document.querySelectorAll('[role="option"]')].map((el) => el.textContent);

  it("不再用浏览器自带的 datalist", () => {
    setup(LIST, { initialEditId: "new" });
    expect(document.querySelector("datalist")).toBeNull();
    expect(groupInput().getAttribute("list")).toBeNull();
  });

  it("点开列出已有分组，点一个就填进去", async () => {
    const { onChange } = setup(LIST, { initialEditId: "new" });
    fireEvent.change(screen.getByPlaceholderText("例如：端口查看"), { target: { value: "新命令" } });
    fireEvent.change(screen.getByPlaceholderText("ss -tlnp"), { target: { value: "ls" } });
    await act(async () => fireEvent.focus(groupInput()));
    expect(options()).toEqual(["甲", "乙"]);
    await act(async () => fireEvent.click(screen.getByRole("option", { name: "乙" })));
    expect(groupInput().value).toBe("乙");
    expect(screen.queryByRole("listbox")).toBeNull();
    fireEvent.click(screen.getByText("保存"));
    expect(onChange.mock.calls[0][0].at(-1)).toMatchObject({ name: "新命令", group: "乙" });
  });

  it("也能直接输入一个新分组名；输入时按文字过滤已有分组", async () => {
    const { onChange } = setup(LIST, { initialEditId: "new" });
    fireEvent.change(screen.getByPlaceholderText("例如：端口查看"), { target: { value: "新命令" } });
    fireEvent.change(screen.getByPlaceholderText("ss -tlnp"), { target: { value: "ls" } });
    await act(async () => fireEvent.change(groupInput(), { target: { value: "甲" } }));
    expect(options()).toEqual(["甲"]);
    await act(async () => fireEvent.change(groupInput(), { target: { value: "全新分组" } }));
    // 没有匹配的已有分组：不弹下拉，输入的值照样有效
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(groupInput().value).toBe("全新分组");
    fireEvent.click(screen.getByText("保存"));
    expect(onChange.mock.calls[0][0].at(-1)).toMatchObject({ name: "新命令", group: "全新分组" });
  });

  it("键盘：↓ 选中、Enter 填入；Esc 只收起下拉", async () => {
    setup(LIST, { initialEditId: "new" });
    await act(async () => fireEvent.focus(groupInput()));
    await act(async () => fireEvent.keyDown(groupInput(), { key: "ArrowDown" }));
    await act(async () => fireEvent.keyDown(groupInput(), { key: "ArrowDown" }));
    await act(async () => fireEvent.keyDown(groupInput(), { key: "Enter" }));
    expect(groupInput().value).toBe("乙");
    await act(async () => fireEvent.keyDown(groupInput(), { key: "ArrowDown" }));
    expect(screen.queryByRole("listbox")).not.toBeNull();
    await act(async () => fireEvent.keyDown(groupInput(), { key: "Escape" }));
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("键盘：还没选中时第一下 ↑ 落到最后一项（不跳过它）", async () => {
    setup(LIST, { initialEditId: "new" });
    await act(async () => fireEvent.focus(groupInput()));
    await act(async () => fireEvent.keyDown(groupInput(), { key: "ArrowUp" }));
    await act(async () => fireEvent.keyDown(groupInput(), { key: "Enter" }));
    expect(groupInput().value).toBe("乙");
  });

  it("编辑已有命令：当前分组在列表里打勾；没有任何分组时没有下拉箭头", async () => {
    const view = setup(LIST, { initialEditId: "a" });
    expect(groupInput().value).toBe("甲");
    await act(async () => fireEvent.click(screen.getByLabelText("展开选项")));
    expect(screen.getByRole("option", { name: "甲" }).getAttribute("aria-selected")).toBe("true");
    view.view.unmount();
    setup([cmd("p")], { initialEditId: "new" });
    expect(screen.queryByLabelText("展开选项")).toBeNull();
  });
});
