<template>
  <div style="max-width: 800px">
    <el-form :model="form" label-width="140px" :disabled="loading">
      <el-divider content-position="left">Jira</el-divider>
      <el-form-item label="Base URL" required>
        <el-input v-model="form.jira_base_url" placeholder="http://jira.example.com" />
      </el-form-item>
      <el-form-item label="用户名" required>
        <el-input v-model="form.jira_user" />
      </el-form-item>
      <el-form-item label="密码">
        <el-input
          v-model="form.jira_password"
          type="password"
          show-password
          :placeholder="pwPlaceholder"
        />
      </el-form-item>
      <el-form-item label="Tempo 版本">
        <el-input-number v-model="form.tempo_version" :min="1" :max="10" />
      </el-form-item>
      <el-form-item label="Worker">
        <el-input v-model="form.worker" />
      </el-form-item>

      <el-divider content-position="left">时间</el-divider>
      <el-form-item label="检查时间" required>
        <el-input v-model="form.check_time" placeholder="13:00" style="width: 120px" />
      </el-form-item>
      <el-form-item label="工作开始" required>
        <el-input v-model="form.work_start" placeholder="09:00" style="width: 120px" />
      </el-form-item>
      <el-form-item label="工作结束" required>
        <el-input v-model="form.work_end" placeholder="18:00" style="width: 120px" />
      </el-form-item>
      <el-form-item label="每日总秒数">
        <el-input-number v-model="form.total_daily_seconds" :min="3600" :max="43200" :step="3600" />
      </el-form-item>
      <el-form-item label="工时开始时间">
        <el-input v-model="form.worklog_start" placeholder="留空=使用工作开始时间" style="width: 120px" />
      </el-form-item>

      <el-divider content-position="left">AI</el-divider>
      <el-form-item label="Base URL" required>
        <el-input v-model="form.ai_base_url" placeholder="http://ai/v1" />
      </el-form-item>
      <el-form-item label="API Key">
        <el-input
          v-model="form.ai_api_key"
          type="password"
          show-password
          :placeholder="aiPlaceholder"
        />
      </el-form-item>
      <el-form-item label="模型" required>
        <el-input v-model="form.ai_model" />
      </el-form-item>

      <el-divider content-position="left">仓库映射</el-divider>
      <el-table :data="form.repos" border size="small" style="margin-bottom: 12px">
        <el-table-column label="本地路径" min-width="200">
          <template #default="{ row }">
            <el-input v-model="row.local_path" size="small" />
          </template>
        </el-table-column>
        <el-table-column label="Issue Key" width="150">
          <template #default="{ row }">
            <el-input v-model="row.issue_key" size="small" />
          </template>
        </el-table-column>
        <el-table-column label="Git Email (可选)" width="200">
          <template #default="{ row }">
            <el-input v-model="row.git_email" size="small" placeholder="留空=用 git config" />
          </template>
        </el-table-column>
        <el-table-column width="60">
          <template #default="{ $index }">
            <el-button type="danger" size="small" text @click="form.repos.splice($index, 1)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
      <el-button size="small" @click="form.repos.push({ local_path: '', issue_key: '', git_email: '' })">
        添加仓库
      </el-button>

      <el-divider content-position="left">本地路径</el-divider>
      <el-form-item label="日志目录" required>
        <el-input v-model="form.log_dir" />
      </el-form-item>
      <el-form-item label="节假日目录">
        <el-input v-model="form.holidays_dir" />
      </el-form-item>

      <el-form-item style="margin-top: 24px">
        <el-button type="primary" :loading="saving" @click="onSave">保存</el-button>
      </el-form-item>
    </el-form>
  </div>
</template>

<script setup>
import { ref, reactive, onMounted } from 'vue'
import { ElMessage } from 'element-plus'
import { getConfig, putConfig } from '../api.js'

const TIME_RE = /^([01]\d|2[0-3]):[0-5]\d$/

const loading = ref(true)
const saving = ref(false)
const pwPlaceholder = ref('已配置 ✓（留空保持不变）')
const aiPlaceholder = ref('已配置 ✓（留空保持不变）')

const form = reactive({
  jira_base_url: '', jira_user: '', jira_password: '',
  tempo_version: 4, worker: '',
  check_time: '', work_start: '', work_end: '',
  total_daily_seconds: 28800, worklog_start: '',
  ai_base_url: '', ai_api_key: '', ai_model: '',
  repos: [],
  log_dir: '', holidays_dir: ''
})

onMounted(async () => {
  try {
    const cfg = await getConfig()
    Object.assign(form, {
      jira_base_url: cfg.jira_base_url || '',
      jira_user: cfg.jira_user || '',
      jira_password: '',
      tempo_version: cfg.tempo_version ?? 4,
      worker: cfg.worker || '',
      check_time: cfg.check_time || '',
      work_start: cfg.work_start || '',
      work_end: cfg.work_end || '',
      total_daily_seconds: cfg.total_daily_seconds ?? 28800,
      worklog_start: cfg.worklog_start || '',
      ai_base_url: cfg.ai_base_url || '',
      ai_api_key: '',
      ai_model: cfg.ai_model || '',
      repos: (cfg.repos || []).map(r => ({
        local_path: r.local_path || '',
        issue_key: r.issue_key || '',
        git_email: r.git_email || ''
      })),
      log_dir: cfg.log_dir || '',
      holidays_dir: cfg.holidays_dir || ''
    })
    if (cfg.jira_password_configured) pwPlaceholder.value = '已配置 ✓（留空保持不变）'
    else pwPlaceholder.value = '未配置'
    if (cfg.ai_api_key_configured) aiPlaceholder.value = '已配置 ✓（留空保持不变）'
    else aiPlaceholder.value = '未配置'
  } catch (e) {
    ElMessage.error('加载配置失败: ' + (e.error || e.message || '未知'))
  } finally {
    loading.value = false
  }
})

function validate() {
  if (!form.jira_base_url.trim()) return 'Jira Base URL 不能为空'
  if (!form.jira_user.trim()) return 'Jira 用户名不能为空'
  if (!TIME_RE.test(form.check_time)) return '检查时间格式错误（HH:MM）'
  if (!TIME_RE.test(form.work_start)) return '工作开始时间格式错误'
  if (!TIME_RE.test(form.work_end)) return '工作结束时间格式错误'
  if (form.worklog_start && !TIME_RE.test(form.worklog_start)) return '工时开始时间格式错误'
  if (!form.ai_base_url.trim()) return 'AI Base URL 不能为空'
  if (!form.ai_model.trim()) return 'AI 模型不能为空'
  if (!form.log_dir.trim()) return '日志目录不能为空'
  for (const r of form.repos) {
    if (!r.local_path.trim()) return '仓库本地路径不能为空'
    if (!r.issue_key.trim()) return 'Issue Key 不能为空'
  }
  return null
}

async function onSave() {
  const err = validate()
  if (err) { ElMessage.warning(err); return }
  saving.value = true
  try {
    const body = { ...form }
    // 敏感字段：留空 = 不变（省略或 null）
    if (!body.jira_password) body.jira_password = null
    if (!body.ai_api_key) body.ai_api_key = ''
    await putConfig(body)
    ElMessage.success('已保存并热生效')
    form.jira_password = ''
    form.ai_api_key = ''
  } catch (e) {
    ElMessage.error(e.error || '保存失败')
  } finally {
    saving.value = false
  }
}
</script>
