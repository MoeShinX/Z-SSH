/** @vitest-environment jsdom */
//! 凭据面板：点「新建」要出现空表单（以前表单只在选中凭据或名称非空时显示，点了「新建」反而收起，
//! 新建不了凭据，只能先生成密钥再改）。

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import { useVault } from "../../stores/vault";
import KeychainPanel from "./KeychainPanel";

const saveIdentity = vi.fn(async (_draft: unknown) => ({}));

beforeEach(() => {
  saveIdentity.mockClear();
  useVault.setState({ identities: [], saveIdentity } as never);
});

afterEach(cleanup);

function nameInput(): HTMLInputElement {
  const label = screen.getByText("名称");
  return label.parentElement!.querySelector("input") as HTMLInputElement;
}

describe("凭据面板", () => {
  it("点「新建」出现空表单，填好名称和密码能保存，保存后表单收起", async () => {
    render(<KeychainPanel onClose={() => {}} onError={() => {}} />);
    expect(screen.queryByText("新建凭据")).toBeNull();

    fireEvent.click(screen.getByText("新建"));
    expect(screen.getByText("新建凭据")).toBeTruthy();
    expect(nameInput().value).toBe("");

    fireEvent.change(nameInput(), { target: { value: "运维密码" } });
    const secret = document.querySelector('input[type="password"]') as HTMLInputElement;
    fireEvent.change(secret, { target: { value: "not-a-real-secret" } });
    fireEvent.click(screen.getByText("保存"));
    await vi.waitFor(() => expect(saveIdentity).toHaveBeenCalledTimes(1));
    expect(saveIdentity.mock.calls[0][0]).toMatchObject({
      id: null,
      name: "运维密码",
      kind: "password",
      secret: "not-a-real-secret",
    });
    await vi.waitFor(() => expect(screen.queryByText("新建凭据")).toBeNull());
  });

  it("新建时名称还空着也不会被收起；点「清空」收起", () => {
    render(<KeychainPanel onClose={() => {}} onError={() => {}} />);
    fireEvent.click(screen.getByText("新建"));
    fireEvent.change(nameInput(), { target: { value: "x" } });
    fireEvent.change(nameInput(), { target: { value: "" } });
    expect(screen.getByText("新建凭据")).toBeTruthy();
    fireEvent.click(screen.getByText("清空"));
    expect(screen.queryByText("新建凭据")).toBeNull();
  });
});
