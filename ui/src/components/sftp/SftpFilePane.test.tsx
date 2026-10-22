/** @vitest-environment jsdom */
//! 双栏里一栏的组件测试（M3-2 第 6.1 节）：
//! - 虚拟列表：20000 条只渲染可视区附近的一小段，滚到底能看到最后一条；
//! - 多选和键盘：Ctrl+A、Delete、F2、F5、Backspace、Enter 各自走到对应的回调；
//! - 非 UTF-8 名字：标灰、不能拖（已知限制）。

import { act, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type { SftpEntry } from "../../api/sftp";
import SftpFilePane, { type SftpFilePaneProps } from "./SftpFilePane";

beforeAll(() => {
  // jsdom 没有 ResizeObserver，也量不出高度：给一个固定的可视区高度。
  class FakeResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get() {
      return 600;
    },
  });
});

afterEach(() => {
  vi.clearAllMocks();
});

function entry(name: string, patch: Partial<SftpEntry> = {}): SftpEntry {
  return {
    name,
    path: `/data/${name}`,
    kind: "file",
    name_is_utf8: true,
    hidden: false,
    ...patch,
  };
}

function manyEntries(count: number): SftpEntry[] {
  return Array.from({ length: count }, (_, index) =>
    entry(`file-${String(index).padStart(5, "0")}.txt`, {
      size: index,
      mtime: 1_700_000_000 + index,
    }),
  );
}

function baseProps(entries: SftpEntry[]): SftpFilePaneProps {
  return {
    side: "remote",
    path: "/data/sub",
    loading: false,
    error: null,
    entries,
    filter: { text: "", showHidden: false },
    order: { key: "name", descending: false },
    focused: true,
    onPathChange: vi.fn(),
    onFilterChange: vi.fn(),
    onOrderChange: vi.fn(),
    onRefresh: vi.fn(),
    onFocusPane: vi.fn(),
    onActivate: vi.fn(),
    onContextMenu: vi.fn(),
    onDropInto: vi.fn(),
    onDragStart: vi.fn(),
    onDeleteSelection: vi.fn(),
    onRenameSelection: vi.fn(),
    onBrowse: vi.fn(),
  };
}

const rowsIn = (container: HTMLElement) =>
  container.querySelectorAll("[data-row]");
const listOf = (container: HTMLElement) =>
  container.querySelector("[tabindex]") as HTMLElement;

describe("虚拟列表：20000 条不卡", () => {
  it("只渲染可视区附近的一小段，状态栏仍然写 20000 项", () => {
    const { container, getByText } = render(
      <SftpFilePane {...baseProps(manyEntries(20_000))} />,
    );
    const rendered = rowsIn(container).length;
    expect(rendered).toBeGreaterThan(0);
    // 600px / 26px ≈ 24 行 + 上下各 8 行余量；远小于 20000。
    expect(rendered).toBeLessThan(60);
    expect(getByText("20000 项")).toBeTruthy();
  });

  it("滚到底：最后一条渲染出来，第一条不再渲染", () => {
    const { container } = render(
      <SftpFilePane {...baseProps(manyEntries(20_000))} />,
    );
    const list = listOf(container);
    act(() => {
      list.scrollTop = 20_000 * 26;
      fireEvent.scroll(list);
    });
    const texts = Array.from(rowsIn(container)).map(
      (row) => row.textContent ?? "",
    );
    expect(texts.some((text) => text.includes("file-19999.txt"))).toBe(true);
    expect(texts.some((text) => text.includes("file-00000.txt"))).toBe(false);
    expect(rowsIn(container).length).toBeLessThan(60);
  });

  it("20000 条首次渲染的耗时有个粗门槛（回归护栏）", () => {
    const started = performance.now();
    render(<SftpFilePane {...baseProps(manyEntries(20_000))} />);
    // 排序 + 过滤 20000 条 + 渲染一屏：保守门槛 3 秒（真卡的时候会差一个数量级）。
    expect(performance.now() - started).toBeLessThan(3000);
  });
});

