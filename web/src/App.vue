<!-- 蜂鸟管理台：单文件 Vue3 应用（无路由库，页签切换；弹窗化 CRUD + SVG 图表） -->
<script setup>
import { ref, reactive, onMounted, computed } from 'vue'

/* ---------------- 基础状态 ---------------- */
const me = ref(undefined)           // undefined=会话探测中（刷新时不闪登录页），null=未登录，对象=已登录
const tab = ref('overview')          // 当前页签
const toast = ref('')                // 全局提示
const settings = reactive({ site_title: '', site_slogan: '', theme_color: '' })
const modal = ref(null)              // 当前弹窗：null | 'channel' | 'template' | 'task' | 'token'

const notify = (msg) => { toast.value = msg; setTimeout(() => (toast.value = ''), 2500) }
const closeModal = () => { modal.value = null }
const onKeydown = (e) => { if (e.key === 'Escape') { modal.value = null; newTaskToken.value = ''; confirmState.show = false } }

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

/* ---------------- 概览 + 图表数据 ---------------- */
const overview = ref({})
const loadOverview = () => api('GET', '/api/overview').then((r) => (overview.value = r))

// 近 7 天逐日补零（UTC 口径，与后端 date('now') 一致）
const days7 = computed(() => {
  const map = Object.fromEntries((overview.value.days7 || []).map((d) => [d.date, d]))
  const out = []
  for (let i = 6; i >= 0; i--) {
    const date = new Date(Date.now() - i * 864e5).toISOString().slice(0, 10)
    const d = map[date] || { date, total: 0, ok: 0 }
    out.push({ date, total: d.total, ok: d.ok, failed: d.total - d.ok })
  }
  return out
})
const barMax = computed(() => Math.max(1, ...days7.value.map((d) => d.total)))

// 渠道分布环形图（近 7 天 TOP8）
const PALETTE = ['#10b981', '#3b82f6', '#f59e0b', '#8b5cf6', '#ec4899', '#14b8a6', '#f43f5e', '#94a3b8']
const channels7 = computed(() => overview.value.channels7 || [])
const donutTotal = computed(() => channels7.value.reduce((s, c) => s + c.count, 0))
const donutSegs = computed(() => {
  const C = 2 * Math.PI * 56
  let acc = 0
  return channels7.value.map((c, i) => {
    const frac = donutTotal.value ? c.count / donutTotal.value : 0
    const seg = {
      ...c, frac, color: PALETTE[i % PALETTE.length],
      dasharray: `${(frac * C).toFixed(1)} ${C.toFixed(1)}`,
      dashoffset: (-acc * C).toFixed(1),
    }
    acc += frac
    return seg
  })
})

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
const CHANNEL_TYPE_LABELS = {
  dingtalk_bot: '钉钉机器人', wecom_bot: '企微机器人', wecom_app: '企微应用',
  feishu_bot: '飞书机器人', telegram: 'Telegram', bark: 'Bark', ntfy: 'ntfy', webhook: 'Webhook',
}
const channels = ref([])
const chForm = reactive({ id: null, type: 'webhook', name: '', config: {}, proxy: '', enabled: true })
const loadChannels = () => api('GET', '/api/channels').then((r) => (channels.value = r.channels))
const chFields = computed(() => CHANNEL_FIELDS[chForm.type] || [])
const openChannelNew = () => {
  Object.assign(chForm, { id: null, type: 'webhook', name: '', config: {}, proxy: '', enabled: true })
  modal.value = 'channel'
}
const editChannel = (c) => {
  Object.assign(chForm, {
    id: c.id, type: c.type, name: c.name,
    config: { ...c.config }, proxy: c.config.proxy || '', enabled: c.enabled,
  })
  delete chForm.config.proxy
  modal.value = 'channel'
}
const saveChannel = async () => {
  const cfg = { ...chForm.config }
  if (chForm.proxy) cfg.proxy = chForm.proxy
  const body = { type: chForm.type, name: chForm.name, config: cfg, enabled: chForm.enabled }
  try {
    if (chForm.id) await api('PUT', `/api/channels/${chForm.id}`, body)
    else await api('POST', '/api/channels', body)
    notify('渠道已保存'); modal.value = null; await loadChannels()
  } catch (e) { notify('保存失败：' + e.message) }
}
const testChannel = async (c) => {
  notify('测试发送中…')
  try {
    const r = await api('POST', `/api/channels/${c.id}/test`)
    notify(r.ok ? `✅ 渠道正常（${r.cost_ms}ms${r.via_proxy ? '，经代理' : ''}）` : `❌ ${r.error}`)
  } catch (e) { notify('❌ ' + e.message) }
}
const delChannel = (c) => askConfirm('删除渠道',
  `确定删除渠道「${c.name}」？已绑定它的任务推送时会失败。`,
  async () => { await api('DELETE', `/api/channels/${c.id}`); notify('已删除'); await loadChannels() })

