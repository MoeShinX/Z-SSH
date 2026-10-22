import { describe, expect, it } from "vitest";

import {
  identityFitsAuth,
  identityKindChangeClearsSecret,
  identitySecretToSend,
  inlinePassphraseToSend,
} from "./authIdentity";

describe("导入私钥的口令：留空不改、明确清除（C14）", () => {
  it("口令框留空 = 保留原口令（重新粘贴私钥也不清掉）", () => {
    expect(inlinePassphraseToSend({ kind: "key_inline", passphrase: "", clear: false })).toBeNull();
  });

  it("填了新口令就发新口令（只改口令时后端会沿用原私钥内容）", () => {
    expect(inlinePassphraseToSend({ kind: "key_inline", passphrase: "p", clear: false })).toBe("p");
  });

  it("勾了清除口令：发空串，输入框里残留的内容也不发", () => {
    expect(inlinePassphraseToSend({ kind: "key_inline", passphrase: "", clear: true })).toBe("");
    expect(inlinePassphraseToSend({ kind: "key_inline", passphrase: "p", clear: true })).toBe("");
  });

  it("别的类型不发口令", () => {
    expect(inlinePassphraseToSend({ kind: "key_file", passphrase: "p", clear: false })).toBeNull();
    expect(inlinePassphraseToSend({ kind: "password", passphrase: "p", clear: true })).toBeNull();
  });
});

describe("认证方式和凭据类型配套（R-10 H2）", () => {
  it("密码认证只接受密码凭据", () => {
    expect(identityFitsAuth("password", "password")).toBe(true);
    expect(identityFitsAuth("key_inline", "password")).toBe(false);
    expect(identityFitsAuth("key_file", "password")).toBe(false);
    expect(identityFitsAuth("agent", "password")).toBe(false);
  });

  it("私钥认证只接受私钥凭据", () => {
    expect(identityFitsAuth("key_file", "key")).toBe(true);
    expect(identityFitsAuth("key_inline", "key")).toBe(true);
    expect(identityFitsAuth("password", "key")).toBe(false);
  });

  it("agent 认证只列 agent 凭据", () => {
    expect(identityFitsAuth("agent", "agent")).toBe(true);
    expect(identityFitsAuth("password", "agent")).toBe(false);
  });
});

describe("保存凭据时发给后端的秘密（R7-01）", () => {
  const editingInline = { kind: "key_inline" as const };

  it("类型没变、输入框留空：不改原来的秘密", () => {
    expect(
      identitySecretToSend({
        editing: editingInline,
        kind: "key_inline",
        secret: "",
        inlineKey: "",
      }),
    ).toBeNull();
  });

  it("改了类型、没填新内容：清掉原来的秘密，不能带到新类型里", () => {
    expect(
      identitySecretToSend({
        editing: editingInline,
        kind: "password",
        secret: "",
        inlineKey: "",
      }),
    ).toBe("");
    expect(
      identitySecretToSend({
        editing: { kind: "key_file" },
        kind: "password",
        secret: "",
        inlineKey: "",
      }),
    ).toBe("");
    expect(
      identitySecretToSend({
        editing: { kind: "password" },
        kind: "agent",
        secret: "",
        inlineKey: "",
      }),
    ).toBe("");
  });

  it("填了新内容就发新内容；导入私钥看私钥正文框", () => {
    expect(
      identitySecretToSend({
        editing: editingInline,
        kind: "password",
        secret: "pw",
        inlineKey: "",
      }),
    ).toBe("pw");
    expect(
      identitySecretToSend({
        editing: { kind: "password" },
        kind: "key_inline",
        secret: "old",
        inlineKey: "KEY",
      }),
    ).toBe("KEY");
  });

  it("新建时留空就是没有秘密", () => {
    expect(
      identitySecretToSend({
        editing: null,
        kind: "password",
        secret: "",
        inlineKey: "",
      }),
    ).toBe("");
  });

  it("只有改了类型且原来存过秘密才提示会清掉", () => {
    expect(
      identityKindChangeClearsSecret({ kind: "key_inline", has_secret: true }, "password"),
    ).toBe(true);
    expect(
      identityKindChangeClearsSecret({ kind: "key_inline", has_secret: false }, "password"),
    ).toBe(false);
    expect(identityKindChangeClearsSecret({ kind: "password", has_secret: true }, "password")).toBe(
      false,
    );
    expect(identityKindChangeClearsSecret(null, "password")).toBe(false);
  });
});
