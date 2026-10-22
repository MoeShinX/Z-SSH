-- M1-05：主机的代理策略。写好的迁移不再改，新增列走新迁移。
--
-- default = 跟随全局"默认代理"；none = 不用代理；custom = 用 hosts.proxy_id 指定的那个。
-- 旧数据一律迁成 default（卡片规定）。
ALTER TABLE hosts ADD COLUMN proxy_mode TEXT NOT NULL DEFAULT 'default';
