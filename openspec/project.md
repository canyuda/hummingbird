# 蜂鸟（Hummingbird）· OpenSpec 项目说明

> 极致省内存的多渠道消息推送网关（Rust workspace，单二进制内嵌 Web 管理台），目标设备：树莓派 3B+（1GB RAM）。

## 项目目标

自托管消息推送网关：单二进制 + SQLite，多渠道出站（钉钉/企微/飞书/Telegram/Bark/ntfy/webhook，
渠道级代理），任务 token 模型的外部推送 API，内嵌 Vue3 管理台。核心硬指标：空闲 RSS < 8MB、
二进制 < 15MB、冷启动 < 1s。

## 架构铁律（所有变更必须遵守）

- **同步栈**：tiny_http + std 线程 + ureq + rusqlite，永不引入 async 运行时。
- **SQLite 单连接 + Mutex**；无常驻后台线程（允许"临时 spawn 完成即回收"的短命线程）。
- 路由集中在 main.rs dispatch；URL 解析在路由层完成。
- 任务模型安全不变量：调用方只能传模板+变量，不能指定渠道；token 仅存 SHA-256 哈希。
- 新增依赖必须对照内存/体积预算论证，默认零新增。

## 文档体系

- 详细架构：《docs/自研消息网关设计文档.md》（v0.9+，修订历史在文档头）。
- 变更流程：`openspec/changes/<change-id>/`（proposal → design → tasks → spec deltas）。

## 规格基线

- `push-retry`：已建立（源自变更 `add-push-retry`，2026-11-16 归档）。
- 其余能力尚未反向抽取基线；后续变更落地后按 delta 合并进 `openspec/specs/` 逐步补齐。
