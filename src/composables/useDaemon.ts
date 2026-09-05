import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { health, quit } from "../client";
import { client } from "../client/client.gen";
import { setEndpoint, type DaemonEndpoint } from "../daemon-endpoint";

export type DaemonStatus = "STARTING" | "READY" | "UNAVAILABLE";

// Module-level singleton: one daemon per window (feature SPEC §6.2).
const status = ref<DaemonStatus>("STARTING");
const endpoint = ref<DaemonEndpoint | null>(null);
const error = ref("");

function apply(next: DaemonEndpoint | null): void {
  endpoint.value = next;
  setEndpoint(next);
  client.setConfig(
    next
      ? {
          baseUrl: `http://127.0.0.1:${next.port}`,
          headers: { Authorization: `Bearer ${next.bearer}` },
        }
      : { baseUrl: undefined, headers: undefined },
  );
}

/** Reads the endpoint file, points the client at it, and matches the UUID. */
async function verify(): Promise<boolean> {
  let read: DaemonEndpoint | null;
  try {
    read = await invoke<DaemonEndpoint | null>("read_endpoint");
  } catch (e) {
    error.value = String(e);
    return false;
  }
  if (!read) {
    error.value = "NO_ENDPOINT";
    return false;
  }
  apply(read);
  try {
    const answer = await health();
    const uuid = (answer.data as { daemon_uuid?: string } | undefined)
      ?.daemon_uuid;
    if (answer.error || !uuid) {
      error.value = "HEALTH_REFUSED";
      return false;
    }
    if (uuid !== read.daemon_uuid) {
      error.value = "DAEMON_UUID_MISMATCH";
      return false;
    }
  } catch (e) {
    error.value = String(e);
    return false;
  }
  error.value = "";
  return true;
}

/** Verify, and on any failure start the daemon exactly once and verify again. */
async function boot(): Promise<void> {
  status.value = "STARTING";
  if (await verify()) {
    status.value = "READY";
    return;
  }
  try {
    apply(await invoke<DaemonEndpoint>("start_daemon"));
  } catch (e) {
    apply(null);
    error.value = String(e);
    status.value = "UNAVAILABLE";
    return;
  }
  status.value = (await verify()) ? "READY" : "UNAVAILABLE";
}

/** `POST /quit`, wait for health to stop answering, then close the shell. */
async function quitAll(): Promise<void> {
  try {
    await quit();
  } catch {
    // The daemon may already be gone; the poll below settles it either way.
  }
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 250));
    try {
      const answer = await health();
      if (answer.error) break;
    } catch {
      break;
    }
  }
  await invoke("exit_app");
}

export function useDaemon() {
  return { status, endpoint, error, verify, boot, retry: boot, quit: quitAll };
}
