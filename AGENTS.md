# AGENTS.md（蜂鸟工作区）

极致省内存的多渠道消息推送网关（Rust workspace，单二进制内嵌 Web 管理台），目标设备：树莓派 3B+（1 GB）。

## 常用命令

```bash
cargo build --release        # 后端。web/dist 已入库，无需 npm 即可构建
cargo test                   # 单元测试（渲染器/渠道校验/HMAC/URL 编码）
bash tests/e2e.sh            # 端到端：起服务+webhook 接收器，44 项断言（依赖 cargo/curl/python3/lsof，macOS/Linux 向）
cd web && npm run build      # 仅改前端时；产物输出 web/dist，经 rust-embed 内嵌进二进制
```

运行：`./target/release/hummingbird`（默认 `0.0.0.0:8080`、`./hummingbird.db`；环境变量 `HB_PORT` / `HB_DB_PATH`）；CLI 子命令 `reset-password`。

## 架构铁律（改动前必读）

- **同步栈，永不引入 async 运行时**：tiny_http + std 线程 + ureq + rusqlite。加 tokio/async-hyper 一类的依赖即违反核心设计。
- **内存预算是硬指标**：常态空闲 RSS < 8 MB、二进制 < 15 MB（实测 7.5 MB / 3.0 MB）。release profile 已是 opt-level="z" + fat LTO + panic=abort；新增依赖必须对照预算论证。chrono 关 default-features、仅 UTC。
- **SQLite 单连接 + Mutex**（db.rs），无连接池；无后台常驻线程，日志清理走懒触发。
- **路由集中在 main.rs dispatch**：URL 解析统一在这一层，handler 只收解析好的参数。端点顺序：公开端点 → 任务 token 推送 → session 守卫的管理 API → 内嵌静态前端。
- **任务模型安全不变量**：任务 = token（1:1）+ "模板 → 渠道组"绑定；调用方只能传模板和变量、**不能指定渠道**。token 仅存 SHA-256 哈希；渠道 secret 在接口响应中脱敏。

## 约定

- 代码注释与文档用中文；注释中的"设计 §x"指向 `docs/自研消息网关设计文档.md`（v0.8，**改 auth / push / channels 等敏感区域前先读对应章节**）。
- 前端是单个 `web/src/App.vue` 的 Vue3（纯 JS，无 router/pinia），刻意保持极简。
- `web/dist` 构建产物**有意入库**（保证 cargo build 开箱即用）：改前端后需重新构建并连同 dist 一起提交，不要把它加进 .gitignore。`*.db` 运行期数据绝不入库。
- README 为中英双语（README.md / README.en.md），更新时两份保持同步。
- 尚未 git init、无 CI；LICENSE 为 MIT（版权人 canyuda）。
