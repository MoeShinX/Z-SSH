/** @vitest-environment jsdom */
//! SFTP 标签顶上的主机下拉：列出主机库，选了就交给上层换主机；连接中不能再选。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
}));

import type { Host } from "../../api/vault";
import { useVault } from "../../stores/vault";
import SftpHostBar from "./SftpHostBar";

afterEach(cleanup);
beforeEach(() => {
  useVault.setState({
    hosts: [
      { id: 1, name: "NTTPC", username: "root", host: "10.0.0.1" },
      { id: 2, name: "DE1", username: "admin", host: "10.0.0.2" },
    ] as unknown as Host[],
  });
  Element.prototype.hasPointerCapture = () => false;
  Element.prototype.setPointerCapture = () => {};
  Element.prototype.releasePointerCapture = () => {};
  Element.prototype.scrollIntoView = () => {};
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

async function open(trigger: HTMLElement) {
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false, pointerId: 1, pointerType: "mouse" });
  });
}

describe("SFTP 主机下拉", () => {
  it("选另一台主机交给上层", async () => {
    const onPick = vi.fn();
    render(<SftpHostBar hostId={1} busy={false} onPick={onPick} />);
    expect(screen.getByLabelText("SFTP 主机").textContent).toContain("NTTPC");
    await open(screen.getByLabelText("SFTP 主机"));
    await act(async () => fireEvent.click(screen.getByRole("option", { name: /DE1/ })));
    expect(onPick).toHaveBeenCalledWith({ id: 2, name: "DE1" });
  });

  it("还没选主机时提示选择；连接中下拉不可用", () => {
    const { rerender } = render(<SftpHostBar hostId={0} busy={false} onPick={() => {}} />);
    expect(screen.getByLabelText("SFTP 主机").textContent).toContain("选择主机");
    rerender(<SftpHostBar hostId={1} busy onPick={() => {}} />);
    expect((screen.getByLabelText("SFTP 主机") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("连接中…")).toBeTruthy();
  });
});
