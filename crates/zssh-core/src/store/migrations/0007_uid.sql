-- S-1：同步用的全局标识 uid（UUID v4）和修改时间。
--
-- 本机数据库仍用自增 id；同步包里的引用一律用 uid，换一台电脑才对得上。
-- uid 由数据访问层在插入时生成；老数据由 Database::open 在迁移后逐条补上。
-- 索引放在这条加列的迁移里（基线里不能给后续迁移才加的列建索引）。
ALTER TABLE groups ADD COLUMN uid TEXT;
ALTER TABLE groups ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE hosts ADD COLUMN uid TEXT;
ALTER TABLE identities ADD COLUMN uid TEXT;
ALTER TABLE identities ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE proxies ADD COLUMN uid TEXT;
ALTER TABLE proxies ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;

CREATE UNIQUE INDEX idx_groups_uid ON groups(uid);
CREATE UNIQUE INDEX idx_hosts_uid ON hosts(uid);
CREATE UNIQUE INDEX idx_identities_uid ON identities(uid);
CREATE UNIQUE INDEX idx_proxies_uid ON proxies(uid);
