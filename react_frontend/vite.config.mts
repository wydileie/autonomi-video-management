import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  build: {
    outDir: "build",
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) return undefined;
          if (id.includes("hls.js")) return "hls";
          if (id.includes("react-router")) return "router";
          if (id.includes("/react/") || id.includes("/react-dom/")) return "react";
          return "vendor";
        },
      },
    },
  },
  envPrefix: ["VITE_", "REACT_APP_"],
  plugins: [react()],
  test: {
    coverage: {
      exclude: [
        "build/**",
        "dist/**",
        "public/**",
        "src/__tests__/**",
        "src/**/*.test.ts",
        "src/**/*.test.tsx",
      ],
      include: ["src/**/*.{ts,tsx}"],
      provider: "v8",
      thresholds: {
        lines: 70,
        functions: 70,
        branches: 70,
        statements: 70,
      },
    },
    environment: "jsdom",
    exclude: ["e2e/**", "node_modules/**", "build/**", "dist/**"],
    globals: true,
  },
});
