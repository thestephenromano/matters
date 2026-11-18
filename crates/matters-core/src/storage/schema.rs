pub const SCHEMA_V1: &str = r#"
PRAGMA journal_mode = WAL;
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
