/**
 * The packaged smoke is the one leg that touches the real per-user root (root
 * SPEC §17), so it starts by deleting it. Guarded by an explicit environment
 * variable: an operator who runs `pnpm smoke` without meaning it loses nothing.
 */
import { execFileSync } from "node:child_process";
import { rmSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const win = process.platform === "win32";

/** The daemon's data root — `Roots::resolve` and the shell's `data_root()`. */
export function dataRoot(): string {
  if (win && !process.env.LOCALAPPDATA)
    throw new Error("LOCALAPPDATA is not set: the roots would be relative");
  return win
    ? join(process.env.LOCALAPPDATA ?? "", "MarketRig")
    : join(homedir(), "Library", "Application Support", "MarketRig");
}

export function endpointPath(): string {
  return join(dataRoot(), "runtime", "endpoint.json");
}

/**
 * `tauri-plugin-log`'s `LogDir` target, which is Tauri's `app_log_dir()`: it
 * keys on the bundle *identifier*, not the product name.
 */
export function logDir(): string {
  return win
    ? join(process.env.LOCALAPPDATA ?? "", "dev.marketrig.desktop", "logs")
    : join(homedir(), "Library", "Logs", "dev.marketrig.desktop");
}

/** Every process the smoke owns; `running()` is also step 5's assertion. */
const PROCESSES = ["marketrig-desktop", "marketrigd", "runtime-standin"];

export function running(): string[] {
  return PROCESSES.filter((name) => {
    try {
      if (win) {
        // `tasklist` exits 0 with a "no tasks" line, so the name must be read.
        const listed = execFileSync(
          "tasklist",
          ["/FI", `IMAGENAME eq ${name}.exe`, "/NH"],
          { encoding: "utf8" },
        );
        return listed.includes(`${name}.exe`);
      }
      execFileSync("pgrep", ["-x", name], { stdio: "ignore" });
      return true;
    } catch {
      return false;
    }
  });
}

export function wipe(): void {
  if (process.env.MARKETRIG_SMOKE_WIPE !== "1") {
    throw new Error(
      "the smoke deletes the real MarketRig data root, log directory, and ~/.marketrig: set MARKETRIG_SMOKE_WIPE=1 to allow it",
    );
  }
  for (const name of PROCESSES) {
    try {
      if (win)
        execFileSync("taskkill", ["/F", "/IM", `${name}.exe`], {
          stdio: "ignore",
        });
      else execFileSync("pkill", ["-x", name], { stdio: "ignore" });
    } catch {
      // Nothing of that name was running, which is the wanted state.
    }
  }
  // The daemon's own log root sits beside the shell's on macOS and inside the
  // data root on Windows (`Roots::resolve`).
  const daemonLogs = join(homedir(), "Library", "Logs", "MarketRig");
  for (const dir of [
    dataRoot(),
    logDir(),
    daemonLogs,
    join(homedir(), ".marketrig"),
  ]) {
    rmSync(dir, { recursive: true, force: true });
  }
}
