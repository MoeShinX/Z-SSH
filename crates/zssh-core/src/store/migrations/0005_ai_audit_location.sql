-- AI-2c：审计增加"执行位置"（终端 / 后台）和降级到后台的原因。
--
-- 写好的迁移不再改，新增列走新迁移。
-- location：terminal = 在用户终端里执行；background = 后台 exec 通道。
-- 旧行为 NULL（AI-2c 之前都是在后台通道执行的）。
ALTER TABLE ai_audit ADD COLUMN location TEXT
    CHECK (location IS NULL OR location IN ('terminal', 'background'));
ALTER TABLE ai_audit ADD COLUMN fallback_reason TEXT;
