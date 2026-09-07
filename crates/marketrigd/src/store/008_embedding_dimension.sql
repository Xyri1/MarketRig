-- Migration 8 — the measured embedding dimension (feature SPEC
-- `openviking-continuity` §2.1, per OV-1): `ov.conf` always writes
-- `embedding.dense.dimension`, because OpenViking silently falls back to 2048
-- for a model outside OpenAI's three named ones. `PUT /memory/provider`
-- measures it once with one embeddings request and stores it here.
-- Forward-only; never edited.

ALTER TABLE memory_provider ADD COLUMN embedding_dimension INTEGER;