describe("多选和键盘", () => {
  const sample = () => [
    entry("sub", { kind: "dir", path: "/data/sub/sub" }),
    entry("a.txt"),
    entry("b.txt"),
    entry("c.txt"),
  ];

  it("Ctrl+A 全选，状态栏写出选中数", () => {
    const { container, getByText } = render(
      <SftpFilePane {...baseProps(sample())} />,
    );
    fireEvent.keyDown(listOf(container), { key: "a", ctrlKey: true });
    expect(getByText("选中 4 项")).toBeTruthy();
  });

  it("Delete：把选中的条目交给删除回调", () => {
    const props = baseProps(sample());
    const { container } = render(<SftpFilePane {...props} />);
    fireEvent.keyDown(listOf(container), { key: "a", ctrlKey: true });
    fireEvent.keyDown(listOf(container), { key: "Delete" });
    expect(props.onDeleteSelection).toHaveBeenCalledTimes(1);
    const names = vi
      .mocked(props.onDeleteSelection)
      .mock.calls[0][0].map((item) => item.name);
    expect(names.sort()).toEqual(["a.txt", "b.txt", "c.txt", "sub"]);
  });

  it("没选中任何东西时 Delete 不触发回调", () => {
    const props = baseProps(sample());
    const { container } = render(<SftpFilePane {...props} />);
    fireEvent.keyDown(listOf(container), { key: "Delete" });
    expect(props.onDeleteSelection).not.toHaveBeenCalled();
  });

  it("F2 重命名当前条目、F5 刷新、Backspace 返回上级、Enter 进入", () => {
    const props = baseProps(sample());
    const { container } = render(<SftpFilePane {...props} />);
    const list = listOf(container);

    fireEvent.keyDown(list, { key: "F2" });
    expect(props.onRenameSelection).toHaveBeenCalledTimes(1);
    // 排序后目录在前：当前焦点是第一行 sub。
    expect(vi.mocked(props.onRenameSelection).mock.calls[0][0].name).toBe(
      "sub",
    );

    fireEvent.keyDown(list, { key: "F5" });
    expect(props.onRefresh).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(list, { key: "Backspace" });
    expect(props.onPathChange).toHaveBeenCalledWith("/data");

    fireEvent.keyDown(list, { key: "Enter" });
    expect(props.onActivate).toHaveBeenCalledTimes(1);
    expect(vi.mocked(props.onActivate).mock.calls[0][0].name).toBe("sub");
  });

  it("方向键下移再 Enter：进的是下面那一条", () => {
    const props = baseProps(sample());
    const { container } = render(<SftpFilePane {...props} />);
    const list = listOf(container);
    fireEvent.keyDown(list, { key: "ArrowDown" });
    fireEvent.keyDown(list, { key: "Enter" });
    expect(vi.mocked(props.onActivate).mock.calls[0][0].name).toBe("a.txt");
  });

  it("远端根目录上 Backspace 还是根（没有上级）", () => {
    const props = { ...baseProps(sample()), path: "/" };
    const { container } = render(<SftpFilePane {...props} />);
    fireEvent.keyDown(listOf(container), { key: "Backspace" });
    expect(props.onPathChange).toHaveBeenCalledWith("/");
  });

  it("Ctrl+点击 / Shift+点击 多选", () => {
    const { container, getByText } = render(
      <SftpFilePane {...baseProps(sample())} />,
    );
    const rows = rowsIn(container);
    fireEvent.click(rows[1]);
    fireEvent.click(rows[3], { ctrlKey: true });
    expect(getByText("选中 2 项")).toBeTruthy();
    fireEvent.click(rows[0]);
    fireEvent.click(rows[2], { shiftKey: true });
    expect(getByText("选中 3 项")).toBeTruthy();
  });
});

describe("非 UTF-8 名字：标灰、禁操作（已知限制）", () => {
  it("行是灰的、带说明、不能拖", () => {
    const entries = [
      entry("正常.txt"),
      entry("\uFFFD\uFFFD.txt", { name_is_utf8: false }),
    ];
    const { container } = render(<SftpFilePane {...baseProps(entries)} />);
    const rows = Array.from(rowsIn(container));
    const bad = rows.find((row) =>
      (row.textContent ?? "").includes("暂不支持操作"),
    );
    const good = rows.find((row) =>
      (row.textContent ?? "").includes("正常.txt"),
    );
    expect(bad).toBeTruthy();
    expect(bad?.getAttribute("data-draggable")).toBe("false");
    expect(bad?.className).toContain("text-muted-foreground");
    expect(good?.getAttribute("data-draggable")).toBe("true");
    expect(bad?.getAttribute("title")).toContain("不是 UTF-8");
  });
});

