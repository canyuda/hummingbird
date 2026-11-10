<!-- 蜂鸟管理台：单文件 Vue3 应用（无路由库，页签切换） -->
<script setup>
import { ref, reactive, onMounted, computed } from 'vue'

/* ---------------- 基础状态 ---------------- */
const me = ref(null)                 // 当前登录信息
const tab = ref('overview')          // 当前页签
const toast = ref('')                // 全局提示
const settings = reactive({ site_title: '', site_slogan: '', theme_color: '' })

const notify = (msg) => { toast.value = msg; setTimeout(() => (toast.value = ''), 2500) }

/* ---------------- API 封装 ---------------- */
const api = async (method, path, body) => {
  const opt = { method, headers: {} }
  if (body !== undefined) {
    opt.headers['Content-Type'] = 'application/json'
    opt.body = JSON.stringify(body)
  }
  const r = await fetch(path, opt)
  const data = await r.json().catch(() => ({}))
  if (r.status === 401 && me.value) { me.value = null }
  if (!r.ok && data.error) throw new Error(data.error)
  return data
}

/* ---------------- 登录 / 改密 ---------------- */
const loginForm = reactive({ username: 'admin', password: '' })
const pwForm = reactive({ old_password: '', new_password: '' })
const showForceChange = ref(false)

const doLogin = async () => {
  try {
    const r = await api('POST', '/api/auth/login', { ...loginForm })
    me.value = { username: r.username, must_change_password: r.must_change_password }
    showForceChange.value = r.must_change_password
    if (!r.must_change_password) await loadAll()
  } catch (e) { notify('登录失败：' + e.message) }
}
const doChangePw = async () => {
  try {
    await api('PUT', '/api/auth/password', { ...pwForm })
    me.value.must_change_password = false
    showForceChange.value = false
    notify('密码已修改')
    await loadAll()
  } catch (e) { notify('修改失败：' + e.message) }
}
const doLogout = async () => {
  await api('POST', '/api/auth/logout')
  me.value = null
}

/* ---------------- 概览 ---------------- */
const overview = ref({})
const loadOverview = () => api('GET', '/api/overview').then((r) => (overview.value = r))

/* ---------------- 渠道 ---------------- */
const CHANNEL_TYPES = ['dingtalk_bot', 'wecom_bot', 'wecom_app', 'feishu_bot', 'telegram', 'bark', 'ntfy', 'webhook']
// 各渠道需要的私有配置字段（通用 proxy/timeout_ms 不在此列）
const CHANNEL_FIELDS = {
  dingtalk_bot: [['webhook', 'webhook 地址'], ['secret', '加签 secret（可选）']],
  wecom_bot: [['webhook', 'webhook 地址']],
  wecom_app: [['corpid', '企业 ID'], ['secret', '应用 secret'], ['agentid', '应用 agentid（数字）'], ['touser', '接收人（默认 @all）']],
  feishu_bot: [['webhook', 'webhook 地址'], ['secret', '签名 secret（可选）']],
  telegram: [['bot_token', 'Bot Token'], ['chat_id', 'Chat ID']],
  bark: [['key', '设备 Key'], ['server', '服务端（默认官方）']],
  ntfy: [['topic', '主题名'], ['server', '服务端（默认官方）']],
  webhook: [['url', '回调 URL']],
}
const channels = ref([])
const chForm = reactive({ id: null, type: 'webhook', name: '', config: {}, proxy: '', enabled: true })
const loadChannels = () => api('GET', '/api/channels').then((r) => (channels.value = r.channels))
const chFields = computed(() => CHANNEL_FIELDS[chForm.type] || [])
const editChannel = (c) => {
  Object.assign(chForm, {
    id: c.id, type: c.type, name: c.name,
    config: { ...c.config }, proxy: c.config.proxy || '', enabled: c.enabled,
  })
  delete chForm.config.proxy
}
const saveChannel = async () => {
  const cfg = { ...chForm.config }
  if (chForm.proxy) cfg.proxy = chForm.proxy
  const body = { type: chForm.type, name: chForm.name, config: cfg, enabled: chForm.enabled }
  try {
    if (chForm.id) await api('PUT', `/api/channels/${chForm.id}`, body)
    else await api('POST', '/api/channels', body)
    notify('已保存'); await loadChannels(); chForm.id = null; chForm.name = ''; chForm.config = {}; chForm.proxy = ''
  } catch (e) { notify('保存失败：' + e.message) }
}
const testChannel = async (c) => {
  notify('测试发送中…')
  try {
    const r = await api('POST', `/api/channels/${c.id}/test`)
    notify(r.ok ? `✅ 渠道正常（${r.cost_ms}ms${r.via_proxy ? '，经代理' : ''}）` : `❌ ${r.error}`)
  } catch (e) { notify('❌ ' + e.message) }
}
const delChannel = async (c) => {
  if (!confirm(`删除渠道「${c.name}」？`)) return
  await api('DELETE', `/api/channels/${c.id}`); notify('已删除'); await loadChannels()
}

