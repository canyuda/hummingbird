// 推送核心与外部推送 API（设计 §5 v0.8 任务模型）：
//   POST /api/push     Authorization: Bearer <任务token>
//                      {"template":"巡检","variables":{...}} 或 {"messages":[...]}（批量）
//   GET  /push/<token>?template=巡检&host=pi   —— 一行流，template 之外的查询参数即变量
// 渠道由任务绑定决定，调用方不可指定渠道；未绑定模板 → 403。
// 失败重试（openspec add-push-retry）：任务级 retry_json 配置，sync 模式线程内指数退避，
// async 模式落 retry_queue 懒触发 drain；详版见 docs/推送失败自动重试提案.md。

use crate::api_admin::template_defaults;
use crate::api_auth::{err, Resp};
use crate::auth;
use crate::channels::{self, OutMessage};
use crate::http::ReqCtx;
use crate::AppState;
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_MESSAGES: usize = 20; // 单次批量上限（设计 §5）

// ---------------------------------------------------------------------------
// 失败重试（openspec add-push-retry；详版 docs/推送失败自动重试提案.md）
// ---------------------------------------------------------------------------

/// 任务级重试配置（tasks.retry_json；字段缺失/坏数据逐项回退默认，推送主流程不受影响）
#[derive(Clone)]
struct RetryCfg {
    enabled: bool,
    max: u32,
    mode: String, // "sync" | "async"
    base_ms: u64,
}

/// 与 db.rs SCHEMA 中 tasks.retry_json 列默认值保持一致
pub const RETRY_DEFAULT_JSON: &str = r#"{"enabled":true,"max":3,"mode":"sync","base_ms":1000}"#;

const RETRY_MAX_LIMIT: i64 = 10;       // max 边界 0..=10（提案 D5）
const RETRY_BASE_MIN_MS: i64 = 100;    // base_ms 边界（提案 D5）
const RETRY_BASE_MAX_MS: i64 = 60_000;
const DRAIN_BATCH: usize = 20;         // 单次 drain 上限（提案 D7）
const RETRY_STALE_SECS: i64 = 3600;    // 队列滞留过期窗口：1h（提案 D8）

impl RetryCfg {
    fn from_json(s: &str) -> RetryCfg {
        let v: Value = serde_json::from_str(s).unwrap_or_else(|_| json!({}));
        // clamp 只兜坏数据（API 写入侧已 fail fast 校验，正常不会越界）
        RetryCfg {
            enabled: v["enabled"].as_bool().unwrap_or(true),
            max: v["max"].as_i64().unwrap_or(3).clamp(0, RETRY_MAX_LIMIT) as u32,
            mode: v["mode"].as_str().unwrap_or("sync").to_string(),
            base_ms: v["base_ms"]
                .as_i64()
                .unwrap_or(1000)
                .clamp(RETRY_BASE_MIN_MS, RETRY_BASE_MAX_MS) as u64,
        }
    }
    fn active(&self) -> bool {
        self.enabled && self.max > 0
    }
    fn is_async(&self) -> bool {
        self.mode == "async"
    }
}

/// 校验任务表单的 retry 对象（fail fast，越界 400）；合法返回规范化 JSON 串
pub fn validate_retry(v: &Value) -> Result<String, String> {
    let obj = v.as_object().ok_or("retry: object required")?;
    let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let max = obj.get("max").and_then(Value::as_i64).unwrap_or(3);
    if !(0..=RETRY_MAX_LIMIT).contains(&max) {
        return Err(format!("retry.max must be 0..={RETRY_MAX_LIMIT}"));
    }
    let mode = obj.get("mode").and_then(Value::as_str).unwrap_or("sync");
    if mode != "sync" && mode != "async" {
        return Err("retry.mode must be sync or async".into());
    }
    let base_ms = obj.get("base_ms").and_then(Value::as_i64).unwrap_or(1000);
    if !(RETRY_BASE_MIN_MS..=RETRY_BASE_MAX_MS).contains(&base_ms) {
        return Err(format!("retry.base_ms must be {RETRY_BASE_MIN_MS}..={RETRY_BASE_MAX_MS}"));
    }
    Ok(json!({"enabled": enabled, "max": max, "mode": mode, "base_ms": base_ms}).to_string())
}

