import { describe, expect, it } from "vitest";

import {
  canMoveInGroup,
  commandsInGroup,
  groupsOf,
  isMultiline,
  moveQuickCommand,
  moveQuickCommandInGroup,
  normalizeQuickCommandColor,
  parseQuickCommands,
  payloadOf,
  QUICK_COMMAND_COLORS,
  QUICK_COMMAND_COLOR_STYLES,
  searchQuickCommands,
  sectionsOfQuickCommands,
  serializeQuickCommands,
  upsertQuickCommand,
  validateQuickCommand,
  type QuickCommand,
} from "./quickCommands";

const qc = (id: string, patch: Partial<QuickCommand> = {}): QuickCommand => ({
  id,
  name: id,
  command: `echo ${id}`,
  enter: true,
  ...patch,
});

describe("快捷命令", () => {
  it("存取来回不变；坏数据整体当空、坏的单条跳过", () => {
    const list = [qc("a", { group: "常用" }), qc("b", { enter: false })];
    expect(parseQuickCommands(serializeQuickCommands(list))).toEqual([
      qc("a", { group: "常用" }),
      qc("b", { enter: false, group: undefined }),
    ]);
    expect(parseQuickCommands("not json")).toEqual([]);
    expect(parseQuickCommands("{}")).toEqual([]);
    expect(parseQuickCommands('[{"id":"x"},{"id":"y","name":"Y","command":"ls"}]')).toEqual([
      { id: "y", name: "Y", command: "ls", group: undefined, enter: true },
    ]);
  });

  it("分组：按出现顺序列出；选的组没有命令了就显示全部", () => {
    const list = [qc("a", { group: "Debian" }), qc("b"), qc("c", { group: "测试" }), qc("d", { group: "Debian" })];
    expect(groupsOf(list)).toEqual(["Debian", "测试"]);
    expect(commandsInGroup(list, "Debian").map((c) => c.id)).toEqual(["a", "d"]);
    expect(commandsInGroup(list, "").length).toBe(4);
    expect(commandsInGroup(list, "删掉的组").length).toBe(4);
  });

  it("发送内容：换行统一成回车，要执行的末尾补回车", () => {
    expect(payloadOf(qc("a", { command: "ss -tlnp" }))).toBe("ss -tlnp\r");
    expect(payloadOf(qc("a", { command: "ss -tlnp", enter: false }))).toBe("ss -tlnp");
    expect(payloadOf(qc("a", { command: "apt update\r\napt upgrade\n" }))).toBe("apt update\rapt upgrade\r");
    expect(isMultiline(qc("a", { command: "a\nb" }))).toBe(true);
    expect(isMultiline(qc("a", { command: "ls\n" }))).toBe(false);
  });

  it("编辑框校验", () => {
    expect(validateQuickCommand({ name: "x", command: "ls", group: "" })).toBeNull();
    expect(validateQuickCommand({ name: " ", command: "ls", group: "" })).toContain("名称");
    expect(validateQuickCommand({ name: "x", command: "", group: "" })).toContain("命令");
    expect(validateQuickCommand({ name: "x", command: "echo \u001b[2J", group: "" })).toContain("控制字符");
    expect(validateQuickCommand({ name: "x", command: "a\n\tb", group: "" })).toBeNull();
  });

  it("新增 / 替换 / 移动", () => {
    let list = [qc("a"), qc("b")];
    list = upsertQuickCommand(list, qc("c"));
    list = upsertQuickCommand(list, qc("a", { name: "改名" }));
    expect(list.map((c) => c.name)).toEqual(["改名", "b", "c"]);
    expect(moveQuickCommand(list, "c", -1).map((c) => c.id)).toEqual(["a", "c", "b"]);
    expect(moveQuickCommand(list, "a", -1)).toBe(list);
  });
});

