#!/usr/bin/env bash
# 蜂鸟端到端自动化测试：本地起服务 + 本地 webhook 接收器，覆盖完整业务流。
# 用法：bash tests/e2e.sh   （依赖：cargo、curl、python3）
set -u

cd "$(dirname "$0")/.."
BIN=./target/debug/hummingbird
PORT=18980
HOOK_PORT=18981
REL_PORT=18982   # release 内存测量专用（PORT+1 会与仍在运行的 webhook 接收器撞端口）
BASE=http://127.0.0.1:$PORT
TMP=$(mktemp -d /tmp/hb-e2e.XXXXXX)
PASS=0; FAIL=0
SRV_PID=""; HOOK_PID=""
# 退出时统一清理子进程与临时目录（防止端口被僵尸进程占用）
trap '[ -n "$SRV_PID" ] && kill $SRV_PID 2>/dev/null; [ -n "$HOOK_PID" ] && kill $HOOK_PID 2>/dev/null; rm -rf "$TMP"' EXIT
# 端口预清理：上次异常退出残留的进程
lsof -ti tcp:$PORT | xargs kill 2>/dev/null
lsof -ti tcp:$HOOK_PORT | xargs kill 2>/dev/null

say()  { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m PASS\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '\033[1;31m FAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }
# 断言：$1=实际 $2=期望子串 $3=描述
assert_contains() {
  case "$1" in *"$2"*) ok "$3" ;; *) bad "$3 (got: $1)" ;; esac
}

say "构建 debug 二进制"
cargo build 2>/dev/null || { echo "build failed"; exit 1; }

say "启动本地 webhook 接收器（模拟外部服务，全部返回 200）"
python3 - "$HOOK_PORT" >"$TMP/hook.log" 2>&1 <<'PYEOF' &
import sys, http.server
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0)); self.rfile.read(n)
        self.send_response(200); self.end_headers(); self.wfile.write(b'{"ok":true}')
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
PYEOF
HOOK_PID=$!

say "启动蜂鸟服务（全新库）"
HB_DB_PATH="$TMP/hb.db" HB_PORT=$PORT $BIN >"$TMP/server.log" 2>&1 &
SRV_PID=$!
sleep 1.5
INIT_PW=$(grep -o 'password: [A-Za-z0-9]*' "$TMP/server.log" | awk '{print $2}')
[ -n "$INIT_PW" ] && ok "首次启动生成初始密码" || bad "初始密码缺失"

CK=$TMP/cookies
J() { curl -s -b "$CK" -c "$CK" "$@"; }

say "认证流"
R=$(curl -s -c "$CK" -X POST $BASE/api/auth/login -d "{\"username\":\"admin\",\"password\":\"$INIT_PW\"}")
assert_contains "$R" '"ok":true' "登录成功"
assert_contains "$R" '"must_change_password":true' "首登标记改密"
R=$(J $BASE/api/channels)
assert_contains "$R" 'password_change_required' "改密前管理 API 被拦截"
R=$(J -X PUT $BASE/api/auth/password -d "{\"old_password\":\"$INIT_PW\",\"new_password\":\"test-pass-123\"}")
assert_contains "$R" '"ok":true' "修改密码"
R=$(curl -s -X POST $BASE/api/auth/login -d '{"username":"admin","password":"wrong"}')
assert_contains "$R" 'invalid credentials' "错误密码被拒"
R=$(J $BASE/api/auth/me)
assert_contains "$R" '"must_change_password":false' "改密标记清除"

say "渠道 CRUD"
R=$(J -X POST $BASE/api/channels -d '{"type":"webhook","name":"本地钩子","config":{"url":"http://127.0.0.1:'"$HOOK_PORT"'/hook"}}')
assert_contains "$R" '"id":' "创建 webhook 渠道"
CH_ID=$(echo "$R" | grep -o '[0-9]*')
R=$(J -X POST $BASE/api/channels -d '{"type":"bad_type","name":"x"}')
assert_contains "$R" 'invalid channel type' "非法渠道类型被拒"
R=$(J $BASE/api/channels)
assert_contains "$R" '本地钩子' "渠道列表可见"

