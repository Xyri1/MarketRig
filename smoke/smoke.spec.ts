/**
 * The packaged desktop smoke — the five steps of R5 feature SPEC §7.3, in
 * order, against the real per-user root `onPrepare` wiped. Every REST call
 * carries the bearer from the daemon's own endpoint file; no `Origin` header is
 * sent, which the origin layer leaves untouched.
 */
import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";

import { application, standin } from "../wdio.conf";
import { endpointPath, running } from "./wipe";

/** The one desk; the stand-in's script reads its quotes resource by this name. */
const DESK = "smoke";

interface Endpoint {
  port: number;
  credential: string;
}

function endpoint(): Endpoint {
  return JSON.parse(readFileSync(endpointPath(), "utf8")) as Endpoint;
}

async function api<T>(
  method: string,
  route: string,
  body?: unknown,
): Promise<{ status: number; body: T }> {
  const { port, credential } = endpoint();
  const answer = await fetch(`http://127.0.0.1:${port}${route}`, {
    method,
    headers: {
      authorization: `Bearer ${credential}`,
      "content-type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await answer.text();
  return { status: answer.status, body: (text ? JSON.parse(text) : null) as T };
}

async function until(
  what: string,
  ready: () => boolean | Promise<boolean>,
  ms = 60_000,
): Promise<void> {
  const deadline = Date.now() + ms;
  for (;;) {
    if (await ready()) return;
    if (Date.now() >= deadline)
      throw new Error(`timed out waiting for ${what}`);
    await new Promise((done) => setTimeout(done, 250));
  }
}

/**
 * The well's glyphs are only readable when xterm's DOM renderer is live: the
 * WebGL renderer draws into a canvas and disposes `.xterm-rows`. Refusing a
 * WebGL context takes the DOM fallback `useTerminal` already carries, which is
 * the only way a WebDriver client can see what the desk's terminal shows.
 */
async function keepTheWellInTheDom(): Promise<void> {
  await browser.execute(() => {
    const real = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (
      this: HTMLCanvasElement,
      ...args: unknown[]
    ) {
      const kind = args[0];
      if (kind === "webgl" || kind === "webgl2") return null;
      return (real as (...a: unknown[]) => unknown).apply(this, args);
    } as typeof real;
  });
}

/** The element's text, or "" while it is not in the DOM yet. */
async function textOf(selector: string): Promise<string> {
  const element = await $(selector);
  return (await element.isExisting()) ? element.getText() : "";
}

async function wellText(): Promise<string> {
  return browser.execute(
    () => document.querySelector('[data-testid="well"]')?.textContent ?? "",
  );
}

describe("the packaged desktop", () => {
  let deskId = "";
  let quit = false;

  before(async () => {
    // Without `tauri-plugin-wdio` the service's focus check before every find
    // and click waits 5 s for a command that never answers; an explicit
    // window switch is what turns that check off (the service's own rule).
    await browser.tauri.switchWindow("main");
  });

  after(async () => {
    // Step 5 quits the application, so the embedded WebDriver server dies with
    // it and WebdriverIO's own teardown DELETE would fail — a failure that can
    // discard the run's results. Drop the session first, either way.
    if (!quit) return;
    await browser.deleteSession().catch(() => undefined);
    (browser as unknown as { sessionId?: string }).sessionId = undefined;
  });

  it("1 — shows the window on Settings with the daemon answering", async () => {
    await $('[data-testid="tab-settings"]').waitForExist({ timeout: 90_000 });
    await keepTheWellInTheDom();

    expect(existsSync(endpointPath())).toBe(true);
    const health = await api<{ daemon_uuid: string }>("GET", "/health");
    expect(health.status).toBe(200);

    // The daemon discovers a real `codex` or `claude` on the login PATH at
    // start (R3 SPEC §2), so an operator's machine is rarely a first launch.
    // Settings is asserted as the auto-selected tab only when it is one; a
    // machine with a real CLI selects it by hand and step 2 overrides `codex`
    // with the stand-in through the explicit-path field.
    const found = await api<{ runtimes: { state: string }[] }>(
      "GET",
      "/runtimes",
    );
    const firstLaunch = !found.body.runtimes.some(
      (row) => row.state === "AVAILABLE",
    );
    const settings = $('[data-testid="tab-settings"]');
    if (firstLaunch) {
      await until(
        "Settings to be the selected tab",
        async () => (await settings.getAttribute("data-state")) === "active",
      );
    } else {
      await settings.click();
    }
    await until(
      "the Settings tab to show",
      async () => await $('[data-testid="runtime-path-codex"]').isExisting(),
    );

    // The wiped root leaves OpenViking UNCONFIGURED: both path fields empty
    // and no Retry (OpenViking feature SPEC §8).
    expect(await $('[data-testid="openviking-python"]').getValue()).toBe("");
    expect(await $('[data-testid="openviking-node"]').getValue()).toBe("");
    expect(await $('[data-testid="openviking-retry"]').isExisting()).toBe(
      false,
    );
  });

  it("2 — registers the stand-in runtime, starts a desk, and shows its banner", async () => {
    await $('[data-testid="runtime-path-codex"]').setValue(standin);
    await $('[data-testid="runtime-submit-codex"]').click();
    // The real `codex` may already be AVAILABLE, so the wait is for the
    // stand-in's path to be the one registered, not for the state alone.
    await until("the stand-in to be codex", async () => {
      const { body } = await api<{
        runtimes: { runtime: string; state: string; executable_path: string }[];
      }>("GET", "/runtimes");
      return body.runtimes.some(
        (row) =>
          row.runtime === "codex" &&
          row.state === "AVAILABLE" &&
          row.executable_path === standin,
      );
    });

    await $('[data-testid="new-desk"]').click();
    await $('[data-testid="new-desk-name"]').setValue(DESK);
    // A machine with a real `claude` offers two runtimes; the desk is the
    // stand-in's, so `codex` is picked when the trigger shows anything else.
    const runtime = $('[data-testid="new-desk-runtime"]');
    if (!(await runtime.getText()).includes("codex")) {
      // A WebDriver click opens the popover and the pointer-up closes it, so
      // the smoke sends what Reka `Select` listens to: pointerdown on the
      // trigger, pointerup on the item.
      await browser.execute(() => {
        const down = new PointerEvent("pointerdown", {
          bubbles: true,
          button: 0,
          pointerType: "mouse",
        });
        document
          .querySelector('[data-testid="new-desk-runtime"]')
          ?.dispatchEvent(down);
      });
      await until(
        "the runtime list to open",
        async () => (await $$('[role="option"]').length) > 0,
        10_000,
      );
      await browser.execute(() => {
        const up = new PointerEvent("pointerup", {
          bubbles: true,
          button: 0,
          pointerType: "mouse",
        });
        [...document.querySelectorAll('[role="option"]')]
          .find((o) => o.textContent?.trim() === "codex")
          ?.dispatchEvent(up);
      });
      await until(
        "the runtime to be codex",
        async () => (await runtime.getText()).includes("codex"),
        10_000,
      );
    }
    await $('[data-testid="new-desk-submit"]').click();
    await until("the desk to be READY", async () => {
      const { body } = await api<{
        desks: { id: string; name: string; state: string }[];
      }>("GET", "/desks");
      const desk = body.desks.find((row) => row.name === DESK);
      deskId = desk?.id ?? "";
      return desk?.state === "READY";
    });

    await $('[data-testid="session-start"]').click();
    // The stand-in reads the desk's quotes resource through the adapter the
    // daemon registered for it and prints the answer as its first PTY line.
    await until("the stand-in's banner in the well", async () =>
      (await wellText()).includes("MCP_READ"),
    );
  });

  it("3 — keeps the webview while hidden and a second launch shows it again", async () => {
    const at = new Date(Date.now() + 3_000).toISOString();
    const created = await api<{ id: string }>(
      "POST",
      `/desks/${deskId}/triggers`,
      { name: "smoke-once", brief: "the smoke's one-off", schedule: { at } },
    );
    expect(created.status).toBe(201);

    await browser.execute(() => {
      (window as unknown as Record<string, unknown>).__marketrigSmokeMarker =
        "kept";
    });
    // `core:window:allow-close` is deliberately not in the capability, and the
    // embedded driver's own close destroys the window instead of requesting a
    // close, so the smoke hides through the permitted command — the same state
    // the shell's prevented `CloseRequested` leaves behind.
    await browser.execute(() => {
      void (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (cmd: string, args: unknown) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__.invoke("plugin:window|hide", { label: "main" });
    });

    // The firing's TRIGGER_RESULT reaches the stand-in while the window is
    // hidden; the warm parser keeps taking bytes.
    await until("the trigger's prompt to reach the stand-in", async () =>
      (await wellText()).includes("INPUT 1:"),
    );

    const second = spawn(application, [], { stdio: "ignore" });
    const code = await new Promise<number | null>((done) => {
      second.once("exit", done);
      setTimeout(() => done(null), 30_000);
    });
    expect(code).not.toBe(null);

    expect(await browser.getWindowHandles()).toContain("main");
    await until(
      "the window to be visible again",
      async () =>
        browser.execute(() =>
          (
            window as unknown as {
              __TAURI_INTERNALS__: {
                invoke: (cmd: string, args: unknown) => Promise<boolean>;
              };
            }
          ).__TAURI_INTERNALS__.invoke("plugin:window|is_visible", {
            label: "main",
          }),
        ),
      10_000,
    );
    expect(
      await browser.execute(
        () =>
          (window as unknown as Record<string, unknown>).__marketrigSmokeMarker,
      ),
    ).toBe("kept");
  });

  it("4 — approves and denies a gated paper order", async () => {
    const set = await api("PUT", "/settings/policies", {
      paper_order_policy: "REQUIRE_APPROVAL",
    });
    expect(set.status).toBe(200);

    const buy = await api<{ approval: string }>(
      "POST",
      `/desks/${deskId}/orders`,
      {
        action_id: "smoke-buy",
        instrument_id: "AAPL.XNAS",
        side: "BUY",
        type: "MARKET",
        quantity: "1",
      },
    );
    expect(buy.status).toBe(202);
    expect(buy.body.approval).toBe("PENDING");

    await $('[data-testid="tab-approvals"]').click();
    await until(
      "one pending approval in the tab",
      async () => (await $$('[data-testid="approval-row"]').length) === 1,
    );
    // `set_tray_pending` is a Tauri command with no DOM, so the tray's own count
    // is unobservable from WebDriver; the desk row's badge is the same number
    // from the same `useApprovals` total.
    await until(
      "the desk row to show one pending approval",
      async () =>
        (await textOf(`[data-testid="desk-pending-${deskId}"]`)) === "1",
    );

    await $('[data-testid="approval-approve"]').click();
    await until("the buy to be APPROVED", async () => {
      const { body } = await api<{
        actions: { action_id: string; approval: string }[];
      }>("GET", `/desks/${deskId}/history/actions`);
      return body.actions.some(
        (row) => row.action_id === "smoke-buy" && row.approval === "APPROVED",
      );
    });
    // The smoke trades on the real feed: off-hours it has no price, and the
    // sandbox's own outcome for a market order is then MARKET_PRICE_UNAVAILABLE.
    // The position is required only while the feed quotes the instrument.
    const quotes = await api<{
      quotes: { instrument_id: string; health: string }[];
    }>("GET", `/desks/${deskId}/market/quotes`);
    const priced = quotes.body.quotes.some(
      (q) => q.instrument_id === "AAPL.XNAS" && q.health !== "UNAVAILABLE",
    );
    await $('[data-testid="tab-desk"]').click();
    if (priced) {
      await until("the filled position in the Desk tab", async () =>
        (await textOf('[data-testid="desk-positions"]')).includes("AAPL.XNAS"),
      );
    } else {
      console.log(
        "smoke: AAPL.XNAS has no price; the sandbox's denial is the outcome",
      );
      await until(
        "the sandbox's own MARKET_PRICE_UNAVAILABLE outcome",
        async () => {
          const { body } = await api<{
            actions: { action_id: string; outcome?: { status?: string } }[];
          }>("GET", `/desks/${deskId}/history/actions`);
          return body.actions.some(
            (row) =>
              row.action_id === "smoke-buy" && row.outcome?.status === "DENIED",
          );
        },
      );
    }

    const sell = await api("POST", `/desks/${deskId}/orders`, {
      action_id: "smoke-deny",
      instrument_id: "AAPL.XNAS",
      side: "SELL",
      type: "MARKET",
      quantity: "1",
    });
    expect(sell.status).toBe(202);
    await $('[data-testid="tab-approvals"]').click();
    await $('[data-testid="approval-deny"]').click();
    await $('[data-testid="dialog-deny"]').click();
    await until("the denied action in the history", async () => {
      const { body } = await api<{
        actions: { action_id: string; approval: string }[];
      }>("GET", `/desks/${deskId}/history/actions`);
      return body.actions.some(
        (row) => row.action_id === "smoke-deny" && row.approval === "DENIED",
      );
    });
  });

  it("5 — quits the daemon, the runtime, and the application", async () => {
    await $('[data-testid="tab-settings"]').click();
    await $('[data-testid="quit"]').click();
    await $('[data-testid="dialog-quit"]').click();
    quit = true;

    await until(
      "the endpoint file to be gone",
      () => !existsSync(endpointPath()),
    );
    await until(
      "every MarketRig process to be gone",
      () => running().length === 0,
    );
  });
});
