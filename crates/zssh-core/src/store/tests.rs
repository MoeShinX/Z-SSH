//! 存储层的单元测试。
//!
//! 在 crate 内部（而不是 tests/）是因为要读原始数据库文件、直接用 rusqlite 查表，
//! 验证"密文里没有明文"这类性质。

use std::path::PathBuf;

use super::*;
use crate::model::{
    AuthMethod, GroupDraft, HostDraft, IdentityDraft, IdentityKind, ProxyDraft, ProxyKind,
    ProxyMode,
};
use crate::ssh::auth::KeySource;

fn memory_db() -> Database {
    Database::open_in_memory().expect("内存库应该能建起来")
}

/// 临时文件库，测完自己删。用来检查落盘的内容。
struct TempDb {
    path: PathBuf,
    /// 放在最后：目录守卫最后 drop，连同 WAL/SHM 一起删掉。
    _dir: crate::test_util::TestDir,
}

impl TempDb {
    fn new(tag: &str) -> Self {
        let dir = crate::test_util::TestDir::new(&format!("test-{tag}"));
        let path = dir.join("test.db");
        Self { path, _dir: dir }
    }

    fn open(&self) -> Database {
        Database::open(&self.path).expect("文件库应该能建起来")
    }

    /// 主文件和 WAL 的原始字节。
    fn raw_bytes(&self) -> Vec<u8> {
        let mut bytes = std::fs::read(&self.path).expect("应该能读数据库文件");
        let wal = self.path.with_extension("db-wal");
        if let Ok(more) = std::fs::read(&wal) {
            bytes.extend_from_slice(&more);
        }
        bytes
    }
}

// ---------- 迁移 ----------

#[test]
fn fresh_database_has_all_tables_and_latest_version() {
    let db = memory_db();
    let latest = db::MIGRATIONS
        .last()
        .map(|(version, _)| *version)
        .expect("至少要有一个迁移");
    assert_eq!(db.schema_version().expect("读版本号"), latest);

    let tables = db
        .with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut names = Vec::new();
            for row in rows {
                names.push(row?);
            }
            Ok(names)
        })
        .expect("查表名");

    for expected in [
        "ai_audit",
        "groups",
        "hosts",
        "identities",
        "proxies",
        "schema_version",
        "settings",
    ] {
        assert!(
            tables.iter().any(|name| name == expected),
            "少了表 {expected}，实际有 {tables:?}"
        );
    }
}

#[test]
fn migrations_are_idempotent() {
    let db = memory_db();
    db.migrate().expect("重复迁移不该失败");
    db.migrate().expect("再来一次也不该失败");

    let latest = db::MIGRATIONS
        .last()
        .map(|(version, _)| *version)
        .expect("至少要有一个迁移");
    assert_eq!(db.schema_version().expect("读版本号"), latest);
    let rows: i64 = db
        .with_conn(|conn| {
            Ok(conn.query_row("SELECT count(*) FROM schema_version", [], |row| row.get(0))?)
        })
        .expect("数 schema_version");
    assert_eq!(rows, db::MIGRATIONS.len() as i64, "每个迁移只该记录一次");
}

#[test]
fn file_database_uses_wal_and_foreign_keys() {
    let temp = TempDb::new("wal");
    let db = temp.open();

    let (journal, foreign_keys) = db
        .with_conn(|conn| {
            let journal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
            Ok((journal, foreign_keys))
        })
        .expect("读 pragma");

    assert_eq!(journal.to_lowercase(), "wal");
    assert_eq!(foreign_keys, 1, "外键必须打开");
}

// ---------- 分组 ----------

#[test]
fn group_tree_crud() {
    let db = memory_db();
    let root = db
        .group_save(&GroupDraft {
            id: None,
            name: "生产".to_owned(),
            parent_id: None,
            sort: 0,
        })
        .expect("建根分组");
    let child = db
        .group_save(&GroupDraft {
            id: None,
            name: "华东".to_owned(),
            parent_id: Some(root.id),
            sort: 1,
        })
        .expect("建子分组");
    assert_eq!(child.parent_id, Some(root.id));

    let renamed = db
        .group_save(&GroupDraft {
            id: Some(child.id),
            name: "华东机房".to_owned(),
            parent_id: Some(root.id),
            sort: 2,
        })
        .expect("改名");
    assert_eq!(renamed.name, "华东机房");
    assert_eq!(renamed.sort, 2);

    let all = db.group_list().expect("列分组");
    assert_eq!(all.len(), 2);
}

#[test]
fn group_delete_moves_hosts_and_children_to_root() {
    let db = memory_db();
    let parent = db
        .group_save(&GroupDraft {
            id: None,
            name: "父".to_owned(),
            parent_id: None,
            sort: 0,
        })
        .unwrap();
    let child = db
        .group_save(&GroupDraft {
            id: None,
            name: "子".to_owned(),
            parent_id: Some(parent.id),
            sort: 0,
        })
        .unwrap();
    let host = db
        .host_save(&HostDraft {
            id: None,
            group_id: Some(parent.id),
            name: "某台机器".to_owned(),
            host: "example.internal".to_owned(),
            port: 22,
            username: "root".to_owned(),
            auth_method: AuthMethod::Password,
            identity_id: None,
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: vec!["prod".to_owned()],
            sort: 0,

            legacy_algorithms: false,
        })
        .unwrap();

    db.group_delete(parent.id).expect("删父分组");

    let groups = db.group_list().unwrap();
    assert_eq!(groups.len(), 1, "只应该剩下子分组");
    assert_eq!(groups[0].id, child.id);
    assert_eq!(groups[0].parent_id, None, "子分组要挂回根目录");

    let hosts = db.host_list().unwrap();
    assert_eq!(hosts.len(), 1, "主机不能被级联删掉");
    assert_eq!(hosts[0].id, host.id);
    assert_eq!(hosts[0].group_id, None, "主机要移到根目录");
}

#[test]
fn group_cannot_move_under_its_own_child() {
    let db = memory_db();
    let parent = db
        .group_save(&GroupDraft {
            id: None,
            name: "父".to_owned(),
            parent_id: None,
            sort: 0,
        })
        .unwrap();
    let child = db
        .group_save(&GroupDraft {
            id: None,
            name: "子".to_owned(),
            parent_id: Some(parent.id),
            sort: 0,
        })
        .unwrap();

    let err = db
        .group_save(&GroupDraft {
            id: Some(parent.id),
            name: "父".to_owned(),
            parent_id: Some(child.id),
            sort: 0,
        })
        .expect_err("成环要拒绝");
    assert!(matches!(err, ZsshError::InvalidInput(_)), "实际：{err:?}");
}

