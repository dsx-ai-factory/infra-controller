CREATE TABLE attestation_secret_ak_pub(
    secret BYTEA NOT NULL,
    ak_pub BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

  PRIMARY KEY(secret),
  CONSTRAINT attestation_secret_ak_pub_unique_secret UNIQUE(secret)
);

CREATE OR REPLACE FUNCTION delete_old_secret_ak_pub() RETURNS TRIGGER
    LANGUAGE plpgsql
    AS $$
BEGIN
  DELETE FROM attestation_secret_ak_pub WHERE created_at < CURRENT_TIMESTAMP - INTERVAL '1 hour';
  RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS trigger_delete_old_secret_ak_pub ON attestation_secret_ak_pub;
CREATE TRIGGER trigger_delete_old_secret_ak_pub
    AFTER INSERT ON attestation_secret_ak_pub
    EXECUTE PROCEDURE delete_old_secret_ak_pub();
