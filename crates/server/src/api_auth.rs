// 认证 API：登录 / 登出 / 会话信息 / 改密（设计 §5、§8）。
// 登录事件写进程日志（stdout -> journalctl），绝不入库。

use crate::auth;
use crate::http::{json_resp, set_cookie_header, ReqCtx};
use crate::AppState;
use rusqlite::params;
use serde_json::json;
use tiny_http::{Header, Response};

pub struct Session {
    pub user_id: i64,
    pub username: String,
    pub must_change_password: bool,
}

/// 把 hb_session Cookie 解析为有效会话（过期即失败）。
pub fn current_session(state: &AppState, token: &Option<String>) -> Option<Session> {
    let token = token.as_ref()?;
    let hash = auth::sha256_hex(token);
    let conn = state.db.0.lock().unwrap();
    conn.query_row(
        "SELECT u.id, u.username, u.must_change_password
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ?1 AND s.expires_at > datetime('now')",
        params![hash],
        |r| {
            Ok(Session {
                user_id: r.get(0)?,
                username: r.get(1)?,
                must_change_password: r.get::<_, i64>(2)? != 0,
            })
        },
    )
    .ok()
}

pub fn login(state: &AppState, ctx: &ReqCtx) -> Resp {
    let username = ctx.body["username"].as_str().unwrap_or_default().to_string();
    let password = ctx.body["password"].as_str().unwrap_or_default().to_string();

    if state.lockout.is_locked(&username, &ctx.ip) {
        log::warn!("login rejected (locked): {username} from {}", ctx.ip);
        return (403, json!({"error": "locked, retry in 15 minutes"}), None);
    }

    let row = {
        let conn = state.db.0.lock().unwrap();
        conn.query_row(
            "SELECT id, password_hash, must_change_password FROM users WHERE username = ?1",
            params![username],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .ok()
    };

    let verified = row
        .as_ref()
        .map(|(_, hash, _)| auth::verify_password(&password, hash))
        .unwrap_or(false);

    if !verified {
        state.lockout.record_failure(&username, &ctx.ip);
        log::warn!("login failed: {username} from {} ua={}", ctx.ip, ctx.user_agent);
        // 失败响应统一措辞，不给用户名枚举的信息差。
        return (401, json!({"error": "invalid credentials"}), None);
    }

    state.lockout.reset(&username, &ctx.ip);
    let (user_id, _, must_change) = row.unwrap();

    let token = auth::gen_random_string(32);
    let hash = auth::sha256_hex(&token);
    {
        let conn = state.db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (token_hash, user_id, expires_at, ip, user_agent)
             VALUES (?1, ?2, datetime('now', '+7 days'), ?3, ?4)",
            params![hash, user_id, ctx.ip, ctx.user_agent],
        )
        .expect("insert session");
        conn.execute("UPDATE users SET last_login_at = datetime('now') WHERE id = ?1", params![user_id])
            .expect("update last_login");
    }
    log::info!("login ok: {username} from {}", ctx.ip);

    let cookie = format!(
        "hb_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800"
    );
    (
        200,
        json!({"ok": true, "username": username, "must_change_password": must_change}),
        Some(set_cookie_header(&cookie)),
    )
}

pub fn logout(state: &AppState, ctx: &ReqCtx) -> Resp {
    if let Some(token) = &ctx.session_token {
        let hash = auth::sha256_hex(token);
        state
            .db
            .0
            .lock()
            .unwrap()
            .execute("DELETE FROM sessions WHERE token_hash = ?1", params![hash])
            .expect("delete session");
    }
    (
        200,
        json!({"ok": true}),
        Some(set_cookie_header(
            "hb_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
        )),
    )
}

pub fn me(_state: &AppState, session: &Session) -> Resp {
    (
        200,
        json!({"username": session.username, "must_change_password": session.must_change_password}),
        None,
    )
}

pub fn change_password(state: &AppState, session: &Session, ctx: &ReqCtx) -> Resp {
    let old = ctx.body["old_password"].as_str().unwrap_or_default();
    let new = ctx.body["new_password"].as_str().unwrap_or_default();
    if new.len() < 8 {
        return (400, json!({"error": "new password must be at least 8 chars"}), None);
    }

    let stored: String = {
        let conn = state.db.0.lock().unwrap();
        conn.query_row(
            "SELECT password_hash FROM users WHERE id = ?1",
            params![session.user_id],
            |r| r.get(0),
        )
        .unwrap_or_default()
    };
    if !auth::verify_password(old, &stored) {
        return (401, json!({"error": "invalid credentials"}), None);
    }

    let hash = auth::hash_password(new);
    {
        let conn = state.db.0.lock().unwrap();
        conn.execute(
            "UPDATE users SET password_hash = ?1, must_change_password = 0 WHERE id = ?2",
            params![hash, session.user_id],
        )
        .expect("update password");
        // 改密后吊销除当前外的所有会话。
        conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND token_hash != ?2",
            params![session.user_id, auth::sha256_hex(ctx.session_token.as_deref().unwrap_or(""))],
        )
        .expect("purge sessions");
    }
    log::info!("password changed for {}", session.username);
    (200, json!({"ok": true}), None)
}

pub type Resp = (u16, serde_json::Value, Option<Header>);

pub fn into_response(resp: Resp) -> Response<std::io::Cursor<Vec<u8>>> {
    let (status, body, header) = resp;
    let mut r = json_resp(status, &body);
    if let Some(h) = header {
        r = r.with_header(h);
    }
    r
}

pub fn err(status: u16, msg: &str) -> Resp {
    (status, json!({"error": msg}), None)
}
