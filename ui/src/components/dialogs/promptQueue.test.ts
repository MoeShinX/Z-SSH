import { describe, expect, it } from "vitest";

import type { AuthPrompt, HostKeyPrompt } from "../../api/auth";
import { currentPrompt, enqueuePrompt, removePrompt, type PendingPrompt } from "./promptQueue";

function hostKey(reqId: number): PendingPrompt {
  const prompt: HostKeyPrompt = {
    req_id: reqId,
    host: "web-1",
    port: 22,
    key_type: "ssh-ed25519",
    fingerprint: "SHA256:abc",
    status: "unknown",
  };
  return { kind: "hostkey", prompt };
}

function auth(reqId: number): PendingPrompt {
  const prompt: AuthPrompt = {
    req_id: reqId,
    kind: "password",
    name: "",
    instructions: "",
    prompts: [{ text: "Password:", echo: false }],
    can_remember: true,
  };
  return { kind: "auth", prompt };
}

describe("后端提问排队（C12）", () => {
  it("并发的同类提问不会互相覆盖：先来的先显示，答完再显示下一条", () => {
    let queue: readonly PendingPrompt[] = [];
    queue = enqueuePrompt(queue, hostKey(1));
    queue = enqueuePrompt(queue, hostKey(2));
    expect(currentPrompt(queue)?.prompt.req_id).toBe(1);
    queue = removePrompt(queue, 1);
    expect(currentPrompt(queue)?.prompt.req_id).toBe(2);
    queue = removePrompt(queue, 2);
    expect(currentPrompt(queue)).toBeNull();
  });

  it("指纹和认证提问混在一个队里按到达顺序显示", () => {
    let queue: readonly PendingPrompt[] = [];
    queue = enqueuePrompt(queue, auth(3));
    queue = enqueuePrompt(queue, hostKey(4));
    expect(currentPrompt(queue)?.kind).toBe("auth");
    queue = removePrompt(queue, 3);
    expect(currentPrompt(queue)?.kind).toBe("hostkey");
  });

  it("后端宣布结束的提问（超时 / 会话关闭）即使没显示也要从队里摘掉", () => {
    let queue: readonly PendingPrompt[] = [];
    queue = enqueuePrompt(queue, auth(1));
    queue = enqueuePrompt(queue, auth(2));
    queue = enqueuePrompt(queue, auth(3));
    queue = removePrompt(queue, 2);
    expect(queue.map((item) => item.prompt.req_id)).toEqual([1, 3]);
  });

  it("同一个 req_id 重复到达只排一次", () => {
    let queue: readonly PendingPrompt[] = [];
    queue = enqueuePrompt(queue, hostKey(7));
    queue = enqueuePrompt(queue, hostKey(7));
    expect(queue).toHaveLength(1);
  });

  it("摘不存在的 req_id 返回原数组（答完之后后端再发一次结束事件）", () => {
    const queue = enqueuePrompt([], hostKey(1));
    expect(removePrompt(queue, 99)).toBe(queue);
  });
});