// ---------- 主机 ----------

pub(crate) fn sample_host(group_id: Option<i64>, identity_id: Option<i64>) -> HostDraft {
    HostDraft {
        id: None,
        group_id,
        name: "测试机".to_owned(),
        host: "example.internal".to_owned(),
        port: 2222,
        username: "deploy".to_owned(),
        auth_method: AuthMethod::Password,
        identity_id,
        proxy_id: None,
        jump_host_id: None,
        proxy_mode: ProxyMode::Default,
        tags: vec!["测试".to_owned(), "m1".to_owned()],
        sort: 3,
        legacy_algorithms: false,
    }
}

#[test]
fn host_crud_roundtrip() {
    let db = memory_db();
    let saved = db.host_save(&sample_host(None, None)).expect("新建主机");
    assert!(saved.id > 0);
    assert_eq!(saved.tags, vec!["测试".to_owned(), "m1".to_owned()]);
    assert_eq!(saved.created_at, saved.updated_at);

    let mut draft = sample_host(None, None);
    draft.id = Some(saved.id);
    draft.name = "改过名".to_owned();
    draft.port = 22;
    draft.tags = Vec::new();
    let updated = db.host_save(&draft).expect("修改主机");
    assert_eq!(updated.id, saved.id);
    assert_eq!(updated.name, "改过名");
    assert_eq!(updated.port, 22);
    assert!(updated.tags.is_empty());
    assert_eq!(updated.created_at, saved.created_at, "created_at 不该变");

    assert_eq!(db.host_list().unwrap().len(), 1);
    assert_eq!(db.host_get(saved.id).unwrap().name, "改过名");

    db.host_delete(saved.id).expect("删除主机");
    assert!(db.host_list().unwrap().is_empty());
    assert!(matches!(
        db.host_get(saved.id),
        Err(ZsshError::NotFound { .. })
    ));
}

#[test]
fn host_validation_rejects_bad_input() {
    let db = memory_db();

    let mut draft = sample_host(None, None);
    draft.name = "  ".to_owned();
    assert!(db.host_save(&draft).is_err(), "空名字要拒绝");

    let mut draft = sample_host(None, None);
    draft.host = "".to_owned();
    assert!(db.host_save(&draft).is_err(), "空地址要拒绝");

    let mut draft = sample_host(None, None);
    draft.username = " ".to_owned();
    assert!(db.host_save(&draft).is_err(), "空用户名要拒绝");

    let mut draft = sample_host(None, None);
    draft.port = 0;
    assert!(db.host_save(&draft).is_err(), "端口 0 要拒绝");

    let mut draft = sample_host(Some(999), None);
    draft.id = None;
    assert!(db.host_save(&draft).is_err(), "不存在的分组要拒绝");
}

#[test]
fn jump_host_cannot_be_itself() {
    let db = memory_db();
    let host = db.host_save(&sample_host(None, None)).unwrap();

    let mut draft = sample_host(None, None);
    draft.id = Some(host.id);
    draft.jump_host_id = Some(host.id);
    assert!(db.host_save(&draft).is_err(), "跳板机不能是自己");
}

// ---------- 凭据 ----------

#[cfg(windows)]
#[test]
fn identity_secret_is_encrypted_and_db_has_no_plaintext() {
    const PASSWORD: &str = "distinctive-password-7c1f";

    let temp = TempDb::new("secret");
    let db = temp.open();
    let saved = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "生产密码".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some(PASSWORD.to_owned()),

            passphrase: None,
        })
        .expect("保存凭据");
    assert!(saved.has_secret, "存了密码就该有 has_secret");

    // 接口返回的结构体里不能有明文
    let json = serde_json::to_string(&db.identity_list().unwrap()).unwrap();
    assert!(!json.contains(PASSWORD), "列表接口不该带出明文：{json}");
    let json = serde_json::to_string(&saved).unwrap();
    assert!(!json.contains(PASSWORD), "详情接口不该带出明文：{json}");

    // 原始 blob 里也不能有明文
    let blob: Vec<u8> = db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT secret_blob FROM identities WHERE id = ?1",
                [saved.id],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&blob).contains(PASSWORD),
        "secret_blob 里出现了明文"
    );

    // 后端要能解回来
    let plaintext = db
        .with_conn(|conn| identities::identity_secret(conn, saved.id))
        .unwrap()
        .expect("应该能解出密码");
    assert_eq!(plaintext, PASSWORD);

    // 落盘的文件里也不能搜到
    drop(db);
    assert!(
        !String::from_utf8_lossy(&temp.raw_bytes()).contains(PASSWORD),
        "数据库文件里出现了明文"
    );
}

#[cfg(windows)]
#[test]
fn identity_save_without_secret_keeps_the_old_one() {
    let db = memory_db();
    let saved = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "密码".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("first-secret".to_owned()),

            passphrase: None,
        })
        .unwrap();

    // 界面留空 = 不修改
    let updated = db
        .identity_save(&IdentityDraft {
            id: Some(saved.id),
            name: "改个名".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: None,

            passphrase: None,
        })
        .unwrap();
    assert!(updated.has_secret, "留空不该把密码清掉");
    let secret = db
        .with_conn(|conn| identities::identity_secret(conn, saved.id))
        .unwrap();
    assert_eq!(secret.as_deref(), Some("first-secret"));

    // 传空串 = 显式清掉
    let cleared = db
        .identity_save(&IdentityDraft {
            id: Some(saved.id),
            name: "改个名".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some(String::new()),

            passphrase: None,
        })
        .unwrap();
    assert!(!cleared.has_secret, "空串应该把密码清掉");
}

#[test]
fn identity_delete_is_refused_while_a_host_uses_it() {
    let db = memory_db();
    let identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "被引用的凭据".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: None,

            passphrase: None,
        })
        .unwrap();
    let host = db.host_save(&sample_host(None, Some(identity.id))).unwrap();

    let err = db.identity_delete(identity.id).expect_err("被引用时要拒绝");
    match err {
        ZsshError::IdentityInUse {
            ref name,
            ref hosts,
        } => {
            assert_eq!(name, "被引用的凭据");
            assert!(hosts.contains(&host.name), "要报出引用它的主机：{hosts}");
        }
        other => panic!("错误类型不对：{other:?}"),
    }

    // 改掉引用之后就能删了
    let mut draft = sample_host(None, None);
    draft.id = Some(host.id);
    draft.identity_id = None;
    db.host_save(&draft).unwrap();
    db.identity_delete(identity.id).expect("没人引用了应该能删");
}