/// 指数退避：第 n 次重试前的等待毫秒 = base × 2^(n-1)（提案 §3，无 jitter）
fn backoff_ms(base_ms: u64, retry_index: u32) -> u64 {
    base_ms.saturating_mul(1u64 << (retry_index - 1).min(31))
}

/// 毫秒 → SQLite datetime 修饰符（用小数秒：datetime 不支持 "milliseconds" 单位）
fn ms_modifier(ms: u64) -> String {
    format!("+{}.{:03} seconds", ms / 1000, ms % 1000)
}

// ---------------------------------------------------------------------------
// token 认证与绑定解析
// ---------------------------------------------------------------------------

struct Task {
    id: i64,
    enabled: bool,
    retry: RetryCfg,
}

fn auth_task(state: &AppState, token: &str) -> Option<Task> {
    let hash = auth::sha256_hex(token);
    let conn = state.db.0.lock().unwrap();
    let task = conn
        .query_row(
            "SELECT id, enabled, retry_json FROM tasks WHERE token_hash = ?1",
            params![hash],
            |r| {
                Ok(Task {
                    id: r.get(0)?,
                    enabled: r.get::<_, i64>(1)? != 0,
                    retry: RetryCfg::from_json(&r.get::<_, String>(2)?),
                })
            },
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

/// 执行一次推送调用：逐条消息渲染 → 绑定渠道并行发送（sync 模式含线程内退避重试）
/// → 逐渠道写终态日志（async 首发失败转 入队）→ 返回结果数组。
/// 返回 (整体状态码, 响应体)；部分失败仍是 200 + results（语义同设计 §5 的逐渠道结果）。
fn do_push(state: &Arc<AppState>, task: &Task, items: &[SendItem]) -> Resp {
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
            // 发送前确定性失败：不重试（提案 D1 边界）
            results.push(json!({"msg": msg_idx, "template": item.template_ref, "status": "failed",
                                "retries": 0, "error": "message exceeds 64KB"}));
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
                                "retries": 0, "error": "no channels bound"}));
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
                                                    title, content_brief, status, via_proxy, error, cost_ms, retries)
                             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, 'failed', 0, 'channel missing/disabled', 0, 0)",
                            params![task.id, tpl_id, batch_id, cid, msg.title, brief],
                        )
                        .unwrap();
                    }
                    results.push(json!({"msg": msg_idx, "template": item.template_ref,
                                        "channel": format!("#{cid}"), "status": "failed",
                                        "retries": 0, "error": "channel missing/disabled"}));
                }
            }
        }

        // 每渠道一个短命线程并行发送（sync 模式线程内指数退避重试），join 后统一写终态日志
        //（耗时 = 最慢渠道；async 模式只做首发，失败由外层入队）
        let handles: Vec<_> = chans
            .into_iter()
            .map(|(cid, cname, ctype, cfg)| {
                let msg = OutMessage { title: msg.title.clone(), body: msg.body.clone() };
                let retry = task.retry.clone();
                std::thread::spawn(move || {
                    let started = Instant::now();
                    let mut receipt = channels::send(&ctype, &cfg, &msg); // 首发尝试
                    let mut retries: u32 = 0;
                    if !receipt.ok && retry.active() && !retry.is_async() {
                        for n in 1..=retry.max {
                            std::thread::sleep(Duration::from_millis(backoff_ms(retry.base_ms, n)));
                            receipt = channels::send(&ctype, &cfg, &msg);
                            retries = n;
                            if receipt.ok {
                                break;
                            }
                        }
                    }
                    receipt.cost_ms = started.elapsed().as_millis() as u64; // 全程累计耗时
                    (cid, cname, msg, receipt, retries)
                })
            })
            .collect();

        for handle in handles {
            let (cid, cname, msg, receipt, retries) = handle.join().expect("channel thread");

            // async 模式首发失败 → 入队，终态日志与最终 retries 由 drain 写（提案 §5.1）
            if !receipt.ok && task.retry.active() && task.retry.is_async() {
                enqueue_retry(state, task.id, tpl_id, cid, &msg, receipt.error.clone(), &task.retry);
                results.push(json!({"msg": msg_idx, "template": item.template_ref,
                                    "channel": cname, "status": "retrying", "retries": 0,
                                    "error": receipt.error, "cost_ms": receipt.cost_ms}));
                continue;
            }

            let brief: String = msg.body.chars().take(80).collect();
            let conn = state.db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO push_logs (user_id, task_id, template_id, batch_id, channel_id,
                                        title, content_brief, status, via_proxy, error, cost_ms, retries)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                    retries as i64,
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
                "retries": retries,
                "error": receipt.error,
                "cost_ms": receipt.cost_ms,
            }));
        }
    }

    maybe_cleanup(state); // 懒触发日志清理（无常驻后台线程，设计 §3）
    trigger_drain(state); // 懒触发异步重试 drain（提案 §5.3）
    let ok = results.iter().all(|r| r["status"] == "ok");
    (200, json!({"ok": ok, "batch_id": batch_id, "results": results}), None)
}

