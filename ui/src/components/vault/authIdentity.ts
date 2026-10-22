import type { AuthMethod, IdentityKind } from "../../api/vault";

/**
 * 这种凭据能不能配这种认证方式（和后端 `check_identity_matches` 一致）。
 *
 * 密码认证只能用密码凭据：私钥类凭据的秘密是私钥口令或整段私钥，配给密码认证就会被当成密码
 * 发给服务器（R-10 H2）。agent 认证不读凭据里的秘密，只列 agent 凭据，免得看起来像会用到。
 */
export function identityFitsAuth(kind: IdentityKind, method: AuthMethod): boolean {
  switch (method) {
    case "password":
      return kind === "password";
    case "key":
      return kind === "key_file" || kind === "key_inline";
    case "agent":
      return kind === "agent";
  }
}

/** 保存凭据时要发给后端的 `secret`：`null` 表示不改，`""` 表示清掉，其他是新内容。 */
export function identitySecretToSend(input: {
  /** 正在编辑的已有凭据；新建时是 null。 */
  editing: { kind: IdentityKind } | null;
  kind: IdentityKind;
  /** 密码 / 私钥文件口令输入框。 */
  secret: string;
  /** 导入私钥的正文输入框。 */
  inlineKey: string;
}): string | null {
  const { editing, kind, secret, inlineKey } = input;
  const typed = kind === "key_inline" ? inlineKey : kind === "agent" ? "" : secret;
  if (typed !== "") {
    return typed;
  }
  if (!editing) {
    return "";
  }
  // 改了类型又没给新内容：原来存的秘密（整段私钥、私钥口令或密码）对新类型没有意义，
  // 带过去还可能被当成密码发出去，所以清掉。后端对“改类型 + 不改秘密”会直接拒绝（R7-01）。
  return editing.kind === kind ? null : "";
}

/**
 * 保存导入的私钥（key_inline）时要发给后端的 `passphrase`（C14）。
 *
 * 后端把私钥内容和口令分开合并：`null` 保留原口令，`""` 清掉，其他是新口令。
 * 口令框留空的意思是"不改"——不能因为重新粘贴了私钥就顺手把原口令清掉；
 * 要去掉口令得明确勾选"清除口令"。
 */
export function inlinePassphraseToSend(input: {
  kind: IdentityKind;
  /** 口令输入框。 */
  passphrase: string;
  /** 用户勾了"清除已保存的口令"。 */
  clear: boolean;
}): string | null {
  const { kind, passphrase, clear } = input;
  if (kind !== "key_inline") return null;
  if (clear) return "";
  return passphrase !== "" ? passphrase : null;
}

/** 编辑已有凭据时改了类型，且原来存过秘密：保存会清掉它，界面要提前说。 */
export function identityKindChangeClearsSecret(
  editing: { kind: IdentityKind; has_secret: boolean } | null,
  kind: IdentityKind,
): boolean {
  return editing !== null && editing.has_secret && editing.kind !== kind;
}
