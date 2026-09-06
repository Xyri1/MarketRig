import { vi } from "vitest";
import type { ITerminalInitOnlyOptions, ITerminalOptions } from "@xterm/xterm";

// jsdom cannot render; the separate parser test uses the real xterm packages.
export class Terminal {
  unicode = { activeVersion: "6" };
  cols: number;
  rows: number;
  private resized = () => {};
  constructor(public options: ITerminalOptions & ITerminalInitOnlyOptions) {
    this.cols = options.cols ?? 80;
    this.rows = options.rows ?? 24;
  }
  loadAddon(addon: { activate: (term: Terminal) => void }) {
    addon.activate(this);
  }
  open = vi.fn();
  refresh = vi.fn();
  write = vi.fn((_data: string | Uint8Array, callback?: () => void) =>
    callback?.(),
  );
  resize(cols: number, rows: number) {
    this.cols = cols;
    this.rows = rows;
    this.resized();
  }
  onData() {}
  onResize(callback: () => void) {
    this.resized = callback;
  }
  dispose() {}
}
export class FitAddon {
  private term!: Terminal;
  activate(term: Terminal) {
    this.term = term;
  }
  fit = vi.fn(() => this.term.resize(80, 24));
  dispose() {}
  proposeDimensions() {
    return { cols: 80, rows: 24 };
  }
}
export class Unicode11Addon {
  activate() {}
}
export class WebglAddon {
  static fail = false;
  static instances: WebglAddon[] = [];
  loss = () => {};
  constructor() {
    WebglAddon.instances.push(this);
  }
  activate() {
    if (WebglAddon.fail) throw new Error("No WebGL");
  }
  onContextLoss(callback: () => void) {
    this.loss = callback;
  }
  dispose = vi.fn();
}
/** jsdom has no ResizeObserver; counts live observers. */
export class FakeResizeObserver {
  static observing = 0;
  constructor(public callback: () => void) {}
  observe() {
    FakeResizeObserver.observing += 1;
  }
  unobserve() {}
  disconnect() {
    FakeResizeObserver.observing -= 1;
  }
}
globalThis.ResizeObserver =
  FakeResizeObserver as unknown as typeof ResizeObserver;
