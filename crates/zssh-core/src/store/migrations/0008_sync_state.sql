-- S-1：同步删除的墓碑和合并冲突的“另一份”。
--
-- 写好的迁移不再改，新增列走新迁移。
-- tombstone：删除事件保留 90 天，防止别的电脑把删掉的记录同步回来。
-- conflicts：两边都改了同一条记录时，保留没被选中的那一份，界面以后可以改回去。
CREATE TABLE sync_tombstones (
    entity     TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    deleted_at INTEGER NOT NULL,
    device_id  TEXT    NOT NULL,
    PRIMARY KEY (entity, key)
);

CREATE TABLE sync_conflicts (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    at    INTEGER NOT NULL,
    entity TEXT    NOT NULL,
    key   TEXT    NOT NULL,
    kept  TEXT    NOT NULL,
    other TEXT    NOT NULL
);

CREATE INDEX idx_sync_conflicts_at ON sync_conflicts(at);
