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

// The two button skins of §6.5, here rather than in a stylesheet a component
// may not carry.
export const button =
  "rounded-control border border-line px-2 py-1 disabled:opacity-50 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent";
export const buttonPrimary = `${button} bg-accent text-accent-ink`;
