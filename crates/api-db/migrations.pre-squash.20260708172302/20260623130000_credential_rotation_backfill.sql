-- Backfill the initial credential-rotation bookkeeping for existing sites.
--
-- Rotation does not exist yet, so every already-ingested device is, by
-- definition, at version 0 of its site-wide credential. This one-time data
-- migration records that baseline so the future rotation engine starts from a
-- correct picture of reality:
--
--   * sitewide_credential_rotation: target_version = 0 per credential type.
--   * device_credential_rotation:   current_version = 0 for each device that
--     already carries the credential.
--
-- New sites have no ingested devices yet, so every statement below is a no-op
-- there. Secret material itself lives in Vault, never here. Three invariants
-- make this safe without reading Vault (which SQL cannot do):
--
--   1. Every ingested machine, switch, and power shelf authenticates its BMC
--      with the site-wide BMC root (per-device entries, when present, were set
--      from that same root at ingestion). NICo has never rotated a BMC root, so
--      every BMC-bearing device is at v0.
--   2. The dedicated lockdown IKM v0 is copied from the BMC root by
--      `ensure_lockdown_ikm_seeded` during API startup, before the server
--      serves and before anything consumes these rows. Cards currently locked
--      were locked under that value, so they are already at v0.
--   3. Site exploration / ingestion refuses to run unless the site-wide host
--      and DPU UEFI defaults are configured (REQUIRED_SITE_DEFAULT_CREDENTIAL
--      _KEYS), so any ingested host whose UEFI password is set, and every DPU,
--      is at v0 of its UEFI default.
--
-- NVOS is deliberately NOT backfilled. NICo does not change the switch NVOS
-- password from the factory default on ingestion today (REQ-6 "set NVOS from
-- factory" is not yet implemented), so there is no NICo-authoritative "v0" for
-- a switch's NVOS to converge to. NVOS rows are seeded once set-from-factory
-- lands and NICo owns that value.

-- Site-wide rotation targets (one row per backfilled credential type).
INSERT INTO sitewide_credential_rotation (credential_type, target_version)
VALUES ('bmc', 0), ('lockdown_ikm', 0), ('host_uefi', 0), ('dpu_uefi', 0)
ON CONFLICT (credential_type) DO NOTHING;

-- BMC root: every ingested machine (host or DPU) is at v0, keyed by its
-- (earliest) BMC interface MAC.
INSERT INTO device_credential_rotation (device_mac, credential_type, current_version)
SELECT mac_address, 'bmc', 0 FROM (
    SELECT DISTINCT ON (m.id) mi.mac_address AS mac_address
    FROM machines m
    JOIN machine_interfaces mi
        ON mi.machine_id = m.id AND mi.interface_type = 'Bmc'
    ORDER BY m.id, mi.created ASC
) machine_bmcs
ON CONFLICT (device_mac, credential_type) DO NOTHING;

-- BMC root: every live switch is at v0, keyed by its BMC MAC.
INSERT INTO device_credential_rotation (device_mac, credential_type, current_version)
SELECT bmc_mac_address, 'bmc', 0
FROM switches
WHERE deleted IS NULL
  AND bmc_mac_address IS NOT NULL
ON CONFLICT (device_mac, credential_type) DO NOTHING;

-- BMC root: every live power shelf is at v0, keyed by its (PMC) BMC MAC.
INSERT INTO device_credential_rotation (device_mac, credential_type, current_version)
SELECT bmc_mac_address, 'bmc', 0
FROM power_shelves
WHERE deleted IS NULL
  AND bmc_mac_address IS NOT NULL
ON CONFLICT (device_mac, credential_type) DO NOTHING;

-- Lockdown IKM: every currently-locked SuperNIC card is converged at v0,
-- keyed by its card (NIC) MAC.
INSERT INTO device_credential_rotation (device_mac, credential_type, current_version)
SELECT mac_address, 'lockdown_ikm', 0
FROM dpa_interfaces
WHERE deleted IS NULL
  AND card_state->>'lockmode' = 'Locked'
ON CONFLICT (device_mac, credential_type) DO NOTHING;

-- A machine's type is encoded in its id (see crates/uuid/src/machine): the
-- prefix is "fm100" followed by a type character -- 'h' host, 'd' DPU, 'p'
-- predicted host. These literals are exactly MachineType::id_prefix(), so
-- matching them is the canonical way to discriminate type in SQL (there is no
-- separate machine_type column).

-- Host UEFI: every non-DPU machine whose UEFI password has been set is at v0,
-- keyed by its (earliest) BMC MAC. This deliberately covers both real hosts
-- ('fm100h') and predicted hosts ('fm100p') -- anything that is not a DPU --
-- so a predicted host that already carries a UEFI password is captured too.
-- The bios_password_set_time filter excludes machines with no UEFI password.
INSERT INTO device_credential_rotation (device_mac, credential_type, current_version)
SELECT mac_address, 'host_uefi', 0 FROM (
    SELECT DISTINCT ON (m.id) mi.mac_address AS mac_address
    FROM machines m
    JOIN machine_interfaces mi
        ON mi.machine_id = m.id AND mi.interface_type = 'Bmc'
    WHERE m.bios_password_set_time IS NOT NULL
      AND NOT starts_with(m.id, 'fm100d')
    ORDER BY m.id, mi.created ASC
) hosts
ON CONFLICT (device_mac, credential_type) DO NOTHING;

-- DPU UEFI: managed site-wide and guaranteed set by ingestion preconditions,
-- so every DPU is at v0, keyed by its (earliest) BMC MAC.
INSERT INTO device_credential_rotation (device_mac, credential_type, current_version)
SELECT mac_address, 'dpu_uefi', 0 FROM (
    SELECT DISTINCT ON (m.id) mi.mac_address AS mac_address
    FROM machines m
    JOIN machine_interfaces mi
        ON mi.machine_id = m.id AND mi.interface_type = 'Bmc'
    WHERE starts_with(m.id, 'fm100d')
    ORDER BY m.id, mi.created ASC
) dpus
ON CONFLICT (device_mac, credential_type) DO NOTHING;
