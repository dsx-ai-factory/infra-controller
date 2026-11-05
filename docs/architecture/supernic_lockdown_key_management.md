# SuperNIC Lockdown Key Management

This document describes how NICo manages lockdown keys for S-VPC flashed Mellanox Ethernet SuperNICs (and other non-ASTRA SuperNICs) at a site. It is intended for site managers responsible for configuring and operating NICo deployments.

## Background

Mellanox Ethernet SuperNICs (such as Bluefield-3 in NIC mode and ConnectX-8) support a `hw_access` mode that restricts firmware and configuration changes. This prevents tenants on the host from modifying SuperNIC firmware or configuration while a bare-metal instance is in use.

- **ASTRA-enabled hardware** (e.g. ConnectX-9 paired with BF4): SuperNICs are locked down by design and are only manageable via the DPU. Lockdown is managed by ASTRA and NICo does not perform key management
- **Non-ASTRA environments** (e.g. S-VPC on BF3): NICo must manage the cryptographic keys used to lock and unlock each card.

The SuperNIC enforces lockdown using a 64-bit cryptographic key. If NICo locks a card and then loses the key, the card is effectively bricked--there is no recovery path that does not require physical replacement.

This means key management for SuperNIC lockdown has an unusually high blast radius. The strategy described here is designed specifically to minimise that risk while still providing strong per-device key isolation.

## Key Derivation Function (KDF)

