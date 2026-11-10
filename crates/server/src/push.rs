// 推送核心与外部推送 API（设计 §5 v0.8 任务模型）：
//   POST /api/push     Authorization: Bearer <任务token>
//                      {"template":"巡检","variables":{...}} 或 {"messages":[...]}（批量）
//   GET  /push/<token>?template=巡检&host=pi   —— 一行流，template 之外的查询参数即变量
// 渠道由任务绑定决定，调用方不可指定渠道；未绑定模板 → 403。

use crate::api_admin::template_defaults;
use crate::api_auth::{err, Resp};
use crate::auth;
use crate::channels::{self, OutMessage};
use crate::http::ReqCtx;
use crate::AppState;
use rusqlite::params;
use serde_json::{json, Value};

const MAX_MESSAGES: usize = 20; // 单次批量上限（设计 §5）

// ---------------------------------------------------------------------------
// token 认证与绑定解析
// ---------------------------------------------------------------------------

struct Task {
    id: i64,
    enabled: bool,
}

fn auth_task(state: &AppState, token: &str) -> Option<Task> {
    let hash = auth::sha256_hex(token);
    let conn = state.db.0.lock().unwrap();
    let task = conn
        .query_row(
            "SELECT id, enabled FROM tasks WHERE token_hash = ?1",
            params![hash],
            |r| Ok(Task { id: r.get(0)?, enabled: r.get::<_, i64>(1)? != 0 }),
        )
        .ok()?;
    // 惰性更新 last_used（失败不影响推送主流程）
    let _ = conn.execute(
        "UPDATE tasks SET token_last_used_at = datetime('now') WHERE id = ?1",
        params![task.id],
    );
    Some(task)
}

/// 解析模板引用：名字或数字 id；返回 (template_id, title_tpl, body_tpl, defaults)
fn load_template(
    state: &AppState,
    task_id: i64,
    template_ref: &str,
) -> Option<(i64, String, String, Value)> {
    let conn = state.db.0.lock().unwrap();
    // 数字 → 按 id 找；否则按名字找
    let (cond, arg): (&str, String) = match template_ref.parse::<i64>() {
        Ok(id) => ("t.id = ?2", id.to_string()),
        Err(_) => ("t.name = ?2", template_ref.to_string()),
    };
    conn.query_row(
        &format!(
            "SELECT t.id, t.title_tpl, t.body_tpl, t.variables_json
             FROM task_bindings b JOIN templates t ON t.id = b.template_id
             WHERE b.task_id = ?1 AND {cond}"
        ),
        params![task_id, arg],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or(json!([])),
            ))
        },
    )
    .ok()
    .map(|(id, title, body, vars)| (id, title, body, template_defaults(&vars.to_string())))
}

// ---------------------------------------------------------------------------
// 推送主流程
// ---------------------------------------------------------------------------

struct SendItem {
    template_ref: String,
    variables: Value,
}

