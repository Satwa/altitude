-- no-transaction
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

BEGIN IMMEDIATE;

CREATE TABLE tasks_with_codex (
    id TEXT PRIMARY KEY NOT NULL,
    agent_kind TEXT CHECK (agent_kind IS NULL OR agent_kind IN ('claude', 'codex', 'pi')),
    host TEXT NOT NULL,
    removed_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL
);

INSERT INTO tasks_with_codex SELECT id, agent_kind, host, removed_at_ms, created_at_ms FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_with_codex RENAME TO tasks;

COMMIT;

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
