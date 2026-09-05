import { vi } from "vitest";

export type FakeRequest = {
  method: string;
  path: string;
  query: URLSearchParams;
  body: string | null;
};

export type FakeRoute = (request: FakeRequest) => {
  status: number;
  body?: unknown;
};

/** Stubs `fetch`, keyed by `"<METHOD> <path>"`; a route may throw to fail. */
export function installFakeDaemon(routes: Record<string, FakeRoute>): void {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(
      typeof input === "string" ? input : ((input as Request).url ?? input),
    );
    const method = (init?.method ?? "GET").toUpperCase();
    const route = routes[`${method} ${url.pathname}`];
    if (!route) throw new Error(`no fake route for ${method} ${url.pathname}`);
    const answer = route({
      method,
      path: url.pathname,
      query: url.searchParams,
      body: typeof init?.body === "string" ? init.body : null,
    });
    return new Response(
      answer.body === undefined ? null : JSON.stringify(answer.body),
      {
        status: answer.status,
        headers: { "content-type": "application/json" },
      },
    );
  }) as typeof fetch;
}

type Listener = ((event: unknown) => void) | null;

/** The `WebSocket` the composables see; the test drives it by hand. */
export class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  static OPEN = 1;

  readonly url: string;
  readonly sent: unknown[] = [];
  binaryType = "blob";
  readyState = 0;
  onopen: Listener = null;
  onmessage: Listener = null;
  onclose: Listener = null;
  onerror: Listener = null;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(data: unknown): void {
    this.sent.push(data);
  }

  /** Drive: the socket connected. */
  open(): void {
    this.readyState = 1;
    this.onopen?.({});
  }

  /** Drive: a frame arrived (a string, or an ArrayBuffer for PTY bytes). */
  message(data: unknown): void {
    this.onmessage?.({ data });
  }

  /** Both the production `close()` and the test's "the daemon closed it". */
  close(code = 1000): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.onclose?.({ code });
  }
}

export function installFakeWebSocket(): void {
  FakeWebSocket.instances = [];
  globalThis.WebSocket = FakeWebSocket as unknown as typeof WebSocket;
}

/** The `@tauri-apps/api/core` `invoke`; tests mock the module onto it. */
export const fakeInvoke = vi.fn();
