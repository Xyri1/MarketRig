// xterm.js's renderer does not run in jsdom; `vi.mock` both packages to this.
export class Terminal {
  loadAddon() {}
  open() {}
  write() {}
  onData() {}
  onResize() {}
  dispose() {}
}
export class FitAddon {
  fit() {}
  dispose() {}
  proposeDimensions() {
    return { cols: 80, rows: 24 };
  }
}
/** jsdom has no ResizeObserver; counts live observers. */
export class FakeResizeObserver {
  static observing = 0;
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
