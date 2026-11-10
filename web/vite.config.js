// Vite 配置：构建产物输出到 dist/（rust-embed 从这里内嵌进二进制）
// 本地前端开发：npm run dev 起 5173，/api 自动代理到运行中的蜂鸟后端，
// 代理目标读 web/.env.local 的 HB_DEV_API（该文件不入库，缺省回落本机 8080）
import { defineConfig, loadEnv } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const apiTarget = env.HB_DEV_API || 'http://127.0.0.1:8080'
  return {
    plugins: [vue()],
    server: {
      proxy: {
        '/api': { target: apiTarget, changeOrigin: true },
      },
    },
    build: {
      outDir: 'dist',
      assetsDir: 'assets',
      chunkSizeWarningLimit: 300,
    },
  }
})