/* ---------------- 模板 ---------------- */
const templates = ref([])
const tplForm = reactive({ id: null, name: '', title_tpl: '', body_tpl: '', variables: '[]' })
const loadTemplates = () => api('GET', '/api/templates').then((r) => (templates.value = r.templates))
const editTemplate = (t) => Object.assign(tplForm, {
  id: t.id, name: t.name, title_tpl: t.title_tpl, body_tpl: t.body_tpl,
  variables: JSON.stringify(t.variables, null, 0),
})
const saveTemplate = async () => {
  let vars
  try { vars = JSON.parse(tplForm.variables || '[]') } catch { return notify('变量定义不是合法 JSON') }
  const body = { name: tplForm.name, title_tpl: tplForm.title_tpl, body_tpl: tplForm.body_tpl, variables: vars }
  try {
    if (tplForm.id) await api('PUT', `/api/templates/${tplForm.id}`, body)
    else await api('POST', '/api/templates', body)
    notify('已保存'); await loadTemplates(); tplForm.id = null; tplForm.name = ''
  } catch (e) { notify('保存失败：' + e.message) }
}
const delTemplate = async (t) => {
  if (!confirm(`删除模板「${t.name}」？`)) return
  try { await api('DELETE', `/api/templates/${t.id}`); notify('已删除'); await loadTemplates() }
  catch (e) { notify('删除失败：' + e.message) }
}

/* ---------------- 任务 ---------------- */
const tasks = ref([])
const taskForm = reactive({ id: null, name: '', bindings: [] })
const newTaskToken = ref('')
const loadTasks = () => api('GET', '/api/tasks').then((r) => (tasks.value = r.tasks))
const addBinding = () => taskForm.bindings.push({ template_id: '', channel_ids: [] })
const editTask = (t) => Object.assign(taskForm, {
  id: t.id, name: t.name,
  bindings: t.bindings.map((b) => ({ template_id: b.template_id, channel_ids: [...b.channel_ids] })),
})
const saveTask = async () => {
  const bindings = taskForm.bindings
    .filter((b) => b.template_id && b.channel_ids.length)
    .map((b) => ({ template_id: Number(b.template_id), channel_ids: b.channel_ids.map(Number) }))
  if (!bindings.length) return notify('至少需要一条有效绑定')
  try {
    if (taskForm.id) {
      await api('PUT', `/api/tasks/${taskForm.id}`, { name: taskForm.name, bindings })
      notify('已更新')
    } else {
      const r = await api('POST', '/api/tasks', { name: taskForm.name, bindings })
      newTaskToken.value = r.token
    }
    await loadTasks(); taskForm.id = null; taskForm.name = ''; taskForm.bindings = []
  } catch (e) { notify('保存失败：' + e.message) }
}
const regenToken = async (t) => {
  if (!confirm(`重置「${t.name}」的 token？旧 token 立即失效`)) return
  const r = await api('POST', `/api/tasks/${t.id}/token/regenerate`)
  newTaskToken.value = r.token; await loadTasks()
}
const toggleTask = async (t) => { await api('PUT', `/api/tasks/${t.id}`, { enabled: !t.enabled }); await loadTasks() }
const testTask = async (t) => {
  notify('按绑定发送测试中…')
  try {
    const r = await api('POST', `/api/tasks/${t.id}/test`)
    const ok = r.results.every((x) => x.status === 'ok')
    notify(ok ? '✅ 全部渠道正常' : '❌ ' + (r.results.find((x) => x.status !== 'ok') || {}).error)
  } catch (e) { notify('❌ ' + e.message) }
}
const delTask = async (t) => {
  if (!confirm(`删除任务「${t.name}」？`)) return
  await api('DELETE', `/api/tasks/${t.id}`); notify('已删除'); await loadTasks()
}
const curlExample = computed(() => {
  if (!newTaskToken.value) return ''
  return `curl -H "Authorization: Bearer ${newTaskToken.value}" \\\n  http://<pi>:8080/api/push \\\n  -d '{"template":"模板名","variables":{"key":"value"}}'`
})

