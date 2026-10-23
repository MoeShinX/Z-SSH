/** @vitest-environment jsdom */
//! 快捷命令管理面板（UI-10）：分组输入框是可输入的下拉、列表可搜索、按分组折叠、
//! 上移 / 下移只在分组内动、搜索时禁用。

import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import type { QuickCommand } from "../../state/quickCommands";
import QuickCommandBar from "./QuickCommandBar";
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

  it("搜索时分组标题不能折叠（点了没反应，清空搜索后也不会突然折起来）", () => {
    setup();
    fireEvent.change(search(), { target: { value: "run a" } });
    const header = group("甲").querySelector("button") as HTMLButtonElement;
    expect(header.disabled).toBe(true);
    fireEvent.click(header);
    fireEvent.click(screen.getByLabelText("清空搜索"));
    expect(group("甲").querySelector("button")?.getAttribute("aria-expanded")).toBe("true");
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

describe("颜色", () => {
  const radios = () => screen.getAllByRole("radio");
  const radio = (name: string) => screen.getByRole("radio", { name });
  const dotOf = (id: string) => row(id).querySelector("[data-color-dot]") as HTMLElement;
  const barButton = (id: string) => document.querySelector(`[data-quick-command="${id}"]`) as HTMLElement;

  /** 面板和命令栏共用一份列表，像 App 里一样。 */
  function Both({ initial, initialEditId }: { initial: QuickCommand[]; initialEditId?: string }) {
    const [list, setList] = useState(initial);
    return (
      <>
        <QuickCommandsPanel
          commands={list}
          onChange={setList}
          initialEditId={initialEditId}
          onClose={vi.fn()}
          onError={vi.fn()}
        />
        <QuickCommandBar
          commands={list}
          groups={[]}
          group=""
          onGroupChange={vi.fn()}
          onRun={vi.fn()}
          onManage={vi.fn()}
          onCreate={vi.fn()}
          onEdit={vi.fn()}
          onDelete={vi.fn()}
          broadcast={false}
          paneCount={1}
          onToggleBroadcast={vi.fn()}
        />
      </>
    );
  }

  it("表单里一排色块：默认 + 8 个预设，radiogroup，新建时选中「默认」", () => {
    setup(LIST, { initialEditId: "new" });
    expect(screen.getByRole("radiogroup", { name: "颜色" })).toBeTruthy();
    expect(radios().map((el) => el.getAttribute("aria-label"))).toEqual([
      "默认", "红", "橙", "黄", "绿", "青", "蓝", "紫", "粉",
    ]);
    expect(radio("默认").getAttribute("aria-checked")).toBe("true");
    expect(radios().filter((el) => el.getAttribute("aria-checked") === "true").length).toBe(1);
  });

  it("选颜色后保存：onChange 带上 color", () => {
    const { onChange } = setup([cmd("a")], { initialEditId: "a" });
    fireEvent.click(radio("蓝"));
    expect(radio("蓝").getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByText("保存"));
    expect(onChange.mock.calls[0][0][0]).toMatchObject({ id: "a", color: "blue" });
  });

  it("编辑已有颜色的命令：当前色选中；改回默认保存后 color 去掉", () => {
    const { onChange } = setup([{ ...cmd("a"), color: "blue" }], { initialEditId: "a" });
    expect(radio("蓝").getAttribute("aria-checked")).toBe("true");
    fireEvent.click(radio("默认"));
    fireEvent.click(screen.getByText("保存"));
    expect(onChange.mock.calls[0][0][0].color).toBeUndefined();
  });

  it("保存后列表有小圆点、命令栏按钮带样式；改回默认后都去掉", () => {
    render(<Both initial={[cmd("a"), cmd("b")]} initialEditId="a" />);
    expect(dotOf("a").getAttribute("data-color-dot")).toBe("none");
    expect(barButton("a").className).toContain("hover:bg-accent");
    fireEvent.click(radio("绿"));
    fireEvent.click(screen.getByText("保存"));
    expect(dotOf("a").className).toContain("bg-green-500");
    expect(dotOf("a").getAttribute("data-color-dot")).toBe("green");
    expect(barButton("a").className).toContain("bg-green-500/15");
    // 没改的那条不受影响
    expect(dotOf("b").getAttribute("data-color-dot")).toBe("none");
    expect(barButton("b").className).not.toContain("green");

    fireEvent.click(row("a").querySelector('button[title="编辑"]') as HTMLElement);
    fireEvent.click(radio("默认"));
    fireEvent.click(screen.getByText("保存"));
    expect(dotOf("a").getAttribute("data-color-dot")).toBe("none");
    expect(dotOf("a").className).not.toContain("bg-green");
    expect(barButton("a").className).not.toContain("green");
    expect(barButton("a").className).toContain("hover:bg-accent");
  });

  it("键盘：只有选中的色块在 Tab 顺序里；方向键切换并选中（首尾循环）", () => {
    setup([cmd("a")], { initialEditId: "a" });
    expect(radios().filter((el) => el.tabIndex === 0).length).toBe(1);
    expect(radio("默认").tabIndex).toBe(0);
    radio("默认").focus();
    fireEvent.keyDown(radio("默认"), { key: "ArrowRight" });
    expect(radio("红").getAttribute("aria-checked")).toBe("true");
    expect(document.activeElement).toBe(radio("红"));
    expect(radio("红").tabIndex).toBe(0);
    expect(radio("默认").tabIndex).toBe(-1);
    fireEvent.keyDown(radio("红"), { key: "ArrowLeft" });
    expect(radio("默认").getAttribute("aria-checked")).toBe("true");
    fireEvent.keyDown(radio("默认"), { key: "ArrowLeft" });
    expect(radio("粉").getAttribute("aria-checked")).toBe("true");
    fireEvent.keyDown(radio("粉"), { key: "ArrowDown" });
    expect(radio("默认").getAttribute("aria-checked")).toBe("true");
  });
});
