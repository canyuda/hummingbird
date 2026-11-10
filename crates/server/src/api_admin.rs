// 管理 API：渠道 / 模板 / 任务（含绑定与 token）/ 设置 / 推送日志（设计 §5）。
// 全部走 session 认证，由 main.rs 的路由统一守卫后进入这里。

use crate::api_auth::{err, Resp};
use crate::channels::{self, OutMessage};
use crate::http::ReqCtx;
use crate::AppState;
use rusqlite::params;
use serde_json::{json, Value};

const SECRET_KEYS: &[&str] = &["secret", "key", "bot_token", "access_token", "corp_secret"];

/// 渠道配置脱敏：GET 列表/详情时对敏感字段打码（设计 §6）
fn mask_config(config: &Value) -> Value {
    let mut masked = config.clone();
    if let Some(obj) = masked.as_object_mut() {
        for k in SECRET_KEYS {
            if obj.get(*k).map(|v| !v.is_null()).unwrap_or(false) {
                obj.insert(k.to_string(), json!("***"));
            }
        }
        if let Some(webhook) = obj.get("webhook").and_then(|v| v.as_str()).map(|s| s.to_string()) {
            if let Some((base, _)) = webhook.split_once('?') {
                obj.insert("webhook".into(), json!(format!("{base}?***")));
            }
        }
    }
    masked
}

// ---------------------------------------------------------------------------
// 渠道 CRUD + 测试
// ---------------------------------------------------------------------------

pub fn channels_list(state: &AppState) -> Resp {
    let conn = state.db.0.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT id, type, name, config_json, enabled, created_at FROM channels ORDER BY id")
        .unwrap();
    let rows: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "type": r.get::<_, String>(1)?,
                "name": r.get::<_, String>(2)?,
                "config": serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
                "enabled": r.get::<_, i64>(4)? != 0,
                "created_at": r.get::<_, String>(5)?,
            }))
        })
        .unwrap()
        .flatten()
        .map(|mut v| {
            if let Some(cfg) = v["config"].as_object() {
                let masked = mask_config(&Value::Object(cfg.clone()));
                v["config"] = masked;
            }
            v
        })
        .collect();
    (200, json!({"channels": rows}), None)
}

pub fn channels_create(state: &AppState, ctx: &ReqCtx) -> Resp {
    let ctype = ctx.body["type"].as_str().unwrap_or_default().to_string();
    if !channels::CHANNEL_TYPES.contains(&ctype.as_str()) {
        return err(400, "invalid channel type");
    }
    let name = ctx.body["name"].as_str().unwrap_or_default().to_string();
    if name.is_empty() {
        return err(400, "name required");
    }
    let config = ctx.body.get("config").cloned().unwrap_or(json!({}));
    let enabled = ctx.body["enabled"].as_bool().unwrap_or(true);
    let conn = state.db.0.lock().unwrap();
    conn.execute(
        "INSERT INTO channels (user_id, type, name, config_json, enabled) VALUES (1, ?1, ?2, ?3, ?4)",
        params![ctype, name, config.to_string(), enabled as i64],
    )
    .expect("insert channel");
    let id = conn.last_insert_rowid();
    (200, json!({"id": id}), None)
}

pub fn channels_update(state: &AppState, id: i64, ctx: &ReqCtx) -> Resp {
    let conn = state.db.0.lock().unwrap();
    if let Some(name) = ctx.body["name"].as_str() {
        conn.execute("UPDATE channels SET name = ?1 WHERE id = ?2", params![name, id]).unwrap();
    }
    if let Some(config) = ctx.body.get("config").map(|c| c.to_string()) {
        conn.execute("UPDATE channels SET config_json = ?1 WHERE id = ?2", params![config, id]).unwrap();
    }
    if let Some(enabled) = ctx.body["enabled"].as_bool() {
        conn.execute("UPDATE channels SET enabled = ?1 WHERE id = ?2", params![enabled as i64, id]).unwrap();
    }
    (200, json!({"ok": true}), None)
}

