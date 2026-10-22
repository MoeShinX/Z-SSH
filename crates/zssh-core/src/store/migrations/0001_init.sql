-- M1 的初始表结构（设计文档 6.1）。
--
-- 写好的迁移不再改：以后加列、加索引都写新的迁移文件，对应索引也放新文件里。

CREATE TABLE groups (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    name      TEXT    NOT NULL,
    -- 父分组被删掉时，子分组由数据访问层挂回根目录（这里兜底置空）
    parent_id INTEGER REFERENCES groups(id) ON DELETE SET NULL,
    sort      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE identities (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN ('password', 'key_file', 'key_inline', 'agent')),
    -- kind = key_file 时用
    key_path    TEXT,
    -- 密码、私钥口令、私钥内容：DPAPI 加密后的密文，绝不存明文
    secret_blob BLOB
);

CREATE TABLE proxies (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN ('socks5', 'http')),
    host        TEXT    NOT NULL,
    port        INTEGER NOT NULL,
    username    TEXT,
    secret_blob BLOB
);

CREATE TABLE hosts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id     INTEGER REFERENCES groups(id) ON DELETE SET NULL,
    name         TEXT    NOT NULL,
    host         TEXT    NOT NULL,
    port         INTEGER NOT NULL DEFAULT 22,
    username     TEXT    NOT NULL,
    auth_method  TEXT    NOT NULL CHECK (auth_method IN ('password', 'key', 'agent')),
    identity_id  INTEGER REFERENCES identities(id) ON DELETE RESTRICT,
    proxy_id     INTEGER REFERENCES proxies(id) ON DELETE RESTRICT,
    -- 跳板机指向另一台主机；被指向的主机删掉时置空
    jump_host_id INTEGER REFERENCES hosts(id) ON DELETE SET NULL,
    -- JSON 数组文本
    tags         TEXT,
    sort         INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 每个已执行的迁移一行，启动时用它算当前版本
CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY
);

CREATE INDEX idx_hosts_group ON hosts(group_id);
CREATE INDEX idx_groups_parent ON groups(parent_id);
