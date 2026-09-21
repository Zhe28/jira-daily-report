import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import fs from 'fs'
import path from 'path'

export default defineConfig({
  plugins: [
    vue(),
    {
      name: 'recreate-placeholder',
      writeBundle() {
        const dir = path.resolve(__dirname, '../crates/daemon/assets/web')
        const placeholder = path.join(dir, '.placeholder')
        if (!fs.existsSync(placeholder)) {
          fs.writeFileSync(placeholder, '')
        }
      }
    }
  ],
  build: {
    outDir: '../crates/daemon/assets/web',
    emptyOutDir: true
  },
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8765'
    }
  }
})