pub fn channels_delete(state: &AppState, id: i64) -> Resp {
    let conn = state.db.0.lock().unwrap();
    conn.execute("DELETE FROM channels WHERE id = ?1", params![id]).unwrap();
    (200, json!({"ok": true}), None)
}

/// 渠道测试：按当前 config 发一条测试消息（同时是代理配置的验证器，设计 §6）
pub fn channels_test(state: &AppState, id: i64) -> Resp {
    let row = {
        let conn = state.db.0.lock().unwrap();
        conn.query_row(
            "SELECT type, config_json FROM channels WHERE id = ?1",
            params![id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok()
    };
    let Some((ctype, config_str)) = row else {
        return err(404, "channel not found");
    };
    let config: Value = serde_json::from_str(&config_str).unwrap_or(json!({}));
    let msg = OutMessage {
        title: "蜂鸟测试消息".into(),
        body: format!("渠道 #{id} 连通性测试，时间 {}", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")),
    };
    let receipt = channels::send(&ctype, &config, &msg);
    (
        200,
        json!({"ok": receipt.ok, "via_proxy": receipt.via_proxy,
               "error": receipt.error, "cost_ms": receipt.cost_ms}),
        None,
    )
}

// ---------------------------------------------------------------------------
// 模板 CRUD + 渲染预览
// ---------------------------------------------------------------------------

fn template_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "name": r.get::<_, String>(1)?,
        "title_tpl": r.get::<_, String>(2)?,
        "body_tpl": r.get::<_, String>(3)?,
        "variables": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(json!([])),
        "created_at": r.get::<_, String>(5)?,
    }))
}

pub fn templates_list(state: &AppState) -> Resp {
    let conn = state.db.0.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT id, name, title_tpl, body_tpl, variables_json, created_at FROM templates ORDER BY id")
        .unwrap();
    let rows: Vec<Value> = stmt.query_map([], |r| template_row(r)).unwrap().flatten().collect();
    (200, json!({"templates": rows}), None)
}

pub fn templates_create(state: &AppState, ctx: &ReqCtx) -> Resp {
    let name = ctx.body["name"].as_str().unwrap_or_default().to_string();
    if name.is_empty() {
        return err(400, "name required");
    }
    let title_tpl = ctx.body["title_tpl"].as_str().unwrap_or("").to_string();
    let body_tpl = ctx.body["body_tpl"].as_str().unwrap_or("").to_string();
    let variables = ctx.body.get("variables").cloned().unwrap_or(json!([]));
    let conn = state.db.0.lock().unwrap();
    match conn.execute(
        "INSERT INTO templates (user_id, name, title_tpl, body_tpl, variables_json) VALUES (1, ?1, ?2, ?3, ?4)",
        params![name, title_tpl, body_tpl, variables.to_string()],
    ) {
        Ok(_) => (200, json!({"id": conn.last_insert_rowid()}), None),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
            err(409, "template name already exists")
        }
        Err(e) => panic!("insert template: {e}"),
    }
}

pub fn templates_update(state: &AppState, id: i64, ctx: &ReqCtx) -> Resp {
    let conn = state.db.0.lock().unwrap();
    if let Some(v) = ctx.body["name"].as_str() {
        conn.execute("UPDATE templates SET name = ?1 WHERE id = ?2", params![v, id]).unwrap();
    }
    if let Some(v) = ctx.body["title_tpl"].as_str() {
        conn.execute("UPDATE templates SET title_tpl = ?1 WHERE id = ?2", params![v, id]).unwrap();
    }
    if let Some(v) = ctx.body["body_tpl"].as_str() {
        conn.execute("UPDATE templates SET body_tpl = ?1 WHERE id = ?2", params![v, id]).unwrap();
    }
    if let Some(v) = ctx.body.get("variables") {
        conn.execute("UPDATE templates SET variables_json = ?1 WHERE id = ?2", params![v.to_string(), id]).unwrap();
    }
    (200, json!({"ok": true}), None)
}

