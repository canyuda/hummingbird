// Vite 配置：构建产物输出到 dist/（rust-embed 从这里内嵌进二进制）
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    chunkSizeWarningLimit: 300
  }
})
