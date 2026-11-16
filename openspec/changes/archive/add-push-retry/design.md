# 技术设计：推送失败自动重试

> 详版见《docs/推送失败自动重试提案.md》；本文为 OpenSpec 工作流内的技术决策摘要。

## 关键决策（全部经需求方确认，2026-11-16）

| # | 决策 | 选择 | 被否决备选 |
|---|---|---|---|
| D1 | 哪些失败重试 | 所有**发送**失败（网络/超时/业务拒绝） | 仅瞬时错误（需 Receipt 加错误类别） |
| D2 | 执行模型 | 任务级可配：次数（默认 3）+ 模式（sync/async） | 全局单一模式 |
| D3 | 异步实现 | 落库 + 懒触发 | 常驻后台线程（违反 §3 铁律） |
| D4 | 参数位置 | 前端任务表单 | 固定常量 / 全局 settings |
| D5 | 参数边界 | max ∈ 0..10、base_ms ∈ 100..60000，越界 400 | 无上限 |
| D6 | base_ms 暴露 | 表单"高级"折叠区，默认 1000 | 仅 API 可配 |
| D7 | drain 载体 | 临时短命线程（AtomicBool 防重复 spawn）+ 单次上限 20 条 | 请求线程内同步 drain |
| D8 | 队列陈旧 | 禁用跳过；删除级联；滞留 >1h 判 `retry expired` | 无限滞留 |
| D9 | 异步 batch_id | 首响 null，终态日志各自独立成批 | 等终态回填 |
| D10 | 队列可视化 | 仅 overview 待重试计数 | 完整队列页 |
| D11 | 默认模式 | sync | async |

**不重试**（发送前确定性失败）：模板未绑定 403、无绑定渠道、64KB 超限、渠道缺失/停用。
`/api/channels/:id/test` 与 `/api/tasks/:id/test` 保持单发实况。

## 执行模型

### 退避算法

`backoff_ms(base, n) = base × 2^(n-1)`（第 n 次重试前等待；不加 jitter——单机单实例无同步风暴）。
次数口径防 off-by-one：`max=3` = 首发之外最多再试 3 次，最坏 4 次尝试。

### 同步模式

现有"每消息×每渠道一条短命线程并行发送"结构内加循环：失败 → sleep(backoff(n)) → 重发，
成功或耗尽即止；`cost_ms` 记全程累计耗时；响应 `results[]` 增 `retries`。
渠道并行不变，批量总耗时 = 最慢渠道；最坏单渠道 ≈ 4×timeout_ms(10s) + 7s ≈ 47s。

### 异步模式

- 入队：首发失败（仍在请求内同步尝试）→ 写 `retry_queue`（完整 title/body——push_logs 只有
  80 字 brief 不够重发；`max_retries`/`base_ms` 为任务配置**快照**，入队后改任务不影响已入队条目），
  响应该项 `status: "retrying"`。
- 触发点：`do_push` 收尾（同 `maybe_cleanup` 模式）+ `GET /api/messages` + `GET /api/overview`。
  触发 = 若 AtomicBool 未置位则置位并 spawn 临时线程（§3 允许"临时 spawn 完成即回收"），完成复位。
- drain 单次流程（≤20 条，按 next_retry_at 升序）：
  1. 过期（created_at 距今 >1h，不论任务是否禁用——陈旧规则优先）→ 终态失败 `retry expired` 出队；
  2. 到期行 JOIN tasks enabled=1（禁用任务跳过保留）；
  3. 渠道缺失/停用 → 终态失败出队（确定性失败不重试）；
  4. 乐观占位：锁内 `UPDATE next_retry_at = now + backoff(attempts+1) WHERE id=? AND
     next_retry_at=<读值>`，rows_changed=0 视为被并发领取跳过；
  5. 锁外发送（重读渠道最新配置）：成功 → 终态 ok 出队；失败且 attempts+1 > max → 终态失败出队；
     否则占位值即生效继续等。
- `attempts` 语义 = 已发送尝试次数（入队时 1）；日志 `retries = attempts - 1`（0=首发成功）。

### 日志语义

`push_logs` 每条消息×渠道只在**终态**写一行（含 `retries`），统计页无重复计数；未决状态由
`retry_queue` 落库承载（进程重启不丢）。

## 数据模型

```sql
ALTER TABLE tasks ADD COLUMN retry_json TEXT NOT NULL
  DEFAULT '{"enabled":true,"max":3,"mode":"sync","base_ms":1000}';
ALTER TABLE push_logs ADD COLUMN retries INTEGER NOT NULL DEFAULT 0;

CREATE TABLE retry_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    template_id INTEGER, channel_id INTEGER,     -- 无外键：渠道可中途被删，drain 判终态失败
    title TEXT NOT NULL, body TEXT NOT NULL,     -- 完整渲染文本
    attempts INTEGER NOT NULL, max_retries INTEGER NOT NULL, base_ms INTEGER NOT NULL,
    next_retry_at TEXT NOT NULL, last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_retry_queue_due ON retry_queue (next_retry_at);
```

存量库迁移：`PRAGMA table_info` 探测后 ALTER；`CREATE TABLE IF NOT EXISTS` 建新表。
`PRAGMA` 表名为编译期常量（无法参数化），不违反参数化纪律。

## 已知副作用（文档必须注明）

1. **at-least-once 重复**：超时但对方已收到 → 重试必产生重复消息。
2. **异步懒触发滞留**：重试最多等到下一次任意推送/管理台读取（1h 过期窗口兜底）。
3. **同步长尾延迟**：最坏 ~47s/渠道，调用方需设置足够超时。

## 测试策略

- 单测：`backoff_ms` 序列与边界；drain（临时 SQLite + 非法渠道类型 `"nope"` 零出网即失败）：
  耗尽 / 过期 / 禁用跳过 / 乐观占位。
- e2e：本地接收器加 flaky 路径（前 N 次 500）；同步成功（耗时 ≥ 退避和）、耗尽、异步入队→
  轮询终态、参数越界 400、overview pending 计数。
