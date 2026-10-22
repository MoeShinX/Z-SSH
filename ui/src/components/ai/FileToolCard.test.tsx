/** @vitest-environment jsdom */
//! 文件工具卡片（M3-4）：手动模式下 write_file 的确认卡片带差异（长差异折叠、可展开）、
//! 系统路径标红并写明原因；read_file 的卡片不出现确认按钮。

import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PendingApproval } from "./AIChatPanel";
import { ToolCard } from "./AIChatPanel";

afterEach(cleanup);

function approvalWith(lines: number, patch: Partial<PendingApproval> = {}): PendingApproval {
  const diffLines = Array.from({ length: lines }, (_, i) => ({
    kind: "add" as const,
    text: `row ${i}`,
  }));
  return {
    reqId: 5,
    hostLabel: "web-1",
    command: "write_file /home/u/app.conf",
    danger: "safe",
    reasons: [],
    tool: "write_file",
    filePath: "/home/u/app.conf",
    file: {
      path: "/home/u/app.conf",
      is_new: true,
      new_bytes: 100,
      diff: { lines: diffLines, added: lines, removed: 0, omitted_lines: 0, is_new: true },
    },
    ...patch,
  };
}

const writeCall = {
  callId: "c1",
  tool: "zssh_write_file",
  input: { path: "app.conf", content: "x" },
  output: "",
  status: "running" as const,
};

function renderCard(approval: PendingApproval, onAnswer = vi.fn()) {
  const view = render(
    <ToolCard call={writeCall} hosts={{}} pinnedHosts={[]} approval={approval} onAnswer={onAnswer} />,
  );
  return { ...view, onAnswer };
}

describe("write_file 的确认卡片", () => {
  it("显示服务器上的路径、差异、新建说明，和执行 / 拒绝按钮", () => {
    const { container, onAnswer, getByText } = renderCard(approvalWith(3));
    expect(container.querySelector('[data-section="file-path"]')?.textContent).toBe("/home/u/app.conf");
    expect(container.querySelector('[data-section="file-diff"]')).not.toBeNull();
    expect(container.textContent).toContain("新建文件，共 3 行");
    expect(container.querySelectorAll("[data-diff-kind=add]")).toHaveLength(3);
    fireEvent.click(getByText("拒绝"));
    expect(onAnswer).toHaveBeenCalledWith(false);
    fireEvent.click(getByText("执行"));
    expect(onAnswer).toHaveBeenCalledWith(true);
  });

  it("很长的差异折叠，点击展开显示全部", () => {
    const { container } = renderCard(approvalWith(120));
    expect(container.querySelectorAll("[data-diff-kind=add]")).toHaveLength(30);
    const expand = container.querySelector('[data-action="expand-diff"]') as HTMLElement;
    expect(expand.textContent).toContain("还有 90 行");
    fireEvent.click(expand);
    expect(container.querySelectorAll("[data-diff-kind=add]")).toHaveLength(120);
    expect(container.querySelector('[data-action="expand-diff"]')).toBeNull();
  });

  it("系统路径：卡片标红，写明原因", () => {
    const reason = "写入系统路径：系统配置目录 /etc";
    const { container } = renderCard(
      approvalWith(2, {
        danger: "dangerous",
        reasons: [reason],
        filePath: "/etc/hosts",
        command: "write_file /etc/hosts",
      }),
    );
    expect(container.textContent).toContain("危险");
    expect(container.querySelector('[data-section="approval-reasons"]')?.textContent).toContain(reason);
    expect(container.firstElementChild?.className).toContain("destructive");
  });

  it("自动模式写完之后，结果里的标记也让卡片标红并显示备份位置", () => {
    const call = {
      ...writeCall,
      status: "done" as const,
      output:
        "path: /etc/motd\nwritten_bytes: 3\nreplaced: the existing file was overwritten\n" +
        "backup: the previous version was saved on the server at /etc/motd.zssh-bak-1\n" +
        "danger: flagged as a system path (写入系统路径：系统配置目录 /etc)\n",
    };
    const { container } = render(
      <ToolCard call={call} hosts={{}} pinnedHosts={[]} onAnswer={() => {}} />,
    );
    expect(container.querySelector('[data-section="file-danger"]')?.textContent).toContain("/etc");
    expect(container.textContent).toContain("/etc/motd.zssh-bak-1");
    expect(container.firstElementChild?.className).toContain("destructive");
  });
});

describe("read_file 的工具卡片", () => {
  it("显示路径和可折叠的输出，没有确认按钮", () => {
    const call = {
      callId: "c2",
      tool: "zssh_read_file",
      input: { path: "/home/u/notes.txt" },
      output: "path: /home/u/notes.txt\ncontent:\nhello",
      status: "done" as const,
    };
    const { container, queryByText } = render(
      <ToolCard call={call} hosts={{}} pinnedHosts={[]} onAnswer={() => {}} />,
    );
    expect(container.textContent).toContain("读取文件");
    expect(container.querySelector('[data-section="file-path"]')?.textContent).toBe(
      "/home/u/notes.txt",
    );
    expect(container.querySelector("details")).not.toBeNull();
    expect(queryByText("执行")).toBeNull();
    expect(queryByText("拒绝")).toBeNull();
  });

  it("智能体包成 JSON 的输出显示成带真实换行的文本", () => {
    const call = {
      callId: "c3",
      tool: "zssh_read_file",
      input: { path: "/home/u/notes.txt" },
      output: '[{"text":"path: /home/u/notes.txt\\ncontent:\\nhello\\nworld\\n","type":"text"}]',
      status: "done" as const,
    };
    const { container } = render(
      <ToolCard call={call} hosts={{}} pinnedHosts={[]} onAnswer={() => {}} />,
    );
    const pre = container.querySelector("details pre");
    expect(pre?.textContent).toBe("path: /home/u/notes.txt\ncontent:\nhello\nworld\n");
    expect(pre?.textContent).not.toContain('"text"');
    expect(pre?.textContent).not.toContain("\\n");
  });
});

describe("write_file 的属主提示", () => {
  it("属主没能改回去时卡片明确写出从谁变成谁", () => {
    const call = {
      callId: "c4",
      tool: "zssh_write_file",
      input: { path: "/etc/hosts", content: "x" },
      output:
        "path: /etc/hosts\nwritten_bytes: 1\nreplaced: the existing file was overwritten\n" +
        "backup: the previous version was saved on the server at /etc/hosts.zssh-bak-1\n" +
        "owner: could not restore the file's owner: it changed from uid 0 / gid 0 to uid 1000 / gid 1000 (permission denied)\n",
      status: "done" as const,
    };
    const { container } = render(
      <ToolCard call={call} hosts={{}} pinnedHosts={[]} onAnswer={() => {}} />,
    );
    const note = container.querySelector('[data-section="owner-change"]');
    expect(note?.textContent).toContain("uid 0 / gid 0");
    expect(note?.textContent).toContain("uid 1000 / gid 1000");
  });
});
