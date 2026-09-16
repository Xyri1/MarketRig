-- Migration 12 — desk deletion (root SPEC §5.1). Forward-only; never edited.
--
-- One nullable column: NULL is a live desk, an instant is a deleted one, the
-- shape `triggers.deleted_at_ns` already has. No rebuild and no backfill — every
-- existing desk is live. `desks.name` stays a plain UNIQUE column, so a deleted
-- desk keeps its name reserved; a partial unique index over the live rows is the
-- upgrade path when reusing a name matters.
ALTER TABLE desks ADD COLUMN deleted_at_ns INTEGER;
