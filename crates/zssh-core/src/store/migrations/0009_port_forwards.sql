-- 端口转发规则：挂在主机下面，只存本机，不进配置同步。
--
-- 写好的迁移不再改，新增列走新迁移。
-- kind：local = 本机监听、经服务器连到目标（ssh -L）；remote = 服务器监听、连回本机这边的目标（ssh -R）。
-- auto_start：打开这台主机的终端时自动开启。
CREATE TABLE port_forwards (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id    INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    name       TEXT    NOT NULL DEFAULT '',
    kind       TEXT    NOT NULL CHECK (kind IN ('local', 'remote')),
    bind_host  TEXT    NOT NULL,
    bind_port  INTEGER NOT NULL,
    dest_host  TEXT    NOT NULL,
    dest_port  INTEGER NOT NULL,
    auto_start INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_port_forwards_host ON port_forwards(host_id);
