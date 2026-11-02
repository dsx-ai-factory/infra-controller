-- Record which SPDM-capable attesters an endpoint last reported, so endpoints
-- can be counted per variant of a hardware class. NULL until the endpoint is
-- explored again, or when its BMC reports no ComponentIntegrity collection.
ALTER TABLE explored_endpoints
    ADD COLUMN attester_digest TEXT;

-- Each distinct set of SPDM-capable attesters seen for a hardware class. More
-- than one row means the class has reported differing attestable components,
-- which a profile cannot show on its own: its prefix pattern matches whatever
-- a tray reports, so a tray with seven GPU roots of trust instead of eight
-- still attests cleanly. A row outlives the endpoints that reported it, so it
-- records what a class has carried, not what it carries now.
CREATE TABLE hardware_class_attesters (
    hardware_class  text        NOT NULL,
    attester_digest text        NOT NULL,
    attester_ids    jsonb       NOT NULL,
    first_seen      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (hardware_class, attester_digest)
);
