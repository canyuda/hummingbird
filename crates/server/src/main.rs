// 蜂鸟（Hummingbird）—— 极致省内存的多渠道消息推送网关。
// 同步技术栈：tiny_http + std 线程，无 async 运行时（设计文档 §2）。
//
// 路由总入口（dispatch）：URL 解析统一在这一层，handler 只收解析好的参数。
//   公开端点 → 任务 token 推送端点 → session 守卫的管理 API → 内嵌静态前端

mod api_admin;
mod api_auth;
mod auth;
mod channels;
mod config;
mod db;
mod http;
mod push;
mod tmpl;

use crate::api_auth::{err, into_response, Resp};
use crate::config::Config;
use crate::db::Db;
use crate::http::ReqCtx;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;
use tiny_http::{Request, Response};

pub struct AppState {
    pub db: Db,
    pub lockout: auth::Lockout,
    pub cfg: Config,
}

/// 前端构建产物内嵌进二进制（交付物 = 单个可执行文件，设计 §10）
#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist"]
struct WebAssets;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match std::env::args().nth(1).as_deref() {
        None => serve(), // 无参数 = 前台启动服务（设计 §10）
        Some("reset-password") => reset_password(),
        // 未知参数直接报错并打印用法，绝不默默启动（fail fast）
        Some(other) => {
            eprintln!("unknown subcommand: {other}\n\n用法:\n  hummingbird                 # 前台启动服务\n  hummingbird reset-password  # 重置 admin 密码");
            std::process::exit(2);
        }
    }
}

fn serve() {
    let cfg = Config::from_env();
    let state = Arc::new(AppState {
        db: db::init(&cfg.db_path),
        lockout: auth::Lockout::default(),
        cfg: Config::clone(&cfg),
    });
    let server = tiny_http::Server::http(&cfg.bind_addr).expect("bind listen addr");
    log::info!("hummingbird listening on http://{}", cfg.bind_addr);
    // 每连接一个短命线程；无常驻后台线程（设计 §3 线程模型）
    for request in server.incoming_requests() {
        let st = Arc::clone(&state);
        std::thread::spawn(move || route(request, st));
    }
}

/// 防爆破的 admin 密码重置（设计 §10）：新随机密码打印到 stdout，旧密码即刻失效
fn reset_password() {
    let cfg = Config::from_env();
    let db = db::init(&cfg.db_path);
    let password = auth::gen_random_string(16);
    let hash = auth::hash_password(&password);
    let n = db
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE users SET password_hash = ?1, must_change_password = 1 WHERE username = 'admin'",
            [&hash],
        )
        .expect("reset admin password");
    // 安全：重置密码 = 凭证泄露处置，必须同时吊销所有现存会话
    db.0
        .lock()
        .unwrap()
        .execute("DELETE FROM sessions", [])
        .expect("purge sessions on password reset");
    if n == 0 {
        eprintln!("no admin user found; run serve once to bootstrap");
        std::process::exit(1);
    }
    println!("admin password reset to: {password}");
}

fn route(mut request: Request, state: Arc<AppState>) {
    let ctx = http::take_ctx(&mut request);
    let method = ctx.method.clone();
    let path = ctx.path.clone();
    let started = Instant::now();
    let resp = dispatch(ctx, &state);
    log::info!(
        "{} {} -> {:?} ({}ms)",
        method,
        path,
        resp.status_code(),
        started.elapsed().as_millis()
    );
    let _ = request.respond(resp);
}

/// 路径形如 /api/channels/12/test → 提取 (资源 id, 尾部动作)；动作可空
fn path_id_action<'a>(path: &'a str, prefix: &str) -> Option<(i64, &'a str)> {
    let rest = path.strip_prefix(prefix)?;
    match rest.split_once('/') {
        Some((id, action)) => Some((id.parse().ok()?, action)),
        None => Some((rest.parse().ok()?, "")),
    }
}

