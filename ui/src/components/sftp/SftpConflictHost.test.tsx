/** @vitest-environment jsdom */
//! 同名文件对话框的统一入口（SFTP-6 第 3、4 节）：
//! - 几个提问同时到，要排队逐个弹，每一条都得到回答（以前后到的把先到的盖掉，
//!   被盖掉的那条没人答，后端等满 2 分钟按「跳过」）；
//! - 不依赖文件面板在不在屏幕上（面板关了、切到别的主机，SftpTabView 都被卸载）；
//! - 默认选「覆盖」（能续传也是），焦点在「确定」上，直接回车就是覆盖；
//! - 别的窗口的会话、后端已经放弃的提问不弹。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SftpConflictPrompt } from "../../api/sftp";

const mocks = vi.hoisted(() => ({
  handlers: [] as Array<(sftpId: string, prompt: unknown) => void>,
  answer: vi.fn(async () => {}),
}));

vi.mock("../../api/sftp", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/sftp")>();
  return {
    ...actual,
    onSftpConflict: vi.fn(async (handler: (sftpId: string, prompt: unknown) => void) => {
      mocks.handlers.push(handler);
      return () => {
        mocks.handlers = mocks.handlers.filter((item) => item !== handler);
      };
    }),
    sftpConflictAnswer: mocks.answer,
  };
});

import { CONFLICT_PROMPT_TTL_MS, useSftpConflicts } from "../../state/sftpConflicts";
import SftpConflictHost from "./SftpConflictHost";

function prompt(id: number, patch: Partial<SftpConflictPrompt> = {}): SftpConflictPrompt {
  return {
    conflict_id: id,
    job: id,
    direction: "upload",
    source: `C:/work/file${id}.txt`,
    target: `/srv/file${id}.txt`,
    source_size: 100,
    target_size: 40,
    can_resume: false,
    suggested_name: `file${id} (2).txt`,
    ...patch,
  };
}

/** 模拟后端发来一条提问（事件对所有监听者广播）。 */
async function emit(sftpId: string, payload: SftpConflictPrompt) {
  await act(async () => {
    for (const handler of [...mocks.handlers]) handler(sftpId, payload);
  });
}

async function mount(sessionIds: string[]) {
  const view = render(<SftpConflictHost sessionIds={sessionIds} />);
  await act(async () => {});
  return view;
}

beforeEach(() => {
  mocks.handlers = [];
  mocks.answer.mockClear();
  useSftpConflicts.setState({ queue: [] });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("同名文件提问的排队", () => {
  it("两条提问先后到：先弹第一条，答完再弹第二条，两条都有回答（不会盖掉）", async () => {
    await mount(["s1"]);
    await emit("s1", prompt(1));
    await emit("s1", prompt(2));

    expect(screen.getAllByText("/srv/file1.txt").length).toBe(1);
    expect(screen.queryByText("/srv/file2.txt")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(mocks.answer).toHaveBeenNthCalledWith(1, 1, "overwrite", false, undefined);

    // 第二条紧跟着弹出来
    expect(await screen.findByText("/srv/file2.txt")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(mocks.answer).toHaveBeenNthCalledWith(2, 2, "overwrite", false, undefined);
    expect(mocks.answer).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("目标已存在")).toBeNull();
  });

  it("没有任何文件面板在屏幕上也能弹（面板被卸载时提问照样收得到）", async () => {
    // 这里只挂了 Host，没有任何 SftpTabView：就是「停靠面板关了 / 切到别的主机」的情形。
    await mount(["dock-session"]);
    await emit("dock-session", prompt(7));
    expect(screen.getByText("目标已存在")).toBeTruthy();
    expect(screen.getByText("/srv/file7.txt")).toBeTruthy();
  });

  it("别的窗口的会话、已经关掉的会话的提问不弹", async () => {
    const view = await mount(["s1"]);
    await emit("other-window", prompt(1));
    expect(screen.queryByText("目标已存在")).toBeNull();

    await emit("s1", prompt(2));
    expect(screen.getByText("目标已存在")).toBeTruthy();
    expect(screen.getByText("/srv/file2.txt")).toBeTruthy();
    // 会话关了（后端也作废了这条提问）：对话框收掉，不留着等一个永远没人收的回答
    view.rerender(<SftpConflictHost sessionIds={[]} />);
    await act(async () => {});
    expect(screen.queryByText("目标已存在")).toBeNull();
    expect(mocks.answer).not.toHaveBeenCalled();
  });

  it("会话在窗口之间搬家时提问正好在路上：搬到的窗口接着弹", async () => {
    // 提问先到（这时它还不认这个会话），会话随后才归本窗口（停靠面板拆到 Z-SFTP 窗口）
    const view = await mount([]);
    await emit("moved", prompt(4));
    expect(screen.queryByText("目标已存在")).toBeNull();
    view.rerender(<SftpConflictHost sessionIds={["moved"]} />);
    await act(async () => {});
    expect(screen.getByText("/srv/file4.txt")).toBeTruthy();
  });

  it("后端等满 2 分钟已经按跳过处理：提问撤掉，不再弹", async () => {
    vi.useFakeTimers();
    await mount(["s1"]);
    await emit("s1", prompt(3));
    expect(screen.getByText("目标已存在")).toBeTruthy();
    await act(async () => {
      vi.advanceTimersByTime(CONFLICT_PROMPT_TTL_MS + 10);
    });
    expect(screen.queryByText("目标已存在")).toBeNull();
    expect(mocks.answer).not.toHaveBeenCalled();
  });
});

describe("对话框默认选覆盖", () => {
  it("能续传时也默认覆盖：直接点确定就是覆盖", async () => {
    await mount(["s1"]);
    await emit("s1", prompt(1, { can_resume: true, target_size: 10 }));
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(mocks.answer).toHaveBeenCalledWith(1, "overwrite", false, undefined);
  });

  it("打开时焦点在「确定」上，回车就是接受默认的覆盖", async () => {
    await mount(["s1"]);
    await emit("s1", prompt(1, { can_resume: true }));
    const confirm = screen.getByRole("button", { name: "确定" });
    expect(document.activeElement).toBe(confirm);
  });

  it("续传仍可手动选（能续传时）", async () => {
    await mount(["s1"]);
    await emit("s1", prompt(1, { can_resume: true }));
    fireEvent.click(screen.getByText("续传"));
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(mocks.answer).toHaveBeenCalledWith(1, "resume", false, undefined);
  });

  it("复制粘贴的提问默认仍是重命名", async () => {
    await mount(["s1"]);
    await emit("s1", prompt(1, { direction: "remote_copy", suggested_name: "a - 副本.txt" }));
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    expect(mocks.answer).toHaveBeenCalledWith(1, "rename", false, "a - 副本.txt");
  });

  it("点「取消（按跳过）」回答跳过", async () => {
    await mount(["s1"]);
    await emit("s1", prompt(1));
    fireEvent.click(screen.getByRole("button", { name: "取消（按跳过）" }));
    expect(mocks.answer).toHaveBeenCalledWith(1, "skip", false, undefined);
  });
});
