/** @vitest-environment jsdom */
//! 主机编辑面板的密码「小眼睛」：默认不显示明文；点一下才从保险库取出已存的密码显示，
//! 再点一下藏起来；只看不改保存时不重写保险库；凭据换了取出来的密码作废。

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Host, Identity } from "../../api/vault";

const mocks = vi.hoisted(() => ({
  saveHost: vi.fn(),
  saveIdentity: vi.fn(),
  reveal: vi.fn(),
}));

vi.mock("../../api/vault", () => ({
  listGroups: vi.fn(async () => []),
  listHosts: vi.fn(async () => []),
  listIdentities: vi.fn(async () => []),
  listProxies: vi.fn(async () => []),
  hostMemoryGet: vi.fn(async () => null),
  hostMemorySet: vi.fn(async () => {}),
  saveProxy: vi.fn(),
  saveHost: mocks.saveHost,
  saveIdentity: mocks.saveIdentity,
  revealIdentityPassword: mocks.reveal,
  saveGroup: vi.fn(),
  deleteGroup: vi.fn(),
  deleteHost: vi.fn(),
  deleteIdentity: vi.fn(),
  deleteProxy: vi.fn(),
  defaultProxyGet: vi.fn(async () => null),
  defaultProxySet: vi.fn(async () => {}),
}));
vi.mock("../../api/auth", () => ({
  hostkeySaved: vi.fn(async () => null),
  hostkeyForget: vi.fn(async () => 0),
}));
vi.mock("../../api/settings", () => ({
  settingsGet: vi.fn(async () => null),
  settingsSet: vi.fn(async () => {}),
}));

import HostDetailsPanel from "./HostDetailsPanel";
import { useVault } from "../../stores/vault";

const SAVED: Identity = {
  id: 7,
  name: "web-1 的密码",
  kind: "password",
  key_path: null,
  has_secret: true,
};

const HOST: Host = {
  id: 3,
  group_id: null,
  name: "web-1",
  host: "10.0.0.11",
  port: 22,
  username: "deploy",
  auth_method: "password",
  identity_id: SAVED.id,
  proxy_id: null,
  jump_host_id: null,
  proxy_mode: "default",
  tags: [],
  sort: 0,
  legacy_algorithms: false,
  created_at: 0,
  updated_at: 0,
};

afterEach(cleanup);

beforeEach(() => {
  mocks.saveHost.mockReset();
  mocks.saveHost.mockImplementation(async () => HOST);
  mocks.saveIdentity.mockReset();
  mocks.saveIdentity.mockImplementation(async () => SAVED);
  mocks.reveal.mockReset();
  mocks.reveal.mockImplementation(async () => "vault-pw");
  useVault.setState({ groups: [], hosts: [HOST], identities: [SAVED], proxies: [], loaded: true });
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
});

function mount(host: Host, onError: (message: string) => void = () => {}) {
  render(
    <HostDetailsPanel
      host={host}
      groupId={null}
      onClose={() => {}}
      onSaved={() => {}}
      onDelete={() => {}}
      onError={onError}
    />,
  );
}

const field = () => document.querySelector('[data-field="password"]') as HTMLInputElement;
const eye = () => document.querySelector('[data-action="toggle-password"]') as HTMLButtonElement | null;
const clickEye = () => act(async () => eye()!.click());

describe("密码小眼睛", () => {
  it("默认藏着、不取明文；点一下取出已存的密码显示，再点一下藏起来", async () => {
    mount(HOST);
    expect(field().type).toBe("password");
    expect(field().value).toBe("");
    expect(field().placeholder).toBe("••••••••");
    expect(mocks.reveal).not.toHaveBeenCalled();

    await clickEye();
    expect(mocks.reveal).toHaveBeenCalledWith(SAVED.id);
    expect(field().type).toBe("text");
    expect(field().value).toBe("vault-pw");

    await clickEye();
    expect(field().type).toBe("password");
    expect(field().value).toBe("vault-pw");
    expect(mocks.reveal).toHaveBeenCalledTimes(1);
  });

  it("只看不改就保存：不重写保险库，主机还用原来的凭据", async () => {
    mount(HOST);
    await clickEye();
    await act(async () => screen.getByText("保存").click());
    expect(mocks.saveIdentity).not.toHaveBeenCalled();
    expect(mocks.saveHost).toHaveBeenCalledWith(expect.objectContaining({ identity_id: SAVED.id }));
  });

  it("看了之后改了密码：照常写进这条凭据", async () => {
    mount(HOST);
    await clickEye();
    fireEvent.change(field(), { target: { value: "new-pw" } });
    await act(async () => screen.getByText("保存").click());
    expect(mocks.saveIdentity).toHaveBeenCalledWith(
      expect.objectContaining({ id: SAVED.id, secret: "new-pw" }),
    );
  });

  it("自己输入的密码也能点小眼睛看，不去保险库取", async () => {
    mount({ ...HOST, identity_id: null });
    expect(eye()).toBeNull();
    fireEvent.change(field(), { target: { value: "typed" } });
    await clickEye();
    expect(field().type).toBe("text");
    expect(field().value).toBe("typed");
    expect(mocks.reveal).not.toHaveBeenCalled();
  });

  it("没存密码的凭据：没有小眼睛，也不会去取", () => {
    useVault.setState({ identities: [{ ...SAVED, has_secret: false }] });
    mount(HOST);
    expect(eye()).toBeNull();
    expect(field().placeholder).toBe("");
  });

  it("取不出来：报错，框里不显示东西", async () => {
    const onError = vi.fn();
    mocks.reveal.mockImplementation(async () => {
      throw "这台主机没有保存密码";
    });
    mount(HOST, onError);
    await clickEye();
    expect(onError).toHaveBeenCalledWith("这台主机没有保存密码");
    expect(field().type).toBe("password");
    expect(field().value).toBe("");
  });

  it("凭据变了（比如同步过来改成了私钥）：取出来的密码作废，不会存进别的凭据", async () => {
    mount(HOST);
    await clickEye();
    expect(field().value).toBe("vault-pw");
    await act(async () => {
      useVault.setState({ identities: [{ ...SAVED, kind: "key_inline" }] });
    });
    expect(field().value).toBe("");
    expect(field().type).toBe("password");
  });
});
