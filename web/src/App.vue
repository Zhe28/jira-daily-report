<template>
  <el-container v-if="online" style="height: 100vh">
    <el-aside width="200px" style="background: #001529">
      <el-menu
        :default-active="route.path"
        router
        background-color="#001529"
        text-color="#ffffffb3"
        active-text-color="#409eff"
        style="height: 100%; border: none"
      >
        <el-menu-item index="/">
          <span>状态页</span>
        </el-menu-item>
        <el-menu-item index="/config">
          <span>配置页</span>
        </el-menu-item>
      </el-menu>
    </el-aside>
    <el-container>
      <el-header style="display: flex; align-items: center; justify-content: flex-end; border-bottom: 1px solid #eee">
        <span style="display: inline-block; width: 8px; height: 8px; border-radius: 50%; background: #67c23a; margin-right: 8px"></span>
        <span style="font-size: 13px; color: #909399">在线</span>
      </el-header>
      <el-main>
        <router-view />
      </el-main>
    </el-container>
  </el-container>
  <el-result
    v-else
    icon="warning"
    title="连接失败"
    sub-title="daily-report 未运行或端口被占用"
    style="height: 100vh; display: flex; justify-content: center; align-items: center"
  />
</template>

<script setup>
import { ref, onMounted, onUnmounted, provide } from 'vue'
import { useRoute } from 'vue-router'
import { getHealth } from './api.js'

const route = useRoute()
const online = ref(true)
provide('online', online)

let timer = null

async function checkHealth() {
  try {
    await getHealth()
    online.value = true
  } catch {
    online.value = false
  }
}

onMounted(() => {
  checkHealth()
  timer = setInterval(checkHealth, 3000)
})

onUnmounted(() => {
  if (timer) clearInterval(timer)
})
</script>

<style>
body { margin: 0; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; }
</style>
