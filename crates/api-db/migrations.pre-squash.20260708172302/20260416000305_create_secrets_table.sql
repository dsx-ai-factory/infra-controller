-- Encrypted credential storage, replacing Vault KV.
--
-- Each row is one envelope-encrypted credential value: the value is
-- encrypted with a per-row data encryption key (DEK), and the DEK is in turn
-- wrapped by a key encryption key (KEK) that lives outside the database --
-- in carbide's own key config or an external KMS. kek_id records which KEK
-- wrapped this row's DEK so rotation can find and re-wrap rows in place.
--
-- The table is an append-only journal: every write inserts a new row, and a
-- read returns the newest row for the path. seq is the journal order --
-- created_at cannot be, because Postgres fixes now() per transaction, so
-- two writes in one transaction record the same timestamp. Older rows are
-- kept so that credential rotation can roll back by deleting the newest
-- entry.
--
-- Paths beginning with "/" are internal bookkeeping entries (the vault
-- import marker), not credentials; real credential paths never start with
-- a slash.
CREATE TABLE secrets (
    secret_id        UUID PRIMARY KEY,
    -- UNIQUE because seq is the sole cursor for paginated journal walks
    -- (find_batch_after's `seq > $1`), the ORDER BY key, and the
    -- newest-per-path selector. IDENTITY alone does not guarantee
    -- uniqueness (a sequence reset or OVERRIDING SYSTEM VALUE insert could
    -- duplicate it), and a duplicate would make pagination skip or repeat
    -- rows.
    seq              BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    path             TEXT NOT NULL,
    encrypted_value  BYTEA NOT NULL,
    nonce            BYTEA NOT NULL,
    kek_id           TEXT NOT NULL,
    encrypted_dek    BYTEA NOT NULL,
    dek_nonce        BYTEA NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Serves the hot read path: newest row for a path.
CREATE INDEX idx_secrets_path_latest ON secrets (path, seq DESC);

-- Serves re-wrap scans: all rows wrapped by a given KEK.
CREATE INDEX idx_secrets_kek_id ON secrets (kek_id);