describe("列：表头和数据行共用同一份列定义；权限和所有者分开", () => {
  it("表头每一列在数据行里都有同名同类的单元格", () => {
    const entries = [
      entry("a.txt", {
        size: 5,
        mtime: 1_700_000_000,
        permissions: 0o644,
        owner: "alice",
      }),
    ];
    const { container } = render(<SftpFilePane {...baseProps(entries)} />);
    const header = container.querySelector('[data-role="col-header"]');
    const row = container.querySelector("[data-row]");
    const ids = Array.from(header?.querySelectorAll("[data-col]") ?? []).map(
      (el) => el.getAttribute("data-col"),
    );
    expect(ids).toEqual(["name", "size", "mtime", "kind", "perms", "owner"]);
    for (const id of ids) {
      expect(row?.querySelector(`[data-col="${id}"]`)).toBeTruthy();
    }
    expect(row?.querySelector('[data-col="perms"]')?.textContent).toBe("0644");
    expect(row?.querySelector('[data-col="owner"]')?.textContent).toBe("alice");
  });

  it("本地栏没有权限和所有者两列", () => {
    const props = {
      ...baseProps([entry("a.txt")]),
      side: "local" as const,
      path: "D:/data",
    };
    const { container } = render(<SftpFilePane {...props} />);
    const ids = Array.from(
      container
        .querySelector('[data-role="col-header"]')
        ?.querySelectorAll("[data-col]") ?? [],
    ).map((el) => el.getAttribute("data-col"));
    expect(ids).toEqual(["name", "size", "mtime", "kind"]);
  });
});

describe("窄版（停靠面板）的列精简", () => {
  const columnsOf = (container: HTMLElement) =>
    Array.from(
      container
        .querySelector('[data-role="col-header"]')
        ?.querySelectorAll("[data-col]") ?? [],
    ).map((el) => el.getAttribute("data-col"));

  const withWidth = (width: number, run: () => void) => {
    const original = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "clientWidth",
    );
    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get: () => width,
    });
    try {
      run();
    } finally {
      if (original)
        Object.defineProperty(HTMLElement.prototype, "clientWidth", original);
      else
        delete (HTMLElement.prototype as unknown as Record<string, unknown>)
          .clientWidth;
    }
  };

  it("宽度够：名称、大小、修改时间；再窄先藏修改时间；权限、所有者、类型都不显示", () => {
    withWidth(420, () => {
      const { container } = render(
        <SftpFilePane {...baseProps([entry("a.txt")])} compact />,
      );
      expect(columnsOf(container)).toEqual(["name", "size", "mtime"]);
    });
    withWidth(280, () => {
      const { container } = render(
        <SftpFilePane {...baseProps([entry("a.txt")])} compact />,
      );
      expect(columnsOf(container)).toEqual(["name", "size"]);
      const row = container.querySelector("[data-row]");
      expect(row?.querySelector('[data-col="mtime"]')).toBeNull();
      expect(row?.querySelector('[data-col="owner"]')).toBeNull();
    });
  });
});

describe("窄面板里的路径面包屑：放不下时前面几级折成「…」", () => {
  const LONG = "/home/deploy/releases/2026-10-17/app/current";
  const widthDescriptor = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    "clientWidth",
  );
  const withWidth = (width: number) =>
    Object.defineProperty(HTMLElement.prototype, "clientWidth", {
      configurable: true,
      get() {
        return width;
      },
    });
  afterEach(() => {
    if (widthDescriptor)
      Object.defineProperty(
        HTMLElement.prototype,
        "clientWidth",
        widthDescriptor,
      );
    else
      delete (HTMLElement.prototype as unknown as Record<string, unknown>)
        .clientWidth;
  });

  const crumbNames = (container: HTMLElement) =>
    Array.from(container.querySelectorAll("[data-section=crumbs] button"))
      .map((b) => b.textContent)
      .filter((t) => t !== "…");

  it("宽度够：每一级都显示，没有「…」", () => {
    withWidth(2000);
    const { container } = render(
      <SftpFilePane {...baseProps([])} path={LONG} />,
    );
    expect(container.querySelector("[data-section=crumb-ellipsis]")).toBeNull();
    expect(crumbNames(container)).toEqual([
      "home",
      "deploy",
      "releases",
      "2026-10-17",
      "app",
      "current",
    ]);
  });

  it("宽度不够：最后几级留着，前面折成「…」，点它进入路径输入并看到完整路径", () => {
    withWidth(170);
    const { container } = render(
      <SftpFilePane {...baseProps([])} path={LONG} />,
    );
    const ellipsis = container.querySelector(
      "[data-section=crumb-ellipsis]",
    ) as HTMLElement;
    expect(ellipsis).not.toBeNull();
    const shown = crumbNames(container);
    expect(shown[shown.length - 1]).toBe("current");
    expect(shown).not.toContain("home");
    expect(shown.length).toBeLessThan(6);
    fireEvent.click(ellipsis);
    const input = container.querySelector(
      "input.font-mono",
    ) as HTMLInputElement;
    expect(input.value).toBe(LONG);
  });
});