/// 执行一次推送调用：逐条消息渲染 → 绑定渠道并行发送 → 逐渠道写日志 → 返回结果数组。
/// 返回 (整体状态码, 响应体)；部分失败仍是 200 + results（语义同设计 §5 的逐渠道结果）。
fn do_push(state: &AppState, task: &Task, items: &[SendItem]) -> Resp {
    if !task.enabled {
        return err(403, "task disabled");
    }
    if items.is_empty() || items.len() > MAX_MESSAGES {
        return err(400, &format!("messages: 1..{MAX_MESSAGES} items required"));
    }

    // 预解析每条消息的模板（fail fast：任何一条引用了未绑定模板，整单 403）
    let mut plan: Vec<(SendItem, i64, String, String, Value)> = Vec::with_capacity(items.len());
    for item in items {
        let Some((tpl_id, title_tpl, body_tpl, defaults)) =
            load_template(state, task.id, &item.template_ref)
        else {
            return err(403, &format!("template '{}' is not bound to this task", item.template_ref));
        };
        // 变量合并：模板默认值 < 调用方变量
        let mut vars = defaults;
        if let Some(extra) = item.variables.as_object() {
            for (k, v) in extra {
                vars[k] = v.clone();
            }
        }
        plan.push((
            SendItem { template_ref: item.template_ref.clone(), variables: item.variables.clone() },
            tpl_id,
            title_tpl,
            body_tpl,
            vars,
        ));
    }

    let mut results: Vec<Value> = Vec::new();
    let mut batch_id: Option<i64> = None;

    for (msg_idx, (item, tpl_id, title_tpl, body_tpl, vars)) in plan.into_iter().enumerate() {
        let msg = OutMessage {
            title: crate::tmpl::render(&title_tpl, &vars),
            body: crate::tmpl::render(&body_tpl, &vars),
        };
        if msg.title.len() + msg.body.len() > 64 * 1024 {
            results.push(json!({"msg": msg_idx, "template": item.template_ref, "status": "failed",
                                "error": "message exceeds 64KB"}));
            continue;
        }

        // 该模板绑定的渠道（先读库解析出可用渠道，线程里只做纯发送）
        let channel_ids: Vec<i64> = {
            let conn = state.db.0.lock().unwrap();
            conn.query_row(
                "SELECT channel_ids_json FROM task_bindings WHERE task_id = ?1 AND template_id = ?2",
                params![task.id, tpl_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()))
            .unwrap_or_default()
        };
        if channel_ids.is_empty() {
            results.push(json!({"msg": msg_idx, "template": item.template_ref, "status": "failed",
                                "error": "no channels bound"}));
            continue;
        }

        // (id, name, type, config) —— 渠道缺失/停用直接记为失败，不进发送线程。
        // 注意：单连接 Mutex，所有锁都用短作用域，绝不嵌套加锁。
        let mut chans: Vec<(i64, String, String, Value)> = Vec::new();
        for cid in &channel_ids {
            let row = {
                let conn = state.db.0.lock().unwrap();
                conn.query_row(
                    "SELECT id, name, type, config_json FROM channels WHERE id = ?1 AND enabled = 1",
                    params![cid],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or(json!({})),
                        ))
                    },
                )
                .ok()
            };
            match row {
                Some(c) => chans.push(c),
                None => {
                    let brief: String = msg.body.chars().take(80).collect();
                    {
                        let conn = state.db.0.lock().unwrap();
                        conn.execute(
                            "INSERT INTO push_logs (user_id, task_id, template_id, batch_id, channel_id,
                                                    title, content_brief, status, via_proxy, error, cost_ms)
                             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, 'failed', 0, 'channel missing/disabled', 0)",
                            params![task.id, tpl_id, batch_id, cid, msg.title, brief],
                        )
                        .unwrap();
                    }
                    results.push(json!({"msg": msg_idx, "template": item.template_ref,
                                        "channel": format!("#{cid}"), "status": "failed",
                                        "error": "channel missing/disabled"}));
                }
            }
        }

        // 每渠道一个短命线程并行发送，join 后统一写日志（耗时 = 最慢渠道）
        let handles: Vec<_> = chans
            .into_iter()
            .map(|(cid, cname, ctype, cfg)| {
                let msg = OutMessage { title: msg.title.clone(), body: msg.body.clone() };
                std::thread::spawn(move || {
                    let receipt = channels::send(&ctype, &cfg, &msg);
                    (cid, cname, msg, receipt)
                })
            })
            .collect();

        for handle in handles {
            let (cid, cname, msg, receipt) = handle.join().expect("channel thread");
            let brief: String = msg.body.chars().take(80).collect();
            let conn = state.db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO push_logs (user_id, task_id, template_id, batch_id, channel_id,
                                        title, content_brief, status, via_proxy, error, cost_ms)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    task.id,
                    tpl_id,
                    batch_id,
                    cid,
                    msg.title,
                    brief,
                    if receipt.ok { "ok" } else { "failed" },
                    receipt.via_proxy as i64,
                    receipt.error,
                    receipt.cost_ms as i64,
                ],
            )
            .expect("insert push_log");
            let id = conn.last_insert_rowid();
            if batch_id.is_none() {
                batch_id = Some(id); // 批次键 = 本批次首条日志 id（设计 §4）
                // 首行插入时批次键还未知，这里回填，保证单发时 batch_id == id
                conn.execute(
                    "UPDATE push_logs SET batch_id = ?1 WHERE id = ?1",
                    params![id],
                )
                .expect("backfill batch_id");
            }
            results.push(json!({
                "msg": msg_idx,
                "template": item.template_ref,
                "channel": cname,
                "status": if receipt.ok { "ok" } else { "failed" },
                "via_proxy": receipt.via_proxy,
                "error": receipt.error,
                "cost_ms": receipt.cost_ms,
            }));
        }
    }

    maybe_cleanup(state); // 懒触发日志清理（无常驻后台线程，设计 §3）
    let ok = results.iter().all(|r| r["status"] == "ok");
    (200, json!({"ok": ok, "batch_id": batch_id, "results": results}), None)
}

