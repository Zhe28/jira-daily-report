<template>
  <div>
    <el-row :gutter="16">
      <el-col :span="12">
        <el-card shadow="never">
          <template #header>今天 ({{ data.today }})</template>
          <div>
            <el-tag :type="tagType(dayToday?.status)" size="large">{{ dayToday?.status }}</el-tag>
            <span v-if="dayToday?.reason" style="margin-left: 8px; color: #909399">{{ dayToday.reason }}</span>
          </div>
          <div v-if="dayToday?.worklogs && Object.keys(dayToday.worklogs).length" style="margin-top: 12px">
            <div v-for="(sec, key) in dayToday.worklogs" :key="key" style="font-size: 13px; color: #606266">
              {{ key }}: {{ (sec / 3600).toFixed(1) }}h
            </div>
          </div>
        </el-card>
      </el-col>
      <el-col :span="12">
        <el-card shadow="never">
          <template #header>昨天 ({{ data.yesterday }})</template>
          <div>
            <el-tag :type="tagType(dayYesterday?.status)" size="large">{{ dayYesterday?.status }}</el-tag>
            <span v-if="dayYesterday?.reason" style="margin-left: 8px; color: #909399">{{ dayYesterday.reason }}</span>
          </div>
          <div v-if="dayYesterday?.worklogs && Object.keys(dayYesterday.worklogs).length" style="margin-top: 12px">
            <div v-for="(sec, key) in dayYesterday.worklogs" :key="key" style="font-size: 13px; color: #606266">
              {{ key }}: {{ (sec / 3600).toFixed(1) }}h
            </div>
          </div>
        </el-card>
      </el-col>
    </el-row>

    <el-row :gutter="16" style="margin-top: 16px">
      <el-col :span="12">
        <el-card shadow="never">
          <template #header>下次写日志</template>
          <div style="font-size: 16px">{{ data.next_trigger }}</div>
          <div style="font-size: 13px; color: #909399; margin-top: 4px">将处理: {{ data.next_target }}</div>
        </el-card>
      </el-col>
      <el-col :span="12">
        <el-card shadow="never">
          <template #header>日志目录</template>
          <div style="display: flex; align-items: center; gap: 8px">
            <span style="font-size: 13px; word-break: break-all">{{ data.log_dir }}</span>
            <el-button size="small" @click="copyLogDir">复制</el-button>
          </div>
        </el-card>
      </el-col>
    </el-row>

    <el-row style="margin-top: 16px">
      <el-col :span="24">
        <el-card shadow="never">
          <template #header>最近一次执行</template>
          <div v-if="!data.last_run" style="color: #909399">暂无</div>
          <div v-else>
            <div style="margin-bottom: 8px">
              <strong>日期:</strong> {{ data.last_run.date }}
              <el-tag v-if="data.last_run.skipped_reason" type="warning" size="small" style="margin-left: 8px">
                跳过: {{ data.last_run.skipped_reason }}
              </el-tag>
            </div>
            <div v-if="data.last_run.error" style="color: #f56c6c; margin-bottom: 8px">
              错误: {{ data.last_run.error }}
            </div>
            <div style="font-size: 13px; color: #606266">
              <span v-if="data.last_run.created?.length">已写入: {{ data.last_run.created.length }} 条 &nbsp;</span>
              <span v-if="data.last_run.planned?.length">计划: {{ data.last_run.planned.length }} 条 &nbsp;</span>
              <span v-if="data.last_run.skipped_existing?.length">跳过(已有): {{ data.last_run.skipped_existing.length }} 条 &nbsp;</span>
              <span v-if="data.last_run.failed?.length" style="color: #f56c6c">失败: {{ data.last_run.failed.length }} 条</span>
            </div>
            <div v-if="data.last_run.failed?.length" style="margin-top: 8px">
              <div v-for="([issue, err], i) in data.last_run.failed" :key="i" style="font-size: 12px; color: #f56c6c">
                {{ issue }}: {{ err }}
              </div>
            </div>
          </div>
        </el-card>
      </el-col>
    </el-row>
  </div>
</template>

<script setup>
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { ElMessage } from 'element-plus'
import { getStatus } from '../api.js'

const data = ref({
  today: '', yesterday: '', days: [],
  next_trigger: '', next_target: '', log_dir: '',
  last_run: null
})

const dayYesterday = computed(() => data.value.days?.[0])
const dayToday = computed(() => data.value.days?.[1])

function tagType(status) {
  if (status === 'processed') return 'success'
  if (status === 'skipped') return 'warning'
  return 'info'
}

function copyLogDir() {
  navigator.clipboard.writeText(data.value.log_dir || '')
  ElMessage.success('已复制')
}

let timer = null

async function load() {
  try {
    data.value = await getStatus()
  } catch { /* offline handled by App.vue */ }
}

onMounted(() => {
  load()
  timer = setInterval(load, 3000)
})

onUnmounted(() => {
  if (timer) clearInterval(timer)
})
</script>
