-- Retain chain exclusions independently of attachment cleanup and DPF recreation.
CREATE TABLE extension_service_chain_selectors (
    service_id UUID NOT NULL,
    interface_ordinal INTEGER NOT NULL,
    slot INTEGER NOT NULL,
    excluded_dpu_ids TEXT[] NOT NULL DEFAULT '{}',
    version TEXT NOT NULL,
    CONSTRAINT extension_service_chain_selectors_pkey
        PRIMARY KEY (service_id, interface_ordinal, slot),
    CONSTRAINT extension_service_chain_selectors_service_id_fkey
        FOREIGN KEY (service_id) REFERENCES extension_services(id),
    CONSTRAINT extension_service_chain_selectors_interface_ordinal_nonnegative
        CHECK (interface_ordinal >= 0),
    CONSTRAINT extension_service_chain_selectors_slot_nonnegative
        CHECK (slot >= 0),
    CONSTRAINT extension_service_chain_selectors_excluded_dpu_ids_nonnull
        CHECK (array_position(excluded_dpu_ids, NULL) IS NULL)
);