/* ---------------- 模板 ---------------- */
const templates = ref([])
const tplForm = reactive({ id: null, name: '', title_tpl: '', body_tpl: '' })
const tplVars = ref([])   // 结构化变量行：[{key, desc, default}]
const loadTemplates = () => api('GET', '/api/templates').then((r) => (templates.value = r.templates))
const openTemplateNew = () => {
  Object.assign(tplForm, { id: null, name: '', title_tpl: '', body_tpl: '' })
  tplVars.value = []
  modal.value = 'template'
}
const editTemplate = (t) => {
  Object.assign(tplForm, { id: t.id, name: t.name, title_tpl: t.title_tpl, body_tpl: t.body_tpl })
  tplVars.value = (t.variables || []).map((v) => ({ key: v.key || '', desc: v.desc || '', default: v.default || '' }))
  modal.value = 'template'
}
const addTplVar = (key = '') => tplVars.value.push({ key, desc: '', default: '' })
// 扫描标题/正文占位符，提示未定义的变量
const undefinedVars = computed(() => {
  const used = new Set()
  for (const m of (tplForm.title_tpl + ' ' + tplForm.body_tpl).matchAll(/\{\{\s*([A-Za-z_]\w*)\s*\}\}/g)) used.add(m[1])
  const defined = new Set(tplVars.value.map((v) => v.key.trim()).filter(Boolean))
  return [...used].filter((k) => !defined.has(k))
})
const saveTemplate = async () => {
  const variables = tplVars.value
    .map((v) => ({ key: v.key.trim(), desc: v.desc.trim(), default: v.default }))
    .filter((v) => v.key)
  if (undefinedVars.value.length) return notify(`模板中有未定义变量：${undefinedVars.value.join('、')}`)
  const body = { name: tplForm.name, title_tpl: tplForm.title_tpl, body_tpl: tplForm.body_tpl, variables }
  try {
    if (tplForm.id) await api('PUT', `/api/templates/${tplForm.id}`, body)
    else await api('POST', '/api/templates', body)
    notify('模板已保存'); modal.value = null; await loadTemplates()
  } catch (e) { notify('保存失败：' + e.message) }
}
const delTemplate = (t) => askConfirm('删除模板',
  `确定删除模板「${t.name}」？被任务绑定的模板会被服务端拒绝删除。`,
  async () => {
    try { await api('DELETE', `/api/templates/${t.id}`); notify('已删除'); await loadTemplates() }
    catch (e) { notify('删除失败：' + e.message) }
  })

/* ---------------- 任务 ---------------- */
const tasks = ref([])
const taskForm = reactive({ id: null, name: '', bindings: [] })
const newTaskToken = ref('')
const loadTasks = () => api('GET', '/api/tasks').then((r) => (tasks.value = r.tasks))
const openTaskNew = () => {
  Object.assign(taskForm, { id: null, name: '', bindings: [{ template_id: '', channel_ids: [] }] })
  modal.value = 'task'
}
const editTask = (t) => {
  Object.assign(taskForm, {
    id: t.id, name: t.name,
    bindings: t.bindings.map((b) => ({ template_id: b.template_id, channel_ids: [...b.channel_ids] })),
  })
  modal.value = 'task'
}
const addBinding = () => taskForm.bindings.push({ template_id: '', channel_ids: [] })
const toggleChannel = (b, id) => {
  const i = b.channel_ids.indexOf(id)
  if (i >= 0) b.channel_ids.splice(i, 1)
  else b.channel_ids.push(id)
}
const saveTask = async () => {
  const bindings = taskForm.bindings
    .filter((b) => b.template_id && b.channel_ids.length)
    .map((b) => ({ template_id: Number(b.template_id), channel_ids: b.channel_ids.map(Number) }))
  if (!bindings.length) return notify('至少需要一条有效绑定（模板 + 渠道）')
  try {
    if (taskForm.id) {
      await api('PUT', `/api/tasks/${taskForm.id}`, { name: taskForm.name, bindings })
      notify('任务已更新')
    } else {
      const r = await api('POST', '/api/tasks', { name: taskForm.name, bindings })
      newTaskToken.value = r.token
    }
    modal.value = null; await loadTasks()
  } catch (e) { notify('保存失败：' + e.message) }
}
const regenToken = (t) => askConfirm('重置 token',
  `任务「${t.name}」的旧 token 将立即失效，使用它的脚本会开始收到 401。确定重置？`,
  async () => {
    const r = await api('POST', `/api/tasks/${t.id}/token/regenerate`)
    newTaskToken.value = r.token; await loadTasks()
  })
