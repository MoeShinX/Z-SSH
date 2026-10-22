//! 凭据的增删改查。密文只走 DPAPI，接口不回传。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::ZsshError;
use crate::model::{Identity, IdentityDraft, IdentityKind};
use crate::store::db::Database;

impl Database {
    pub fn identity_list(&self) -> Result<Vec<Identity>, ZsshError> {
        self.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, name, kind, key_path, secret_blob IS NOT NULL AND length(secret_blob) > 0 \
                 FROM identities ORDER BY name COLLATE NOCASE",
            )?;
            let rows = statement.query_map([], row_to_identity)?;
            let mut identities = Vec::new();
            for row in rows {
                identities.push(row?);
            }
            Ok(identities)
        })
    }

    /// 新建或修改一条凭据。
    ///
    /// `secret` 为 `None` 表示不改动已存的密文；`Some("")` 表示清掉密文。
    /// `key_inline` 的私钥内容和口令在同一个密文里，`secret`（私钥内容）和
    /// `passphrase` 各自按"`None` 保留 / 空串清掉 / 有值更新"处理，见 [`inline_key_update`]。
    pub fn identity_save(&self, draft: &IdentityDraft) -> Result<Identity, ZsshError> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err(ZsshError::InvalidInput("凭据名不能为空".to_owned()));
        }
        if draft.kind == IdentityKind::KeyFile
            && draft.key_path.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(ZsshError::InvalidInput("私钥文件凭据要填路径".to_owned()));
        }

        self.with_conn_write(|conn| {
            let now = crate::store::hosts::now_unix();
            // 修改时先读出原来的类型和密文：R7-01 的检查和 key_inline 的合并都要用
            let existing: Option<(String, Option<Vec<u8>>)> = match draft.id {
                Some(id) => conn
                    .query_row(
                        "SELECT kind, secret_blob FROM identities WHERE id = ?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?,
                None => None,
            };
            let same_kind = existing
                .as_ref()
                .is_some_and(|(kind, _)| kind == draft.kind.as_str());
            let had_secret = existing
                .as_ref()
                .is_some_and(|(_, blob)| blob.as_ref().is_some_and(|b| !b.is_empty()));

            // 类型和秘密是一体的：改了类型却不给新秘密，旧密文（比如整段私钥或私钥口令）
            // 会被新类型继续使用，密码认证时就会把它当 SSH 密码发给服务器（R7-01）。
            // 要么同时给新秘密（空串表示清掉），要么类型不变。
            if existing.is_some() && !same_kind && had_secret && draft.secret.is_none() {
                return Err(ZsshError::InvalidInput(
                    "改凭据类型时要同时重新填写密码或私钥，原来保存的内容不会带到新类型里"
                        .to_owned(),
                ));
            }

            let secret_blob = match draft.kind {
                IdentityKind::KeyInline => {
                    // 只有原来就是 key_inline 才合并旧信封：别的类型的密文（比如密码）
                    // 不能被当成私钥内容或口令带过来
                    let update = inline_key_update(
                        draft.secret.as_deref(),
                        draft.passphrase.as_deref(),
                        || match (draft.id, same_kind) {
                            (Some(id), true) => Ok(identity_secret(conn, id)?
                                .map(|secret| InlineKey::parse(&secret))),
                            _ => Ok(None),
                        },
                    )?;
                    match update {
                        InlineKeyUpdate::Keep => None,
                        InlineKeyUpdate::Clear => Some(Vec::new()),
                        InlineKeyUpdate::Set(inline) => Some(crate::store::secrets::protect(
                            inline.to_secret()?.as_bytes(),
                        )?),
                    }
                }
                _ => match draft.secret.as_deref() {
                    // 留空表示"不修改"，新建时就是没有密文
                    None => None,
                    // 空串表示清掉
                    Some("") => Some(Vec::new()),
                    Some(secret) => Some(crate::store::secrets::protect(secret.as_bytes())?),
                },
            };

            let id = match draft.id {
                Some(id) => {
                    let changed = match &secret_blob {
                        Some(blob) => conn.execute(
                            "UPDATE identities SET name = ?1, kind = ?2, key_path = ?3, \
                             secret_blob = ?4, updated_at = ?5 WHERE id = ?6",
                            params![name, draft.kind.as_str(), draft.key_path, blob, now, id],
                        )?,
                        None => conn.execute(
                            "UPDATE identities SET name = ?1, kind = ?2, key_path = ?3, \
                             updated_at = ?4 WHERE id = ?5",
                            params![name, draft.kind.as_str(), draft.key_path, now, id],
                        )?,
                    };
                    if changed == 0 {
                        return Err(ZsshError::NotFound {
                            entity: "凭据",
                            id,
                        });
                    }
                    id
                }
                None => {
                    conn.execute(
                        "INSERT INTO identities (uid, name, kind, key_path, secret_blob, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            crate::store::new_uid(),
                            name,
                            draft.kind.as_str(),
                            draft.key_path,
                            secret_blob,
                            now
                        ],
                    )?;
                    conn.last_insert_rowid()
                }
            };
            identity_get(conn, id)
        })
    }

    /// 在 Z-SSH 里生成一把新密钥，存成"导入的私钥"凭据。RSA 4096 要几秒，调用方放到阻塞线程里。
    pub fn identity_generate(
        &self,
        name: &str,
        algorithm: crate::ssh::keygen::KeyAlgorithm,
        comment: &str,
        passphrase: Option<&str>,
    ) -> Result<Identity, ZsshError> {
        let key = crate::ssh::keygen::generate(algorithm, comment, passphrase)?;
        self.identity_save(&IdentityDraft {
            id: None,
            name: name.to_owned(),
            kind: IdentityKind::KeyInline,
            key_path: None,
            secret: Some(key.private_openssh.to_string()),
            passphrase: passphrase.filter(|p| !p.is_empty()).map(str::to_owned),
        })
    }

    /// 私钥凭据的公钥和指纹（复制公钥、装到服务器上用）。私钥文件读不了、口令不对都报错。
    pub fn identity_public_key(&self, id: i64) -> Result<(String, String), ZsshError> {
        let (identity, secret) =
            self.with_conn(|conn| Ok((identity_get(conn, id)?, identity_secret(conn, id)?)))?;
        match identity.kind {
            IdentityKind::KeyInline => {
                let inline = InlineKey::parse(secret.as_deref().unwrap_or_default());
                crate::ssh::keygen::public_key_of(&inline.key, inline.passphrase.as_deref())
            }
            IdentityKind::KeyFile => {
                let path = identity
                    .key_path
                    .filter(|path| !path.trim().is_empty())
                    .ok_or_else(|| {
                        ZsshError::InvalidInput("这条凭据没有填私钥文件路径".to_owned())
                    })?;
                let pem = std::fs::read_to_string(crate::sftp::local::long_path(
                    std::path::Path::new(&path),
                ))
                .map_err(|err| ZsshError::InvalidInput(format!("读不了私钥文件：{err}")))?;
                crate::ssh::keygen::public_key_of(&pem, secret.as_deref())
            }
            IdentityKind::Password | IdentityKind::Agent => Err(ZsshError::InvalidInput(
                "这条凭据不是私钥，没有公钥".to_owned(),
            )),
        }
    }

    /// 给 `key_inline` 凭据补上（或换掉）口令。
    ///
    /// 弹窗里勾了"保存口令"时用：导入的私钥内容和口令放在同一个密文里，
    /// 所以要把信封解出来、换上口令、再加密回去。
    pub fn identity_set_inline_passphrase(
        &self,
        id: i64,
        passphrase: &str,
    ) -> Result<Identity, ZsshError> {
        self.with_conn_write(|conn| {
            let identity = identity_get(conn, id)?;
            if identity.kind != IdentityKind::KeyInline {
                return Err(ZsshError::InvalidInput(format!(
                    "凭据「{}」不是导入的私钥，存不了口令",
                    identity.name
                )));
            }

            let secret = identity_secret(conn, id)?;
            let mut inline = InlineKey::parse(secret.as_deref().unwrap_or_default());
            inline.passphrase = Some(passphrase.to_owned());

            let blob = crate::store::secrets::protect(inline.to_secret()?.as_bytes())?;
            conn.execute(
                "UPDATE identities SET secret_blob = ?1, updated_at = ?2 WHERE id = ?3",
                params![blob, crate::store::hosts::now_unix(), id],
            )?;
            identity_get(conn, id)
        })
    }

    /// 删凭据：还被主机引用时拒绝，并列出主机名（任务卡 2.3 的规则）。
    pub fn identity_delete(&self, id: i64) -> Result<(), ZsshError> {
        self.with_conn_write(|conn| {
            let identity = identity_get(conn, id)?;
            let hosts = crate::store::hosts::hosts_referencing(conn, "identity_id", id)?;
            if !hosts.is_empty() {
                return Err(ZsshError::IdentityInUse {
                    name: identity.name,
                    hosts: hosts.join("、"),
                });
            }
            conn.execute("DELETE FROM identities WHERE id = ?1", [id])?;
            Ok(())
        })
    }
}

