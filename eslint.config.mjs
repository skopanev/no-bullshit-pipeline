import js from "@eslint/js";
import globals from "globals";

export default [
  {
    ignores: ["src/vendor/**", "src/dist/**", "src-tauri/**", "dist/**"],
  },
  {
    files: ["src/**/*.js"],
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      globals: {
        ...globals.browser,
      },
    },
    ...js.configs.recommended,
    rules: {
      ...js.configs.recommended.rules,
      "no-undef": "off",
      "no-unused-vars": "off",
      "no-useless-assignment": "off",
      "no-redeclare": "error",
      "no-unreachable": "error",
      "no-debugger": "error",
    },
  },
];