pub fn templates_delete(state: &AppState, id: i64) -> Resp {
    let conn = state.db.0.lock().unwrap();
    // 模板被任务绑定时拒绝删除（外键会拦，但给用户可读的错误）
    let bound: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_bindings WHERE template_id = ?1", params![id], |r| r.get(0))
        .unwrap();
    if bound > 0 {
        return err(409, "template is bound by tasks; unbind first");
    }
    conn.execute("DELETE FROM templates WHERE id = ?1", params![id]).unwrap();
    (200, json!({"ok": true}), None)
}

/// 渲染预览：传 variables 返回替换后的 title/body（设计 §5）
pub fn templates_render(state: &AppState, id: i64, ctx: &ReqCtx) -> Resp {
    let row = {
        let conn = state.db.0.lock().unwrap();
        conn.query_row(
            "SELECT title_tpl, body_tpl, variables_json FROM templates WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .ok()
    };
    let Some((title_tpl, body_tpl, vars_json)) = row else {
        return err(404, "template not found");
    };
    let defaults = template_defaults(&vars_json);
    let mut vars = defaults;
    if let Some(extra) = ctx.body["variables"].as_object() {
        for (k, v) in extra {
            vars[k] = v.clone();
        }
    }
    (
        200,
        json!({"title": crate::tmpl::render(&title_tpl, &vars),
               "body": crate::tmpl::render(&body_tpl, &vars)}),
        None,
    )
}

/// 从模板 variables_json（[{key,desc,default}]）提取默认值合并表
pub fn template_defaults(vars_json: &str) -> Value {
    let arr: Vec<Value> = serde_json::from_str(vars_json).unwrap_or_default();
    let mut out = serde_json::Map::new();
    for v in arr {
        if let (Some(key), Some(default)) = (v["key"].as_str(), v["default"].as_str()) {
            out.insert(key.to_string(), json!(default));
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// 任务 CRUD + token 重生成 + 按绑定测试
// ---------------------------------------------------------------------------

pub fn tasks_list(state: &AppState) -> Resp {
    let conn = state.db.0.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT id, name, enabled, token_prefix, token_last_used_at, created_at, updated_at FROM tasks ORDER BY id")
        .unwrap();
    let mut tasks: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "enabled": r.get::<_, i64>(2)? != 0,
                "token_prefix": r.get::<_, String>(3)?,
                "token_last_used_at": r.get::<_, Option<String>>(4)?,
                "created_at": r.get::<_, String>(5)?,
                "updated_at": r.get::<_, String>(6)?,
            }))
        })
        .unwrap()
        .flatten()
        .collect();

    // 展开每个任务的绑定：[{template_id, template_name, channel_ids}]
    for task in tasks.iter_mut() {
        let task_id = task["id"].as_i64().unwrap();
        let mut b = conn
            .prepare(
                "SELECT b.template_id, t.name, b.channel_ids_json
                 FROM task_bindings b JOIN templates t ON t.id = b.template_id
                 WHERE b.task_id = ?1",
            )
            .unwrap();
        let bindings: Vec<Value> = b
            .query_map(params![task_id], |r| {
                Ok(json!({
                    "template_id": r.get::<_, i64>(0)?,
                    "template_name": r.get::<_, String>(1)?,
                    "channel_ids": serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(json!([])),
                }))
            })
            .unwrap()
            .flatten()
            .collect();
        task["bindings"] = json!(bindings);
    }
    (200, json!({"tasks": tasks}), None)
}

