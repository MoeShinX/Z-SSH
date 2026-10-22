-- M1-04：老服务器兼容开关。写好的迁移不再改，新增列走新迁移。
ALTER TABLE hosts ADD COLUMN legacy_algorithms INTEGER NOT NULL DEFAULT 0;
