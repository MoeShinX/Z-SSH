import { describe, expect, it } from "vitest";

import {
  commandsInGroup,
  groupsOf,
  isMultiline,
  moveQuickCommand,
  parseQuickCommands,
  payloadOf,
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