say "渠道测试按钮（兼连通性验证）"
R=$(J -X POST $BASE/api/channels/$CH_ID/test)
assert_contains "$R" '"ok":true' "webhook 渠道连通"

say "模板 CRUD 与渲染"
R=$(J -X POST $BASE/api/templates -d '{"name":"巡检","title_tpl":"巡检:{{host}}","body_tpl":"主机 {{host}} 状态 {{status}}","variables":[{"key":"host","desc":"主机名","default":"pi"},{"key":"status","desc":"状态","default":"ok"}]}')
assert_contains "$R" '"id":' "创建模板"
TPL_ID=$(echo "$R" | grep -o '[0-9]*')
R=$(J -X POST $BASE/api/templates/$TPL_ID/render -d '{"variables":{"host":"mac","status":"warn"}}')
assert_contains "$R" '主机 mac 状态 warn' "变量渲染正确"
assert_contains "$R" '巡检:mac' "标题渲染正确"
R=$(J -X POST $BASE/api/templates -d '{"name":"巡检","title_tpl":"x"}')
assert_contains "$R" 'already exists' "模板重名被拒"

say "任务（绑定 + token）"
R=$(J -X POST $BASE/api/tasks -d '{"name":"运维通知","bindings":[{"template_id":'"$TPL_ID"',"channel_ids":['"$CH_ID"']}]}')
assert_contains "$R" '"token":"hb' "创建任务并返回 token（hb 前缀）"
TOKEN=$(echo "$R" | grep -o '"token":"[^"]*"' | cut -d'"' -f4)
R=$(J $BASE/api/tasks)
assert_contains "$R" '运维通知' "任务列表可见"
assert_contains "$R" '"token_prefix"' "token 前缀可见（明文不可见）"
echo "$R" | grep -q "$TOKEN" && bad "明文 token 泄露在列表接口" || ok "列表不泄露明文 token"

say "推送：POST 单发"
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $TOKEN" -d '{"template":"巡检","variables":{"host":"pi","status":"ok"}}')
assert_contains "$R" '"ok":true' "单发成功"
assert_contains "$R" '"status":"ok"' "渠道回执 ok"
assert_contains "$R" '"batch_id":' "批次 id 返回"

say "推送：默认变量（未传 status 时用模板默认值）"
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $TOKEN" -d '{"template":"巡检","variables":{"host":"backup-1"}}')
assert_contains "$R" '"ok":true' "默认变量补全成功"

say "推送：批量（两条不同变量）"
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $TOKEN" -d '{"messages":[{"template":"巡检","variables":{"host":"a"}},{"template":"巡检","variables":{"host":"b"}}]}')
assert_contains "$R" '"ok":true' "批量成功"

say "推送：GET 一行流"
R=$(curl -s "$BASE/push/$TOKEN?template=%E5%B7%A1%E6%A3%80&host=mac&status=warn")
assert_contains "$R" '"ok":true' "GET 一行流成功"

say "推送：省略 template（任务只有一个绑定）"
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $TOKEN" -d '{"variables":{"host":"auto"}}')
assert_contains "$R" '"ok":true' "隐式模板解析成功"

say "推送：安全边界"
R=$(curl -s -X POST $BASE/api/push -d '{"template":"巡检"}')
assert_contains "$R" 'Bearer' "无 token 被拒（401）"
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer hb0000" -d '{"template":"巡检"}')
assert_contains "$R" 'invalid task token' "伪造 token 被拒"
# 未绑定模板：新建一个不绑定的模板试图发
R=$(J -X POST $BASE/api/templates -d '{"name":"孤儿"}')
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $TOKEN" -d '{"template":"孤儿"}')
assert_contains "$R" 'not bound' "未绑定模板 403"
# 批量超限：25 条
MSGS='['
for i in $(seq 1 25); do MSGS+='{"template":"巡检"},' ; done
MSGS="${MSGS%,}]"
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $TOKEN" -d "{\"messages\":$MSGS}")
assert_contains "$R" '1..20' "批量超限被拒"

