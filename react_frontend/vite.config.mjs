import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  build: {
    outDir: "build",
  },
  envPrefix: ["VITE_", "REACT_APP_"],
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
  },
});
