import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  test: {
    /**
     * Vitest stubs CSS imports to "" by default, which would hide the design
     * tokens from palette.contrast.test.ts — it reads index.css as raw text so
     * the stylesheet stays the single source of truth for the palette. Scoped
     * to that one file; no test imports a stylesheet for its side effects, so
     * nothing else changes.
     */
    css: { include: [/index\.css/] },
    environment: "jsdom",
    setupFiles: ["./vitest.setup.ts"],
  },
});
