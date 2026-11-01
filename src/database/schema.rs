//! Schema and migrations. `PRAGMA user_version` tracks the applied version.

pub const MIGRATIONS: &[&str] = &[
    // v1
    "CREATE TABLE bookmarks (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        name      TEXT NOT NULL,
        path      TEXT NOT NULL UNIQUE,
        position  INTEGER NOT NULL DEFAULT 0
     );
     CREATE TABLE recent_locations (
        path       TEXT PRIMARY KEY,
        visited_at INTEGER NOT NULL,
        visits     INTEGER NOT NULL DEFAULT 1
     );
     CREATE TABLE recent_files (
        path      TEXT PRIMARY KEY,
        opened_at INTEGER NOT NULL
     );
     CREATE TABLE tabs (
        position INTEGER PRIMARY KEY,
        path     TEXT NOT NULL,
        active   INTEGER NOT NULL DEFAULT 0
     );
     CREATE TABLE window_state (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
     );
     CREATE TABLE user_preferences (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
     );",
    // v2
    "CREATE TABLE saved_searches (
        query   TEXT PRIMARY KEY,
        used_at INTEGER NOT NULL
     );",
    // v3
    "CREATE TABLE connections (
        id       INTEGER PRIMARY KEY AUTOINCREMENT,
        name     TEXT NOT NULL,
        uri      TEXT NOT NULL UNIQUE,
        position INTEGER NOT NULL DEFAULT 0
     );",
    // v4
    "CREATE TABLE file_tags (
        path TEXT NOT NULL,
        tag  TEXT NOT NULL,
        PRIMARY KEY (path, tag)
     );
     CREATE INDEX file_tags_by_tag ON file_tags (tag);",
    // v5
    "CREATE TABLE custom_actions (
        id       INTEGER PRIMARY KEY AUTOINCREMENT,
        name     TEXT NOT NULL,
        command  TEXT NOT NULL,
        patterns TEXT NOT NULL DEFAULT '',
        wait     INTEGER NOT NULL DEFAULT 0,
        position INTEGER NOT NULL DEFAULT 0
     );",
];