pub fn tasks_create(state: &AppState, ctx: &ReqCtx) -> Resp {
    let name = ctx.body["name"].as_str().unwrap_or_default().to_string();
    if name.is_empty() {
        return err(400, "name required");
    }
    let Some(bindings) = ctx.body["bindings"].as_array() else {
        return err(400, "bindings required");
    };

    // 先校验绑定引用的模板/渠道都存在（fail fast，不留半成品任务）
    {
        let conn = state.db.0.lock().unwrap();
        for b in bindings {
            let tid = b["template_id"].as_i64().unwrap_or(0);
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM templates WHERE id = ?1", params![tid], |r| r.get(0))
                .unwrap();
            if exists == 0 {
                return err(400, &format!("template {tid} not found"));
            }
            for cid in b["channel_ids"].as_array().cloned().unwrap_or_default() {
                let cid = cid.as_i64().unwrap_or(0);
                let exists: i64 = conn
                    .query_row("SELECT COUNT(*) FROM channels WHERE id = ?1", params![cid], |r| r.get(0))
                    .unwrap();
                if exists == 0 {
                    return err(400, &format!("channel {cid} not found"));
                }
            }
        }
    }

    let token = crate::auth::gen_task_token(); // 明文只在本次响应里出现一次
    let token_hash = crate::auth::sha256_hex(&token);
    let token_prefix = token.chars().take(8).collect::<String>();
    {
        let conn = state.db.0.lock().unwrap();
        match conn.execute(
            "INSERT INTO tasks (user_id, name, token_hash, token_prefix) VALUES (1, ?1, ?2, ?3)",
            params![name, token_hash, token_prefix],
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                return err(409, "task name already exists");
            }
            Err(e) => panic!("insert task: {e}"),
        }
        let task_id = conn.last_insert_rowid();
        for b in bindings {
            conn.execute(
                "INSERT INTO task_bindings (task_id, template_id, channel_ids_json) VALUES (?1, ?2, ?3)",
                params![
                    task_id,
                    b["template_id"].as_i64().unwrap_or(0),
                    b["channel_ids"].to_string()
                ],
            )
            .unwrap();
        }
    }
    (200, json!({"ok": true, "token": token, "note": "token 只显示这一次，请保存"}), None)
}

pub fn tasks_update(state: &AppState, id: i64, ctx: &ReqCtx) -> Resp {
    let conn = state.db.0.lock().unwrap();
    if let Some(name) = ctx.body["name"].as_str() {
        conn.execute(
            "UPDATE tasks SET name = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![name, id],
        )
        .unwrap();
    }
    if let Some(enabled) = ctx.body["enabled"].as_bool() {
        conn.execute(
            "UPDATE tasks SET enabled = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![enabled as i64, id],
        )
        .unwrap();
    }
    // 传入 bindings 时整体替换（更新语义，设计 §5）
    if let Some(bindings) = ctx.body["bindings"].as_array() {
        conn.execute("DELETE FROM task_bindings WHERE task_id = ?1", params![id]).unwrap();
        for b in bindings {
            conn.execute(
                "INSERT INTO task_bindings (task_id, template_id, channel_ids_json) VALUES (?1, ?2, ?3)",
                params![id, b["template_id"].as_i64().unwrap_or(0), b["channel_ids"].to_string()],
            )
            .unwrap();
        }
        conn.execute("UPDATE tasks SET updated_at = datetime('now') WHERE id = ?1", params![id]).unwrap();
    }
    (200, json!({"ok": true}), None)
}

pub fn tasks_delete(state: &AppState, id: i64) -> Resp {
    let conn = state.db.0.lock().unwrap();
    conn.execute("DELETE FROM task_bindings WHERE task_id = ?1", params![id]).unwrap();
    conn.execute("DELETE FROM tasks WHERE id = ?1", params![id]).unwrap();
    (200, json!({"ok": true}), None)
}

/// token 重生成：旧 token 立即失效（hash 被覆盖）
pub fn tasks_regenerate(state: &AppState, id: i64) -> Resp {
    let token = crate::auth::gen_task_token();
    {
        let conn = state.db.0.lock().unwrap();
        let n = conn
            .execute(
                "UPDATE tasks SET token_hash = ?1, token_prefix = ?2, updated_at = datetime('now') WHERE id = ?3",
                params![crate::auth::sha256_hex(&token), token.chars().take(8).collect::<String>(), id],
            )
            .unwrap();
        if n == 0 {
            return err(404, "task not found");
        }
    }
    (200, json!({"ok": true, "token": token, "note": "旧 token 已失效"}), None)
}