#[test]
fn proxy_delete_is_refused_while_a_host_uses_it() {
    let db = memory_db();
    let proxy = db
        .proxy_save(&ProxyDraft {
            id: None,
            name: "本地代理".to_owned(),
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 10808,
            username: None,
            secret: None,
        })
        .unwrap();
    assert!(!proxy.has_secret);

    let mut draft = sample_host(None, None);
    draft.proxy_id = Some(proxy.id);
    let host = db.host_save(&draft).unwrap();

    let err = db.proxy_delete(proxy.id).expect_err("被引用时要拒绝");
    match err {
        ZsshError::ProxyInUse { ref hosts, .. } => {
            assert!(hosts.contains(&host.name), "要报出引用它的主机：{hosts}");
        }
        other => panic!("错误类型不对：{other:?}"),
    }

    draft.id = Some(host.id);
    draft.proxy_id = None;
    db.host_save(&draft).unwrap();
    db.proxy_delete(proxy.id).expect("没人引用了应该能删");
}

#[cfg(windows)]
#[test]
fn host_login_reads_the_stored_password() {
    let db = memory_db();
    let identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "登录用".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("vault-password".to_owned()),

            passphrase: None,
        })
        .unwrap();
    let host = db.host_save(&sample_host(None, Some(identity.id))).unwrap();

    let login = db.host_login(host.id).expect("应该能取到登录信息");
    assert_eq!(login.host, "example.internal");
    assert_eq!(login.port, 2222);
    assert_eq!(login.username, "deploy");
    assert_eq!(login.auth.password.as_deref(), Some("vault-password"));
}

#[test]
fn host_login_returns_missing_password_as_none() {
    let db = memory_db();
    let host = db.host_save(&sample_host(None, None)).unwrap();

    // 没配凭据：登录信息里没有密码，连接时去问用户（M1-04）
    let login = db.host_login(host.id).expect("没配凭据也要能取到登录信息");
    assert!(login.auth.password.is_none());
    assert_eq!(login.auth.method, AuthMethod::Password);
    assert!(!login.legacy_algorithms, "老算法开关默认是关的");

    // 配了凭据但里面没有密码：同一个行为
    let empty = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "空的".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: None,
            passphrase: None,
        })
        .unwrap();
    let mut draft = sample_host(None, Some(empty.id));
    draft.id = Some(host.id);
    db.host_save(&draft).unwrap();
    assert!(db.host_login(host.id).unwrap().auth.password.is_none());
}

#[test]
fn host_login_builds_key_material() {
    let db = memory_db();
    let host = db.host_save(&sample_host(None, None)).unwrap();

    // 私钥文件：路径 + 口令
    let file_identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "私钥文件".to_owned(),
            kind: IdentityKind::KeyFile,
            key_path: Some("C:/keys/id_ed25519".to_owned()),
            secret: Some("key-passphrase".to_owned()),
            passphrase: None,
        })
        .unwrap();
    let mut draft = sample_host(None, Some(file_identity.id));
    draft.id = Some(host.id);
    draft.auth_method = AuthMethod::Key;
    db.host_save(&draft).unwrap();

    match db.host_login(host.id).unwrap().auth.key {
        Some(KeySource::File { path, passphrase }) => {
            assert_eq!(path.to_string_lossy(), "C:/keys/id_ed25519");
            assert_eq!(passphrase.as_deref(), Some("key-passphrase"));
        }
        other => panic!("应该是私钥文件，实际 {other:?}"),
    }

    // 私钥认证但没选凭据：明确报错
    let mut draft = sample_host(None, None);
    draft.id = Some(host.id);
    draft.auth_method = AuthMethod::Key;
    db.host_save(&draft).unwrap();
    let err = db.host_login(host.id).expect_err("没凭据要拒绝");
    assert!(err.to_string().contains("没选凭据"), "实际：{err}");

    // 拿密码凭据去做私钥认证：也要拒绝
    let password_identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "密码凭据".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("pw".to_owned()),
            passphrase: None,
        })
        .unwrap();
    let mut draft = sample_host(None, Some(password_identity.id));
    draft.id = Some(host.id);
    draft.auth_method = AuthMethod::Key;
    // 保存时就拒绝（R-10 H2）；绕过保存（旧数据 / 同步来的）直接写进库，登录时也要拒绝。
    let err = db.host_save(&draft).expect_err("保存时就要拒绝");
    assert!(err.to_string().contains("不是私钥凭据"), "实际：{err}");
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE hosts SET auth_method = 'key', identity_id = ?1 WHERE id = ?2",
            rusqlite::params![password_identity.id, host.id],
        )?;
        Ok(())
    })
    .unwrap();
    let err = db.host_login(host.id).expect_err("密码凭据不能当私钥用");
    assert!(err.to_string().contains("不是私钥凭据"), "实际：{err}");

    // agent 认证不需要材料
    let mut draft = sample_host(None, None);
    draft.id = Some(host.id);
    draft.auth_method = AuthMethod::Agent;
    db.host_save(&draft).unwrap();
    let login = db.host_login(host.id).unwrap();
    assert_eq!(login.auth.method, AuthMethod::Agent);
    assert!(login.auth.key.is_none());
}

#[cfg(windows)]
#[test]
fn host_login_reads_an_inline_key_with_passphrase() {
    let db = memory_db();
    let identity = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "导入的私钥".to_owned(),
            kind: IdentityKind::KeyInline,
            key_path: None,
            secret: Some("PRIVATE-KEY-TEXT-PLACEHOLDER".to_owned()),
            passphrase: Some("inline-passphrase".to_owned()),
        })
        .unwrap();

    // 密文里不能出现明文
    let blob: Vec<u8> = db
        .with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT secret_blob FROM identities WHERE id = ?1",
                [identity.id],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let text = String::from_utf8_lossy(&blob);
    assert!(
        !text.contains("PRIVATE-KEY-TEXT-PLACEHOLDER"),
        "私钥内容不该明文落盘"
    );
    assert!(!text.contains("inline-passphrase"), "口令不该明文落盘");

    let host = db
        .host_save(&HostDraft {
            id: None,
            group_id: None,
            name: "导入私钥的主机".to_owned(),
            host: "example.internal".to_owned(),
            port: 22,
            username: "deploy".to_owned(),
            auth_method: AuthMethod::Key,
            identity_id: Some(identity.id),
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: Vec::new(),
            sort: 0,
            legacy_algorithms: false,
        })
        .unwrap();

    match db.host_login(host.id).unwrap().auth.key {
        Some(KeySource::Inline { pem, passphrase }) => {
            assert_eq!(pem, "PRIVATE-KEY-TEXT-PLACEHOLDER");
            assert_eq!(passphrase.as_deref(), Some("inline-passphrase"));
        }
        other => panic!("应该是导入的私钥，实际 {other:?}"),
    }
}