/* ---------------- 日志 ---------------- */
const logs = ref([])
const logFilter = reactive({ status: '', task: '' })
const loadLogs = async () => {
  const q = new URLSearchParams()
  if (logFilter.status) q.set('status', logFilter.status)
  if (logFilter.task) q.set('task', logFilter.task)
  const r = await api('GET', '/api/messages' + (q.toString() ? '?' + q : ''))
  logs.value = r.messages
}

/* ---------------- 设置 ---------------- */
const loadSettings = async () => {
  const r = await api('GET', '/api/settings')
  Object.assign(settings, r.settings)
  document.title = settings.site_title || '蜂鸟'
  document.documentElement.style.setProperty('--theme', settings.theme_color || '#16a34a')
}
const saveSettings = async () => {
  await api('PUT', '/api/settings', {
    site_title: settings.site_title, site_slogan: settings.site_slogan,
    site_icon: settings.site_icon, theme_color: settings.theme_color,
    log_retention_days: settings.log_retention_days,
  })
  notify('设置已保存'); await loadSettings()
}

/* ---------------- 初始化 ---------------- */
const loadAll = async () => {
  await Promise.all([loadOverview(), loadChannels(), loadTemplates(), loadTasks(), loadLogs(), loadSettings()])
}
const switchTab = async (t) => {
  tab.value = t
  if (t === 'logs') await loadLogs()
  if (t === 'overview') await loadOverview()
}
onMounted(async () => {
  try {
    const r = await api('GET', '/api/auth/me')
    me.value = r
    showForceChange.value = r.must_change_password
    if (!r.must_change_password) await loadAll()
  } catch { me.value = null }
})
</script>