/// `key_inline` 凭据的密文内容：私钥内容 + 可选口令。
///
/// 同一个 `secret_blob` 里要放两样东西，所以包成 JSON。M1-03 存过裸的私钥文本，
/// 解析不出 JSON 就按"整段都是私钥内容、没有口令"处理。
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InlineKey {
    /// OpenSSH 或 PEM 格式的私钥文本
    pub key: String,
    #[serde(default)]
    pub passphrase: Option<String>,
}

/// `Debug` 手写：私钥内容和口令都是明文秘密，不能进日志（R-8 第 11 节）。
impl std::fmt::Debug for InlineKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineKey")
            .field("key", &"***")
            .field("passphrase", &self.passphrase.as_ref().map(|_| "***"))
            .finish()
    }
}

impl InlineKey {
    /// 解析保险库里解出来的文本，兼容裸私钥。
    pub fn parse(secret: &str) -> Self {
        match serde_json::from_str::<Self>(secret) {
            Ok(parsed) if !parsed.key.trim().is_empty() => parsed,
            _ => Self {
                key: secret.to_owned(),
                passphrase: None,
            },
        }
    }

    /// 序列化成要加密的文本。
    pub fn to_secret(&self) -> Result<String, ZsshError> {
        serde_json::to_string(self)
            .map_err(|err| ZsshError::Secret(format!("私钥内容序列化失败：{err}")))
    }
}