#[test]
fn upgrading_from_version_one_keeps_data_and_defaults_the_new_column() {
    let temp = TempDb::new("upgrade");
    // 手工铺一个只跑了 0001 的库，再插一条旧结构的主机
    {
        let conn = rusqlite::Connection::open(&temp.path).expect("建库");
        let (version, sql) = super::db::MIGRATIONS.first().expect("第一个迁移");
        assert_eq!(*version, 1);
        conn.execute_batch(sql).expect("跑 0001");
        conn.execute("INSERT INTO schema_version (version) VALUES (1)", [])
            .expect("记版本号");
        conn.execute(
            "INSERT INTO hosts (group_id, name, host, port, username, auth_method, tags, sort, \
             created_at, updated_at) VALUES (NULL, '旧主机', 'old.example.com', 22, 'root', \
             'password', '[]', 0, 1, 1)",
            [],
        )
        .expect("插旧数据");
    }

    // 打开时应该自动升级到最新
    let db = temp.open();
    let latest = super::db::MIGRATIONS
        .last()
        .map(|(version, _)| *version)
        .expect("至少要有一个迁移");
    assert_eq!(db.schema_version().expect("读版本号"), latest);

    let hosts = db.host_list().expect("列主机");
    assert_eq!(hosts.len(), 1, "旧数据要留着");
    assert_eq!(hosts[0].name, "旧主机");
    assert!(!hosts[0].legacy_algorithms, "新列默认是 0（关闭）");
}

// ---------- 跳板机链展开 ----------

/// 造一台主机，`jump` 是它的跳板机 id。
fn host_with_jump(db: &Database, name: &str, jump: Option<i64>) -> i64 {
    let mut draft = sample_host(None, None);
    draft.name = name.to_owned();
    draft.jump_host_id = jump;
    db.host_save(&draft).expect("建主机").id
}

#[test]
fn chain_of_a_single_host_is_just_the_host() {
    let db = memory_db();
    let host = host_with_jump(&db, "目标", None);

    let chain = db.host_chain(host).expect("展开");
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].id, host);
}

#[test]
fn chain_puts_the_outermost_jump_first() {
    let db = memory_db();
    // 目标 → jump1 → jump2（jump2 是最外层，我们最先连它）
    let jump2 = host_with_jump(&db, "jump2", None);
    let jump1 = host_with_jump(&db, "jump1", Some(jump2));
    let target = host_with_jump(&db, "目标", Some(jump1));

    let chain = db.host_chain(target).expect("展开");
    let names: Vec<String> = chain.iter().map(|host| host.name.clone()).collect();
    assert_eq!(names, vec!["jump2", "jump1", "目标"]);
}

#[test]
fn chain_detects_a_loop() {
    let db = memory_db();
    let a = host_with_jump(&db, "a", None);
    let b = host_with_jump(&db, "b", Some(a));
    // 让 a 的跳板机指向 b，形成 a → b → a
    let mut draft = sample_host(None, None);
    draft.id = Some(a);
    draft.name = "a".to_owned();
    draft.jump_host_id = Some(b);
    db.host_save(&draft).expect("改成环");

    let err = db.host_chain(a).expect_err("成环要报错");
    assert!(
        err.to_string().contains("成环"),
        "错误信息要说清是环路：{err}"
    );
}

#[test]
fn chain_stops_at_five_hops() {
    let db = memory_db();
    // 造 6 级：h1 ← h2 ← ... ← h6（h6 的跳板机是 h5，依此类推），
    // 目标挂在最深的 h6 上，一共超过 5 跳
    let mut previous = host_with_jump(&db, "h1", None);
    for index in 2..=6 {
        previous = host_with_jump(&db, &format!("h{index}"), Some(previous));
    }
    let target = host_with_jump(&db, "目标", Some(previous));

    let err = db.host_chain(target).expect_err("超过 5 级要报错");
    assert!(err.to_string().contains("最多"), "错误信息要提限制：{err}");

    // 5 跳是允许的（目标 + 4 个跳板机）
    let four_jumps = host_with_jump(&db, "j末端", None);
    let mut previous = four_jumps;
    for index in 1..=3 {
        previous = host_with_jump(&db, &format!("j{index}"), Some(previous));
    }
    let ok_target = host_with_jump(&db, "短目标", Some(previous));
    let chain = db.host_chain(ok_target).expect("5 跳应该允许");
    assert_eq!(chain.len(), 5);
}

// ---------- 迁移 2 → 3 与设置 ----------

#[test]
fn upgrading_from_version_two_keeps_data_and_defaults_proxy_mode() {
    let temp = TempDb::new("proxy-mode");

    // 手工铺一个只跑到 0002 的库：有 proxy_id 的老主机
    {
        let conn = rusqlite::Connection::open(&temp.path).expect("建库");
        for (version, sql) in super::db::MIGRATIONS.iter().take(2) {
            conn.execute_batch(sql).expect("跑早期迁移");
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                [version],
            )
            .expect("记版本号");
        }
        conn.execute(
            "INSERT INTO proxies (name, kind, host, port, username, secret_blob) \
             VALUES ('老代理', 'socks5', '127.0.0.1', 1080, NULL, NULL)",
            [],
        )
        .expect("插代理");
        conn.execute(
            "INSERT INTO hosts (group_id, name, host, port, username, auth_method, proxy_id, \
             tags, sort, created_at, updated_at, legacy_algorithms) \
             VALUES (NULL, '老主机', 'old.example.com', 22, 'root', 'password', 1, '[]', 0, 1, 1, 0)",
            [],
        )
        .expect("插旧数据");
    }

    let db = temp.open();
    let latest = super::db::MIGRATIONS
        .last()
        .map(|(version, _)| *version)
        .expect("至少要有一个迁移");
    assert_eq!(
        db.schema_version().expect("读版本号"),
        latest,
        "应该升到最新"
    );
    assert_eq!(latest, 11);

    let hosts = db.host_list().expect("列主机");
    assert_eq!(hosts.len(), 1, "旧数据要留着");
    assert_eq!(hosts[0].name, "老主机");
    assert_eq!(
        hosts[0].proxy_mode,
        ProxyMode::Default,
        "旧数据迁成 default"
    );
    assert_eq!(hosts[0].proxy_id, Some(1), "proxy_id 不能被动过");
}

