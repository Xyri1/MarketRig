import js from "@eslint/js";
import pluginVue from "eslint-plugin-vue";
import tseslint from "typescript-eslint";

export default [
  {
    ignores: [
      "src/client/**",
      "dist/**",
      "node_modules/**",
      "target/**",
      "scripts/**",
      // Upstream's plugin trees, vendored byte for byte (feature SPEC
      // `openviking-continuity` §4.1); linting them would be linting upstream.
      "crates/marketrigd/seed/**",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  // After typescript-eslint, whose base config would otherwise claim .vue files.
  ...pluginVue.configs["flat/essential"],
  {
    files: ["**/*.vue"],
    languageOptions: { parserOptions: { parser: tseslint.parser } },
    // `vue-tsc` is the checker for a block typescript-eslint's own config
    // would have exempted; `no-undef` here only misreads DOM types.
    rules: { "no-undef": "off" },
  },
];