// ---------------------------------------------------------------------------
// 异步重试：入队 + 懒触发 drain（临时短命线程，无常驻后台线程，设计 §3；提案 §5）
// ---------------------------------------------------------------------------

/// 首发失败入队：完整渲染文本 + 配置快照；下次尝试时刻 = now + backoff(1)。
/// 注意：锁只在单条 INSERT 的短作用域内，绝不与发送等长操作嵌套。
fn enqueue_retry(
    state: &AppState,
    task_id: i64,
    template_id: i64,
    channel_id: i64,
    msg: &OutMessage,
    error: Option<String>,
    retry: &RetryCfg,
) {
    let conn = state.db.0.lock().unwrap();
    conn.execute(
        "INSERT INTO retry_queue (task_id, template_id, channel_id, title, body,
                                  attempts, max_retries, base_ms, next_retry_at, last_error)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, datetime('now', ?8), ?9)",
        params![
            task_id,
            template_id,
            channel_id,
            msg.title,
            msg.body,
            retry.max as i64,
            retry.base_ms as i64,
            ms_modifier(backoff_ms(retry.base_ms, 1)),
            error,
        ],
    )
    .expect("enqueue retry");
}

fn drain_inflight() -> &'static AtomicBool {
    static INFLIGHT: AtomicBool = AtomicBool::new(false);
    &INFLIGHT
}

/// 懒触发 drain：已有 drain 在跑则直接返回；否则 spawn 临时线程执行，请求零阻塞。
/// 触发点：do_push 收尾、GET /api/messages、GET /api/overview（提案 §5.3）。
pub fn trigger_drain(state: &Arc<AppState>) {
    if drain_inflight().swap(true, Ordering::SeqCst) {
        return;
    }
    let st = Arc::clone(state);
    std::thread::spawn(move || {
        drain(&st);
        drain_inflight().store(false, Ordering::SeqCst);
    });
}

/// 队列到期行（JOIN tasks 过滤禁用任务：禁用是可逆的，重新启用后继续重试）
struct DueRow {
    id: i64,
    task_id: i64,
    template_id: i64,
    channel_id: i64,
    title: String,
    body: String,
    attempts: i64,
    max_retries: i64,
    base_ms: i64,
    next_retry_at: String,
}

/// 终态出队 + 写 push_logs（单锁内完成）。异步终态各自独立成批：batch_id 回填为自身 id（提案 D9）。
fn finalize_retry(
    state: &AppState,
    row_id: i64,
    task_id: i64,
    template_id: i64,
    channel_id: i64,
    title: &str,
    body: &str,
    ok: bool,
    via_proxy: bool,
    error: Option<String>,
    cost_ms: u64,
    retries: i64,
) {
    let conn = state.db.0.lock().unwrap();
    conn.execute("DELETE FROM retry_queue WHERE id = ?1", params![row_id])
        .expect("dequeue retry");
    let brief: String = body.chars().take(80).collect();
    conn.execute(
        "INSERT INTO push_logs (user_id, task_id, template_id, batch_id, channel_id,
                                title, content_brief, status, via_proxy, error, cost_ms, retries)
         VALUES (1, ?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            task_id,
            template_id,
            channel_id,
            title,
            brief,
            if ok { "ok" } else { "failed" },
            via_proxy as i64,
            error,
            cost_ms as i64,
            retries,
        ],
    )
    .expect("insert final push_log");
    let id = conn.last_insert_rowid();
    conn.execute("UPDATE push_logs SET batch_id = ?1 WHERE id = ?1", params![id])
        .expect("backfill batch_id");
}

