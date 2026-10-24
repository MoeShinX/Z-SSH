/** @vitest-environment jsdom */
//! 凭据面板：点「新建」要出现空表单（以前表单只在选中凭据或名称非空时显示，点了「新建」反而收起，
//! 新建不了凭据，只能先生成密钥再改）。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

const mocks = vi.hoisted(() => ({ pickKeyFile: vi.fn(async () => null as string | null) }));
vi.mock("../../api/dialog", () => ({ pickKeyFile: mocks.pickKeyFile }));

import { useVault } from "../../stores/vault";
import KeychainPanel from "./KeychainPanel";

const saveIdentity = vi.fn(async (_draft: unknown) => ({}));

beforeEach(() => {
  saveIdentity.mockClear();
  mocks.pickKeyFile.mockClear();
  mocks.pickKeyFile.mockResolvedValue(null);
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

describe("私钥文件路径", () => {
  function keyPathInput(): HTMLInputElement {
    return screen.getByPlaceholderText(/id_ed25519/) as HTMLInputElement;
  }

  it("类型选「私钥文件」后能从文件管理器选：选中的路径填进输入框", async () => {
    render(<KeychainPanel onClose={() => {}} onError={() => {}} />);
    fireEvent.click(screen.getByText("新建"));
    // 类型下拉选「私钥文件」
    fireEvent.click(screen.getByRole("combobox"));
    fireEvent.click(screen.getByText("私钥文件"));
    expect(keyPathInput()).toBeTruthy();

    mocks.pickKeyFile.mockResolvedValue("C:\keys\id_ed25519");
    await act(async () => fireEvent.click(screen.getByText("浏览…")));
    expect(mocks.pickKeyFile).toHaveBeenCalledTimes(1);
    expect(keyPathInput().value).toBe("C:\keys\id_ed25519");
  });

  it("在文件管理器里点了取消：路径保持原样", async () => {
    render(<KeychainPanel onClose={() => {}} onError={() => {}} />);
    fireEvent.click(screen.getByText("新建"));
    fireEvent.click(screen.getByRole("combobox"));
    fireEvent.click(screen.getByText("私钥文件"));
    fireEvent.change(keyPathInput(), { target: { value: "C:\old\key" } });

    mocks.pickKeyFile.mockResolvedValue(null);
    await act(async () => fireEvent.click(screen.getByText("浏览…")));
    expect(keyPathInput().value).toBe("C:\old\key");
  });
});