say "token 重生成：旧的立即失效"
R=$(J -X POST $BASE/api/tasks/1/token/regenerate)
NEW_TOKEN=$(echo "$R" | grep -o '"token":"[^"]*"' | cut -d'"' -f4)
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $TOKEN" -d '{"template":"巡检"}')
assert_contains "$R" 'invalid task token' "旧 token 失效"
R=$(curl -s -X POST $BASE/api/push -H "Authorization: Bearer $NEW_TOKEN" -d '{"template":"巡检"}')
assert_contains "$R" '"ok":true' "新 token 可用"

say "推送日志与概览"
R=$(J "$BASE/api/messages")
assert_contains "$R" '"task":"运维通知"' "日志记录任务名"
assert_contains "$R" '"channel":"本地钩子"' "日志记录渠道名"
R=$(J "$BASE/api/messages?status=failed")
assert_contains "$R" '"total":0' "失败过滤为空"
R=$(J $BASE/api/overview)
assert_contains "$R" '"today_total":' "概览统计返回"
assert_contains "$R" '"days7"' "概览含近7天逐日数据"
assert_contains "$R" '"channels7"' "概览含渠道分布数据"

say "设置（站点样式 + 保留天数）"
R=$(J -X PUT $BASE/api/settings -d '{"site_title":"我的蜂鸟","theme_color":"#ff0000"}')
assert_contains "$R" '"ok":true' "更新设置"
R=$(J $BASE/api/settings)
assert_contains "$R" '我的蜂鸟' "设置读取回显"
assert_contains "$R" '"theme_color":"#ff0000"' "主题色生效"

say "模板删除保护"
R=$(J -X DELETE $BASE/api/templates/$TPL_ID)
assert_contains "$R" 'bound by tasks' "被绑定的模板拒绝删除"

say "内存占用（release 构建，设计目标：空闲 < 8MB）"
cargo build --release 2>/dev/null
# 口径说明：设计目标"<8MB"指常态空闲。首启会因 bootstrap 的 argon2(m=8MiB)
# 一次性分配抬高 RSS（已声明的瞬时峰值），因此先首启建库、再二启测常态。
HB_DB_PATH="$TMP/rel.db" HB_PORT=$REL_PORT ./target/release/hummingbird >"$TMP/rel1.log" 2>&1 &
REL_PID=$!
sleep 1.5
kill $REL_PID 2>/dev/null; sleep 0.5
HB_DB_PATH="$TMP/rel.db" HB_PORT=$REL_PORT ./target/release/hummingbird >"$TMP/rel2.log" 2>&1 &
REL_PID=$!
sleep 1.5
RSS=$(ps -o rss= -p $REL_PID | tr -d ' ')
echo "  release 常态空闲 RSS: ${RSS}KB（首启含 argon2 峰值，不作为常态指标）"
if [ -n "$RSS" ] && [ "$RSS" -lt 8192 ]; then
  ok "常态 RSS < 8MB（${RSS}KB）"
else
  bad "RSS 超标: ${RSS}KB —— release 启动日志："
  sed -n '1,15p' "$TMP/rel1.log" "$TMP/rel2.log" 2>/dev/null
fi
kill $REL_PID 2>/dev/null

say "前端内嵌与 SPA 服务"
R=$(curl -s $BASE/)
assert_contains "$R" 'assets/index-' "GET / 返回内嵌 Vue 管理台"
R=$(curl -s $BASE/some/spa/route)
assert_contains "$R" 'assets/index-' "未知路径回落 index.html（SPA）"

say "reset-password CLI"
R=$(HB_DB_PATH="$TMP/hb.db" $BIN reset-password)
assert_contains "$R" 'admin password reset to:' "CLI 重置密码输出"

say "清理（由 trap 兜底）"
kill $SRV_PID $HOOK_PID 2>/dev/null

printf '\n\033[1m结果: %d 通过 / %d 失败\033[0m\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
