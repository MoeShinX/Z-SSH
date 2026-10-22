-- 主机长期记忆：AI 用 remember_host 工具记下的笔记，每台主机一份。
--
-- 写好的迁移不再改，新增列走新迁移。只存本机，不进配置同步和离线备份。
CREATE TABLE host_memory (
    host_id    INTEGER PRIMARY KEY REFERENCES hosts(id) ON DELETE CASCADE,
    content    TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
);