const toggleTask = async (t) => {
  try { await api('PUT', `/api/tasks/${t.id}`, { enabled: !t.enabled }); await loadTasks() }
  catch (e) { notify('操作失败：' + e.message) }
}
/* 任务测试弹窗：选模板 + 填测试变量，结果留在弹窗内，可调参反复重测 */
const testModal = reactive({ task: null, template_id: null, vars: {}, results: null, sending: false })
const testTplDef = computed(() => templates.value.find((x) => x.id === testModal.template_id))
const resetTestVars = () => {
  const v = {}
  for (const item of testTplDef.value?.variables || []) v[item.key] = item.default || ''
  testModal.vars = v
}
const openTest = (t) => {
  Object.assign(testModal, {
    task: t, template_id: t.bindings[0] ? t.bindings[0].template_id : null,
    vars: {}, results: null, sending: false,
  })
  resetTestVars()
  modal.value = 'test'
}
const sendTest = async () => {
  if (!testModal.template_id) return notify('请选择模板')
  const variables = {}
  for (const [k, v] of Object.entries(testModal.vars)) if (v !== '') variables[k] = v
  testModal.sending = true; testModal.results = null
  try {
    const r = await api('POST', `/api/tasks/${testModal.task.id}/test`,
      { template_id: testModal.template_id, variables })
    testModal.results = r.results
    notify(r.results.every((x) => x.status === 'ok') ? '✅ 全部渠道正常' : '❌ 存在失败渠道，见弹窗结果')
  } catch (e) { notify('❌ ' + e.message) }
  testModal.sending = false
}
/* 通用确认弹窗（替代浏览器原生 confirm） */
const confirmState = reactive({ show: false, title: '', message: '', onOk: null })
const askConfirm = (title, message, onOk) => Object.assign(confirmState, { show: true, title, message, onOk })
const runConfirm = async () => { const f = confirmState.onOk; confirmState.show = false; await f?.() }
const delTask = (t) => askConfirm('删除任务',
  `确定删除任务「${t.name}」？其 token 将立即失效，使用它的脚本会开始收到 401。`,
  async () => { await api('DELETE', `/api/tasks/${t.id}`); notify('已删除'); await loadTasks() })
