-- Record which SPDM-capable attesters an endpoint last reported, so endpoints
-- can be counted per variant of a hardware class. NULL until the endpoint is
-- explored again, or when its BMC reports no ComponentIntegrity collection.
ALTER TABLE explored_endpoints
    ADD COLUMN attester_digest TEXT;

-- Each distinct set of SPDM-capable attesters seen for a hardware class. More
-- than one row for a class means the class spans hardware with differing
-- attestable components, which pattern matching alone cannot show: a prefix
-- still matches when a tray reports seven roots of trust instead of eight.
CREATE TABLE hardware_class_attesters (
    hardware_class  text        NOT NULL,
    attester_digest text        NOT NULL,
    attester_ids    jsonb       NOT NULL,
    first_seen      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (hardware_class, attester_digest)
);