fn dispatch(ctx: ReqCtx, state: &Arc<AppState>) -> Response<std::io::Cursor<Vec<u8>>> {
    let ReqCtx { method, path, .. } = &ctx;
    let m = method.as_str();

    // ---- 公开端点（无认证） ----
    match (m, path.as_str()) {
        ("GET", "/api/health") => return into_response((200, json!({"status": "ok"}), None)),
        ("POST", "/api/auth/login") => return into_response(api_auth::login(state, &ctx)),
        ("POST", "/api/auth/logout") => return into_response(api_auth::logout(state, &ctx)),
        _ => {}
    }

    // ---- 外部推送端点（任务 token 认证，设计 §5 v0.8） ----
    if (m == "POST" && path == "/api/push") || (m == "GET" && path.starts_with("/push/")) {
        return into_response(if m == "POST" {
            push::api_push(state, &ctx)
        } else {
            push::get_push(state, &ctx)
        });
    }

    // ---- session 守卫的管理 API ----
    if path.starts_with("/api/") {
        let Some(session) = api_auth::current_session(state, &ctx.session_token) else {
            return into_response(err(401, "unauthorized"));
        };
        if session.must_change_password {
            // 首登改密完成前，只放行 me / password
            return into_response(match (m, path.as_str()) {
                ("GET", "/api/auth/me") => api_auth::me(state, &session),
                ("PUT", "/api/auth/password") => api_auth::change_password(state, &session, &ctx),
                _ => err(403, "password_change_required"),
            });
        }
        return into_response(admin_dispatch(m, path, &ctx, state));
    }

    // ---- 内嵌静态前端（非 /api 的 GET 一律回落到 SPA） ----
    if m == "GET" {
        return serve_static(path);
    }
    into_response(err(404, "not found"))
}

/// 管理端点分发表（已过 session + 首登改密守卫）
fn admin_dispatch(m: &str, path: &str, ctx: &ReqCtx, state: &Arc<AppState>) -> Resp {
    // 渠道
    if path == "/api/channels" {
        return match m {
            "GET" => api_admin::channels_list(state),
            "POST" => api_admin::channels_create(state, ctx),
            _ => err(405, "method not allowed"),
        };
    }
    if let Some((id, action)) = path_id_action(path, "/api/channels/") {
        return match (m, action) {
            ("PUT", "") => api_admin::channels_update(state, id, ctx),
            ("DELETE", "") => api_admin::channels_delete(state, id),
            ("POST", "test") => api_admin::channels_test(state, id),
            _ => err(400, "bad path"),
        };
    }

    // 模板
    if path == "/api/templates" {
        return match m {
            "GET" => api_admin::templates_list(state),
            "POST" => api_admin::templates_create(state, ctx),
            _ => err(405, "method not allowed"),
        };
    }
    if let Some((id, action)) = path_id_action(path, "/api/templates/") {
        return match (m, action) {
            ("PUT", "") => api_admin::templates_update(state, id, ctx),
            ("DELETE", "") => api_admin::templates_delete(state, id),
            ("POST", "render") => api_admin::templates_render(state, id, ctx),
            _ => err(400, "bad path"),
        };
    }

    // 任务（含绑定与 token）
    if path == "/api/tasks" {
        return match m {
            "GET" => api_admin::tasks_list(state),
            "POST" => api_admin::tasks_create(state, ctx),
            _ => err(405, "method not allowed"),
        };
    }
    if let Some((id, action)) = path_id_action(path, "/api/tasks/") {
        return match (m, action) {
            ("PUT", "") => api_admin::tasks_update(state, id, ctx),
            ("DELETE", "") => api_admin::tasks_delete(state, id),
            ("POST", "test") => api_admin::tasks_test(state, id),
            ("POST", "token/regenerate") => api_admin::tasks_regenerate(state, id),
            _ => err(400, "bad path"),
        };
    }

    match (m, path) {
        ("GET", "/api/auth/me") => api_auth::me(state, &api_auth::current_session(state, &ctx.session_token).unwrap()),
        ("GET", "/api/overview") => api_admin::overview(state),
        ("GET", "/api/messages") => api_admin::messages_list(state, ctx),
        ("GET", "/api/settings") => api_admin::settings_get(state),
        ("PUT", "/api/settings") => api_admin::settings_put(state, ctx),
        _ => err(404, "not found"),
    }
}

/// 静态资源：命中内嵌文件返回；否则返回 index.html（SPA 前端路由）
fn serve_static(path: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let rel = path.trim_start_matches('/');
    let file = if rel.is_empty() { "index.html" } else { rel };
    if let Some(embedded) = WebAssets::get(file) {
        return Response::from_data(embedded.data.into_owned()).with_header(http::ctype(mime_of(file)));
    }
    // SPA：未知路径回落 index.html
    match WebAssets::get("index.html") {
        Some(index) => Response::from_data(index.data.into_owned())
            .with_header(http::ctype("text/html; charset=utf-8")),
        None => into_response(err(404, "no embedded frontend")),
    }
}

fn mime_of(file: &str) -> &'static str {
    match file.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}