The security properties below assume secret, randomly generated input-key material (IKM) with high entropy. [RFC 5869, Section 4](https://www.rfc-editor.org/rfc/rfc5869.html#section-4) explains that HKDF neither adds entropy nor slows password guessing. A leaked card key and known context let attackers test guesses of a weak IKM offline.

NICo uses HKDF-SHA256 ([RFC 5869](https://datatracker.ietf.org/doc/html/rfc5869)), which is a standard, well-analysed key derivation function (KDF) that has the following properties:

- **Stable keys**: The same IKM, host MachineId, and card MAC address produce the same key. NICo can re-derive it while those inputs remain available.
- **Independent keys**: Different SuperNICs produce cryptographically independent keys; compromising one device does not weaken any other.
- **IKM protection**: Recovering a secret, high-entropy IKM from a leaked lockdown key is computationally impractical.

### SuperNIC Key Derivation

NICo Core derives each lockdown key from the following inputs:

| Component | Source | Description |
|-----------|--------|-------------|
| Versioned Lockdown IKM | `CredentialKey::NicLockdownIkm` | Dedicated site-wide lockdown secret at the selected IKM version |
| KDF Context | `dpa_interfaces` | Card MAC address and owning host MachineId, encoded using KDF context V1 |

KDF context V1 contains only the MAC address and MachineId. It has no tenant ID or VPC VNI input and does not provide a tenant-mixing option.

NICo Core seeds IKM version 0 from `BmcCredentialType::SiteWideRoot` only if that lockdown IKM is absent. Seeding waits for a non-empty BMC root and retries every 60 seconds when deferred. An existing lockdown IKM remains unchanged. Seeding copies the BMC secret unchanged; that secret must meet the secret, random, high-entropy IKM assumptions above.

The [nico-admin-cli credential add-nic-lockdown-ikm](../manuals/nico-admin-cli/commands/credential/credential-add-nic-lockdown-ikm.md) reference describes setting version 0 directly. The command requires `--password` and provides no environment-variable, standard-input, or interactive password option. Its argument value can appear in process listings; typing it inline can also record it in shell history. Do not replace an IKM version that cards still use: deriving with the replacement value produces a different key.

## Operational Flow

NICo Core derives lockdown keys and sends lock and unlock requests to Scout, which runs the hardware operation. During allocation, the DPA interface moves through `Unlocking`, firmware and profile application, and `Locking`. Reported `Unlocked` status permits configuration; reported `Locked` status permits the DPA interface to become `Assigned`.

The DPA flow also unlocks SuperNICs during host decommissioning. Idle key rotation instead unlocks and re-locks cards without entering the firmware and profile assignment pipeline. Tenant release alone does not unlock cards.

Successful lock and unlock calls return an inferred status, not independent hardware readback. Use a separate status query to check `Locked` or `Unlocked`. The query does not identify the installed key or establish which IKM version produced it.

### Generic NIC Firmware Recovery

The [generic NIC firmware recovery implementation (#8596)](https://github.com/dsx-ai-factory/infra-controller/pull/8596) is unreleased and has no production lifecycle admission entry point. Production admission is tracked in [issue #6680](https://github.com/dsx-ai-factory/infra-controller/issues/6680). The following describes recovery of an already persisted operation, not an operator update procedure.

The operation preserves the original lockdown policy, including its MAC address and IKM version. NICo Core combines that identity with the operation's host MachineId using the same V1 derivation, without requiring a DPA inventory record. Restoration retains that exact IKM version; an originally unlocked card remains unlocked.

Derived keys are absent from persisted operations and reports. NICo Core derives the key when sending a lockdown change authorized by the persisted state. A missing requested IKM, backend error, or 30-second credential lookup timeout leaves the operation retrying observation and change. NICo Core logs "NIC lockdown credential lookup failed" for lookup failures.

Refer to [NIC Firmware Recovery](state_machines/managedhost.md#nic-firmware-recovery) for policy restoration, failure handling, and diagnostics.

## HKDF Construction

The HKDF for each SuperNIC is generated as follows:

```text
┌─────────────────────────────────────────────────────────────────┐
│                        HKDF-SHA256                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│   IKM (Input Key Material)                                      │
│   └── Versioned site-wide lockdown IKM                          │
│                                                                 │
│   Salt                                                          │
│   └── None                                                      │
│                                                                 │
│   Info (KdfContext)                                             │
│   └── "supernic-lock:v1:{mac_address}:{machine_id}"             │
│                                                                 │
│   Output                                                        │
│   └── 64-bit (8 bytes) lockdown key → 16 hex characters         │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### Info String Format (Versioned)

The `info` parameter uses a versioned string format for domain separation and future extensibility:

```text
Version 1:  "supernic-lock:v1:{mac_address}:{machine_id}"

Example:
  MAC Address: "00:11:22:33:44:56"
  MachineId:   "fm100hsdhjkfasjkdhaskjdhasd"

  Info string: "supernic-lock:v1:00:11:22:33:44:56:fm100hsdhjkfasjkdhaskjdhasd"
```

## Key Version Rotation

The IKM version selects the credential independently of KDF context version V1. After selecting a version, lookup requires that exact credential; a missing credential fails lookup rather than selecting another secret.

Dedicated lockdown IKM credentials remain separate from the BMC site-wide root after initial seeding. Rotating a BMC credential does not change an existing lockdown IKM.

The [nico-admin-cli credential rotate](../manuals/nico-admin-cli/commands/credential/credential-rotate.md) interface supports the `lockdown-ikm` family. It creates a new versioned secret and advances the site's target version. Staging a target alone does not migrate cards: `nic_lockdown_ikm_rotation_enabled` defaults to `false`.

With rotation enabled, allocation locks migrate cards to the site target as tenants cycle. An operator's per-host force request also permits migration with the site setting disabled. When rotation is enabled or forced, idle hosts use `RotatingNicLockdown` to unlock lagging cards and re-lock them at the target. After that idle rekey starts, its lock phase continues toward the target even if the site setting is disabled.

NICo Core selects the IKM version for DPA requests as follows:

- Lock requests first retain an in-flight `rotating_to_version`. Otherwise, an enabled or forced allocation migration, or an idle rekey, selects the site target. Without migration, the request uses the card's tracked `current_version`, falling back to version 0 when untracked.
- Unlock requests use `rotating_to_version`, then `current_version`, then version 0, independently of the rotation setting.
- Administrative lock and unlock requests use that same in-flight, current, then version 0 order. They do not migrate a card to the site target or update its convergence bookkeeping.

A missing site target during migration is an error, not a fallback to version 0. The [nico-admin-cli credential rotation-status](../manuals/nico-admin-cli/commands/credential/credential-rotation-status.md) reference describes inspecting site or per-card convergence for the `lockdown-ikm` family.

## Backup and Recovery

Re-derivation requires the original host MachineId, card MAC address, and exact IKM version used to lock the card. As a site manager, back up every retained IKM version in the credential store. Include current or staged site targets and versions referenced by cards or saved firmware operations. Do not retain only the newest version.

Also back up the NICo Core database, regardless of the credential backend. It holds card identities, tracked rotation versions, site targets, and any persisted firmware operation's original lockdown context. A Vault backup alone does not preserve that context. Refer to [Back up the Databases](../manuals/upgrade.md#back-up-the-databases) for the database backup procedure.

For Postgres credentials, the database backup must also preserve the credential journal. Retain every key encryption key (KEK) needed to decrypt it. Refer to [Secrets Storage](../configuration/secrets-storage.md) for credential storage protection and key custody requirements.

## Security Properties

The following table summarizes the derivation's security properties:

| Property | Guarantee |
|----------|-----------|
| Determinism | The same IKM, host MachineId, and card MAC address derive the same key, so unlock is reproducible when those inputs are retained |
| Per-device isolation | Different SuperNICs derive cryptographically independent keys |
| IKM Protection | Recovering a secret, high-entropy IKM from a leaked lockdown key is computationally impractical |
| Host and Card Binding | The V1 derivation binds the key to its host MachineId and card MAC address; it does not bind it to a tenancy |

### Threat Considerations

- **A compromised SuperNIC** exposes only that device's key. Without the lockdown IKM, the attacker cannot derive keys for any other device.
- **A leak of derivation context** (MAC addresses, MachineIds) is not a security event on its own. These values are not secret; security depends on a confidential, high-entropy lockdown IKM.
- **A tenant reverse-engineering the derivation algorithm** still cannot derive any keys without the lockdown IKM.

### Storage of the Lockdown IKM

- The versioned lockdown IKM resides in the credential store. Follow the [Backup and Recovery](#backup-and-recovery) requirements for secrets and database context.
- Derived keys exist only in memory during lock and unlock operations; they are never written to disk or to the database.
- The 64-bit key size is a hardware constraint of the SuperNIC. While shorter than typical cryptographic keys, the derivation produces pseudorandom bits bound to the host and card context.
