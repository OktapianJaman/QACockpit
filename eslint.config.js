import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: ["dist", "node_modules", "src-tauri", "*.config.js"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.ts"],
    languageOptions: {
      globals: {
        window: "readonly",
        document: "readonly",
        localStorage: "readonly",
        navigator: "readonly",
        FileReader: "readonly",
        Blob: "readonly",
        FileList: "readonly",
        HTMLElement: "readonly",
        HTMLInputElement: "readonly",
        HTMLSelectElement: "readonly",
        HTMLTextAreaElement: "readonly",
        HTMLButtonElement: "readonly",
        HTMLImageElement: "readonly",
        ClipboardEvent: "readonly",
        DragEvent: "readonly",
        RegExpMatchArray: "readonly",
        console: "readonly",
      },
    },
    rules: {
      // The codebase intentionally uses `any` at the Tauri IPC boundary.
      "@typescript-eslint/no-explicit-any": "off",
    },
  }
);
