# 实现任务清单：add-push-retry

> 按 序号 顺序推进；每组完成后跑 `cargo test`，全绿再进下一组。

## 1. 数据层（db.rs）

- [x] 1.1 SCHEMA：`tasks` 增 `retry_json`（默认 `'{"enabled":true,"max":3,"mode":"sync","base_ms":1000}'`）、`push_logs` 增 `retries INTEGER NOT NULL DEFAULT 0`
- [x] 1.2 新建 `retry_queue` 表 + `idx_retry_queue_due` 索引（DDL 见 design.md）
- [x] 1.3 存量库迁移：`PRAGMA table_info` 探测缺失列后 `ALTER TABLE ADD COLUMN`
- [x] 1.4 验证：旧库（无新列）启动迁移无错、新库建表完整

## 2. 重试核心（push.rs）

- [x] 2.1 `RetryCfg` 结构 + `retry_json` 解析（坏数据回退默认并 log warn）+ `backoff_ms(base, n)` 纯函数
- [x] 2.2 `validate_retry(&Value) -> Result<String, String>`：边界校验（max 0..10 / base_ms 100..60000 / mode 白名单），返回规范化 JSON
- [x] 2.3 `auth_task` 读取 `retry_json` 进 `Task` 结构
- [x] 2.4 同步模式：渠道发送闭包内重试循环（sleep 退避、累计 cost_ms、产出 retries）
- [x] 2.5 异步模式：首发失败 `enqueue_retry`（完整 title/body + 配置快照 + next_retry_at=now+backoff(1)），响应项 `status:"retrying"`
- [x] 2.6 `trigger_drain`（AtomicBool 防重复 spawn 临时线程）+ `drain`（过期清扫 → 到期领取 → 锁外发送 → 终态出队）；`do_push` 收尾挂触发
- [x] 2.7 终态写日志统一走"插入 + batch_id 回填"helper，含 `retries`
- [x] 2.8 单测：backoff 序列/边界、validate_retry 用例、drain 耗尽/过期/禁用跳过/乐观占位（临时 SQLite + 非法渠道类型零出网）

## 3. API 接线（api_admin.rs / main.rs）

- [x] 3.1 `tasks_create` / `tasks_update`：接受 `retry`，校验失败 400；`tasks_list` 回传解析后的 `retry` 对象
- [x] 3.2 `messages_list`：行数据增 `retries`；入口挂 `trigger_drain`
- [x] 3.3 `overview`：增 `pending_retries` 计数；入口挂 `trigger_drain`
- [x] 3.4 签名调整：相关 handler 改收 `&Arc<AppState>`（drain 线程需要 'static）

## 4. 前端（web/src/App.vue → npm run build）

- [x] 4.1 任务表单"失败重试"区块：开关 / 次数(0-10) / 模式下拉；基数(100-60000)收进"高级"折叠区
- [x] 4.2 `openTaskNew`/`editTask` 初始化 retry（旧数据缺省合并），`saveTask` 提交 retry
- [x] 4.3 日志页：`retries > 0` 显示"重试 n 次"徽标
- [x] 4.4 仪表盘：待重试统计卡（`pending_retries`）
- [x] 4.5 `npm run build` 重建 `web/dist` 并确认产物入库

## 5. 端到端（tests/e2e.sh）

- [x] 5.1 webhook 接收器加 flaky 路径（`/flaky<N>`：前 N 次 500，之后 200；按路径独立计数）
- [x] 5.2 同步成功场景：assert ok + `retries:2` + 耗时 ≥ 300ms（python3 计时）
- [x] 5.3 同步耗尽场景：assert failed + `retries:3`
- [x] 5.4 异步场景：首响 `retrying` → 轮询 messages（兼触发 drain）出现 `retries:1` 终态行；overview 含 `pending_retries`
- [x] 5.5 边界校验：max=11 / base_ms=50 创建任务 → 400
- [x] 5.6 全量回归 44+ 项断言全绿

## 6. 文档维护（截图 + README）

- [x] 6.1 设计文档：头部增 v0.9 修订行；§4 数据模型（retry_json/retries/retry_queue）；§5 反转"不内置重试"并描述重试 API 语义；§12 验收清单增补
- [x] 6.2 ego-browser 实拍改动页面（本地起服务 + 登录）：更新 `docs/screenshots/03-概览.png`（待重试卡片）、`09-推送日志.png`（重试徽标），新增任务弹窗含"失败重试"区块的截图，命名延续 `NN-名称.png` 序号
- [x] 6.3 README.md / README.en.md：新增"推送重试"章节（配置说明 + at-least-once / 异步滞留 / 同步长尾副作用 + 调用超时建议），截图表补新图，中英同步
- [x] 6.4 验证：`cargo test`、`bash tests/e2e.sh` 全绿；release 构建 RSS / 二进制体积对照 §3 预算无劣化

## 7. 归档与发布（用户已授权免确认直接执行）

- [x] 7.1 自动归档（opsx:archive 语义）：delta 合并进 `openspec/specs/push-retry/spec.md`，变更目录移入 `openspec/changes/archive/`，tasks.md 勾选收尾
- [x] 7.2 版本 bump（沿用仓库惯例 patch +1）
- [x] 7.3 本 worktree 建分支提交 → 合并回主仓库（/Users/yiqi/project/hummingbird, main）→ push origin main（GitHub: canyuda/hummingbird）
- [x] 7.4 控制台输出树莓派升级命令（基于 scripts/upgrade.sh 的官方一行流），并说明需打 tag 发 Release 后该命令才能拉到新版
