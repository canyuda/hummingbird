# 变更提案：推送失败自动重试（指数退避，任务级可配）

> change-id: `add-push-retry` · 提出日期 2026-11-16 · 状态：已评审定稿（7 轮 grill 逐项确认），待实现

## Why

网关核心场景是服务器告警推送。渠道偶发抖动（网络超时、代理瞬断、上游 5xx）会丢告警，而调用方
（cron / 监控脚本）只发一次、不会自己重试。网关作为"最后一公里"应承担瞬时失败的自愈责任。

本变更**反转 v0.7/v0.8 设计文档"不内置重试"的裁剪决策**（反转理由与决策全记录见
《docs/推送失败自动重试提案.md》§1-§2）。

## What Changes

- `tasks` 新增 `retry_json` 列：`{"enabled":true,"max":3,"mode":"sync","base_ms":1000}`，
  任务表单可配（次数 0..10、模式 sync/async、退避基数 100..60000ms，越界 400）。
- **同步模式**：渠道发送线程内指数退避（base × 2^(n-1)）重试，响应含最终结果与 `retries`。
- **异步模式**：首发失败写 `retry_queue`（完整渲染文本），懒触发 drain（临时短命线程 +
  AtomicBool 防重复 + 单次上限 20 条 + 乐观占位领取），终态写 `push_logs`。
- `push_logs` 新增 `retries` 列；每条消息×渠道只在终态写一行。
- 队列陈旧策略：滞留 >1h 判 `retry expired` 终态失败；任务禁用跳过；任务删除级联清队。
- API：任务 CRUD 接受/回传 `retry`；`/api/messages` 行增 `retries`；`/api/overview` 增
  `pending_retries`（messages/overview 兼作 drain 触发点）。
- 前端：任务表单"失败重试"区块（基数收进"高级"折叠区）、日志页重试徽标、仪表盘待重试计数。

## Impact

- **代码**：`db.rs`（建表+两列迁移）、`push.rs`（重试核心）、`api_admin.rs`（CRUD 校验/触发点/字段）、
  `web/src/App.vue`、`tests/e2e.sh`；`channels.rs` 零改动；零新依赖。
- **规格**：新增 `push-retry` 能力（见本目录 `specs/push-retry/spec.md`）。
- **文档**：设计文档 §4/§5 修订至 v0.9、README 中英双语新增重试说明。
- **兼容**：存量库 ALTER TABLE 迁移；旧任务默认 sync/3 次/1s；已知副作用（at-least-once 重复、
  异步懒触发滞留、同步最坏 ~47s 长尾）见《docs/推送失败自动重试提案.md》§9。

## 完整版文档

《docs/推送失败自动重试提案.md》为本提案的详版（含决策记录 D1-D11、DDL、drain 五步流程、
测试计划、验收标准）。