// ---------------------------------------------------------------------------
// 两个 HTTP 入口
// ---------------------------------------------------------------------------

pub fn api_push(state: &AppState, ctx: &ReqCtx) -> Resp {
    // Bearer token 认证
    let Some(token) = ctx.bearer_token.clone() else {
        return err(401, "Authorization: Bearer <task_token> required");
    };
    let Some(task) = auth_task(state, &token) else {
        return err(401, "invalid task token");
    };

    // 单发：{"template","variables"}；批量：{"messages":[{template,variables},...]}
    let items: Vec<SendItem> = if let Some(arr) = ctx.body["messages"].as_array() {
        arr.iter()
            .map(|m| SendItem {
                template_ref: m["template"].as_str().unwrap_or_default().to_string(),
                variables: m.get("variables").cloned().unwrap_or(json!({})),
            })
            .collect()
    } else {
        vec![SendItem {
            // 单发且任务只有一个绑定时可省略 template
            template_ref: ctx.body["template"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            variables: ctx.body.get("variables").cloned().unwrap_or(json!({})),
        }]
    };
    // 空模板引用：尝试任务唯一绑定
    let items = resolve_implicit_template(state, task.id, items);
    do_push(state, &task, &items)
}

/// GET 一行流：/push/<token>?template=X&host=pi —— 其余查询参数全部作为变量
pub fn get_push(state: &AppState, ctx: &ReqCtx) -> Resp {
    let Some(token) = ctx.path.strip_prefix("/push/") else {
        return err(404, "not found");
    };
    if token.is_empty() || token.contains('/') {
        return err(400, "bad token path");
    }
    let Some(task) = auth_task(state, token) else {
        return err(401, "invalid task token");
    };

    let mut template_ref = String::new();
    let mut vars = serde_json::Map::new();
    for (k, v) in form_urlencoded::parse(ctx.query.as_bytes()) {
        if k == "template" {
            template_ref = v.to_string();
        } else {
            vars.insert(k.to_string(), json!(v.to_string()));
        }
    }
    let items = resolve_implicit_template(
        state,
        task.id,
        vec![SendItem { template_ref, variables: Value::Object(vars) }],
    );
    do_push(state, &task, &items)
}

/// 单条且未指定模板时：任务只有一个绑定 → 用它；多个绑定 → 400 要求指定
fn resolve_implicit_template(state: &AppState, task_id: i64, items: Vec<SendItem>) -> Vec<SendItem> {
    if items.len() == 1 && items[0].template_ref.is_empty() {
        let conn = state.db.0.lock().unwrap();
        let only: Option<String> = conn
            .query_row(
                "SELECT t.name FROM task_bindings b JOIN templates t ON t.id = b.template_id
                 WHERE b.task_id = ?1 LIMIT 2",
                params![task_id],
                |r| r.get(0),
            )
            .ok();
        // 取第二行判断是否唯一（LIMIT 2 只取到 1 行即唯一）
        if let Some(name) = only {
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM task_bindings WHERE task_id = ?1", params![task_id], |r| r.get(0))
                .unwrap();
            if count == 1 {
                let mut fixed = items;
                fixed[0].template_ref = name;
                return fixed;
            }
        }
    }
    items
}

// ---------------------------------------------------------------------------
// 日志懒清理：每次推送顺带检查，距上次清理超过 24h 才执行一次
// ---------------------------------------------------------------------------

fn maybe_cleanup(state: &AppState) {
    let conn = state.db.0.lock().unwrap();
    let last: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'last_cleanup_at'", [], |r| r.get(0))
        .ok();
    let due = last
        .map(|l| {
            conn.query_row(
                "SELECT datetime(?) < datetime('now', '-1 day')",
                params![l],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(1)
            == 1
        })
        .unwrap_or(true); // 从未清理过 → 触发
    if !due {
        return;
    }
    let days: i64 = conn
        .query_row("SELECT value FROM settings WHERE key = 'log_retention_days'", [], |r| r.get::<_, String>(0))
        .unwrap_or_else(|_| "90".into())
        .parse()
        .unwrap_or(90);
    let deleted = conn
        .execute(
            "DELETE FROM push_logs WHERE created_at < datetime('now', ?1)",
            params![format!("-{days} days")],
        )
        .unwrap_or(0);
    if deleted > 0 {
        log::info!("cleanup: pruned {deleted} push_logs older than {days} days");
    }
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('last_cleanup_at', datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = datetime('now')",
        [],
    )
    .ok();
}
