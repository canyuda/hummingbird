// HTTP 基础设施：带容量上限的请求体读取、Cookie 解析、JSON 响应封装。

use serde_json::Value;
use std::io::Read;
use tiny_http::{Header, Request, Response};

const MAX_BODY: usize = 2 * 1024 * 1024; // 2MB 上限：覆盖 20 条 x 64KB 的批量请求

pub struct ReqCtx {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: Value,
    pub ip: String,
    pub user_agent: String,
    pub session_token: Option<String>,
    pub bearer_token: Option<String>,
}

/// 把请求消费为上下文对象；请求体存在时解析为 JSON（非法 JSON 记为 Null）。
pub fn take_ctx(req: &mut Request) -> ReqCtx {
    let url = req.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url, String::new()),
    };
    let ip = req
        .remote_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let mut user_agent = String::new();
    let mut session_token = None;
    let mut bearer_token = None;
    for h in req.headers() {
        if h.field.equiv("User-Agent") {
            user_agent = h.value.as_str().to_string();
        } else if h.field.equiv("Cookie") {
            session_token = parse_cookie(h.value.as_str(), "hb_session");
        } else if h.field.equiv("Authorization") {
            let v = h.value.as_str();
            bearer_token = v.strip_prefix("Bearer ").map(|t| t.trim().to_string());
        }
    }
    let mut raw = Vec::new();
    let _ = req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut raw);
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    ReqCtx {
        method: req.method().to_string(),
        path,
        query,
        body,
        ip,
        user_agent,
        session_token,
        bearer_token,
    }
}

fn parse_cookie(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

pub fn json_resp(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(ctype("application/json"))
}

pub fn error_resp(status: u16, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_resp(status, &serde_json::json!({ "error": msg }))
}

pub fn ctype(mime: &str) -> Header {
    Header::from_bytes(&b"Content-Type"[..], mime.as_bytes()).unwrap()
}

pub fn set_cookie_header(value: &str) -> Header {
    Header::from_bytes(&b"Set-Cookie"[..], value.as_bytes()).unwrap()
}