describe("M3-5：复制 / 粘贴 / 属性的快捷键，和空白处右键", () => {
  const sample = () => [
    entry("sub", { kind: "dir", path: "/data/sub/sub" }),
    entry("a.txt"),
    entry("b.txt"),
  ];
  function withHandlers() {
    const props = {
      ...baseProps(sample()),
      onCopySelection: vi.fn<(entries: SftpEntry[]) => void>(),
      onPasteHere: vi.fn<() => void>(),
      onShowProperties: vi.fn<(entries: SftpEntry[]) => void>(),
    };
    return props;
  }

  it("Ctrl+C 把选中的条目交给复制回调，Ctrl+X 不接剪切", () => {
    const props = withHandlers();
    const { container } = render(<SftpFilePane {...props} />);
    const list = listOf(container);
    fireEvent.keyDown(list, { key: "a", ctrlKey: true });
    fireEvent.keyDown(list, { key: "c", ctrlKey: true });
    expect(props.onCopySelection).toHaveBeenCalledTimes(1);
    expect(
      vi
        .mocked(props.onCopySelection)
        .mock.calls[0][0].map((item) => item.name)
        .sort(),
    ).toEqual(["a.txt", "b.txt", "sub"]);
    // 剪切已经去掉：Ctrl+X 不拦按键（fireEvent 返回 true = 没有 preventDefault）。
    expect(fireEvent.keyDown(list, { key: "x", ctrlKey: true })).toBe(true);
  });

  it("Ctrl+V 粘贴到当前目录；没选中东西也能粘贴", () => {
    const props = withHandlers();
    const { container } = render(<SftpFilePane {...props} />);
    fireEvent.keyDown(listOf(container), { key: "v", ctrlKey: true });
    expect(props.onPasteHere).toHaveBeenCalledTimes(1);
  });

  it("没选中任何东西时 Ctrl+C 什么也不做", () => {
    const props = withHandlers();
    const { container } = render(<SftpFilePane {...props} />);
    fireEvent.keyDown(listOf(container), { key: "c", ctrlKey: true });
    expect(props.onCopySelection).not.toHaveBeenCalled();
  });

  it("Alt+Enter 是属性，不是「回车打开」", () => {
    const props = withHandlers();
    const { container } = render(<SftpFilePane {...props} />);
    fireEvent.keyDown(listOf(container), { key: "Enter", altKey: true });
    expect(props.onShowProperties).toHaveBeenCalledTimes(1);
    expect(props.onActivate).not.toHaveBeenCalled();
  });

  it("功能还没接上（没传回调）时按键原样放过，不拦下", () => {
    const props = baseProps(sample());
    const { container } = render(<SftpFilePane {...props} />);
    const list = listOf(container);
    // fireEvent 返回 false = 有人调用了 preventDefault。
    expect(fireEvent.keyDown(list, { key: "c", ctrlKey: true })).toBe(true);
    expect(fireEvent.keyDown(list, { key: "v", ctrlKey: true })).toBe(true);
    expect(fireEvent.keyDown(list, { key: "x", ctrlKey: true })).toBe(true);
  });

  it("按键发生在列表外面（比如终端或别的输入框）时，列表的快捷键不会触发", () => {
    const props = withHandlers();
    const { container } = render(
      <div>
        <textarea data-testid="terminal-like" />
        <SftpFilePane {...props} />
      </div>,
    );
    const outside = container.querySelector(
      '[data-testid="terminal-like"]',
    ) as HTMLElement;
    // 终端里的 Ctrl+C：没人拦它（事件没经过文件列表）。
    expect(fireEvent.keyDown(outside, { key: "c", ctrlKey: true })).toBe(true);
    fireEvent.keyDown(outside, { key: "v", ctrlKey: true });
    fireEvent.keyDown(outside, { key: "Enter", altKey: true });
    expect(props.onCopySelection).not.toHaveBeenCalled();
    expect(props.onPasteHere).not.toHaveBeenCalled();
    expect(props.onShowProperties).not.toHaveBeenCalled();
  });

  it("右键点在行上：target 是 items；点在空白处：target 是 blank，选择清掉", () => {
    const props = withHandlers();
    const { container } = render(<SftpFilePane {...props} />);
    const list = listOf(container);
    const row = container.querySelector('[data-row="1"]') as HTMLElement;
    fireEvent.contextMenu(row, { clientX: 10, clientY: 20 });
    const first = vi.mocked(props.onContextMenu).mock.calls[0][0];
    expect(first.target).toBe("items");
    expect(first.entries.map((item) => item.name)).toEqual(["a.txt"]);

    // 空白处（列表容器本身，不在任何行上）。
    fireEvent.contextMenu(list, { clientX: 300, clientY: 500 });
    const second = vi.mocked(props.onContextMenu).mock.calls[1][0];
    expect(second.target).toBe("blank");
    expect(second.entries).toEqual([]);
  });
});

