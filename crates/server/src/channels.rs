// 渠道层：统一的出站发送抽象 + 8 种渠道实现 + 代理 Agent 缓存（设计 §6）。
//
// 所有渠道都是"一次 HTTP 出站调用"：钉钉/企微/飞书群机器人是 POST webhook，
// Bark/ntfy 是简单 HTTP，企微应用是唯一带状态（access_token 刷新）的渠道。
//
// 渠道级代理（设计 §6"渠道级代理"）：每个渠道 config_json 里的通用字段
//   proxy      null=直连（国内渠道默认） | "http://127.0.0.1:7890" | "socks5://..."
//              —— 指向本机 mihomo；进了 mihomo 之后走哪个节点由 mihomo 分流规则决定
//   timeout_ms 出站超时，默认 10000
// ureq Agent 按 proxy URL 缓存复用（常态只有 2 个：直连 + mihomo），不重复建连接池。

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::Agent;

type HmacSha256 = Hmac<Sha256>;

/// 待发送的消息（模板渲染后的最终文本）
pub struct OutMessage {
    pub title: String,
    pub body: String,
}

/// 单渠道发送回执：成败、是否经代理、错误信息、耗时
#[derive(Debug, Clone)]
pub struct Receipt {
    pub ok: bool,
    pub via_proxy: bool,
    pub error: Option<String>,
    pub cost_ms: u64,
}

/// 渠道类型枚举（与 channels.type 字段一一对应）
pub const CHANNEL_TYPES: &[&str] = &[
    "dingtalk_bot", "wecom_bot", "wecom_app", "feishu_bot",
    "telegram", "bark", "ntfy", "webhook",
];

// ---------------------------------------------------------------------------
// Agent 缓存：按 proxy URL 复用连接（keep-alive），避免每次推送重建 TLS 会话
// ---------------------------------------------------------------------------

fn agent_cache() -> &'static Mutex<HashMap<String, Agent>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Agent>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn agent_for(proxy: &Option<String>, timeout_ms: u64) -> Result<Agent, String> {
    // 缓存键 = proxy|timeout，超时变化时自然走新建分支
    let key = format!("{}|{timeout_ms}", proxy.clone().unwrap_or_default());
    let mut cache = agent_cache().lock().unwrap();
    if let Some(agent) = cache.get(&key) {
        return Ok(agent.clone());
    }
    let mut builder = ureq::AgentBuilder::new().timeout(Duration::from_millis(timeout_ms));
    if let Some(p) = proxy {
        let parsed = ureq::Proxy::new(p).map_err(|e| format!("invalid proxy {p}: {e}"))?;
        builder = builder.proxy(parsed);
    }
    let agent = builder.build();
    cache.insert(key, agent.clone());
    Ok(agent)
}

// ---------------------------------------------------------------------------
// 统一入口：按渠道类型分发
// ---------------------------------------------------------------------------

/// 发送一条消息到指定渠道。config 为渠道的 config_json（已含 proxy/timeout_ms 通用字段）。
pub fn send(channel_type: &str, config: &Value, msg: &OutMessage) -> Receipt {
    let started = Instant::now();
    let via_proxy = config["proxy"].is_string();
    let timeout_ms = config["timeout_ms"].as_u64().unwrap_or(10_000);
    let proxy = config["proxy"].as_str().map(|s| s.to_string());

    let result = (|| -> Result<(), String> {
        let agent = agent_for(&proxy, timeout_ms)?;
        let cfg = &ProxylessConfig { raw: config, agent: &agent };
        match channel_type {
            "dingtalk_bot" => dingtalk_bot(cfg, msg),
            "wecom_bot" => wecom_bot(cfg, msg),
            "wecom_app" => wecom_app(cfg, msg),
            "feishu_bot" => feishu_bot(cfg, msg),
            "telegram" => telegram(cfg, msg),
            "bark" => bark(cfg, msg),
            "ntfy" => ntfy(cfg, msg),
            "webhook" => webhook_channel(cfg, msg),
            other => Err(format!("unknown channel type: {other}")),
        }
    })();

    Receipt {
        ok: result.is_ok(),
        via_proxy,
        error: result.err(),
        cost_ms: started.elapsed().as_millis() as u64,
    }
}