/// 保存 `key_inline` 凭据时密文要怎么变。
#[derive(Debug)]
pub(crate) enum InlineKeyUpdate {
    /// 密文不动
    Keep,
    /// 清掉整个密文（私钥内容连同口令）
    Clear,
    /// 换成这份私钥内容 + 口令
    Set(InlineKey),
}

/// 合并 `key_inline` 的私钥内容和口令（C14）。
///
/// 两样东西在同一个密文里，但界面上是两个独立的输入框，所以各自按
/// "`None` 保留 / 空串清掉 / 有值更新"处理，不能只看 `secret` 决定整个更新：
/// 原来只改口令（`secret` 为 `None`）时密文不动却报保存成功；重新粘贴私钥不填口令
/// 又把原口令清掉了。
///
/// `old` 只在需要旧信封时才调用（解密有代价，也可能失败）；新建或原来不是
/// `key_inline` 时由调用方返回 `None`。
pub(crate) fn inline_key_update(
    secret: Option<&str>,
    passphrase: Option<&str>,
    old: impl FnOnce() -> Result<Option<InlineKey>, ZsshError>,
) -> Result<InlineKeyUpdate, ZsshError> {
    if secret == Some("") {
        return Ok(InlineKeyUpdate::Clear);
    }
    if secret.is_none() && passphrase.is_none() {
        return Ok(InlineKeyUpdate::Keep);
    }
    // 两样都给了新值就用不着旧信封
    let old = match (secret, passphrase) {
        (Some(_), Some(_)) => None,
        _ => old()?,
    };
    let key = match (secret, &old) {
        (Some(key), _) => key.to_owned(),
        (None, Some(old)) if !old.key.trim().is_empty() => old.key.clone(),
        // 只改口令却没有私钥内容：口令没有地方放，不能假装保存成功
        (None, _) => {
            return Err(ZsshError::InvalidInput(
                "这条凭据还没有导入私钥内容，先粘贴私钥再设置口令".to_owned(),
            ))
        }
    };
    let passphrase = match passphrase {
        None => old.and_then(|old| old.passphrase),
        Some("") => None,
        Some(passphrase) => Some(passphrase.to_owned()),
    };
    Ok(InlineKeyUpdate::Set(InlineKey { key, passphrase }))
}

pub(crate) fn identity_get(conn: &Connection, id: i64) -> Result<Identity, ZsshError> {
    conn.query_row(
        "SELECT id, name, kind, key_path, secret_blob IS NOT NULL AND length(secret_blob) > 0 \
         FROM identities WHERE id = ?1",
        [id],
        row_to_identity,
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => ZsshError::NotFound {
            entity: "凭据", id
        },
        other => other.into(),
    })
}