/// drain 主体：过期清扫 → 到期行逐条（渠道缺失终态 → 乐观占位 → 锁外发送 → 终态/续期）。
/// 在临时线程中运行，单次至多 DRAIN_BATCH 条（提案 §5.3）。
fn drain(state: &Arc<AppState>) {
    // 1) 过期清扫：滞留超窗的条目直接终态失败——内容已过时，不再发送。
    //    不看任务启停：陈旧规则优先于"禁用保留"（发了也是噪音告警）。
    let expired: Vec<(i64, i64, i64, i64, String, String)> = {
        let conn = state.db.0.lock().unwrap();
        conn.prepare(
            "SELECT id, task_id, template_id, channel_id, title, body FROM retry_queue
             WHERE created_at < datetime('now', ?1)",
        )
        .and_then(|mut stmt| {
            let rows: Vec<_> = stmt
                .query_map(params![format!("-{RETRY_STALE_SECS} seconds")], |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    ))
                })?
                .flatten()
                .collect();
            Ok(rows)
        })
        .unwrap_or_default()
    };
    for (id, task_id, template_id, channel_id, title, body) in expired {
        finalize_retry(state, id, task_id, template_id, channel_id, &title, &body,
                       false, false, Some("retry expired".into()), 0, 0);
        log::info!("retry {id}: expired (stale > {RETRY_STALE_SECS}s)");
    }

    // 2) 到期行（任务启用；禁用任务的行保留）
    let due: Vec<DueRow> = {
        let conn = state.db.0.lock().unwrap();
        conn.prepare(
            "SELECT q.id, q.task_id, q.template_id, q.channel_id, q.title, q.body,
                    q.attempts, q.max_retries, q.base_ms, q.next_retry_at
             FROM retry_queue q JOIN tasks t ON t.id = q.task_id
             WHERE q.next_retry_at <= datetime('now') AND t.enabled = 1
             ORDER BY q.next_retry_at LIMIT ?1",
        )
        .and_then(|mut stmt| {
            let rows: Vec<DueRow> = stmt
                .query_map(params![DRAIN_BATCH as i64], |r| {
                    Ok(DueRow {
                        id: r.get(0)?,
                        task_id: r.get(1)?,
                        template_id: r.get(2)?,
                        channel_id: r.get(3)?,
                        title: r.get(4)?,
                        body: r.get(5)?,
                        attempts: r.get(6)?,
                        max_retries: r.get(7)?,
                        base_ms: r.get(8)?,
                        next_retry_at: r.get(9)?,
                    })
                })?
                .flatten()
                .collect();
            Ok(rows)
        })
        .unwrap_or_default()
    };

    for row in due {
        // 渠道缺失/停用：确定性失败，终态出队（重试也不会好）
        let chan = {
            let conn = state.db.0.lock().unwrap();
            conn.query_row(
                "SELECT type, config_json FROM channels WHERE id = ?1 AND enabled = 1",
                params![row.channel_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .ok()
        };
        let Some((ctype, cfg)) = chan else {
            finalize_retry(state, row.id, row.task_id, row.template_id, row.channel_id,
                           &row.title, &row.body, false, false,
                           Some("channel missing/disabled".into()), 0, row.attempts - 1);
            continue;
        };

        // 乐观占位：锁内把 next_retry_at 推进到"本次再失败时"的下一退避时刻；
        // rows_changed=0 说明已被并发 drain 领取，跳过防双发。
        let claimed = {
            let wait = ms_modifier(backoff_ms(row.base_ms as u64, (row.attempts + 1) as u32));
            let conn = state.db.0.lock().unwrap();
            conn.execute(
                "UPDATE retry_queue SET next_retry_at = datetime('now', ?1)
                 WHERE id = ?2 AND next_retry_at = ?3",
                params![wait, row.id, row.next_retry_at],
            )
            .unwrap()
                == 1
        };
        if !claimed {
            continue;
        }

        // 锁外真实发送（用渠道最新配置）
        let msg = OutMessage { title: row.title.clone(), body: row.body.clone() };
        let receipt = channels::send(&ctype, &serde_json::from_str(&cfg).unwrap_or(json!({})), &msg);
        let attempts_now = row.attempts + 1;
        if receipt.ok {
            finalize_retry(state, row.id, row.task_id, row.template_id, row.channel_id,
                           &row.title, &row.body, true, receipt.via_proxy, None,
                           receipt.cost_ms, attempts_now - 1);
            log::info!("retry {}: ok after {attempts_now} attempts", row.id);
        } else if attempts_now > row.max_retries {
            // 耗尽：attempts_now = max_retries + 1（首发 + max 次重试）
            finalize_retry(state, row.id, row.task_id, row.template_id, row.channel_id,
                           &row.title, &row.body, false, receipt.via_proxy,
                           receipt.error.clone(), receipt.cost_ms, row.max_retries);
            log::warn!("retry {}: exhausted after {attempts_now} attempts", row.id);
        } else {
            // 占位已推进 next_retry_at，这里只更新计数与最近错误
            let conn = state.db.0.lock().unwrap();
            conn.execute(
                "UPDATE retry_queue SET attempts = ?1, last_error = ?2 WHERE id = ?3",
                params![attempts_now, receipt.error, row.id],
            )
            .expect("update retry attempts");
        }
    }
}

// ---------------------------------------------------------------------------
// 两个 HTTP 入口
// ---------------------------------------------------------------------------

pub fn api_push(state: &Arc<AppState>, ctx: &ReqCtx) -> Resp {
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
pub fn get_push(state: &Arc<AppState>, ctx: &ReqCtx) -> Resp {
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

// ---------------------------------------------------------------------------
// 单元测试：退避序列 / 参数校验 / drain 路径（零出网：渠道类型用非法值即失败）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static DB_SEQ: AtomicU64 = AtomicU64::new(0);

    /// 每个测试独立临时库（并行测试不互抢文件）
    fn test_state() -> Arc<AppState> {
        let path = std::env::temp_dir().join(format!(
            "hb-retry-test-{}-{}.db",
            std::process::id(),
            DB_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        Arc::new(AppState {
            db: crate::db::init(path.to_str().unwrap()),
            lockout: crate::auth::Lockout::default(),
            cfg: crate::config::Config {
                db_path: path.to_str().unwrap().into(),
                bind_addr: String::new(),
            },
        })
    }

    /// 种子数据：任务 + 一个 type 非法的渠道（发送即刻失败、零出网）
    fn seed(state: &AppState) -> (i64, i64) {
        let conn = state.db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, name, token_hash, token_prefix) VALUES (1, 't', 'h', 'hb')",
            [],
        )
        .unwrap();
        let task_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO channels (user_id, type, name, config_json) VALUES (1, 'nope', 'c', '{}')",
            [],
        )
        .unwrap();
        (task_id, conn.last_insert_rowid())
    }

    fn enqueue(state: &AppState, task_id: i64, channel_id: i64, attempts: i64, max: i64) -> i64 {
        let conn = state.db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO retry_queue (task_id, template_id, channel_id, title, body,
                                      attempts, max_retries, base_ms, next_retry_at)
             VALUES (?1, 1, ?2, 't', 'b', ?3, ?4, 100, datetime('now', '-1 second'))",
            params![task_id, channel_id, attempts, max],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn queue_count(state: &AppState) -> i64 {
        state.db.0.lock().unwrap()
            .query_row("SELECT COUNT(*) FROM retry_queue", [], |r| r.get(0))
            .unwrap()
    }

    /// (status, retries, error) 全量终态日志
    fn logs(state: &AppState) -> Vec<(String, i64, Option<String>)> {
        let conn = state.db.0.lock().unwrap();
        let out: Vec<_> = conn
            .prepare("SELECT status, retries, error FROM push_logs")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .flatten()
            .collect();
        out
    }

    #[test]
    fn backoff_sequence() {
        assert_eq!(backoff_ms(1000, 1), 1000);
        assert_eq!(backoff_ms(1000, 2), 2000);
        assert_eq!(backoff_ms(1000, 3), 4000);
        assert_eq!(backoff_ms(1000, 10), 512_000); // max=10 的第 10 次：1000 × 2^9
        assert_eq!(backoff_ms(100, 1), 100); // base 下界
        assert_eq!(backoff_ms(60_000, 10), 30_720_000); // base 上界 × 2^9，u64 内
    }

    #[test]
    fn validate_retry_bounds() {
        // 空对象 → 全默认（表单可省略字段）；按 Value 比较规避序列化键序差异
        let s = validate_retry(&json!({})).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&s).unwrap(),
            serde_json::from_str::<Value>(RETRY_DEFAULT_JSON).unwrap()
        );
        // 完整合法对象原样规范化
        assert!(validate_retry(&json!({"enabled": true, "max": 5, "mode": "async", "base_ms": 2000}))
            .unwrap()
            .contains("\"max\":5"));
        // 越界 / 非法值 fail fast
        assert!(validate_retry(&json!({"max": 11})).unwrap_err().contains("0..=10"));
        assert!(validate_retry(&json!({"max": -1})).unwrap_err().contains("0..=10"));
        assert!(validate_retry(&json!({"base_ms": 50})).unwrap_err().contains("100..=60000"));
        assert!(validate_retry(&json!({"base_ms": 60_001})).unwrap_err().contains("100..=60000"));
        assert!(validate_retry(&json!({"mode": "later"})).unwrap_err().contains("sync or async"));
        assert!(validate_retry(&json!("x")).is_err()); // 非对象
    }

    #[test]
    fn retry_cfg_from_json_fallback() {
        let cfg = RetryCfg::from_json("not json");
        assert!(cfg.active() && !cfg.is_async() && cfg.max == 3); // 坏数据 → 默认
        let cfg = RetryCfg::from_json("{\"max\":0}");
        assert!(!cfg.active()); // max=0 = 关闭
    }

    #[test]
    fn drain_exhausts_and_logs_final() {
        let state = test_state();
        let (task_id, cid) = seed(&state);
        enqueue(&state, task_id, cid, 1, 1); // 首发已失败(attempts=1)，max=1 → 本次即最后一次
        drain(&state);
        assert_eq!(queue_count(&state), 0);
        let rows = logs(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "failed");
        assert_eq!(rows[0].1, 1); // retries = 1
        assert!(rows[0].2.as_deref().unwrap().contains("unknown channel type"));
    }

    #[test]
    fn drain_continues_when_quota_left() {
        let state = test_state();
        let (task_id, cid) = seed(&state);
        let id = enqueue(&state, task_id, cid, 1, 3); // 还有余量 → 续期而非终态
        drain(&state);
        assert_eq!(queue_count(&state), 1); // 仍在队列
        let (attempts,): (i64,) = state.db.0.lock().unwrap()
            .query_row("SELECT attempts FROM retry_queue WHERE id = ?1", params![id], |r| {
                Ok((r.get(0)?,))
            })
            .unwrap();
        assert_eq!(attempts, 2);
        assert!(logs(&state).is_empty()); // 未终态不写日志（无重复计数）
    }

    #[test]
    fn drain_expires_stale_rows() {
        let state = test_state();
        let (task_id, cid) = seed(&state);
        // 伪造 2 小时前入队
        state.db.0.lock().unwrap()
            .execute(
                "INSERT INTO retry_queue (task_id, template_id, channel_id, title, body,
                                          attempts, max_retries, base_ms, next_retry_at, created_at)
                 VALUES (?1, 1, ?2, 't', 'b', 1, 3, 100, datetime('now', '-2 hours'),
                         datetime('now', '-2 hours'))",
                params![task_id, cid],
            )
            .unwrap();
        drain(&state);
        assert_eq!(queue_count(&state), 0);
        let rows = logs(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "failed");
        assert_eq!(rows[0].2.as_deref(), Some("retry expired")); // 过期不发送
    }

    #[test]
    fn drain_skips_disabled_task() {
        let state = test_state();
        let (task_id, cid) = seed(&state);
        enqueue(&state, task_id, cid, 1, 3);
        state.db.0.lock().unwrap()
            .execute("UPDATE tasks SET enabled = 0 WHERE id = ?1", params![task_id])
            .unwrap();
        drain(&state);
        assert_eq!(queue_count(&state), 1); // 禁用 → 跳过保留
        assert!(logs(&state).is_empty());
    }

    #[test]
    fn drain_missing_channel_finalizes() {
        let state = test_state();
        let (task_id, cid) = seed(&state);
        enqueue(&state, task_id, cid, 1, 3);
        state.db.0.lock().unwrap()
            .execute("DELETE FROM channels WHERE id = ?1", params![cid])
            .unwrap();
        drain(&state);
        assert_eq!(queue_count(&state), 0);
        let rows = logs(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2.as_deref(), Some("channel missing/disabled"));
    }
}
