-- Master encryption key id is stored in each ciphertext envelope (key_encryption v1 JSON).
-- Site config `current_encryption_key_id` selects the key for new encryption only.
ALTER TABLE tenant_identity_config DROP COLUMN encryption_key_id;
