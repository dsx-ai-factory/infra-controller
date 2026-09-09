-- Attestation profiles: one row per hardware class, naming which attesters
-- machines of that class require. The key is a HwType variant name as recorded
-- in explored_endpoints.hardware_class, or the reserved 'any' fallback.
CREATE TABLE attestation_profiles (
    hardware_class  text         PRIMARY KEY,
    version         varchar(64)  NOT NULL,
    policy_document jsonb        NOT NULL,
    updated_at      timestamptz  NOT NULL DEFAULT now(),
    updated_by      varchar(256) NOT NULL
);