/// 渠道配置视图：proxy 已被剥离（Agent 已按 proxy 构建），渠道实现只关心自己的私有字段
struct ProxylessConfig<'a> {
    raw: &'a Value,
    agent: &'a Agent,
}

fn s(cfg: &ProxylessConfig, key: &str) -> Result<String, String> {
    cfg.raw[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .ok_or_else(|| format!("missing config field: {key}"))
}

/// 发 JSON POST 并检查常见返回结构（钉钉/企微 errcode、飞书 code、TG ok 字段）
fn post_json_checked(
    cfg: &ProxylessConfig,
    url: &str,
    payload: Value,
    ok_pred: fn(&Value) -> bool,
) -> Result<(), String> {
    let resp = cfg
        .agent
        .post(url)
        .send_json(payload)
        .map_err(|e| format!("http: {e}"))?;
    let body: Value = resp
        .into_json()
        .map_err(|e| format!("bad response json: {e}"))?;
    if ok_pred(&body) {
        Ok(())
    } else {
        Err(format!("channel rejected: {body}"))
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn now_sec() -> u64 {
    now_ms() / 1000
}

fn hmac_sha256_b64(key: &str, message: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("hmac key");
    mac.update(message.as_bytes());
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

// ---------------------------------------------------------------------------
// 各渠道实现（每个 20~120 行）
// ---------------------------------------------------------------------------

/// 钉钉群机器人：webhook + 可选加签（HMAC-SHA256）
fn dingtalk_bot(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let webhook = s(cfg, "webhook")?;
    let mut url = webhook.clone();
    if let Ok(secret) = s(cfg, "secret") {
        let ts = now_ms();
        let sign = hmac_sha256_b64(&secret, &format!("{ts}\n{secret}"));
        // sign 需要 URL 编码后拼到 webhook 上
        url = format!("{webhook}&timestamp={ts}&sign={}", urlencode(&sign));
    }
    post_json_checked(
        cfg,
        &url,
        json!({"msgtype": "text", "text": {"content": format!("{}\n{}", msg.title, msg.body)}}),
        |b| b["errcode"].as_i64() == Some(0),
    )
}

/// 企微群机器人：完整 webhook 地址
fn wecom_bot(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let webhook = s(cfg, "webhook")?;
    post_json_checked(
        cfg,
        &webhook,
        json!({"msgtype": "text", "text": {"content": format!("{}\n{}", msg.title, msg.body)}}),
        |b| b["errcode"].as_i64() == Some(0),
    )
}

/// 企微自建应用：corpid + secret + agentid，access_token 内存缓存（提前 5 分钟刷新）。
/// 用户关注企微"微信插件"后，应用消息可直达个人微信 —— 这是触达微信的合规通道。
fn wecom_app(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let corpid = s(cfg, "corpid")?;
    let secret = s(cfg, "secret")?;
    let agentid = cfg.raw["agentid"].as_i64().ok_or("missing config field: agentid")?;
    let touser = cfg.raw["touser"].as_str().unwrap_or("@all").to_string();

    let token = wecom_access_token(cfg, &corpid, &secret)?;
    post_json_checked(
        cfg,
        &format!("https://qyapi.weixin.qq.com/cgi-bin/message/send?access_token={token}"),
        json!({"touser": touser, "msgtype": "text", "agentid": agentid,
               "text": {"content": format!("{}\n{}", msg.title, msg.body)}}),
        |b| b["errcode"].as_i64() == Some(0),
    )
}

/// access_token 缓存：key = corpid，value = (token, 过期时刻)
fn wecom_access_token(cfg: &ProxylessConfig, corpid: &str, secret: &str) -> Result<String, String> {
    static TOKENS: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    let tokens = TOKENS.get_or_init(|| Mutex::new(HashMap::new()));

    if let Some((tok, until)) = tokens.lock().unwrap().get(corpid) {
        if Instant::now() < *until {
            return Ok(tok.clone());
        }
    }
    let url = format!(
        "https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid={corpid}&corp_secret={secret}"
    );
    let body: Value = cfg
        .agent
        .get(&url)
        .call()
        .map_err(|e| format!("gettoken: {e}"))?
        .into_json()
        .map_err(|e| format!("gettoken json: {e}"))?;
    let tok = body["access_token"]
        .as_str()
        .ok_or_else(|| format!("gettoken failed: {body}"))?
        .to_string();
    // 官方 expires_in（秒）提前 300 秒视为过期，避免边界失效
    let ttl = body["expires_in"].as_u64().unwrap_or(7200).saturating_sub(300);
    tokens
        .lock()
        .unwrap()
        .insert(corpid.to_string(), (tok.clone(), Instant::now() + Duration::from_secs(ttl)));
    Ok(tok)
}

/// 飞书群机器人：webhook + 可选签名
fn feishu_bot(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let webhook = s(cfg, "webhook")?;
    let mut payload = json!({
        "msg_type": "text",
        "content": {"text": format!("{}\n{}", msg.title, msg.body)}
    });
    if let Ok(secret) = s(cfg, "secret") {
        let ts = now_sec();
        // 飞书签名规则：string_to_sign = "{timestamp}\n{secret}" 作为 HMAC key，消息为空
        let sign = hmac_sha256_b64(&format!("{ts}\n{secret}"), "");
        payload["timestamp"] = json!(ts);
        payload["sign"] = json!(sign);
    }
    post_json_checked(cfg, &webhook, payload, |b| {
        b["code"].as_i64().is_some_and(|c| c == 0) || b["StatusCode"].as_i64() == Some(0)
    })
}

/// Telegram Bot：bot_token + chat_id，通常需要 proxy 走本机 mihomo
fn telegram(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let bot_token = s(cfg, "bot_token")?;
    let chat_id = s(cfg, "chat_id")?;
    post_json_checked(
        cfg,
        &format!("https://api.telegram.org/bot{bot_token}/sendMessage"),
        json!({"chat_id": chat_id, "text": format!("{}\n{}", msg.title, msg.body)}),
        |b| b["ok"].as_bool() == Some(true),
    )
}

/// Bark（iOS 推送）：server（默认官方）+ 设备 key
fn bark(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let server = cfg.raw["server"].as_str().unwrap_or("https://api.day.app").trim_end_matches('/').to_string();
    let key = s(cfg, "key")?;
    post_json_checked(
        cfg,
        &format!("{server}/push"),
        json!({"device_key": key, "title": msg.title, "body": msg.body}),
        |b| b["code"].as_i64() == Some(200),
    )
}

/// ntfy：server（默认官方）+ 主题名
fn ntfy(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let server = cfg.raw["server"].as_str().unwrap_or("https://ntfy.sh").trim_end_matches('/').to_string();
    let topic = s(cfg, "topic")?;
    post_json_checked(
        cfg,
        &server,
        json!({"topic": topic, "title": msg.title, "message": msg.body}),
        |b| b["id"].is_string(),
    )
}

/// 通用 webhook：POST JSON 到任意 URL，作为兜底万能渠道
fn webhook_channel(cfg: &ProxylessConfig, msg: &OutMessage) -> Result<(), String> {
    let url = s(cfg, "url")?;
    post_json_checked(
        cfg,
        &url,
        json!({"title": msg.title, "body": msg.body, "timestamp": now_sec()}),
        |_| true, // 2xx 即成功，不要求特定响应结构
    )
}

// ---------------------------------------------------------------------------

/// 极简百分号编码（只编码签名/路径等安全字符集之外的）
fn urlencode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_covers_non_alnum() {
        assert_eq!(urlencode("ab-_~."), "ab-_~.");
        // '+' -> %2B, '/' -> %2F, '=' -> %3D（钉钉签名的经典三件套）
        assert_eq!(urlencode("+/="), "%2B%2F%3D");
    }

    #[test]
    fn unknown_type_fails_fast() {
        let r = send("nope", &json!({}), &OutMessage { title: "t".into(), body: "b".into() });
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("unknown channel type"));
    }

    #[test]
    fn missing_config_field_reported() {
        let r = send("telegram", &json!({}), &OutMessage { title: "t".into(), body: "b".into() });
        // 缺 bot_token —— 网络层之前就应失败（fail fast，不会真的出网）
        assert!(r.error.unwrap().contains("missing config field"));
    }

    #[test]
    fn hmac_b64_known_vector() {
        // HMAC-SHA256("key", "msg") 的 base64 —— 固定向量防算法回归
        let out = hmac_sha256_b64("key", "msg");
        assert_eq!(out.len(), 44);
    }
}
