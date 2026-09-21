import { createRouter, createWebHistory } from 'vue-router'
import StatusPage from './views/StatusPage.vue'
import ConfigPage from './views/ConfigPage.vue'

const routes = [
  { path: '/', component: StatusPage },
  { path: '/config', component: ConfigPage }
]

export default createRouter({
  history: createWebHistory(),
  routes
})
