/**
 * The packaged desktop smoke (R5 feature SPEC §7.3), operator-run and never in
 * CI. It drives the bundle `pnpm build` produced with `--features wdio`, which
 * is the only build carrying `tauri-plugin-wdio-webdriver` — the embedded
 * WebDriver server `driverProvider: 'embedded'` connects to. macOS has no
 * external WebKit driver and the CrabNebula one is paid, so embedded is the
 * only provider that works on both platforms, and one mode is one config.
 */
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { join, resolve } from "node:path";

import type { TauriCapabilities } from "@wdio/tauri-service";

import { logDir, wipe } from "./smoke/wipe";

const win = process.platform === "win32";

/**
 * `src-tauri` is a member of the root Cargo workspace, so its artifacts land in
 * the workspace `target/` — there is no `src-tauri/target/`.
 */
const target = resolve(process.env.CARGO_TARGET_DIR ?? "target");
const release = join(target, "release");

export const application = win
  ? join(release, "MarketRig.exe")
  : join(
      release,
      "bundle",
      "macos",
      "MarketRig.app",
      "Contents",
      "MacOS",
      "MarketRig",
    );

/** `runtime-standin`, the desk's runtime for the whole run (R3 SPEC §9.1). */
export const standin = join(
  release,
  win ? "runtime-standin.exe" : "runtime-standin",
);

// The launcher and its worker each load this file, so the stamp travels on the
// environment: both write into one evidence directory.
const stamp = (process.env.MARKETRIG_SMOKE_STAMP ??= new Date()
  .toISOString()
  .replace(/[:.]/g, "-")
  .replace("Z", ""));
const evidence = join(
  target,
  "acceptance",
  `smoke-${process.platform}-${stamp}`,
);

// The shell inherits this process's environment and passes it to the daemon it
// spawns, which passes it to every runtime launch — so one assignment here arms
// the stand-in's script for the whole run, exactly as the gate's harness does.
process.env.MARKETRIG_STANDIN_SCRIPT = resolve("smoke", "standin-script.json");

const capability: TauriCapabilities = {
  browserName: "tauri",
  "tauri:options": { application },
};

export const config: WebdriverIO.Config = {
  runner: "local",
  specs: ["./smoke/smoke.spec.ts"],
  maxInstances: 1,
  capabilities: [capability],
  services: [["@wdio/tauri-service", { driverProvider: "embedded" }]],
  framework: "mocha",
  mochaOpts: { ui: "bdd", timeout: 120_000 },
  // `outputDir` on the reporter writes the run's report beside the runner logs,
  // so the evidence directory needs no copy of its own.
  reporters: [["spec", { outputDir: evidence }]],
  outputDir: evidence,
  logLevel: "info",
  waitforTimeout: 30_000,

  onPrepare(): void {
    wipe();
    mkdirSync(evidence, { recursive: true });
    execFileSync(
      "cargo",
      ["build", "--release", "-p", "marketrig-acceptance"],
      {
        stdio: "inherit",
      },
    );
  },

  onComplete(): void {
    try {
      copyFileSync(
        join(logDir(), "MarketRig.log"),
        join(evidence, "MarketRig.log"),
      );
    } catch {
      // Step 5 quit the shell; a run that never logged leaves no file.
    }
  },
};
