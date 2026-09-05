export type DaemonEndpoint = {
  port: number;
  bearer: string;
  daemon_uuid: string;
};

// In memory only, never persisted (per feature SPEC §6.2). `useDaemon` sets it.
export let endpoint: DaemonEndpoint | null = null;

export function setEndpoint(next: DaemonEndpoint | null): void {
  endpoint = next;
}
