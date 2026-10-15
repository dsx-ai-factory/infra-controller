-- Store service-network requirements on the registration record.
ALTER TABLE extension_services
    ADD COLUMN service_vpc_interfaces JSONB NOT NULL DEFAULT '[]'::jsonb;

-- One registered service interface keeps the same MAC across its attachments and DPUs.
-- Reuse across DPUs is safe because each service-side Layer-2 domain terminates
-- locally and is never bridged between DPUs. The (service_id, interface_ordinal)
-- key therefore owns the stable MAC identity.
CREATE TABLE extension_service_interface_macs (
    service_id UUID NOT NULL,
    interface_ordinal INTEGER NOT NULL,
    mac_address MACADDR NOT NULL,
    CONSTRAINT extension_service_interface_macs_pkey
        PRIMARY KEY (service_id, interface_ordinal),
    CONSTRAINT extension_service_interface_macs_interface_ordinal_nonnegative
        CHECK (interface_ordinal >= 0),
    CONSTRAINT extension_service_interface_macs_mac_address_key
        UNIQUE (mac_address),
    CONSTRAINT extension_service_interface_macs_service_id_fkey
        FOREIGN KEY (service_id) REFERENCES extension_services(id)
);
