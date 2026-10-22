/** @vitest-environment jsdom */
//! 生成密钥 / 安装公钥对话框：生成后能看到公钥；安装后按开关决定要不要把主机改成密钥登录。

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Host, Identity } from "../../api/vault";

const KEY: Identity = {
  id: 7,
  name: "测试密钥",
  kind: "key_inline",
  key_path: null,
  has_secret: true,
} as Identity;

const HOST: Host = {
  id: 3,
  group_id: null,
  name: "测试机",
  host: "example.invalid",
  port: 22,
  username: "tester",
  auth_method: "password",
  identity_id: null,
  proxy_id: null,
  jump_host_id: null,
  proxy_mode: "inherit",
  tags: [],
  sort: 0,
  legacy_algorithms: false,
  created_at: 0,
  updated_at: 0,
} as unknown as Host;

vi.mock("../../api/vault", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../api/vault")>();
  return {
    ...actual,
    listGroups: vi.fn(async () => []),
    listHosts: vi.fn(async () => []),
    listIdentities: vi.fn(async () => []),
    listProxies: vi.fn(async () => []),
    saveHost: vi.fn(async (draft: unknown) => draft),
    identityGenerate: vi.fn(async () => KEY),
    identityPublicKey: vi.fn(async () => ({
      public_key: "ssh-ed25519 AAAAtest z-ssh",
      fingerprint: "SHA256:test",
    })),
    hostInstallPublicKey: vi.fn(async () => "added"),
  };
});

import { GenerateKeyDialog, InstallKeyDialog } from "./SshKeyDialogs";
import { useVault } from "../../stores/vault";

afterEach(cleanup);

beforeEach(() => {
  vi.clearAllMocks();
  useVault.setState({ identities: [KEY], hosts: [HOST], loaded: true });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

describe("SSH 密钥对话框", () => {
  it("生成密钥后显示公钥", async () => {
    const api = await import("../../api/vault");
    const onGenerated = vi.fn();
    render(<GenerateKeyDialog onClose={() => {}} onGenerated={onGenerated} onError={() => {}} />);
    await act(async () => screen.getByRole("button", { name: "生成" }).click());
    expect(api.identityGenerate).toHaveBeenCalledWith(
      expect.objectContaining({ algorithm: "ed25519", passphrase: null }),
    );
    expect(onGenerated).toHaveBeenCalledWith(KEY);
    expect(screen.getByDisplayValue("ssh-ed25519 AAAAtest z-ssh")).toBeTruthy();
  });

  it("两次口令不一致不生成", async () => {
    const api = await import("../../api/vault");
    const onError = vi.fn();
    render(<GenerateKeyDialog onClose={() => {}} onError={onError} />);
    const [first] = document.querySelectorAll<HTMLInputElement>('input[type="password"]');
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(first, "abc");
      first.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => screen.getByRole("button", { name: "生成" }).click());
    expect(onError).toHaveBeenCalledWith("两次输入的口令不一样");
    expect(api.identityGenerate).not.toHaveBeenCalled();
  });

  it("安装公钥后默认把主机改成密钥登录", async () => {
    const api = await import("../../api/vault");
    const onDone = vi.fn();
    render(<InstallKeyDialog host={HOST} onClose={() => {}} onDone={onDone} onError={() => {}} />);
    await act(async () => {});
    await act(async () => screen.getByRole("button", { name: "安装" }).click());
    expect(api.hostInstallPublicKey).toHaveBeenCalledWith(3, 7);
    expect(api.saveHost).toHaveBeenCalledWith(
      expect.objectContaining({ id: 3, auth_method: "key", identity_id: 7 }),
    );
    expect(onDone.mock.calls[0]?.[0]).toContain("公钥已装到");
  });
});
