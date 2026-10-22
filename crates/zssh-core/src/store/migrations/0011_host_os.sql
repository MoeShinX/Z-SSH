-- 主机的系统类型（连上之后探测，见 ssh::os_probe），主机列表里显示对应的系统图标。
--
-- 写好的迁移不再改，新增列走新迁移。只存本机，不进配置同步和备份：下次连上会重新探测。
CREATE TABLE host_os (
    host_id    INTEGER PRIMARY KEY REFERENCES hosts(id) ON DELETE CASCADE,
    os         TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);