describe("选择按文件身份记（排序、过滤、刷新后不会选错文件）", () => {
  const names = ["alpha.txt", "beta.txt", "gamma.txt"];
  const rowNames = (container: HTMLElement) =>
    [...rowsIn(container)].map((row) => row.textContent ?? "");
  const selectedNames = (container: HTMLElement) =>
    [...rowsIn(container)]
      .filter((row) => row.className.includes("bg-primary/15"))
      .map((row) => names.find((name) => row.textContent?.includes(name)));

  it("选中 beta 后列表顺序变了（倒序 / 后台刷新多了条目），选中的还是 beta", () => {
    const props = baseProps(names.map((name) => entry(name)));
    const { container, rerender } = render(<SftpFilePane {...props} />);
    const beta = [...rowsIn(container)].find((row) => row.textContent?.includes("beta.txt"))!;
    fireEvent.click(beta);
    expect(selectedNames(container)).toEqual(["beta.txt"]);

    rerender(<SftpFilePane {...props} order={{ key: "name", descending: true }} />);
    expect(rowNames(container)[0]).toContain("gamma.txt");
    expect(selectedNames(container)).toEqual(["beta.txt"]);

    // 后台刷新：前面插进来一个新文件，行号整体后移
    const refreshed = [entry("aaa-new.txt"), ...names.map((name) => entry(name))];
    rerender(<SftpFilePane {...props} entries={refreshed} />);
    expect(selectedNames(container)).toEqual(["beta.txt"]);
  });

  it("报给上层的是选中的条目本身，被过滤掉的不算", () => {
    const onSelectionChange = vi.fn();
    const props = { ...baseProps(names.map((name) => entry(name))), onSelectionChange };
    const { container, rerender } = render(<SftpFilePane {...props} />);
    act(() => {
      fireEvent.keyDown(listOf(container), { key: "a", ctrlKey: true });
    });
    const last = onSelectionChange.mock.calls.at(-1)?.[0] as SftpEntry[];
    expect(last.map((e) => e.name)).toEqual(names);

    rerender(<SftpFilePane {...props} filter={{ text: "gam", showHidden: false }} />);
    const filtered = onSelectionChange.mock.calls.at(-1)?.[0] as SftpEntry[];
    expect(filtered.map((e) => e.name)).toEqual(["gamma.txt"]);
  });

  it("Delete 删的是选中的那个文件，不是同一行号上的别的文件", () => {
    const props = baseProps(names.map((name) => entry(name)));
    const { container, rerender } = render(<SftpFilePane {...props} />);
    fireEvent.click([...rowsIn(container)].find((row) => row.textContent?.includes("alpha.txt"))!);
    rerender(<SftpFilePane {...props} order={{ key: "name", descending: true }} />);
    act(() => {
      fireEvent.keyDown(listOf(container), { key: "Delete" });
    });
    const deleted = (props.onDeleteSelection as ReturnType<typeof vi.fn>).mock.calls[0][0] as SftpEntry[];
    expect(deleted.map((e) => e.name)).toEqual(["alpha.txt"]);
  });
});
