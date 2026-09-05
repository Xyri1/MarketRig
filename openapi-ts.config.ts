export default {
  input: "openapi.json",
  output: "src/client",
  plugins: [
    "@hey-api/typescript",
    { name: "@hey-api/sdk", operations: { strategy: "flat" } },
    { name: "@hey-api/client-fetch", runtimeConfigPath: "./src/hey-api.ts" },
  ],
};
