import type { Desk } from "./client";

/** The five gutter roles of feature SPEC §6.3, in their precedence order. */
export type DeskState = "live" | "pending" | "attention" | "failure" | "idle";

export function deskState(
  desk: Desk,
  live: boolean,
  pending: number,
  attention: boolean,
): DeskState {
  if (live) return "live";
  if (pending > 0) return "pending";
  if (attention) return "attention";
  if (desk.state === "FAILED" || desk.workspace_status === "UNAVAILABLE")
    return "failure";
  return "idle";
}

/** Written out so Tailwind's scanner sees each utility (per D72: no literal colour). */
export const gutterClass: Record<DeskState, string> = {
  live: "bg-state-live",
  pending: "bg-state-pending",
  attention: "bg-state-attention",
  failure: "bg-state-failure",
  idle: "bg-state-idle",
};

// The control skins of §6.5, here rather than in a stylesheet a component
// may not carry. Every control is 28 px tall so rows of mixed controls align.
const focus =
  "focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent";
export const button = `inline-flex h-7 shrink-0 items-center justify-center gap-1 rounded-control border border-line bg-panel-raised px-2.5 whitespace-nowrap hover:border-line-strong disabled:opacity-50 disabled:hover:border-line ${focus}`;
export const buttonPrimary = `${button} border-accent bg-accent text-accent-ink hover:border-accent hover:opacity-90`;
/** A borderless control for chrome actions that should not compete with data. */
export const buttonQuiet = `inline-flex h-7 shrink-0 items-center justify-center rounded-control px-2 text-ink-muted hover:bg-panel hover:text-ink ${focus}`;
export const input = `terminal h-7 min-w-0 rounded-control border border-line bg-panel-raised px-2 hover:border-line-strong focus:border-accent focus:outline-none`;
// A Select trigger boxed like the text inputs, so an empty value still reads
// as a control; the ▾ its content carries gives it the inputs' height.
export const selectTrigger = `${button} justify-between gap-2 data-[placeholder]:text-ink-muted`;
export const selectContent =
  "z-10 min-w-36 rounded-control border border-line bg-panel-raised p-1 shadow-lg";
export const selectItem =
  "terminal cursor-default rounded-control px-2 py-1 outline-none data-[highlighted]:bg-panel data-[state=checked]:text-accent";
/** A section title: prose at the base size, never smaller (§6.5). */
export const eyebrow = "font-semibold text-ink-muted";
/** A machine token shown as a small bordered pill. */
export const chip =
  "terminal inline-flex h-5 shrink-0 items-center rounded-pill border border-line px-1.5 text-xs text-ink-muted";
export const dialogOverlay = "fixed inset-0 z-20 bg-well/40";
export const dialogContent =
  "fixed top-1/2 left-1/2 z-30 flex w-80 -translate-x-1/2 -translate-y-1/2 flex-col gap-4 rounded-panel border border-line bg-panel-raised p-5 shadow-xl";
/** Failure text: the daemon's message, verbatim, in the failure role. */
export const failureText =
  "terminal text-sm wrap-anywhere whitespace-pre-wrap text-state-failure";