/// 老版本库文件：`tests/fixtures/legacy-v6.db` 是用 1～6 号迁移铺出来的真实 SQLite
/// 文件（含分组、凭据、代理、跳板机、主机的引用关系）。测试只复制它，不重新构造。
fn copy_legacy_fixture(path: &std::path::Path) {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("legacy-v6.db");
    std::fs::copy(&fixture, path).expect("复制老版本库文件");
}

fn uids_of(db: &Database, table: &str) -> Vec<String> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!("SELECT uid FROM {table} ORDER BY id"))?;
        let rows = statement.query_map([], |row| row.get::<_, Option<String>>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?.unwrap_or_default());
        }
        Ok(out)
    })
    .expect("读 uid")
}

#[test]
fn upgrading_a_version_six_database_backfills_unique_uids_and_keeps_references() {
    let temp = TempDb::new("uid-migration");
    copy_legacy_fixture(&temp.path);

    let db = temp.open();
    assert_eq!(
        db.schema_version().expect("读版本号"),
        11,
        "老库应该升到 11"
    );

    // 四张表、每条老记录都有合法的 UUID v4，而且互不相同。
    for table in ["groups", "hosts", "identities", "proxies"] {
        let uids = uids_of(&db, table);
        assert!(!uids.is_empty(), "{table} 应该有老数据");
        let mut seen = std::collections::HashSet::new();
        for uid in &uids {
            let parsed = uuid::Uuid::parse_str(uid)
                .unwrap_or_else(|_| panic!("{table} 的 uid 不是 UUID：{uid}"));
            assert_eq!(parsed.get_version_num(), 4, "{table} 要 UUID v4：{uid}");
            assert!(seen.insert(uid.clone()), "{table} 的 uid 重复：{uid}");
        }
    }

    // 老数据本身和引用关系原样保留。
    let groups = db.group_list().expect("列分组");
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[1].parent_id, Some(groups[0].id));
    let host = db.host_get(2).expect("读老主机");
    assert_eq!(host.name, "web-1");
    assert_eq!(host.group_id, Some(2));
    assert_eq!(host.identity_id, Some(2));
    assert_eq!(host.proxy_id, Some(1));
    assert_eq!(host.jump_host_id, Some(1));
    assert!(host.legacy_algorithms);
    assert_eq!(host.updated_at, 1_700_000_200);
    assert_eq!(db.identity_list().expect("列凭据").len(), 2);
    assert_eq!(db.proxy_list().expect("列代理").len(), 1);

    // 新记录也拿到自己的 uid，不和老记录撞。
    let old: std::collections::HashSet<String> = uids_of(&db, "hosts").into_iter().collect();
    let fresh = db.host_save(&sample_host(None, None)).expect("新建主机");
    let fresh_uid = db
        .with_conn(|conn| {
            let uid: String =
                conn.query_row("SELECT uid FROM hosts WHERE id = ?1", [fresh.id], |row| {
                    row.get(0)
                })?;
            Ok(uid)
        })
        .expect("读新 uid");
    assert!(uuid::Uuid::parse_str(&fresh_uid).is_ok());
    assert!(!old.contains(&fresh_uid), "新记录的 uid 不能和老记录重复");
}

#[test]
fn proxy_mode_roundtrips() {
    let db = memory_db();
    for mode in [ProxyMode::Default, ProxyMode::None, ProxyMode::Custom] {
        let mut draft = sample_host(None, None);
        draft.name = format!("主机-{}", mode.as_str());
        draft.proxy_mode = mode;
        let saved = db.host_save(&draft).expect("保存");
        assert_eq!(saved.proxy_mode, mode);

        let read_back = db.host_get(saved.id).expect("读回来");
        assert_eq!(read_back.proxy_mode, mode, "读回来要一样");
    }
}

#[test]
fn default_proxy_setting_roundtrips() {
    let db = memory_db();
    assert!(db.default_proxy_id().expect("读默认代理").is_none());

    let proxy = db
        .proxy_save(&ProxyDraft {
            id: None,
            name: "本地代理".to_owned(),
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 10808,
            username: None,
            secret: None,
        })
        .unwrap();

    db.set_default_proxy_id(Some(proxy.id)).expect("设置");
    assert_eq!(db.default_proxy_id().expect("读回来"), Some(proxy.id));

    db.set_default_proxy_id(None).expect("清掉");
    assert!(db.default_proxy_id().expect("读回来").is_none());
}

#[test]
fn ai_exec_mode_defaults_to_manual_and_roundtrips() {
    let db = memory_db();
    assert_eq!(db.ai_exec_mode().expect("读默认模式"), "manual");

    db.set_ai_exec_mode("auto").expect("设置 auto");
    assert_eq!(db.ai_exec_mode().expect("读回来"), "auto");

    db.setting_set(crate::store::settings::AI_EXEC_MODE_KEY, "42")
        .expect("写个坏值");
    assert_eq!(db.ai_exec_mode().expect("坏值回退"), "manual");
}

#[test]
fn write_backup_keep_defaults_and_clamps() {
    let db = memory_db();
    assert_eq!(
        db.ai_write_backup_keep().expect("读默认"),
        crate::store::settings::AI_WRITE_BACKUP_KEEP_DEFAULT
    );
    db.setting_set(crate::store::settings::AI_WRITE_BACKUP_KEEP_KEY, "2")
        .expect("写 2");
    assert_eq!(db.ai_write_backup_keep().expect("读 2"), 2);
    db.setting_set(crate::store::settings::AI_WRITE_BACKUP_KEEP_KEY, "0")
        .expect("写 0");
    assert_eq!(db.ai_write_backup_keep().expect("0 夹到最小值"), 1);
    db.setting_set(crate::store::settings::AI_WRITE_BACKUP_KEEP_KEY, "100000")
        .expect("写个很大的值");
    assert_eq!(
        db.ai_write_backup_keep().expect("夹到最大值"),
        crate::store::settings::AI_WRITE_BACKUP_KEEP_MAX
    );
    db.setting_set(crate::store::settings::AI_WRITE_BACKUP_KEEP_KEY, "\"x\"")
        .expect("写坏值");
    assert_eq!(
        db.ai_write_backup_keep().expect("坏值用默认"),
        crate::store::settings::AI_WRITE_BACKUP_KEEP_DEFAULT
    );
}

