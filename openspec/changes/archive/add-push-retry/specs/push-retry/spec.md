# push-retry 能力 · 增量规格（delta）

> 本变更新增能力；落地后合并入 `openspec/specs/push-retry/spec.md`。

## ADDED Requirements

### Requirement: 任务级重试配置

任务 SHALL 支持重试配置 `retry_json`，字段：`enabled`（bool，默认 true）、`max`（重试次数，
默认 3，不含首发）、`mode`（`sync` | `async`，默认 `sync`）、`base_ms`（退避基数，默认 1000）。
任务创建/更新接口 SHALL 接受并回传该配置；未指定时取默认值。

#### Scenario: 创建任务携带重试配置

- **WHEN** `POST /api/tasks` 携带 `"retry":{"enabled":true,"max":5,"mode":"async","base_ms":2000}`
- **THEN** 创建成功，`GET /api/tasks` 回传该配置

#### Scenario: 非法参数被拒（fail fast）

- **WHEN** `retry.max = 11` 或 `retry.base_ms = 50` 或 `retry.mode = "later"`
- **THEN** 返回 400 与明确错误信息，任务不被创建/更新

#### Scenario: 存量库升级

- **WHEN** 使用无 `retry_json` 列的旧数据库启动新版本
- **THEN** 自动迁移加列，旧任务取默认配置（sync / 3 次 / 1s），推送行为不变

### Requirement: 发送前确定性失败不重试

以下失败 SHALL NOT 触发重试：模板未绑定（403）、无绑定渠道、消息超 64KB、渠道缺失/停用。
渠道测试与任务测试接口 SHALL 保持单次发送实况。

#### Scenario: 渠道被删后的推送

- **WHEN** 任务绑定的渠道已删除，调用推送
- **THEN** 该渠道结果为 failed（`channel missing/disabled`），不产生重试或队列条目

### Requirement: 同步重试（指数退避）

`mode=sync` 且 `enabled=true` 且 `max>0` 时，单渠道发送失败 SHALL 按指数退避
`base_ms × 2^(n-1)` 重试至多 `max` 次；响应 `results[]` 每项 SHALL 含 `retries`
（0 = 首发成功）；`cost_ms` SHALL 为全程累计耗时。渠道间并行不受退避拖累。

#### Scenario: 两次失败后第三次成功

- **WHEN** 渠道前 2 次调用失败、第 3 次成功，任务配置 max=3、base_ms=100
- **THEN** 响应该项 `status:"ok"`、`retries:2`，耗时 ≥ 300ms（100+200 退避）

#### Scenario: 重试耗尽

- **WHEN** 渠道持续失败，max=3
- **THEN** 响应该项 `status:"failed"`、`retries:3`，`push_logs` 落一条终态 failed 日志（含 retries）

### Requirement: 异步重试（落库 + 懒触发）

`mode=async` 时首发失败 SHALL 写入 `retry_queue`（含完整渲染文本与配置快照），响应该项
`status:"retrying"` 且首响 `batch_id` 为 null。drain SHALL 由推送收尾、`GET /api/messages`、
`GET /api/overview` 懒触发，在临时短命线程中执行（不阻塞请求、无常驻线程），单次至多处理 20 条，
领取 SHALL 采用乐观占位防止并发双发。

#### Scenario: 入队与终态日志

- **WHEN** 渠道第 1 次失败、第 2 次成功，任务配置 async / max=3 / base_ms=200
- **THEN** 首响该项 `status:"retrying"`；随后任一触发点推动 drain，`push_logs` 出现终态 ok 行
  且 `retries:1`，队列清空

#### Scenario: 队列滞留过期

- **WHEN** 队列条目 `created_at` 距今超过 1 小时
- **THEN** drain 将其判为终态失败（`error = retry expired`）出队，不再发送

#### Scenario: 任务禁用与删除

- **WHEN** 任务被禁用
- **THEN** drain 跳过其队列条目（保留，重新启用后继续）
- **WHEN** 任务被删除
- **THEN** 其队列条目被级联清理

#### Scenario: 待重试计数可见

- **WHEN** 队列非空时请求 `GET /api/overview`
- **THEN** 响应含 `pending_retries` 计数

### Requirement: 终态日志与统计一致性

`push_logs` 每条消息×渠道 SHALL 只在终态写一行（含 `retries` 与最终结果）；`GET /api/messages`
行数据 SHALL 含 `retries`。重试过程不得产生重复计数行。

#### Scenario: 异步终态不重复计数

- **WHEN** 一条消息异步重试后成功
- **THEN** 概览统计（今日总数/成功）对该消息×渠道只计 1 次
