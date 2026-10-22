import { describe, expect, it } from "vitest";

import type { ChatItem } from "./chatHistory";
import { elapsedText, showThinking } from "./ThinkingIndicator";

const user: ChatItem = { kind: "user", text: "你好" };
const reply: ChatItem = { kind: "assistant", text: "在" };
const tool = (status: "running" | "done" | "error"): ChatItem => ({
  kind: "tool",
  call: { callId: "c1", tool: "zssh_run_command", input: {}, output: "", status },
});

describe("思考中提示", () => {
  it("回合在跑、AI 还没动静时显示；出字、命令在跑、等批准、回合结束都不显示", () => {
    expect(showThinking(true, [user], false)).toBe(true);
    expect(showThinking(true, [], false)).toBe(true);
    expect(showThinking(true, [user, reply], false)).toBe(false);
    expect(showThinking(true, [user, tool("running")], false)).toBe(false);
    // 命令跑完了、AI 在想下一步
    expect(showThinking(true, [user, tool("done")], false)).toBe(true);
    expect(showThinking(true, [user, tool("error")], false)).toBe(true);
    expect(showThinking(true, [user], true)).toBe(false);
    // 并行：最后一张先完成、前面的还在跑，不显示；都完成了才显示
    expect(showThinking(true, [user, tool("running"), tool("done")], false)).toBe(false);
    expect(showThinking(true, [user, tool("done"), tool("done")], false)).toBe(true);
    // 上一轮遗留的卡片不算
    expect(showThinking(true, [tool("running"), user, tool("done")], false)).toBe(true);
    expect(showThinking(false, [user], false)).toBe(false);
  });

  it("等待时间：不到一分钟按秒，超过按分秒", () => {
    expect(elapsedText(0)).toBe("0秒");
    expect(elapsedText(11_900)).toBe("11秒");
    expect(elapsedText(75_000)).toBe("1分15秒");
    expect(elapsedText(-5)).toBe("0秒");
  });
});