/// 按绑定逐模板逐渠道发测试：模板变量用默认值渲染
pub fn tasks_test(state: &AppState, id: i64) -> Resp {
    let rows = {
        let conn = state.db.0.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT t.title_tpl, t.body_tpl, t.variables_json, b.channel_ids_json, t.name
                 FROM task_bindings b JOIN templates t ON t.id = b.template_id
                 WHERE b.task_id = ?1",
            )
            .unwrap();
        stmt.query_map(params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .flatten()
        .collect::<Vec<_>>()
    };
    if rows.is_empty() {
        return err(400, "task has no bindings");
    }
    let conn = state.db.0.lock().unwrap();
    let mut results = Vec::new();
    for (title_tpl, body_tpl, vars_json, channel_ids_json, tpl_name) in rows {
        let vars = template_defaults(&vars_json);
        let msg = OutMessage {
            title: crate::tmpl::render(&title_tpl, &vars),
            body: crate::tmpl::render(&body_tpl, &vars),
        };
        let cids: Vec<i64> = serde_json::from_str::<Value>(&channel_ids_json)
            .unwrap_or(json!([]))
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
            .unwrap_or_default();
        for cid in cids {
            let row = conn
                .query_row(
                    "SELECT type, config_json, name FROM channels WHERE id = ?1 AND enabled = 1",
                    params![cid],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
                )
                .ok();
            let Some((ctype, cfg, cname)) = row else {
                results.push(json!({"template": tpl_name, "channel": format!("#{cid}"), "status": "failed", "error": "channel missing/disabled"}));
                continue;
            };
            let receipt = channels::send(&ctype, &serde_json::from_str(&cfg).unwrap_or(json!({})), &msg);
            results.push(json!({
                "template": tpl_name, "channel": cname,
                "status": if receipt.ok { "ok" } else { "failed" },
                "via_proxy": receipt.via_proxy, "error": receipt.error, "cost_ms": receipt.cost_ms,
            }));
        }
    }
    (200, json!({"results": results}), None)
}

// ---------------------------------------------------------------------------
// 设置（站点样式 + 日志保留天数）与推送日志查询
// ---------------------------------------------------------------------------

const SETTING_DEFAULTS: &[(&str, &str)] = &[
    ("site_title", "蜂鸟 Hummingbird"),
    ("site_slogan", "轻量多渠道消息推送网关"),
    ("site_icon", ""),
    ("theme_color", "#16a34a"),
    ("log_retention_days", "90"),
];

pub fn settings_get(state: &AppState) -> Resp {
    let conn = state.db.0.lock().unwrap();
    let mut out = serde_json::Map::new();
    for (k, default) in SETTING_DEFAULTS {
        let v: String = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![k], |r| r.get(0))
            .unwrap_or_else(|_| default.to_string());
        out.insert(k.to_string(), json!(v));
    }
    (200, json!({"settings": Value::Object(out)}), None)
}

pub fn settings_put(state: &AppState, ctx: &ReqCtx) -> Resp {
    let Some(obj) = ctx.body.as_object() else {
        return err(400, "json object required");
    };
    let conn = state.db.0.lock().unwrap();
    for (k, v) in obj {
        if !SETTING_DEFAULTS.iter().any(|(allowed, _)| allowed == k) {
            return err(400, &format!("unknown setting: {k}"));
        }
        let val = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![k, val],
        )
        .unwrap();
    }
    (200, json!({"ok": true}), None)
}