const curlExample = computed(() => {
  if (!newTaskToken.value) return ''
  return `curl -H "Authorization: Bearer ${newTaskToken.value}" \\\n  http://<pi>:8080/api/push \\\n  -d '{"template":"模板名","variables":{"key":"value"}}'`
})
const copyToken = async () => {
  try { await navigator.clipboard.writeText(newTaskToken.value); notify('已复制到剪贴板') }
  catch { notify('复制失败，请手动选择复制') }
}

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
  document.documentElement.style.setProperty('--theme', settings.theme_color || '#10b981')
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
const NAV = [
  ['overview', '📊', '概览'], ['channels', '📡', '渠道'], ['templates', '📝', '模板'],
  ['tasks', '🔗', '任务'], ['logs', '🧾', '推送日志'], ['settings', '⚙️', '设置'],
]
const loadAll = async () => {
  await Promise.all([loadOverview(), loadChannels(), loadTemplates(), loadTasks(), loadLogs(), loadSettings()])
}
const switchTab = async (t) => {
  tab.value = t
  if (t === 'logs') await loadLogs()
  if (t === 'overview') await loadOverview()
}
onMounted(async () => {
  window.addEventListener('keydown', onKeydown)
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
      <label class="field" style="margin-top:10px">新密码（≥8位） <input type="password" v-model="pwForm.new_password" @keyup.enter="doChangePw" /></label>
      <button style="margin-top:16px" @click="doChangePw">确认修改</button>
    </div>
  </div>

  <!-- 登录页 -->
  <div v-else-if="me === null" class="login-wrap">
    <div class="card login-box">
      <h1>🐦 {{ settings.site_title || '蜂鸟 Hummingbird' }}</h1>
      <p class="muted">{{ settings.site_slogan || '轻量多渠道消息推送网关' }}</p>
      <label class="field">用户名 <input v-model="loginForm.username" @keyup.enter="doLogin" /></label>
      <label class="field" style="margin-top:10px">密码 <input type="password" v-model="loginForm.password" @keyup.enter="doLogin" /></label>
      <button style="margin-top:16px" @click="doLogin">登 录</button>
    </div>
  </div>

  <!-- 主界面 -->
  <div v-else-if="me" class="layout">
    <aside class="sidebar">
      <div class="brand">🐦 {{ settings.site_title || '蜂鸟' }}<small>{{ settings.site_slogan }}</small></div>
      <div v-for="[id, icon, label] in NAV" :key="id"
           class="nav-item" :class="{ active: tab === id }" @click="switchTab(id)">
        {{ icon }} {{ label }}
      </div>
      <div class="nav-item" style="margin-top:auto" @click="doLogout">🚪 退出登录</div>
    </aside>

    <main class="main">
      <!-- 概览 -->
      <template v-if="tab === 'overview'">
        <div class="page-head"><h2>概览</h2><span class="muted">数据口径 UTC</span></div>
        <div class="stat-grid">
          <div class="stat"><span class="muted">今日推送</span><b>{{ overview.today_total ?? '—' }}</b></div>
          <div class="stat"><span class="muted">今日成功</span><b style="color:var(--ok)">{{ overview.today_ok ?? '—' }}</b></div>
          <div class="stat"><span class="muted">今日失败</span><b style="color:var(--danger)">{{ overview.today_failed ?? '—' }}</b></div>
          <div class="stat"><span class="muted">渠道数</span><b>{{ overview.channels ?? '—' }}</b></div>
          <div class="stat"><span class="muted">任务数</span><b>{{ overview.tasks ?? '—' }}</b></div>
        </div>

        <div class="chart-grid">
          <div class="card chart-card">
            <h3>近 7 天推送量</h3>
            <svg class="chart-svg" viewBox="0 0 720 220" role="img">
              <!-- 网格线与刻度 -->
              <g v-for="f in [0, .25, .5, .75, 1]" :key="f">
                <line x1="44" :y1="172 - f * 140" x2="716" :y2="172 - f * 140" stroke="#eef2f7" stroke-width="1" />
                <text x="38" :y="176 - f * 140" text-anchor="end" font-size="11" fill="#94a3b8">{{ Math.round(barMax * f) }}</text>
              </g>
              <!-- 堆叠柱：成功（主题色）+ 失败（红） -->
              <g v-for="(d, i) in days7" :key="d.date">
                <title>{{ d.date }}｜成功 {{ d.ok }}，失败 {{ d.failed }}</title>
                <rect v-if="d.ok" :x="44 + i * 96 + 26" :y="172 - (d.ok / barMax) * 140"
                      width="44" :height="(d.ok / barMax) * 140" rx="3" fill="var(--theme)" opacity=".9" />
                <rect v-if="d.failed" :x="44 + i * 96 + 26" :y="172 - (d.total / barMax) * 140"
                      width="44" :height="(d.failed / barMax) * 140" rx="3" fill="#f87171" />
                <text :x="44 + i * 96 + 48" y="192" text-anchor="middle" font-size="11" fill="#64748b">{{ d.date.slice(5) }}</text>
              </g>
            </svg>
          </div>

          <div class="card chart-card">
            <h3>渠道分布（近 7 天）</h3>
            <div v-if="donutSegs.length" class="donut-wrap">
              <svg viewBox="0 0 160 160">
                <circle cx="80" cy="80" r="56" fill="none" stroke="#eef2f7" stroke-width="22" />
                <circle v-for="s in donutSegs" :key="s.name" cx="80" cy="80" r="56" fill="none"
                        stroke-width="22" :stroke="s.color" :stroke-dasharray="s.dasharray"
                        :stroke-dashoffset="s.dashoffset" transform="rotate(-90 80 80)">
                  <title>{{ s.name }}：{{ s.count }} 条（{{ Math.round(s.frac * 100) }}%）</title>
                </circle>
                <text x="80" y="76" text-anchor="middle" font-size="22" font-weight="700" fill="#0f172a">{{ donutTotal }}</text>
                <text x="80" y="94" text-anchor="middle" font-size="10" fill="#94a3b8">近 7 天推送</text>
              </svg>
              <div class="legend">
                <div v-for="s in donutSegs" :key="s.name" class="row">
                  <span class="dot" :style="{ background: s.color }"></span>
                  <span style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" :title="s.name">{{ s.name }}</span>
                  <span class="muted" style="margin-left:auto">{{ s.count }} · {{ Math.round(s.frac * 100) }}%</span>
                </div>
              </div>
            </div>
            <p v-else class="muted" style="padding:40px 0;text-align:center">近 7 天暂无推送记录</p>
          </div>
        </div>

        <div class="card" style="margin-top:16px">
          <h3 style="font-size:14px">快速开始</h3>
          <p class="muted" style="line-height:2;margin-top:8px">
            1. 渠道页添加推送渠道（钉钉/企微/飞书/TG/Bark…），点「测试」验证连通<br>
            2. 模板页创建消息模板（用 <code>{{ '\{\{key\}\}' }}</code> 占位）<br>
            3. 任务页把模板绑定到渠道，生成 token 给脚本使用<br>
            4. 脚本里 <code>curl -H "Authorization: Bearer &lt;token&gt;" .../api/push</code> 即可推送
          </p>
        </div>
      </template>

      <!-- 渠道 -->
      <template v-if="tab === 'channels'">
        <div class="page-head">
          <h2>渠道 <span class="muted">共 {{ channels.length }} 个</span></h2>
          <button @click="openChannelNew">＋ 新增渠道</button>
        </div>
        <div class="card">
          <table>
            <thead><tr><th>名称</th><th>类型</th><th>代理</th><th>状态</th><th style="text-align:right">操作</th></tr></thead>
            <tbody>
              <tr v-for="c in channels" :key="c.id">
                <td style="font-weight:500">{{ c.name }}</td>
                <td><span class="tag gray">{{ CHANNEL_TYPE_LABELS[c.type] || c.type }}</span></td>
                <td>{{ c.config.proxy ? '✅ 经代理' : '—' }}</td>
                <td><span class="tag" v-if="c.enabled">启用</span><span class="tag gray" v-else>停用</span></td>
                <td>
                  <div class="row" style="justify-content:flex-end">
                    <button class="ghost" @click="testChannel(c)">测试</button>
                    <button class="ghost" @click="editChannel(c)">编辑</button>
                    <button class="danger" @click="delChannel(c)">删除</button>
                  </div>
                </td>
              </tr>
              <tr v-if="!channels.length"><td colspan="5" class="muted" style="text-align:center;padding:30px 0">暂无渠道，点右上角「新增渠道」开始</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 模板 -->
      <template v-if="tab === 'templates'">
        <div class="page-head">
          <h2>模板 <span class="muted">共 {{ templates.length }} 个</span></h2>
          <button @click="openTemplateNew">＋ 新增模板</button>
        </div>
        <div class="card">
          <table>
            <thead><tr><th>名称</th><th>标题</th><th>正文</th><th>变量</th><th style="text-align:right">操作</th></tr></thead>
            <tbody>
              <tr v-for="t in templates" :key="t.id">
                <td style="font-weight:500">{{ t.name }}</td>
                <td><code>{{ t.title_tpl }}</code></td>
                <td class="muted">{{ t.body_tpl.slice(0, 50) }}</td>
                <td>
                  <span class="chip mini" v-for="v in t.variables" :key="v.key" style="cursor:default">{{ v.key }}</span>
                  <span v-if="!t.variables?.length" class="muted">—</span>
                </td>
                <td>
                  <div class="row" style="justify-content:flex-end">
                    <button class="ghost" @click="editTemplate(t)">编辑</button>
                    <button class="danger" @click="delTemplate(t)">删除</button>
                  </div>
                </td>
              </tr>
              <tr v-if="!templates.length"><td colspan="5" class="muted" style="text-align:center;padding:30px 0">暂无模板</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 任务 -->
      <template v-if="tab === 'tasks'">
        <div class="page-head">
          <h2>任务 <span class="muted">共 {{ tasks.length }} 个</span></h2>
          <button @click="openTaskNew">＋ 新增任务</button>
        </div>
        <div class="card">
          <table>
            <thead><tr><th>任务</th><th>启用</th><th>token 前缀</th><th>绑定</th><th>最近使用</th><th style="text-align:right">操作</th></tr></thead>
            <tbody>
              <tr v-for="t in tasks" :key="t.id">
                <td style="font-weight:500">{{ t.name }}</td>
                <td><label class="switch"><input type="checkbox" :checked="t.enabled" @change="toggleTask(t)"><i></i></label></td>
                <td><code>{{ t.token_prefix }}…</code></td>
                <td>
                  <div v-for="b in t.bindings" :key="b.template_id" class="muted" style="line-height:1.8">
                    {{ b.template_name }} → {{ b.channel_names ? b.channel_names.join('、') : b.channel_ids.join(', ') }}
                  </div>
                </td>
                <td class="muted" style="white-space:nowrap">{{ t.token_last_used_at || '未使用' }}</td>
                <td>
                  <div class="row" style="justify-content:flex-end">
                    <button class="ghost" @click="openTest(t)">测试</button>
                    <button class="ghost" @click="regenToken(t)">重置 token</button>
                    <button class="ghost" @click="editTask(t)">编辑</button>
                    <button class="danger" @click="delTask(t)">删除</button>
                  </div>
                </td>
              </tr>
              <tr v-if="!tasks.length"><td colspan="6" class="muted" style="text-align:center;padding:30px 0">暂无任务</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 推送日志 -->
      <template v-if="tab === 'logs'">
        <div class="page-head">
          <h2>推送日志</h2>
          <div class="row">
            <select v-model="logFilter.status" @change="loadLogs" style="width:auto">
              <option value="">全部状态</option><option value="ok">成功</option><option value="failed">失败</option>
            </select>
            <button class="ghost" @click="loadLogs">刷新</button>
          </div>
        </div>
        <div class="card">
          <table>
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
              <tr v-if="!logs.length"><td colspan="7" class="muted" style="text-align:center;padding:30px 0">暂无日志</td></tr>
            </tbody>
          </table>
        </div>
      </template>

      <!-- 设置 -->
      <template v-if="tab === 'settings'">
        <div class="page-head"><h2>站点与系统设置</h2></div>
        <div class="card" style="max-width:560px">
          <div class="form-grid">
            <label class="field">站点标题 <input v-model="settings.site_title" /></label>
            <label class="field">标语 <input v-model="settings.site_slogan" /></label>
            <label class="field">主题色
              <div class="row">
                <input type="color" :value="settings.theme_color || '#10b981'"
                       @input="settings.theme_color = $event.target.value" style="width:46px;padding:2px" />
                <input v-model="settings.theme_color" placeholder="#10b981" style="flex:1" />
              </div>
            </label>
            <label class="field">日志保留天数 <input v-model="settings.log_retention_days" type="number" /></label>
          </div>
          <button style="margin-top:16px" @click="saveSettings">保存设置</button>
        </div>
      </template>
    </main>
  </div>

  <!-- 会话探测中：刷新时不闪登录页 -->
  <div v-else class="login-wrap"><div class="splash">🐦</div></div>

  <!-- 渠道弹窗 -->
  <div v-if="modal === 'channel'" class="overlay" @click.self="closeModal">
    <div class="modal">
      <div class="modal-head"><h3>{{ chForm.id ? '编辑渠道 · ' + chForm.name : '新增渠道' }}</h3>
        <button class="modal-x" @click="closeModal">✕</button></div>
      <div class="modal-body">
        <div class="form-grid">
          <label class="field">类型
            <select v-model="chForm.type">
              <option v-for="t in CHANNEL_TYPES" :key="t" :value="t">{{ CHANNEL_TYPE_LABELS[t] }}（{{ t }}）</option>
            </select>
          </label>
          <label class="field">名称 <input v-model="chForm.name" placeholder="如：运维钉钉群" /></label>
          <label class="field" v-for="[k, label] in chFields" :key="k">{{ label }}
            <input v-model="chForm.config[k]" :placeholder="k" />
          </label>
          <label class="field full">代理（可选，Telegram 等境外渠道需要）
            <input v-model="chForm.proxy" placeholder="http://127.0.0.1:7890 或 socks5://…" />
          </label>
          <div class="field full spread" style="flex-direction:row;align-items:center">
            <span style="font-size:13px;color:var(--text)">启用该渠道</span>
            <label class="switch"><input type="checkbox" v-model="chForm.enabled"><i></i></label>
          </div>
        </div>
      </div>
      <div class="modal-foot">
        <button class="ghost" @click="closeModal">取消</button>
        <button @click="saveChannel">保存</button>
      </div>
    </div>
  </div>

  <!-- 模板弹窗 -->
  <div v-if="modal === 'template'" class="overlay" @click.self="closeModal">
    <div class="modal wide">
      <div class="modal-head"><h3>{{ tplForm.id ? '编辑模板 · ' + tplForm.name : '新增模板' }}</h3>
        <button class="modal-x" @click="closeModal">✕</button></div>
      <div class="modal-body">
        <div class="form-grid">
          <label class="field">名称 <input v-model="tplForm.name" placeholder="如：巡检报告" /></label>
          <label class="field">标题模板 <input v-model="tplForm.title_tpl" placeholder="巡检:{{host}}" /></label>
          <label class="field full">正文模板
            <textarea v-model="tplForm.body_tpl" rows="3" placeholder="主机 {{host}} 状态 {{status}}"></textarea>
          </label>
        </div>
        <div style="margin-top:14px">
          <div class="spread" style="margin-bottom:8px">
            <span class="muted">变量定义（调用方未传时使用默认值）</span>
            <button class="ghost" style="padding:4px 12px" @click="addTplVar()">＋ 变量</button>
          </div>
          <div class="var-row" v-for="(v, i) in tplVars" :key="i">
            <input v-model="v.key" placeholder="变量名（host）" style="font-family:monospace" />
            <input v-model="v.desc" placeholder="说明（主机名）" />
            <input v-model="v.default" placeholder="默认值（pi）" />
            <button class="modal-x" @click="tplVars.splice(i, 1)">✕</button>
          </div>
          <p v-if="!tplVars.length" class="muted">暂无变量。标题/正文里的 <code>{{ '\{\{key\}\}' }}</code> 需在此定义（无默认值时调用方必须传）。</p>
          <div v-if="undefinedVars.length" class="hint">
            模板中使用了未定义的变量，点击添加：
            <span class="chip mini" v-for="u in undefinedVars" :key="u" style="margin:0 3px" @click="addTplVar(u)">{{ u }}</span>
          </div>
        </div>
      </div>
      <div class="modal-foot">
        <button class="ghost" @click="closeModal">取消</button>
        <button @click="saveTemplate">保存</button>
      </div>
    </div>
  </div>

  <!-- 任务弹窗 -->
  <div v-if="modal === 'task'" class="overlay" @click.self="closeModal">
    <div class="modal wide">
      <div class="modal-head"><h3>{{ taskForm.id ? '编辑任务 · ' + taskForm.name : '新增任务' }}</h3>
        <button class="modal-x" @click="closeModal">✕</button></div>
      <div class="modal-body">
        <label class="field">任务名 <input v-model="taskForm.name" placeholder="如：运维通知" style="max-width:280px" /></label>
        <div style="margin-top:14px">
          <div class="spread" style="margin-bottom:8px">
            <span class="muted">绑定（模板 → 渠道组，可多条；调用方只能发绑定的模板）</span>
            <button class="ghost" style="padding:4px 12px" @click="addBinding">＋ 绑定</button>
          </div>
          <div v-for="(b, i) in taskForm.bindings" :key="i" class="binding">
            <div class="binding-head">
              <select v-model="b.template_id">
                <option value="" disabled>选择模板</option>
                <option v-for="t in templates" :key="t.id" :value="t.id">{{ t.name }}</option>
              </select>
              <span class="badge" v-if="b.channel_ids.length">已选 {{ b.channel_ids.length }} 个渠道</span>
              <button class="modal-x" style="margin-left:auto" @click="taskForm.bindings.splice(i, 1)">✕</button>
            </div>
            <div class="chips">
              <span v-for="c in channels" :key="c.id" class="chip"
                    :class="{ on: b.channel_ids.includes(c.id) }" @click="toggleChannel(b, c.id)">{{ c.name }}</span>
              <span v-if="!channels.length" class="muted">先去渠道页添加渠道</span>
            </div>
          </div>
          <p v-if="!taskForm.bindings.length" class="muted">至少一条绑定：选模板 → 点选渠道。</p>
        </div>
      </div>
      <div class="modal-foot">
        <button class="ghost" @click="closeModal">取消</button>
        <button @click="saveTask">{{ taskForm.id ? '保存修改' : '创建任务' }}</button>
      </div>
    </div>
  </div>

  <!-- 新 token 弹窗（只显示一次） -->
  <div v-if="newTaskToken" class="overlay" @click.self="newTaskToken = ''">
    <div class="modal">
      <div class="modal-head"><h3>🔑 任务 token（只显示这一次）</h3>
        <button class="modal-x" @click="newTaskToken = ''">✕</button></div>
      <div class="modal-body">
        <p class="muted">请立即保存到脚本/密码管理器，关闭后无法再次查看：</p>
        <p><code style="user-select:all;display:block;padding:10px;word-break:break-all">{{ newTaskToken }}</code></p>
        <pre class="muted" style="white-space:pre-wrap;background:var(--bg);padding:10px;border-radius:8px">{{ curlExample }}</pre>
      </div>
      <div class="modal-foot">
        <button class="ghost" @click="copyToken">复制 token</button>
        <button @click="newTaskToken = ''">我已保存，关闭</button>
      </div>
    </div>
  </div>

  <!-- 任务测试弹窗：选模板 + 填测试变量，结果留在弹窗内 -->
  <div v-if="modal === 'test'" class="overlay" @click.self="closeModal">
    <div class="modal">
      <div class="modal-head"><h3>🧪 测试任务 · {{ testModal.task?.name }}</h3>
        <button class="modal-x" @click="closeModal">✕</button></div>
      <div class="modal-body">
        <label class="field">模板
          <select v-model="testModal.template_id" @change="resetTestVars" style="max-width:280px">
            <option v-for="b in testModal.task?.bindings" :key="b.template_id" :value="b.template_id">{{ b.template_name }}</option>
          </select>
        </label>
        <div v-if="testTplDef?.variables?.length" style="margin-top:12px">
          <span class="muted">测试变量（留空用模板默认值）</span>
          <div class="form-grid" style="margin-top:8px">
            <label v-for="v in testTplDef.variables" :key="v.key" class="field">
              {{ v.key }}{{ v.desc ? '（' + v.desc + '）' : '' }}
              <input v-model="testModal.vars[v.key]" :placeholder="v.default ? '默认 ' + v.default : ''" />
            </label>
          </div>
        </div>
        <p v-else class="muted" style="margin-top:12px">该模板未定义变量</p>
        <div v-if="testModal.results" style="margin-top:14px">
          <span class="muted">发送结果</span>
          <div v-for="(r, i) in testModal.results" :key="i" class="binding" style="margin-top:8px">
            <div class="row">
              <b style="font-size:13.5px">{{ r.channel }}</b>
              <span class="tag" v-if="r.status === 'ok'">ok</span><span class="tag red" v-else>failed</span>
              <span class="muted" style="margin-left:auto">{{ r.cost_ms }}ms{{ r.via_proxy ? ' · 经代理' : '' }}</span>
            </div>
            <p v-if="r.error" class="muted" style="margin:6px 0 0;word-break:break-all">{{ r.error }}</p>
          </div>
        </div>
      </div>
      <div class="modal-foot">
        <button class="ghost" @click="closeModal">关闭</button>
        <button :disabled="testModal.sending" @click="sendTest">{{ testModal.sending ? '发送中…' : '发送测试' }}</button>
      </div>
    </div>
  </div>

  <!-- 通用确认弹窗（替代浏览器 confirm） -->
  <div v-if="confirmState.show" class="overlay" @click.self="confirmState.show = false">
    <div class="modal" style="width:400px">
      <div class="modal-head"><h3>{{ confirmState.title }}</h3>
        <button class="modal-x" @click="confirmState.show = false">✕</button></div>
      <div class="modal-body"><p style="margin:0;line-height:1.8">{{ confirmState.message }}</p></div>
      <div class="modal-foot">
        <button class="ghost" @click="confirmState.show = false">取消</button>
        <button class="danger" style="background:var(--danger);border-color:var(--danger);color:#fff" @click="runConfirm">确认</button>
      </div>
    </div>
  </div>

  <div v-if="toast" class="toast">{{ toast }}</div>
</template>
