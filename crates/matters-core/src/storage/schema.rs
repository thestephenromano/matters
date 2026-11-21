pub const SCHEMA_V1: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS areas (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY NOT NULL,
    area_id TEXT REFERENCES areas(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    start_date TEXT,
    time_of_day TEXT,
    is_someday INTEGER NOT NULL DEFAULT 0,
    deadline TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    completed_at INTEGER,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS headings (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY NOT NULL,
    area_id TEXT REFERENCES areas(id) ON DELETE SET NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    heading_id TEXT REFERENCES headings(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    start_date TEXT,
    time_of_day TEXT,
    is_someday INTEGER NOT NULL DEFAULT 0,
    deadline TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    completed_at INTEGER,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS checklist_items (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    is_completed INTEGER NOT NULL DEFAULT 0,
    sort_order REAL NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS task_tags (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (task_id, tag_id)
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_start_date ON tasks(start_date);
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id);
CREATE INDEX IF NOT EXISTS idx_tasks_area ON tasks(area_id);
CREATE INDEX IF NOT EXISTS idx_tasks_heading ON tasks(heading_id);
CREATE INDEX IF NOT EXISTS idx_projects_status ON projects(status);
CREATE INDEX IF NOT EXISTS idx_projects_area ON projects(area_id);
"#;

/// v2: Trash is a timestamp beside `status` rather than a status value, so a
/// trashed task keeps its completion state. Rows using the old
/// `status = 'trashed'` are converted; their earlier status was not recorded,
/// so it is inferred from `completed_at`.
pub const MIGRATION_V2: &str = r#"
ALTER TABLE tasks ADD COLUMN trashed_at INTEGER;
UPDATE tasks
   SET trashed_at = updated_at,
       status = CASE WHEN completed_at IS NOT NULL THEN 'completed' ELSE 'open' END
 WHERE status = 'trashed';
CREATE INDEX IF NOT EXISTS idx_tasks_trashed ON tasks(trashed_at);
"#;

/// v3: projects can be trashed the same way as tasks (see v2).
pub const MIGRATION_V3: &str = r#"
ALTER TABLE projects ADD COLUMN trashed_at INTEGER;
UPDATE projects
   SET trashed_at = updated_at,
       status = CASE WHEN completed_at IS NOT NULL THEN 'completed' ELSE 'open' END
 WHERE status = 'trashed';
CREATE INDEX IF NOT EXISTS idx_projects_trashed ON projects(trashed_at);
"#;

/// v4: areas can be trashed, taking their projects and tasks with them.
pub const MIGRATION_V4: &str = r#"
ALTER TABLE areas ADD COLUMN trashed_at INTEGER;
CREATE INDEX IF NOT EXISTS idx_areas_trashed ON areas(trashed_at);
"#;

/// v5: Today keeps its own manual order, separate from a task's order in its
/// Inbox, area or project (`sort_order`).
pub const MIGRATION_V5: &str = r#"
ALTER TABLE tasks ADD COLUMN today_order REAL NOT NULL DEFAULT 0;
"#;

/// v6: a task's checklist is read by task, in order, on every list reload
/// (progress) and when a task opens.
pub const MIGRATION_V6: &str = r#"
CREATE INDEX IF NOT EXISTS idx_checklist_items_task ON checklist_items(task_id, sort_order);
"#;

/// v7: tasks that finishing their project closed are marked, so reopening
/// the project reopens exactly those (#55). Only a closed task is marked.
pub const MIGRATION_V7: &str = r#"
ALTER TABLE tasks ADD COLUMN closed_by_project INTEGER NOT NULL DEFAULT 0;
"#;

/// v8: tags (#57). A tag can be nested under another (`parent_id`; deleting
/// the parent lifts its children, as `Database::delete_tag` does), names
/// are unique ignoring case, and projects and areas take tags too. Added
/// columns rather than a rebuild: rebuilding `tags` would cascade into
/// `task_tags`.
pub const MIGRATION_V8: &str = r#"
ALTER TABLE tags ADD COLUMN parent_id TEXT REFERENCES tags(id) ON DELETE SET NULL;
ALTER TABLE tags ADD COLUMN sort_order REAL NOT NULL DEFAULT 0;
ALTER TABLE tags ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tags ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_name_nocase ON tags(name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_tags_parent ON tags(parent_id);
CREATE INDEX IF NOT EXISTS idx_task_tags_tag ON task_tags(tag_id);
CREATE TABLE IF NOT EXISTS project_tags (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (project_id, tag_id)
);
CREATE INDEX IF NOT EXISTS idx_project_tags_tag ON project_tags(tag_id);
CREATE TABLE IF NOT EXISTS area_tags (
    area_id TEXT NOT NULL REFERENCES areas(id) ON DELETE CASCADE,
    tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (area_id, tag_id)
);
CREATE INDEX IF NOT EXISTS idx_area_tags_tag ON area_tags(tag_id);
"#;

/// v11: repeating tasks (#61). The newest occurrence of a series carries the
/// rule (`repeat_rule`, JSON); every occurrence names its series (its first
/// occurrence's id) and the date it was for.
pub const MIGRATION_V11: &str = r#"
ALTER TABLE tasks ADD COLUMN repeat_rule TEXT;
ALTER TABLE tasks ADD COLUMN repeat_series TEXT;
ALTER TABLE tasks ADD COLUMN repeat_date TEXT;
CREATE INDEX IF NOT EXISTS idx_tasks_repeat_series ON tasks(repeat_series);
"#;

/// v10: reminders (#60). A task's reminder is a local time of day on its
/// start date. `reminder_deliveries` is this device's record of the
/// reminders it has shown, by task and local date and time, so each is shown
/// once; it is never synced.
pub const MIGRATION_V10: &str = r#"
ALTER TABLE tasks ADD COLUMN reminder_time TEXT;
CREATE TABLE IF NOT EXISTS reminder_deliveries (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    due TEXT NOT NULL,
    delivered_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, due)
);
"#;

/// v9: the sync engine's journal (#35). Every engine update, local or
/// received, in commit order; `outgoing` marks local ones. `sync_state`
/// exists once sync is enabled. Both stay empty while sync is off.
pub const MIGRATION_V9: &str = r#"
CREATE TABLE IF NOT EXISTS sync_journal (
    seq INTEGER PRIMARY KEY,
    bytes BLOB NOT NULL,
    outgoing INTEGER NOT NULL CHECK (outgoing IN (0, 1))
);
CREATE TABLE IF NOT EXISTS sync_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    enabled_at INTEGER NOT NULL
);
"#;

/// v12: sealed records (#36). Each recorded local update is sealed in its
/// own write transaction; `sync_writers` holds each open's writer
/// incarnation and record-chain head.
pub const MIGRATION_V12: &str = r#"
CREATE TABLE IF NOT EXISTS sync_writers (
    incarnation BLOB PRIMARY KEY CHECK (length(incarnation) = 32),
    opened_at INTEGER NOT NULL,
    head BLOB CHECK (head IS NULL OR length(head) = 48)
);
CREATE TABLE IF NOT EXISTS sync_records (
    seq INTEGER PRIMARY KEY REFERENCES sync_journal(seq),
    record_id BLOB NOT NULL UNIQUE CHECK (length(record_id) = 32),
    incarnation BLOB NOT NULL REFERENCES sync_writers(incarnation),
    bytes BLOB NOT NULL,
    acked INTEGER NOT NULL DEFAULT 0 CHECK (acked IN (0, 1))
);
"#;

/// v13: this device's membership of a sync collection (#37). The device's
/// private keys live in the platform keyring under `key_name`; SQLite holds
/// only public, signed or encrypted material. The authority is rebuilt on
/// each open by replaying `sync_controls` in order.
pub const MIGRATION_V13: &str = r#"
CREATE TABLE IF NOT EXISTS sync_device (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    application BLOB NOT NULL CHECK (length(application) = 16),
    tenant BLOB NOT NULL CHECK (length(tenant) = 16),
    collection BLOB NOT NULL CHECK (length(collection) = 16),
    document BLOB NOT NULL CHECK (length(document) = 16),
    root BLOB NOT NULL CHECK (length(root) = 48),
    device BLOB NOT NULL CHECK (length(device) = 48),
    key_name TEXT NOT NULL,
    -- active: writes seal. revoked or frozen: writes are kept locally,
    -- unsent, for explicit recovery.
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'revoked', 'frozen'))
);
-- Signed control records in the order this device accepted them, with
-- the competing record of a fork as evidence.
CREATE TABLE IF NOT EXISTS sync_controls (
    seq INTEGER PRIMARY KEY,
    bytes BLOB NOT NULL UNIQUE
);
-- Key envelopes addressed to this device, one per epoch manifest.
CREATE TABLE IF NOT EXISTS sync_envelopes (
    manifest BLOB PRIMARY KEY CHECK (length(manifest) = 48),
    bytes BLOB NOT NULL
);
"#;