#[cfg(windows)]
#[test]
fn proxy_login_returns_the_password() {
    let db = memory_db();
    let proxy = db
        .proxy_save(&ProxyDraft {
            id: None,
            name: "带密码的代理".to_owned(),
            kind: ProxyKind::Http,
            host: "proxy.example.internal".to_owned(),
            port: 3128,
            username: Some("user".to_owned()),
            secret: Some("proxy-password".to_owned()),
        })
        .unwrap();

    let login = db.proxy_login(proxy.id).expect("取代理信息");
    assert_eq!(login.host, "proxy.example.internal");
    assert_eq!(login.port, 3128);
    assert_eq!(login.username.as_deref(), Some("user"));
    assert_eq!(login.password.as_deref(), Some("proxy-password"));
    // Debug 不能带出密码
    assert!(!format!("{login:?}").contains("proxy-password"));
}

// ---------- AI 审计 ----------

fn audit_entry<'a>(danger: &'a str, decision: &'a str) -> audit::NewAiAudit<'a> {
    audit::NewAiAudit {
        at: 0,
        scope: "scope-x",
        host_label: "host-x",
        command: "echo hi",
        danger,
        decision,
        exit_code: None,
        duration_ms: None,
        location: None,
        fallback_reason: None,
        tool: "run_command",
        path: None,
        bytes: None,
        backup_path: None,
    }
}

#[test]
fn upgrading_from_version_three_creates_ai_audit() {
    let temp = TempDb::new("ai-audit");

    {
        let conn = rusqlite::Connection::open(&temp.path).expect("建库");
        for (version, sql) in super::db::MIGRATIONS.iter().take(3) {
            conn.execute_batch(sql).expect("跑早期迁移");
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                [version],
            )
            .expect("记版本号");
        }
    }

    let db = temp.open();
    assert_eq!(db.schema_version().expect("读版本号"), 11);
    db.ai_audit_insert(audit::NewAiAudit {
        at: 1,
        scope: "scope-a",
        host_label: "web-1",
        command: "uname -a",
        danger: "safe",
        decision: "auto",
        exit_code: Some(0),
        duration_ms: Some(5),
        location: Some("background"),
        fallback_reason: None,
        tool: "run_command",
        path: None,
        bytes: None,
        backup_path: None,
    })
    .expect("升级后能写审计");
}

#[test]
fn ai_audit_roundtrips_and_lists_newest_first() {
    let db = memory_db();
    let first = db
        .ai_audit_insert(audit::NewAiAudit {
            at: 1_000,
            scope: "scope-a",
            host_label: "web-1",
            command: "uname -a",
            danger: "safe",
            decision: "auto",
            exit_code: Some(0),
            duration_ms: Some(12),
            location: Some("terminal"),
            fallback_reason: None,
            tool: "run_command",
            path: None,
            bytes: None,
            backup_path: None,
        })
        .expect("写第一条");
    let second = db
        .ai_audit_insert(audit::NewAiAudit {
            at: 2_000,
            scope: "scope-a",
            host_label: "web-1",
            command: "rm -rf /",
            danger: "dangerous",
            decision: "denied",
            exit_code: None,
            duration_ms: None,
            location: None,
            fallback_reason: None,
            tool: "run_command",
            path: None,
            bytes: None,
            backup_path: None,
        })
        .expect("写第二条");

    let rows = db.ai_audit_list(None, 10).expect("读审计");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, second, "最新在前");
    assert_eq!(rows[1].id, first);

    assert_eq!(rows[0].at, 2_000);
    assert_eq!(rows[0].scope, "scope-a");
    assert_eq!(rows[0].host_label, "web-1");
    assert_eq!(rows[0].command, "rm -rf /");
    assert_eq!(rows[0].danger, "dangerous");
    assert_eq!(rows[0].decision, "denied");
    assert_eq!(rows[0].exit_code, None);
    assert_eq!(rows[0].duration_ms, None);
    assert_eq!(rows[1].exit_code, Some(0));
    assert_eq!(rows[1].duration_ms, Some(12));
}

#[test]
fn ai_audit_list_filters_by_scope_and_limit() {
    let db = memory_db();
    for i in 0..5 {
        db.ai_audit_insert(audit::NewAiAudit {
            at: i,
            scope: if i % 2 == 0 { "scope-a" } else { "scope-b" },
            host_label: "host",
            command: "ls",
            danger: "safe",
            decision: "auto",
            exit_code: Some(0),
            duration_ms: Some(1),
            location: Some("background"),
            fallback_reason: None,
            tool: "run_command",
            path: None,
            bytes: None,
            backup_path: None,
        })
        .expect("写审计");
    }

    let only_a = db.ai_audit_list(Some("scope-a"), 10).expect("按 scope 查");
    assert_eq!(only_a.len(), 3);
    assert!(only_a.iter().all(|row| row.scope == "scope-a"));

    let limited = db.ai_audit_list(None, 2).expect("限条数");
    assert_eq!(limited.len(), 2);

    let none = db.ai_audit_list(Some("scope-z"), 10).expect("空 scope");
    assert!(none.is_empty());
}

#[test]
fn ai_audit_rejects_unknown_values() {
    let db = memory_db();
    assert!(db.ai_audit_insert(audit_entry("scary", "auto")).is_err());
    assert!(db.ai_audit_insert(audit_entry("safe", "maybe")).is_err());
}

#[test]
fn ai_audit_table_has_no_output_columns() {
    let db = memory_db();
    let columns = db
        .with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT name FROM pragma_table_info('ai_audit') ORDER BY cid")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut names = Vec::new();
            for row in rows {
                names.push(row?);
            }
            Ok(names)
        })
        .expect("查列名");
    assert_eq!(
        columns,
        vec![
            "id",
            "at",
            "scope",
            "host_label",
            "command",
            "danger",
            "decision",
            "exit_code",
            "duration_ms",
            "location",
            "fallback_reason",
            "tool",
            "path",
            "bytes",
            "backup_path"
        ],
        "审计表只能有这些列，绝不能加输出相关的列"
    );
}

