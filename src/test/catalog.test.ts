import { describe, expect, it } from "vitest";
import { parse } from "@vue/compiler-sfc";
import en from "../locales/en.json";

// Every rendered component, as raw source. Vite's glob keeps this free of node:fs.
const sources: Record<string, string> = {
  ...import.meta.glob("../components/**/*.vue", {
    query: "?raw",
    import: "default",
    eager: true,
  }),
  ...import.meta.glob("../App.vue", {
    query: "?raw",
    import: "default",
    eager: true,
  }),
};

const LETTER = /\p{L}/u;
const TEXT_ATTRS = new Set(["title", "placeholder", "aria-label"]);

type Node = {
  type: number;
  content?: string;
  children?: Node[];
  branches?: Node[];
  props?: { type: number; name?: string; value?: { content: string } }[];
};

function bareStrings(node: Node, found: string[]): string[] {
  if (node.type === 2 && node.content && LETTER.test(node.content)) {
    found.push(`text ${JSON.stringify(node.content.trim())}`);
  }
  for (const prop of node.props ?? []) {
    // type 6 is a static attribute; a binding is type 7 and always fine.
    if (
      prop.type === 6 &&
      TEXT_ATTRS.has(prop.name ?? "") &&
      prop.value &&
      LETTER.test(prop.value.content)
    ) {
      found.push(`attribute ${prop.name}="${prop.value.content}"`);
    }
  }
  for (const child of [...(node.children ?? []), ...(node.branches ?? [])])
    bareStrings(child, found);
  return found;
}

function lookup(key: string): unknown {
  return key
    .split(".")
    .reduce<unknown>(
      (at, part) =>
        at && typeof at === "object"
          ? (at as Record<string, unknown>)[part]
          : undefined,
      en,
    );
}

describe("the en catalog", () => {
  it("has at least one component to check", () => {
    expect(Object.keys(sources).length).toBeGreaterThan(0);
  });

  for (const [path, source] of Object.entries(sources)) {
    it(`${path} carries no bare string`, () => {
      const ast = parse(source).descriptor.template?.ast as Node | undefined;
      expect(ast ? bareStrings(ast, []) : []).toEqual([]);
    });

    it(`${path} names only keys that exist`, () => {
      const missing = [...source.matchAll(/\bt\(\s*['"]([^'"]+)['"]/g)]
        .map((m) => m[1])
        .filter((key) => typeof lookup(key) !== "string");
      expect(missing).toEqual([]);
    });
  }
});