describe("按分组上移 / 下移、搜索、归拢（UI-10）", () => {
  const cmd = (id: string, group?: string, name = id, command = `run ${id}`): QuickCommand => ({
    id,
    name,
    command,
    group,
    enter: true,
  });
  const ids = (list: QuickCommand[]) => list.map((item) => item.id);
  // 全局顺序：a(甲) x(乙) b(甲) y(未分组) c(甲) z(乙)
  const list = [cmd("a", "甲"), cmd("x", "乙"), cmd("b", "甲"), cmd("y"), cmd("c", "甲"), cmd("z", "乙")];

  it("组内下移 / 上移：和同组相邻的那条交换，别的分组原地不动", () => {
    expect(ids(moveQuickCommandInGroup(list, "a", 1))).toEqual(["b", "x", "a", "y", "c", "z"]);
    expect(ids(moveQuickCommandInGroup(list, "c", -1))).toEqual(["a", "x", "c", "y", "b", "z"]);
    // 未分组也是一个"组"
    expect(ids(moveQuickCommandInGroup([cmd("p"), cmd("q", "甲"), cmd("r")], "r", -1))).toEqual(["r", "q", "p"]);
  });

  it("组内的相对顺序恰好变一位（命令栏选了这个分组后看到的顺序）", () => {
    const after = moveQuickCommandInGroup(list, "b", 1);
    expect(ids(commandsInGroup(after, "甲"))).toEqual(["a", "c", "b"]);
    expect(ids(commandsInGroup(after, "乙"))).toEqual(ids(commandsInGroup(list, "乙")));
  });

  it("组内第一条不能上移、最后一条不能下移（原样返回同一个数组）；找不到 id 也原样返回", () => {
    expect(moveQuickCommandInGroup(list, "a", -1)).toBe(list);
    expect(moveQuickCommandInGroup(list, "c", 1)).toBe(list);
    expect(moveQuickCommandInGroup(list, "y", 1)).toBe(list);
    expect(moveQuickCommandInGroup(list, "nope", 1)).toBe(list);
    expect(canMoveInGroup(list, "a")).toEqual([false, true]);
    expect(canMoveInGroup(list, "b")).toEqual([true, true]);
    expect(canMoveInGroup(list, "c")).toEqual([true, false]);
    expect(canMoveInGroup(list, "y")).toEqual([false, false]);
  });

  it("搜索按名字、命令、分组，不分大小写，空词返回原列表", () => {
    const items = [cmd("1", "Debian", "端口查看", "ss -TLNP"), cmd("2", undefined, "更新", "apt update")];
    expect(searchQuickCommands(items, "")).toBe(items);
    expect(ids(searchQuickCommands(items, "tlnp"))).toEqual(["1"]);
    expect(ids(searchQuickCommands(items, "端口"))).toEqual(["1"]);
    expect(ids(searchQuickCommands(items, "debian"))).toEqual(["1"]);
    expect(ids(searchQuickCommands(items, "APT"))).toEqual(["2"]);
    expect(searchQuickCommands(items, "没有")).toEqual([]);
  });

  it("按分组归拢：分组按第一次出现排，未分组放最后，组内保持相对顺序", () => {
    const sections = sectionsOfQuickCommands(list);
    expect(sections.map((section) => [section.group, ids(section.items)])).toEqual([
      ["甲", ["a", "b", "c"]],
      ["乙", ["x", "z"]],
      ["", ["y"]],
    ]);
    expect(sectionsOfQuickCommands([cmd("p")]).map((section) => section.group)).toEqual([""]);
    expect(sectionsOfQuickCommands([])).toEqual([]);
  });
});

describe("快捷命令颜色", () => {
  it("已知颜色保留；未知值当作没设，但整条命令不丢", () => {
    const raw = JSON.stringify([
      { id: "a", name: "A", command: "ls", color: "red" },
      { id: "b", name: "B", command: "ls", color: "magenta" },
      { id: "c", name: "C", command: "ls", color: 5 },
      { id: "d", name: "D", command: "ls" },
    ]);
    const list = parseQuickCommands(raw);
    expect(list.map((item) => [item.id, item.color])).toEqual([
      ["a", "red"],
      ["b", undefined],
      ["c", undefined],
      ["d", undefined],
    ]);
  });

  it("序列化：有颜色才写 color 字段；往返一致", () => {
    const list = [qc("a", { color: "teal" }), qc("b")];
    const written = JSON.parse(serializeQuickCommands(list)) as Record<string, unknown>[];
    expect(written[0].color).toBe("teal");
    expect("color" in written[1]).toBe(false);
    expect(parseQuickCommands(serializeQuickCommands(list)).map((item) => item.color)).toEqual(["teal", undefined]);
  });

  it("颜色表：8 个预设，每个都有中文名和各处要用的样式", () => {
    expect(QUICK_COMMAND_COLORS).toEqual(["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink"]);
    for (const id of QUICK_COMMAND_COLORS) {
      const style = QUICK_COMMAND_COLOR_STYLES[id];
      expect(style.label).not.toBe("");
      expect(style.button).toContain(`bg-${id}-500/15`);
      expect(style.dot).toBe(`bg-${id}-500`);
      expect(style.icon).toContain(`text-${id}-`);
      expect(normalizeQuickCommandColor(id)).toBe(id);
    }
    expect(normalizeQuickCommandColor("nope")).toBeUndefined();
    expect(normalizeQuickCommandColor(undefined)).toBeUndefined();
  });
});
