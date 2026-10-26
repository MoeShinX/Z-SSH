/** @vitest-environment jsdom */
//! 主机指纹弹窗：指纹变化时列出新旧指纹、默认只关闭，明确点"更新指纹并连接"才继续；
//! 吊销只能关闭；未知主机照旧三个选项。

import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HostKeyPrompt } from "../../api/auth";
import HostKeyDialog from "./HostKeyDialog";

afterEach(() => cleanup());

const OLD = "SHA256:oldoldoldoldoldoldoldoldoldoldoldoldoldold";
const NEW = "SHA256:newnewnewnewnewnewnewnewnewnewnewnewnewnew";

function show(patch: Partial<HostKeyPrompt>) {
  const onAnswer = vi.fn();
  const prompt: HostKeyPrompt = {
    req_id: 1,
    host: "web-1.example",
    port: 22,
    key_type: "ssh-ed25519",
    fingerprint: NEW,
    status: "unknown",
    stored_fingerprint: null,
    ...patch,
  };
  render(<HostKeyDialog prompt={prompt} onAnswer={onAnswer} />);
  return onAnswer;
}

const button = (action: string) =>
  document.querySelector(`[data-action="${action}"]`) as HTMLButtonElement | null;

describe("主机指纹弹窗", () => {
  it("指纹变化：显示旧指纹和新指纹，点更新才回答 trust_and_save", () => {
    const onAnswer = show({ status: "changed", stored_fingerprint: OLD });
    const text = document.body.textContent ?? "";
    expect(text).toContain("主机指纹发生变化");
    expect(text).toContain(OLD);
    expect(text).toContain(NEW);
    expect(button("trust-once")).toBeNull();

    fireEvent.click(button("replace")!);
    expect(onAnswer).toHaveBeenCalledWith("trust_and_save");
  });

  it("指纹变化：关闭就是拒绝", () => {
    const onAnswer = show({ status: "changed", stored_fingerprint: OLD });
    fireEvent.click(button("reject")!);
    expect(onAnswer).toHaveBeenCalledWith("reject");
  });

  it("密钥被吊销：只有关闭", () => {
    const onAnswer = show({ status: "revoked", stored_fingerprint: OLD });
    expect(document.body.textContent).toContain("主机密钥已被吊销");
    expect(button("replace")).toBeNull();
    expect(button("trust-once")).toBeNull();
    expect(button("trust-and-save")).toBeNull();
    fireEvent.click(button("reject")!);
    expect(onAnswer).toHaveBeenCalledWith("reject");
  });

  it("未知主机：信任并保存 / 仅本次 / 取消", () => {
    const onAnswer = show({ status: "unknown" });
    expect(button("replace")).toBeNull();
    fireEvent.click(button("trust-once")!);
    expect(onAnswer).toHaveBeenCalledWith("trust_once");
  });
});