#[test]
fn ai_audit_stores_file_tool_metadata_and_execution_location_but_no_content() {
    let db = memory_db();
    db.ai_audit_insert(audit::NewAiAudit {
        at: 5,
        scope: "scope-f",
        host_label: "web-1",
        command: "write_file /etc/hosts",
        danger: "dangerous",
        decision: "allowed",
        exit_code: Some(0),
        duration_ms: Some(30),
        location: Some("terminal"),
        fallback_reason: Some("原因"),
        tool: "write_file",
        path: Some("/etc/hosts"),
        bytes: Some(1234),
        backup_path: Some("/etc/hosts.zssh-bak-20260101-000000"),
    })
    .expect("写文件工具审计");
    let rows = db.ai_audit_list(Some("scope-f"), 5).expect("读审计");
    let row = &rows[0];
    assert_eq!(row.tool, "write_file");
    assert_eq!(row.path.as_deref(), Some("/etc/hosts"));
    assert_eq!(row.bytes, Some(1234));
    assert_eq!(
        row.backup_path.as_deref(),
        Some("/etc/hosts.zssh-bak-20260101-000000")
    );
    assert_eq!(row.danger, "dangerous");
    // 之前 insert 漏写这两列（读出来永远是 None）：现在要真的落库。
    assert_eq!(row.location.as_deref(), Some("terminal"));
    assert_eq!(row.fallback_reason.as_deref(), Some("原因"));

    // 表里没有任何装文件内容的列。
    let columns: Vec<String> = db
        .with_conn(|conn| {
            let mut statement = conn.prepare("PRAGMA table_info(ai_audit)")?;
            let names = statement
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(names)
        })
        .expect("读表结构");
    assert!(
        columns
            .iter()
            .all(|c| !c.contains("content") && !c.contains("output")),
        "审计表不能有存内容的列：{columns:?}"
    );
}

#[test]
fn change_listener_fires_for_synced_data_only() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let db = memory_db();
    let fired = Arc::new(AtomicUsize::new(0));
    let counter = fired.clone();
    db.set_change_listener(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let count = || fired.load(Ordering::SeqCst);

    let group = db
        .group_save(&GroupDraft {
            id: None,
            name: "g".to_owned(),
            parent_id: None,
            sort: 0,
        })
        .unwrap();
    assert_eq!(count(), 1, "新建分组");
    let host = db
        .host_save(&HostDraft {
            id: None,
            group_id: Some(group.id),
            name: "h".to_owned(),
            host: "h.example.internal".to_owned(),
            port: 22,
            username: "root".to_owned(),
            auth_method: AuthMethod::Password,
            identity_id: None,
            proxy_id: None,
            jump_host_id: None,
            proxy_mode: ProxyMode::Default,
            tags: Vec::new(),
            sort: 0,
            legacy_algorithms: false,
        })
        .unwrap();
    assert_eq!(count(), 2, "新建主机");
    db.setting_set("ui_theme", "\"dark\"").unwrap();
    assert_eq!(count(), 3, "会同步的设置");

    // 不会同步的东西不算“变了”：面板宽度、同步自己的状态、读操作。
    db.setting_set("layout.ai_panel_width", "500").unwrap();
    db.setting_set("sync_auto", "{}").unwrap();
    let _ = db.host_list().unwrap();
    assert_eq!(count(), 3);

    // 失败的写入不通知。
    assert!(db.host_delete(host.id + 999).is_err());
    assert_eq!(count(), 3);
    db.host_delete(host.id).unwrap();
    db.group_delete(group.id).unwrap();
    assert_eq!(count(), 5);
}

fn identity_of_kind(db: &Database, name: &str, kind: IdentityKind) -> crate::model::Identity {
    db.identity_save(&IdentityDraft {
        id: None,
        name: name.to_owned(),
        kind,
        key_path: (kind == IdentityKind::KeyFile).then(|| r"C:\keys\id_ed25519".to_owned()),
        secret: None,
        passphrase: None,
    })
    .unwrap()
}

#[test]
fn saving_a_host_refuses_an_identity_of_the_wrong_kind() {
    // R-10 H2：密码认证配私钥凭据，私钥（或它的口令）会被当成密码发给服务器。
    let db = memory_db();
    let inline = identity_of_kind(&db, "内联私钥", IdentityKind::KeyInline);
    let file = identity_of_kind(&db, "私钥文件", IdentityKind::KeyFile);
    let password = identity_of_kind(&db, "密码", IdentityKind::Password);

    for key_identity in [&inline, &file] {
        let err = db
            .host_save(&sample_host(None, Some(key_identity.id)))
            .expect_err("密码认证不能配私钥凭据");
        assert!(err.to_string().contains("不是密码凭据"), "{err}");
    }
    let mut key_host = sample_host(None, Some(password.id));
    key_host.auth_method = AuthMethod::Key;
    assert!(db.host_save(&key_host).is_err(), "私钥认证不能配密码凭据");

    // 配套的组合照常能存。
    db.host_save(&sample_host(None, Some(password.id)))
        .expect("密码认证 + 密码凭据");
    key_host.identity_id = Some(inline.id);
    db.host_save(&key_host).expect("私钥认证 + 私钥凭据");
}

#[test]
fn host_login_refuses_to_send_a_key_identity_as_a_password() {
    // 旧数据、同步来的数据可能绕过保存时的检查：连接时也要拦，不能把私钥类凭据的秘密发出去。
    let db = memory_db();
    let inline = identity_of_kind(&db, "内联私钥", IdentityKind::KeyInline);
    let host = db.host_save(&sample_host(None, None)).unwrap();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE hosts SET identity_id = ?1 WHERE id = ?2",
            rusqlite::params![inline.id, host.id],
        )?;
        Ok(())
    })
    .unwrap();

    let err = db
        .host_login(host.id)
        .map(|_| ())
        .expect_err("密码认证配私钥凭据不能登录");
    assert!(err.to_string().contains("不是密码凭据"), "{err}");
}

