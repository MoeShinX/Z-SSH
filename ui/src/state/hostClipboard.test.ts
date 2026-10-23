import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api/vault", () => ({
  copyHostPassword: vi.fn(),
  copyHostInfo: vi.fn(),
}));

import * as api from "../api/vault";
import type { Identity } from "../api/vault";
import {
  canCopyPassword,
  copyInfoToClipboard,
  copyPasswordToClipboard,
} from "./hostClipboard";

const identity = (over: Partial<Identity>): Identity => ({
  id: 5,
  name: "生产",
  kind: "password",
  key_path: null,
  has_secret: true,
  ...over,
});

describe("canCopyPassword", () => {
  const host = { auth_method: "password" as const, identity_id: 5 as number | null };

  it("密码登录 + 密码类凭据 + 凭据里存了密码：能复制", () => {
    expect(canCopyPassword(host, [identity({})])).toBe(true);
  });

  it("密钥登录、agent 登录不能复制", () => {
    expect(canCopyPassword({ ...host, auth_method: "key" }, [identity({})])).toBe(false);
    expect(canCopyPassword({ ...host, auth_method: "agent" }, [identity({})])).toBe(false);
  });

  it("没引用凭据、凭据找不到不能复制", () => {
    expect(canCopyPassword({ ...host, identity_id: null }, [identity({})])).toBe(false);
    expect(canCopyPassword(host, [])).toBe(false);
    expect(canCopyPassword(host, [identity({ id: 9 })])).toBe(false);
  });

  it("私钥类凭据不能当密码复制", () => {
    for (const kind of ["key_file", "key_inline", "agent"] as const) {
      expect(canCopyPassword(host, [identity({ kind })])).toBe(false);
    }
  });

  it("凭据里没存密码不能复制", () => {
    expect(canCopyPassword(host, [identity({ has_secret: false })])).toBe(false);
  });
});

describe("复制命令的提示", () => {
  beforeEach(() => {
    vi.mocked(api.copyHostPassword).mockReset();
    vi.mocked(api.copyHostInfo).mockReset();
  });

  it("复制密码成功：提示 30 秒后清除，命令只带主机 id", async () => {
    vi.mocked(api.copyHostPassword).mockResolvedValue();
    const result = await copyPasswordToClipboard(3);
    expect(api.copyHostPassword).toHaveBeenCalledWith(3);
    expect(result).toEqual({ ok: true, message: "已复制密码，30 秒后自动清除" });
  });

  it("复制密码失败：带上后端给的原因", async () => {
    vi.mocked(api.copyHostPassword).mockRejectedValue("这台主机没有保存密码");
    const result = await copyPasswordToClipboard(3);
    expect(result.ok).toBe(false);
    expect(result.message).toBe("复制密码失败：这台主机没有保存密码");
  });

  it("复制连接信息：带密码和不带密码的提示不同", async () => {
    vi.mocked(api.copyHostInfo).mockResolvedValueOnce({ with_password: true });
    expect(await copyInfoToClipboard(4)).toEqual({
      ok: true,
      message: "已复制连接信息（含密码），30 秒后自动清除",
    });
    vi.mocked(api.copyHostInfo).mockResolvedValueOnce({ with_password: false });
    expect(await copyInfoToClipboard(4)).toEqual({ ok: true, message: "已复制连接信息" });
    expect(api.copyHostInfo).toHaveBeenCalledWith(4);
  });

  it("复制连接信息失败：Error 和字符串原因都能带出来", async () => {
    vi.mocked(api.copyHostInfo).mockRejectedValueOnce(new Error("剪贴板被占用，请稍后再试"));
    expect((await copyInfoToClipboard(4)).message).toBe("复制连接信息失败：剪贴板被占用，请稍后再试");
    vi.mocked(api.copyHostInfo).mockRejectedValueOnce("找不到主机");
    const result = await copyInfoToClipboard(4);
    expect(result.ok).toBe(false);
    expect(result.message).toBe("复制连接信息失败：找不到主机");
  });
});
