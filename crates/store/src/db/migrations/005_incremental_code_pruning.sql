-- Make account-code pruning churn-driven. Previously each prune re-scanned every account row still
-- valid past the cutoff. A code pinned at one prune can only become collectable at a later prune if
-- its maximal referencing interval ends between the two cutoffs, so it suffices to scan rows whose
-- `valid_until` crossed the cutoff since the previous prune and probe each candidate code for a
-- surviving reference.

-- Existence probe for candidate codes: for a given `code_commitment`, is there any row whose
-- validity interval reaches past the cutoff (`valid_until > cutoff`)?
CREATE INDEX idx_accounts_code_probe
    ON accounts(code_commitment, valid_until)
    WHERE code_commitment IS NOT NULL;

-- Single-row (id = 0) record of the cutoff through which account-code pruning has completed.
-- Updated in the same transaction as the prune itself, so it is exact and crash-consistent. The
-- row is absent until the first prune under this schema, which runs a full (non-windowed) pass.
CREATE TABLE prune_progress (
    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    codes_cutoff BIGINT NOT NULL
);