#[test]
fn changing_an_identitys_kind_without_new_material_is_refused() {
    // R7-01：已有私钥的凭据改成密码类型、不填新密码，旧私钥不能被当成密码留下来。
    let db = memory_db();
    let saved = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "内联私钥".to_owned(),
            kind: IdentityKind::KeyInline,
            key_path: None,
            secret: Some("fake-inline-key-material-for-test".to_owned()),
            passphrase: None,
        })
        .unwrap();
    let to_password = |secret: Option<String>| IdentityDraft {
        id: Some(saved.id),
        name: "内联私钥".to_owned(),
        kind: IdentityKind::Password,
        key_path: None,
        secret,
        passphrase: None,
    };

    let err = db
        .identity_save(&to_password(None))
        .expect_err("改类型又不给新秘密必须拒绝");
    assert!(err.to_string().contains("重新填写"), "{err}");
    let after = db.identity_list().unwrap();
    assert_eq!(after[0].kind, IdentityKind::KeyInline, "拒绝后类型不能变");
    assert!(after[0].has_secret, "拒绝后旧秘密还在原类型下");

    // 给了新密码：类型和秘密一起换，密文里不再是旧私钥。
    db.identity_save(&to_password(Some("new-password".to_owned())))
        .expect("同时给新密码就可以");
    let secret = db
        .with_conn(|conn| identities::identity_secret(conn, saved.id))
        .unwrap();
    assert_eq!(secret.as_deref(), Some("new-password"));
}

#[test]
fn changing_kind_is_allowed_when_there_is_nothing_stored_or_the_secret_is_cleared() {
    let db = memory_db();
    let file = identity_of_kind(&db, "私钥文件", IdentityKind::KeyFile);
    let draft = |secret: Option<String>| IdentityDraft {
        id: Some(file.id),
        name: "私钥文件".to_owned(),
        kind: IdentityKind::Password,
        key_path: None,
        secret,
        passphrase: None,
    };
    // 原来没有保存任何秘密：没有东西会被带到新类型里。
    db.identity_save(&draft(None))
        .expect("没有旧秘密可以直接改类型");

    // 有秘密时显式清掉（空串）也可以改类型。
    let inline = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "内联".to_owned(),
            kind: IdentityKind::KeyInline,
            key_path: None,
            secret: Some("key".to_owned()),
            passphrase: None,
        })
        .unwrap();
    let cleared = db
        .identity_save(&IdentityDraft {
            id: Some(inline.id),
            name: "内联".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some(String::new()),
            passphrase: None,
        })
        .expect("显式清掉秘密后可以改类型");
    assert!(!cleared.has_secret);
}

#[test]
fn editing_an_inline_key_merges_key_and_passphrase_separately() {
    // C14：导入的私钥只改口令要真的改进密文；重新粘贴私钥不填口令要保留原口令。
    let db = memory_db();
    let saved = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "内联私钥".to_owned(),
            kind: IdentityKind::KeyInline,
            key_path: None,
            secret: Some("fake-inline-key-material-for-test".to_owned()),
            passphrase: Some("first-pass".to_owned()),
        })
        .unwrap();
    let edit = |secret: Option<&str>, passphrase: Option<&str>| IdentityDraft {
        id: Some(saved.id),
        name: "内联私钥".to_owned(),
        kind: IdentityKind::KeyInline,
        key_path: None,
        secret: secret.map(str::to_owned),
        passphrase: passphrase.map(str::to_owned),
    };
    let stored = || {
        let secret = db
            .with_conn(|conn| identities::identity_secret(conn, saved.id))
            .unwrap()
            .expect("应该有密文");
        identities::InlineKey::parse(&secret)
    };

    db.identity_save(&edit(None, Some("second-pass"))).unwrap();
    let inline = stored();
    assert_eq!(inline.key, "fake-inline-key-material-for-test");
    assert_eq!(inline.passphrase.as_deref(), Some("second-pass"));

    db.identity_save(&edit(Some("another-fake-key-material"), None))
        .unwrap();
    let inline = stored();
    assert_eq!(inline.key, "another-fake-key-material");
    assert_eq!(
        inline.passphrase.as_deref(),
        Some("second-pass"),
        "口令留空 = 不改"
    );

    db.identity_save(&edit(None, Some(""))).unwrap();
    assert_eq!(stored().passphrase, None, "空串 = 清掉口令");

    // 只改名字：两样都不动
    db.identity_save(&edit(None, None)).unwrap();
    assert_eq!(stored().key, "another-fake-key-material");
}

#[test]
fn switching_to_inline_key_does_not_reuse_the_old_password_as_passphrase() {
    // 原来是密码类型：改成导入私钥时不能把旧密码当成口令合并进来。
    let db = memory_db();
    let saved = db
        .identity_save(&IdentityDraft {
            id: None,
            name: "原来是密码".to_owned(),
            kind: IdentityKind::Password,
            key_path: None,
            secret: Some("old-password".to_owned()),
            passphrase: None,
        })
        .unwrap();
    db.identity_save(&IdentityDraft {
        id: Some(saved.id),
        name: "原来是密码".to_owned(),
        kind: IdentityKind::KeyInline,
        key_path: None,
        secret: Some("fake-inline-key-material-for-test".to_owned()),
        passphrase: None,
    })
    .unwrap();
    let secret = db
        .with_conn(|conn| identities::identity_secret(conn, saved.id))
        .unwrap()
        .expect("应该有密文");
    let inline = identities::InlineKey::parse(&secret);
    assert_eq!(inline.passphrase, None);
    assert!(!secret.contains("old-password"));
}

#[test]
fn deleting_the_default_proxy_clears_the_settings_that_point_at_it() {
    let db = memory_db();
    let save = |name: &str| {
        db.proxy_save(&ProxyDraft {
            id: None,
            name: name.to_owned(),
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 10808,
            username: None,
            secret: None,
        })
        .unwrap()
    };
    let gone = save("要删的");
    let kept = save("留着的");
    let app_key = crate::sync::app_proxy::APP_NETWORK_PROXY_KEY;

    db.set_default_proxy_id(Some(gone.id)).unwrap();
    db.setting_set(app_key, &format!("\"proxy:{}\"", gone.id))
        .unwrap();
    db.proxy_delete(gone.id).unwrap();
    assert_eq!(db.default_proxy_id().unwrap(), None, "SSH 默认代理改成不用");
    assert_eq!(db.setting_get(app_key).unwrap(), None, "软件网络回到默认");

    // 指向别的代理的设置不受影响
    db.set_default_proxy_id(Some(kept.id)).unwrap();
    db.setting_set(app_key, &format!("\"proxy:{}\"", kept.id))
        .unwrap();
    let other = save("另一个");
    db.proxy_delete(other.id).unwrap();
    assert_eq!(db.default_proxy_id().unwrap(), Some(kept.id));
    assert_eq!(
        db.setting_get(app_key).unwrap().as_deref(),
        Some(format!("\"proxy:{}\"", kept.id).as_str())
    );
}
