import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [
    vue(),
    {
      name: "dbx-build-signal",
      closeBundle() {
        console.log("DBX_UI_BUILD_SUCCESS");
      },
    },
  ],
  build: {
    outDir: "ui",
    emptyOutDir: true,
    target: "es2022",
  },
});
