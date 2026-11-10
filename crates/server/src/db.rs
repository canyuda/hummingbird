// SQLite 层：单连接 + Mutex，WAL 模式，pragma 按极致省内存调优（设计 §3）。
// 所有语句一律参数化，任何地方都不允许字符串拼接 SQL。

use rusqlite::Connection;
use std::sync::Mutex;

pub struct Db(pub Mutex<Connection>);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'admin',
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    last_login_at TEXT,
    must_change_password INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id),
    expires_at TEXT NOT NULL,
    ip         TEXT,
    user_agent TEXT
);

CREATE TABLE IF NOT EXISTS channels (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     INTEGER NOT NULL,
    type        TEXT NOT NULL,
    name        TEXT NOT NULL,
    config_json TEXT NOT NULL DEFAULT '{}',
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS templates (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id        INTEGER NOT NULL,
    name           TEXT NOT NULL,
    title_tpl      TEXT NOT NULL DEFAULT '',
    body_tpl       TEXT NOT NULL DEFAULT '',
    variables_json TEXT NOT NULL DEFAULT '[]',
    created_at     TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (user_id, name)
);

CREATE TABLE IF NOT EXISTS tasks (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id            INTEGER NOT NULL,
    name               TEXT NOT NULL,
    enabled            INTEGER NOT NULL DEFAULT 1,
    token_hash         TEXT NOT NULL UNIQUE,
    token_prefix       TEXT NOT NULL,
    token_last_used_at TEXT,
    created_at         TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at         TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (user_id, name)
);

CREATE TABLE IF NOT EXISTS task_bindings (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id           INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    template_id       INTEGER NOT NULL REFERENCES templates(id),
    channel_ids_json  TEXT NOT NULL DEFAULT '[]',
    UNIQUE (task_id, template_id)
);

CREATE TABLE IF NOT EXISTS push_logs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL,
    task_id       INTEGER,
    template_id   INTEGER,
    batch_id      INTEGER,
    channel_id    INTEGER,
    title         TEXT NOT NULL DEFAULT '',
    content_brief TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL,
    via_proxy     INTEGER NOT NULL DEFAULT 0,
    error         TEXT,
    cost_ms       INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_push_logs_user_time ON push_logs (user_id, created_at DESC);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// 打开数据库：应用 pragma 与建表 DDL；首次运行时引导创建 admin 账号
/// （随机密码只在 stdout 打印一次）。
pub fn init(db_path: &str) -> Db {
    let conn = Connection::open(db_path).expect("open sqlite db");
    conn.pragma_update(None, "journal_mode", "WAL").expect("WAL");
    conn.pragma_update(None, "busy_timeout", 5000).expect("busy_timeout");
    conn.pragma_update(None, "cache_size", -512).expect("cache_size"); // 页缓存压到 512KB
    conn.pragma_update(None, "mmap_size", 67_108_864).expect("mmap_size"); // 64MB mmap：页可被系统回收，不占堆
    conn.pragma_update(None, "foreign_keys", "ON").expect("foreign_keys");
    conn.execute_batch(SCHEMA).expect("apply schema");

    let user_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .expect("count users");
    if user_count == 0 {
        let password = crate::auth::gen_random_string(16);
        let hash = crate::auth::hash_password(&password);
        conn.execute(
            "INSERT INTO users (username, password_hash, must_change_password) VALUES ('admin', ?1, 1)",
            [&hash],
        )
        .expect("bootstrap admin");
        // 初始密码只在首启时输出这一次（丢了用 reset-password 重置）。
        println!("=====================================================");
        println!(" First run: initial admin account created.");
        println!("   username: admin");
        println!("   password: {password}");
        println!(" (shown only this once; login forces a change,");
        println!("  `hummingbird reset-password` if lost)");
        println!("=====================================================");
    }
    Db(Mutex::new(conn))
}
