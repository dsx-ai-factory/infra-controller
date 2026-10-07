-- JWT signing key rotation: two slots (encrypted + public JSON per slot), overlap / expiry GC.
-- Replaces flat key_id, algorithm, signing_key_public (PEM), encrypted_signing_key with slotted columns.

CREATE TYPE tenant_identity_current_signing_key_slot_t AS ENUM ('signing_key_1', 'signing_key_2');

ALTER TABLE tenant_identity_config RENAME COLUMN encrypted_signing_key TO encrypted_signing_key_1;

ALTER TABLE tenant_identity_config ADD COLUMN signing_key_public_1 JSONB;

UPDATE tenant_identity_config
SET signing_key_public_1 = jsonb_build_object(
    'v', 1,
    'kid', key_id,
    'alg', algorithm,
    'public_pem', signing_key_public::text
)
WHERE signing_key_public IS NOT NULL;

ALTER TABLE tenant_identity_config DROP COLUMN signing_key_public;
ALTER TABLE tenant_identity_config DROP COLUMN key_id;
ALTER TABLE tenant_identity_config DROP COLUMN algorithm;

-- Either slot may be empty; the active slot must be populated by the application when issuing tokens.
ALTER TABLE tenant_identity_config ALTER COLUMN signing_key_public_1 DROP NOT NULL;
ALTER TABLE tenant_identity_config ALTER COLUMN encrypted_signing_key_1 DROP NOT NULL;

ALTER TABLE tenant_identity_config ADD COLUMN signing_key_public_2 JSONB;
ALTER TABLE tenant_identity_config ADD COLUMN encrypted_signing_key_2 TEXT;

ALTER TABLE tenant_identity_config
    ADD COLUMN current_signing_key_slot tenant_identity_current_signing_key_slot_t NOT NULL DEFAULT 'signing_key_1';

ALTER TABLE tenant_identity_config ADD COLUMN non_active_slot_expires_at TIMESTAMPTZ;
