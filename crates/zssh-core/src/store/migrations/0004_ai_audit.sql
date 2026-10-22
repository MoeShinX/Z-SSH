-- AI-1：工具桥 run_command 的审计。写好的迁移不再改，新增列走新迁移。
--
-- 只记元数据，**绝不记命令输出**（输出可能含敏感信息，卡片第 5 节）。
-- decision：auto = 自动放行；allowed = 人工允许；denied = 人工拒绝；timeout = 审批超时按拒绝。
-- exit_code 和 duration_ms 在没执行（被拒 / 审批超时）时为 NULL。
CREATE TABLE ai_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          INTEGER NOT NULL,
    scope       TEXT    NOT NULL,
    host_label  TEXT    NOT NULL,
    command     TEXT    NOT NULL,
    danger      TEXT    NOT NULL CHECK (danger IN ('safe', 'dangerous')),
    decision    TEXT    NOT NULL CHECK (decision IN ('auto', 'allowed', 'denied', 'timeout')),
    exit_code   INTEGER,
    duration_ms INTEGER
);

CREATE INDEX idx_ai_audit_scope ON ai_audit(scope, at);
