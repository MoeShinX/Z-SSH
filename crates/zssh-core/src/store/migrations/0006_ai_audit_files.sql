-- M3-4：文件工具（list_dir / read_file / write_file / stat）的审计。
--
-- 写好的迁移不再改，新增列走新迁移。
-- tool：工具名；旧行（只有 run_command）补成 run_command。
-- path：文件工具操作的路径；bytes：读 = 返回的字节数，写 = 写入的字节数；
-- backup_path：write_file 覆盖时在服务器上留的备份。
-- **只记元数据，不记文件内容**（和 run_command 不记命令输出一致）。
ALTER TABLE ai_audit ADD COLUMN tool TEXT NOT NULL DEFAULT 'run_command';
ALTER TABLE ai_audit ADD COLUMN path TEXT;
ALTER TABLE ai_audit ADD COLUMN bytes INTEGER;
ALTER TABLE ai_audit ADD COLUMN backup_path TEXT;