/// 解出明文密文，只给后端连接用，不进任何返回给前端的结构体。
pub(crate) fn identity_secret(conn: &Connection, id: i64) -> Result<Option<String>, ZsshError> {
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT secret_blob FROM identities WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => ZsshError::NotFound {
                entity: "凭据", id
            },
            other => other.into(),
        })?;

    match blob {
        Some(blob) if !blob.is_empty() => {
            let plaintext = crate::store::secrets::unprotect(&blob)?;
            String::from_utf8(plaintext)
                .map(Some)
                .map_err(|err| ZsshError::Secret(format!("解出来的凭据不是 UTF-8：{err}")))
        }
        _ => Ok(None),
    }
}

pub(crate) fn ensure_identity_exists(conn: &Connection, id: i64) -> Result<(), ZsshError> {
    identity_get(conn, id).map(|_| ())
}

fn row_to_identity(row: &Row<'_>) -> rusqlite::Result<Identity> {
    let kind: String = row.get(2)?;
    Ok(Identity {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: crate::store::parse_column(&kind, IdentityKind::parse)?,
        key_path: row.get(3)?,
        has_secret: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_envelope(passphrase: Option<&str>) -> Result<Option<InlineKey>, ZsshError> {
        Ok(Some(InlineKey {
            key: "old-key-material".to_owned(),
            passphrase: passphrase.map(str::to_owned),
        }))
    }

    fn set(update: InlineKeyUpdate) -> InlineKey {
        match update {
            InlineKeyUpdate::Set(inline) => inline,
            other => panic!("应该是 Set，实际 {other:?}"),
        }
    }

    #[test]
    fn inline_update_changes_only_the_passphrase_and_keeps_the_key() {
        // C14：只改口令时私钥内容沿用旧信封，口令换成新的
        let inline =
            set(inline_key_update(None, Some("new-pass"), || old_envelope(Some("old"))).unwrap());
        assert_eq!(inline.key, "old-key-material");
        assert_eq!(inline.passphrase.as_deref(), Some("new-pass"));
    }

    #[test]
    fn inline_update_new_key_without_passphrase_keeps_the_old_passphrase() {
        // C14：重新粘贴私钥、口令框留空（None = 不改）不能把原口令清掉
        let inline =
            set(inline_key_update(Some("new-key"), None, || old_envelope(Some("old"))).unwrap());
        assert_eq!(inline.key, "new-key");
        assert_eq!(inline.passphrase.as_deref(), Some("old"));
    }

    #[test]
    fn inline_update_empty_passphrase_clears_it() {
        let inline = set(inline_key_update(None, Some(""), || old_envelope(Some("old"))).unwrap());
        assert_eq!(inline.key, "old-key-material");
        assert_eq!(inline.passphrase, None);
    }

    #[test]
    fn inline_update_keep_and_clear_do_not_touch_the_old_envelope() {
        let untouched = || -> Result<Option<InlineKey>, ZsshError> { panic!("不该解旧密文") };
        assert!(matches!(
            inline_key_update(None, None, untouched).unwrap(),
            InlineKeyUpdate::Keep
        ));
        assert!(matches!(
            inline_key_update(Some(""), Some("x"), untouched).unwrap(),
            InlineKeyUpdate::Clear
        ));
        let both = set(inline_key_update(Some("k"), Some("p"), untouched).unwrap());
        assert_eq!(
            (both.key.as_str(), both.passphrase.as_deref()),
            ("k", Some("p"))
        );
    }

    #[test]
    fn inline_update_passphrase_without_any_key_is_refused() {
        let err = inline_key_update(None, Some("p"), || Ok(None)).unwrap_err();
        assert!(err.to_string().contains("先粘贴私钥"), "{err}");
    }

    #[test]
    fn inline_key_debug_hides_the_private_key_and_passphrase() {
        // 内联私钥和口令都在 `InlineKey` 里，Debug 输出不能带（R-8 第 11 节）。
        // 测试数据故意不用真实的私钥头（隐私扫描会把源码里的 `BEGIN ... PRIVATE KEY`
        // 当成泄漏的私钥），只要是一段不该出现在 Debug 里的秘密内容就够了。
        let key = InlineKey {
            key: "ssh-ed25519-secret-key-material\nnot-a-real-key-body".to_owned(),
            passphrase: Some("s3cret-passphrase".to_owned()),
        };
        let debug = format!("{key:?}");
        assert!(!debug.contains("secret-key-material"), "{debug}");
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(debug.contains("***"), "{debug}");
        // 解析 / 序列化逻辑不受 Debug 改动影响。
        assert_eq!(
            InlineKey::parse(r#"{"key":"k","passphrase":"p"}"#).passphrase,
            Some("p".to_owned())
        );
    }
}
