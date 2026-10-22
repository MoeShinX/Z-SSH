// vitest/config 的 defineConfig 多认识 `test` 这一段。
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    // 端口和 tauri.conf.json 的 devUrl 对齐，被占用时直接报错而不是换端口。
    port: 1420,
    strictPort: true,
    // vitest 要读 ../scripts/screenshots 下的截图脚本公共模块。
    fs: { allow: [".."] },
  },
  build: {
    // 只跑在 WebView2 上，不用照顾老浏览器。
    target: "chrome110",
    // 不生成 .map：正式版没有开发者工具，用不上，打进程序白占地方（开发时 vite dev 自带映射，不受影响）。
    sourcemap: false,
  },
  test: {
    // 默认 node：分屏树、布局这些纯函数用不到 DOM。
    // 需要 DOM 的用例在文件头写 @vitest-environment jsdom。
    environment: "node",
    include: [
      "src/**/*.test.ts",
      "src/**/*.test.tsx",
      "../scripts/screenshots/*.test.mjs",
    ],
  },
});
