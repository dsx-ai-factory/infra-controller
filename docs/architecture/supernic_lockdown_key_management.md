# SuperNIC Lockdown Key Management

This document describes how NICo manages lockdown keys for S-VPC flashed Mellanox Ethernet SuperNICs (and other non-ASTRA SuperNICs) at a site. It is intended for site managers responsible for configuring and operating NICo deployments.

## Background

Mellanox Ethernet SuperNICs (such as Bluefield-3 in NIC mode and ConnectX-8) support a `hw_access` mode that restricts firmware and configuration changes. This prevents tenants on the host from modifying SuperNIC firmware or configuration while a bare-metal instance is in use.

- **ASTRA-enabled hardware** (e.g. ConnectX-9 paired with BF4): SuperNICs are locked down by design and are only manageable via the DPU. Lockdown is managed by ASTRA and NICo does not perform key management
- **Non-ASTRA environments** (e.g. S-VPC on BF3): NICo must manage the cryptographic keys used to lock and unlock each card.

The SuperNIC enforces lockdown using a 64-bit cryptographic key. If NICo locks a card and then loses the key, the card is effectively bricked--there is no recovery path that does not require physical replacement.

This means key management for SuperNIC lockdown has an unusually high blast radius. The strategy described here is designed specifically to minimise that risk while still providing strong per-device key isolation.

## Key Derivation Function (KDF)

NICo uses HKDF-SHA256 ([RFC 5869](https://datatracker.ietf.org/doc/html/rfc5869)), which is a standard, well-analysed key derivation function (KDF) that has the following properties:

- **Stable keys**: The same SuperNIC always yields the same key, so NICo can re-derive it at any time.
- **Independent keys**: Different SuperNICs produce cryptographically independent keys; compromising one device does not weaken any other.
- **Non-reversible keys**: The lockdown key cannot be used to recover the site-wide root.

Because the per-device key can be re-derived from the site-wide root and information that is permanently readable from the card itself, *NICo can recover from total database loss* without bricking any cards, provided the site-wide root itself survives: it does with the default Vault store, and with the Postgres store only if the database backup and its KEK do. As long as the site-wide root is preserved, any card can be unlocked.

The site-wide root is the single piece of data that must survive at all costs. It is held in the NICo credential store, Vault by default, and should be backed up according to that store's disaster-recovery procedures; with the Postgres store, that means the database and its KEK (refer to [Secrets Storage](../configuration/secrets-storage.md) for details).

### SuperNIC Key Derivation

Each SuperNIC receives a unique, stable lockdown key derived from the following inputs:

| Component | Source | Description |
|-----------|--------|-------------|
| **Master Secret (IKM)** | `BmcCredentialType::SiteWideRoot` | The site-wide root secret stored in the credentials database |
| **KdfContext** | `dpa_interfaces` table, flexible | The device-specific binding: MAC address + MachineId |

### Optional Per-Tenant Uniqueness

NICo can optionally mix tenant-derived context (such as VPC VNI or Tenant ID) into the key derivation. This produces a different lockdown key for each tenant allocation on the same card, so a key disclosed during one tenancy cannot be used against the card during a future tenancy.

This option introduces a trade-off: the tenant context lives in the database, so enabling it means the database becomes a hard dependency for unlock. If the database is lost and not recoverable, cards locked with tenant-mixed keys cannot be re-derived.

| Input | Source | Survives Database Loss? |
|-------|--------|-------------------------|
| Site-wide root | Credential store: Vault by default, or Postgres | Yes with Vault; with Postgres, only if the database or its backup and the KEK survive |
| NIC MAC address | Hardware | Yes |
| MachineId | Hardware | Yes |
| VPC VNI / Tenant ID | Database | No |

Site managers should choose between hardware-only and tenant-mixed derivation based on their backup and recovery posture for the NICo database.

## Operational Flow

Lockdown and unlock are driven by `scout` (the NICo on-host agent) in cooperation with the NICo API:

1. When a maintenance operation (firmware update, configuration change) needs to run, the API instructs `scout` to unlock the SuperNIC.
2. `scout` derives the appropriate lockdown key, sends it to the card, and confirms the card is unlocked.
3. After maintenance completes, `scout` re-locks the card using the same derivation.
4. Before releasing a host to a tenant, NICo verifies the card is locked.

Site managers do not interact with keys directly; the workflow is fully automated as part of the host lifecycle.

## HKDF Construction

The HKDF for each SuperNIC is generated as follows:

```text
┌─────────────────────────────────────────────────────────────────┐
│                        HKDF-SHA256                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│   IKM (Input Key Material)                                      │
│   └── BmcCredentialType::SiteWideRoot                           │
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

  Info string: "supernic-lock:v1:00:11:22:33:44:56":fm100hsdhjkfasjkdhaskjdhasd"
```

## Key Version Rotation

The derivation scheme is versioned. When NICo introduces a new derivation version (for example, to incorporate additional context), it can produce a new candidate key without invalidating cards that are still locked with an older version. During unlock, NICo tries the most recent version first and falls back through previous versions as needed.

This allows the derivation to evolve over time without coordinated re-locking of every card at the site.

<Note>Rotating the site-wide root is *not currently supported* by NICo.</Note>

## Security Properties

| Property | Guarantee |
|----------|-----------|
| Determinism | The same SuperNIC always derives the same key, so unlock is reproducible. |
| Per-device isolation | Different SuperNICs derive cryptographically independent keys. |
| Non-reversibility | A leaked lockdown key cannot be used to recover the site-wide root. |
| Replay resistance | A leaked key only affects the single device it was derived for (and, with tenant-mixed derivation, only that one tenancy on that one device). |

### Threat Considerations

- **A compromised SuperNIC** exposes only that device's key. Without the site-wide root, the attacker cannot derive keys for any other device.
- **A leak of derivation context** (MAC addresses, MachineIds) is not a security event on its own. These values are not secret; the security of the scheme rests solely on the confidentiality of the site-wide root.
- **A tenant reverse-engineering the derivation algorithm** still cannot derive any keys without the site-wide root.

### Storage of the Site-Wide Root

- The site-wide root is stored encrypted at rest in the credential store and is provisioned during site bring-up.
- Derived keys exist only in memory during lock/unlock operations; they are never written to disk or to the database.
- The 64-bit key size is a hardware constraint of the SuperNIC. While shorter than typical cryptographic keys, the derivation ensures the 64 bits are device-unique and indistinguishable from random.