<template>
  <!-- 强制改密对话框（首登） -->
  <div v-if="me && showForceChange" class="login-wrap">
    <div class="card login-box">
      <h1>修改初始密码</h1>
      <p class="muted">首次登录必须修改初始密码后才能使用</p>
      <label class="field">旧密码 <input type="password" v-model="pwForm.old_password" /></label>
      <label class="field" style="margin-top:10px">新密码（≥8位） <input type="password" v-model="pwForm.new_password" /></label>
      <button style="margin-top:14px;width:100%" @click="doChangePw">确认修改</button>
    </div>
  </div>

  <!-- 登录页 -->
  <div v-else-if="!me" class="login-wrap">
    <div class="card login-box">
      <h1>🐦 {{ settings.site_title || '蜂鸟 Hummingbird' }}</h1>
      <p class="muted">{{ settings.site_slogan || '轻量多渠道消息推送网关' }}</p>
      <label class="field">用户名 <input v-model="loginForm.username" @keyup.enter="doLogin" /></label>
      <label class="field" style="margin-top:10px">密码 <input type="password" v-model="loginForm.password" @keyup.enter="doLogin" /></label>
      <button style="margin-top:14px;width:100%" @click="doLogin">登录</button>
    </div>
  </div>

  <!-- 主界面 -->
  <div v-else class="layout">
    <aside class="sidebar">
      <div class="brand">🐦 {{ settings.site_title || '蜂鸟' }}<small>{{ settings.site_slogan }}</small></div>
      <div v-for="t in ['overview','channels','templates','tasks','logs','settings']" :key="t"
           class="nav-item" :class="{ active: tab === t }" @click="switchTab(t)">
        {{ { overview: '概览', channels: '渠道', templates: '模板', tasks: '任务', logs: '推送日志', settings: '设置' }[t] }}
      </div>
      <div class="nav-item" style="margin-top:auto" @click="doLogout">退出登录</div>
    </aside>

    <main class="main">
      <!-- 概览 -->
      <template v-if="tab === 'overview'">
        <div class="stat-grid">
          <div class="stat"><span class="muted">今日推送</span><b>{{ overview.today_total ?? '—' }}</b></div>
          <div class="stat"><span class="muted">今日成功</span><b style="color:var(--theme)">{{ overview.today_ok ?? '—' }}</b></div>
          <div class="stat"><span class="muted">今日失败</span><b style="color:var(--danger)">{{ overview.today_failed ?? '—' }}</b></div>
          <div class="stat"><span class="muted">渠道数</span><b>{{ overview.channels ?? '—' }}</b></div>
          <div class="stat"><span class="muted">任务数</span><b>{{ overview.tasks ?? '—' }}</b></div>
        </div>
        <div class="card" style="margin-top:16px">
          <b>快速开始</b>
          <p class="muted" style="line-height:1.9">
            1. 渠道页添加推送渠道（钉钉/企微/飞书/TG/Bark…），点「测试」验证连通<br>
            2. 模板页创建消息模板（用 <code>{{ '\{\{key\}\}' }}</code> 占位）<br>
            3. 任务页把模板绑定到渠道，生成 token 给脚本使用<br>
            4. 脚本里 <code>curl -H "Authorization: Bearer &lt;token&gt;" .../api/push</code> 即可推送
          </p>
        </div>
      </template>

      <!-- 渠道 -->
      <template v-if="tab === 'channels'">
        <div class="card">
          <div class="spread"><b>{{ chForm.id ? '编辑渠道 #' + chForm.id : '新增渠道' }}</b></div>
          <div class="form-grid" style="margin-top:10px">
            <label class="field">类型
              <select v-model="chForm.type">
                <option v-for="t in CHANNEL_TYPES" :key="t" :value="t">{{ t }}</option>
              </select>
            </label>
            <label class="field">名称 <input v-model="chForm.name" placeholder="如：运维钉钉群" /></label>
            <label class="field v-for" v-for="[k, label] in chFields" :key="k">{{ label }}
              <input v-model="chForm.config[k]" :placeholder="k" />
            </label>
            <label class="field">代理（可选，TG 需要）
              <input v-model="chForm.proxy" placeholder="http://127.0.0.1:7890 或 socks5://…" />
            </label>
            <label class="field">启用 <input type="checkbox" v-model="chForm.enabled" style="min-width:auto" /></label>
          </div>
          <div class="row" style="margin-top:12px">
            <button @click="saveChannel">保存</button>
            <button class="ghost" v-if="chForm.id" @click="chForm.id = null">取消编辑</button>
          </div>
        </div>
        <div class="card">
          <table>
            <thead><tr><th>名称</th><th>类型</th><th>代理</th><th>状态</th><th>操作</th></tr></thead>
            <tbody>
              <tr v-for="c in channels" :key="c.id">
                <td>{{ c.name }}</td>
                <td><code>{{ c.type }}</code></td>
                <td>{{ c.config.proxy ? '✅' : '—' }}</td>
                <td><span class="tag" v-if="c.enabled">启用</span><span class="tag gray" v-else>停用</span></td>
                <td>
                  <div class="row">
                    <button class="ghost" @click="testChannel(c)">测试</button>
                    <button class="ghost" @click="editChannel(c)">编辑</button>
                    <button class="danger" @click="delChannel(c)">删除</button>
                  </div>
                </td>
              </tr>
              <tr v-if="!channels.length"><td colspan="5" class="muted">暂无渠道</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 模板 -->
      <template v-if="tab === 'templates'">
        <div class="card">
          <b>{{ tplForm.id ? '编辑模板 #' + tplForm.id : '新增模板' }}</b>
          <div class="form-grid" style="margin-top:10px">
            <label class="field">名称 <input v-model="tplForm.name" placeholder="如：巡检报告" /></label>
            <label class="field">标题模板 <input v-model="tplForm.title_tpl" placeholder="巡检:{{host}}" /></label>
            <label class="field full">正文模板 <textarea v-model="tplForm.body_tpl" rows="3" placeholder="主机 {{host}} 状态 {{status}}"></textarea></label>
            <label class="field full">变量定义 JSON（[{key,desc,default}]，default 会作为缺省值）
              <textarea v-model="tplForm.variables" rows="3" placeholder='[{"key":"host","desc":"主机名","default":"pi"}]'></textarea>
            </label>
          </div>
          <button style="margin-top:12px" @click="saveTemplate">保存</button>
        </div>
        <div class="card">
          <table>
            <thead><tr><th>名称</th><th>标题</th><th>正文</th><th>操作</th></tr></thead>
            <tbody>
              <tr v-for="t in templates" :key="t.id">
                <td>{{ t.name }}</td>
                <td><code>{{ t.title_tpl }}</code></td>
                <td class="muted">{{ t.body_tpl.slice(0, 50) }}</td>
                <td><div class="row">
                  <button class="ghost" @click="editTemplate(t)">编辑</button>
                  <button class="danger" @click="delTemplate(t)">删除</button>
                </div></td>
              </tr>
              <tr v-if="!templates.length"><td colspan="4" class="muted">暂无模板</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 任务 -->
      <template v-if="tab === 'tasks'">
        <div v-if="newTaskToken" class="card" style="border-color: var(--theme)">
          <b>🔑 新 token（只显示这一次，立即保存）</b>
          <p><code style="user-select:all">{{ newTaskToken }}</code></p>
          <pre class="muted" style="white-space:pre-wrap">{{ curlExample }}</pre>
          <button class="ghost" @click="newTaskToken = ''">我已保存，关闭</button>
        </div>
        <div class="card">
          <b>{{ taskForm.id ? '编辑任务 #' + taskForm.id : '新增任务' }}</b>
          <label class="field" style="margin-top:10px">任务名 <input v-model="taskForm.name" placeholder="如：运维通知" /></label>
          <div v-for="(b, i) in taskForm.bindings" :key="i" class="row" style="margin-top:10px">
            <select v-model="b.template_id">
              <option value="" disabled>选择模板</option>
              <option v-for="t in templates" :key="t.id" :value="t.id">{{ t.name }}</option>
            </select>
            <span class="muted">→</span>
            <select v-model="b.channel_ids" multiple size="4" style="min-width:180px">
              <option v-for="c in channels" :key="c.id" :value="c.id">{{ c.name }}</option>
            </select>
            <button class="danger" @click="taskForm.bindings.splice(i, 1)">移除</button>
          </div>
          <div class="row" style="margin-top:10px">
            <button class="ghost" @click="addBinding">+ 添加绑定</button>
            <button @click="saveTask">{{ taskForm.id ? '更新任务' : '创建任务' }}</button>
          </div>
        </div>
        <div class="card">
          <table>
            <thead><tr><th>任务</th><th>token 前缀</th><th>绑定</th><th>最近使用</th><th>操作</th></tr></thead>
            <tbody>
              <tr v-for="t in tasks" :key="t.id">
                <td>{{ t.name }} <span class="tag gray" v-if="!t.enabled">停用</span></td>
                <td><code>{{ t.token_prefix }}…</code></td>
                <td>
                  <div v-for="b in t.bindings" :key="b.template_id" class="muted">
                    {{ b.template_name }} → {{ b.channel_ids.join(', ') }}
                  </div>
                </td>
                <td class="muted">{{ t.token_last_used_at || '未使用' }}</td>
                <td><div class="row">
                  <button class="ghost" @click="testTask(t)">测试</button>
                  <button class="ghost" @click="toggleTask(t)">{{ t.enabled ? '停用' : '启用' }}</button>
                  <button class="ghost" @click="regenToken(t)">重置token</button>
                  <button class="ghost" @click="editTask(t)">编辑</button>
                  <button class="danger" @click="delTask(t)">删除</button>
                </div></td>
              </tr>
              <tr v-if="!tasks.length"><td colspan="5" class="muted">暂无任务</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 推送日志 -->
      <template v-if="tab === 'logs'">
        <div class="card">
          <div class="row">
            <b>推送日志</b>
            <select v-model="logFilter.status" @change="loadLogs">
              <option value="">全部状态</option><option value="ok">成功</option><option value="failed">失败</option>
            </select>
            <button class="ghost" @click="loadLogs">刷新</button>
          </div>
          <table style="margin-top:10px">
            <thead><tr><th>时间</th><th>任务</th><th>渠道</th><th>标题</th><th>状态</th><th>耗时</th><th>错误</th></tr></thead>
            <tbody>
              <tr v-for="l in logs" :key="l.id">
                <td class="muted" style="white-space:nowrap">{{ l.created_at }}</td>
                <td>{{ l.task }}</td>
                <td>{{ l.channel }}</td>
                <td>{{ l.title }}</td>
                <td><span class="tag" v-if="l.status === 'ok'">ok</span><span class="tag red" v-else>failed</span></td>
                <td>{{ l.cost_ms }}ms</td>
                <td class="muted">{{ l.error }}</td>
              </tr>
              <tr v-if="!logs.length"><td colspan="7" class="muted">暂无日志</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 设置 -->
      <template v-if="tab === 'settings'">
        <div class="card">
          <b>站点与系统设置</b>
          <div class="form-grid" style="margin-top:10px">
            <label class="field">站点标题 <input v-model="settings.site_title" /></label>
            <label class="field">标语 <input v-model="settings.site_slogan" /></label>
            <label class="field">主题色 <input v-model="settings.theme_color" placeholder="#16a34a" /></label>
            <label class="field">日志保留天数 <input v-model="settings.log_retention_days" type="number" /></label>
          </div>
          <button style="margin-top:12px" @click="saveSettings">保存设置</button>
        </div>
      </template>
    </main>
  </div>

  <div v-if="toast" class="toast">{{ toast }}</div>
</template>
