//! 保险库 store：凭据按创建时间倒序（新建的在最上面）。

import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
}));

vi.mock("../api/vault", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/vault")>();
  return { ...actual, ...api };
});

import { useVault } from "./vault";

beforeEach(() => {
  vi.clearAllMocks();
});

describe("凭据顺序", () => {
  it("新建的（id 大）排在最上面，旧的往下排", async () => {
    api.listIdentities.mockResolvedValue([
      { id: 1, name: "最早的密码", kind: "password", key_path: null, has_secret: true },
      { id: 7, name: "昨天生成的密钥", kind: "key_file", key_path: "C:\\k\\id_ed25519", has_secret: false },
      { id: 3, name: "上个月的密码", kind: "password", key_path: null, has_secret: true },
    ] as unknown as never);
    await useVault.getState().load();
    expect(useVault.getState().identities.map((item) => item.id)).toEqual([7, 3, 1]);
  });

  it("空库也不报错", async () => {
    api.listIdentities.mockResolvedValue([]);
    await useVault.getState().load();
    expect(useVault.getState().identities).toEqual([]);
  });
});