/// 推送日志：分页（每页 50），支持 task/status/channel 过滤（设计 §5）
pub fn messages_list(state: &AppState, ctx: &ReqCtx) -> Resp {
    // 先收集全部键值对再扫描（find 会消费迭代器，不能边找边用）
    let pairs: Vec<(String, String)> = form_urlencoded::parse(ctx.query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let page: u32 = pairs
        .iter()
        .find(|(k, _)| k == "page")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(1);
    let mut sql = String::from(
        "SELECT p.id, p.batch_id, p.title, p.content_brief, p.status, p.via_proxy, p.error,
                p.cost_ms, p.created_at, p.task_id, p.template_id, p.channel_id,
                t.name, c.name
         FROM push_logs p
         LEFT JOIN tasks t ON t.id = p.task_id
         LEFT JOIN channels c ON c.id = p.channel_id WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    for (k, v) in &pairs {
        match k.as_str() {
            "task" => {
                sql.push_str(&format!(" AND p.task_id = ?{}", args.len() + 1));
                args.push(Box::new(v.parse::<i64>().unwrap_or(0)));
            }
            "channel" => {
                sql.push_str(&format!(" AND p.channel_id = ?{}", args.len() + 1));
                args.push(Box::new(v.parse::<i64>().unwrap_or(0)));
            }
            "status" => {
                sql.push_str(&format!(" AND p.status = ?{}", args.len() + 1));
                args.push(Box::new(v.clone()));
            }
            _ => {}
        }
    }
    // COUNT 与列表用同一套 WHERE（否则 total 对不上过滤结果）
    let where_sql = sql.split(" WHERE 1=1").nth(1).unwrap_or("").to_string();
    let total: i64 = {
        let conn = state.db.0.lock().unwrap();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM push_logs p LEFT JOIN tasks t ON t.id = p.task_id LEFT JOIN channels c ON c.id = p.channel_id WHERE 1=1{where_sql}"),
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    sql.push_str(&format!(
        " ORDER BY p.id DESC LIMIT 50 OFFSET {}",
        (page.max(1) - 1) * 50
    ));

    let conn = state.db.0.lock().unwrap();
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows: Vec<Value> = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "batch_id": r.get::<_, Option<i64>>(1)?,
                "title": r.get::<_, String>(2)?,
                "content_brief": r.get::<_, String>(3)?,
                "status": r.get::<_, String>(4)?,
                "via_proxy": r.get::<_, i64>(5)? != 0,
                "error": r.get::<_, Option<String>>(6)?,
                "cost_ms": r.get::<_, i64>(7)?,
                "created_at": r.get::<_, String>(8)?,
                "task": r.get::<_, Option<String>>(12)?,
                "template_id": r.get::<_, Option<i64>>(10)?,
                "channel": r.get::<_, Option<String>>(13)?,
            }))
        })
        .unwrap()
        .flatten()
        .collect();
    let _ = &total; // total 已在上方按过滤条件计算
    (200, json!({"messages": rows, "total": total, "page": page}), None)
}

/// 概览统计（前端仪表盘用）：今日总数 / 成功 / 失败 + 近 7 天逐日量与渠道分布（UTC 口径）
pub fn overview(state: &AppState) -> Resp {
    let conn = state.db.0.lock().unwrap();
    let (total, ok): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), SUM(CASE WHEN status='ok' THEN 1 ELSE 0 END)
             FROM push_logs WHERE created_at >= date('now')",
            [],
            |r| Ok((r.get(0)?, r.get(1).unwrap_or(0))),
        )
        .unwrap();
    let channels: i64 = conn.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0)).unwrap();
    let tasks: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();

    // 近 7 天逐日（含今日）；时间统一 UTC（chrono 仅 UTC 的设计口径）
    let mut days7 = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT date(created_at) d, COUNT(*), SUM(CASE WHEN status='ok' THEN 1 ELSE 0 END)
                 FROM push_logs
                 WHERE created_at >= date('now', '-6 days', 'start of day')
                 GROUP BY d ORDER BY d",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(json!({"date": r.get::<_, String>(0)?, "total": r.get::<_, i64>(1)?,
                          "ok": r.get::<_, i64>(2)?}))
            })
            .unwrap();
        for row in rows {
            days7.push(row.unwrap());
        }
    }

    // 近 7 天渠道分布 TOP8（饼图用）
    let mut channels7 = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT COALESCE(c.name, '未知') name, COUNT(*) n
                 FROM push_logs p LEFT JOIN channels c ON c.id = p.channel_id
                 WHERE p.created_at >= date('now', '-6 days', 'start of day')
                 GROUP BY p.channel_id ORDER BY n DESC LIMIT 8",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok(json!({"name": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?})))
            .unwrap();
        for row in rows {
            channels7.push(row.unwrap());
        }
    }

    (200, json!({"today_total": total, "today_ok": ok, "today_failed": total - ok,
                 "channels": channels, "tasks": tasks, "days7": days7, "channels7": channels7}), None)
}
