-- Record why NICo selected a host's current boot interface.
-- Defining every planned source now keeps the database contract stable as the
-- Redfish chassis and Scout report selection paths arrive independently.
CREATE TYPE boot_interface_selection_source AS ENUM (
    'expected_machine',
    'operator',
    'redfish_uefi_pci',
    'redfish_chassis_id',
    'redfish_serial_number',
    'scout_report_pci',
    'legacy_unknown'
);

-- A concrete source represents a recorded selection decision and therefore
-- requires the time of that decision. Existing rows receive the truthful
-- legacy value, which may have no decision time. A current writer can also
-- record LegacyUnknown with a time when it cannot attribute the selection
-- mechanism.
ALTER TABLE machine_boot_interfaces
    ADD COLUMN selection_source boot_interface_selection_source
        NOT NULL DEFAULT 'legacy_unknown',
    ADD COLUMN selection_updated_at timestamp with time zone,
    ADD CONSTRAINT machine_boot_interfaces_selection_time_consistent
        CHECK (
            selection_source = 'legacy_unknown'
            OR selection_updated_at IS NOT NULL
        );

-- During a rolling deployment, an API server predating these columns can
-- change the desired boot interface without changing the selection fields.
-- The desired target remains correct, but the source and decision time can
-- describe the preceding selection until a new API server records another
-- selection source.
-- We accept that narrow window while old and new API servers run together
-- instead of adding a database trigger solely to maintain metadata for old
-- writers.
