// @vitest-environment node
import { expect, it } from "vitest";
import { Terminal } from "@xterm/xterm";
import { Unicode11Addon } from "@xterm/addon-unicode11";

it("keeps UTF-8 decoding and Unicode widths across split frames before opening a renderer", async () => {
  const term = new Terminal({ allowProposedApi: true });
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = "11";
  const bytes = new TextEncoder().encode("A🚀中e\u0301B");
  try {
    for (const byte of bytes) {
      await new Promise<void>((resolve) =>
        term.write(new Uint8Array([byte]), resolve),
      );
    }
    expect(term.buffer.active.cursorX).toBe(7);
    expect(term.buffer.active.getLine(0)?.translateToString(true)).toBe(
      "A🚀中e\u0301B",
    );
  } finally {
    term.dispose();
  }
});
