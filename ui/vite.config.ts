import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Tauri reads the dev server from this fixed port; see src-tauri/tauri.conf.json.
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // WebView2 on Windows and WKWebView on macOS both handle modern output, so there is
    // no reason to ship downlevelled bundles to a browser we control.
    target: "esnext",
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    globals: true,
  },
});
