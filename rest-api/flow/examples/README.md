# Examples

This directory contains example input files for Flow CLI commands.

## Files

### `rack-create-example.json`

Example input for `flow rack create`. Demonstrates a GB200 NVL72 rack with one
of each component type: TOR switches, compute trays, NVL switches, and power
shelves.

**Usage:**

```bash
flow rack create --file examples/rack-create-example.json
```

**JSON format notes:**

- `info.id` — optional UUID; a new one is generated if omitted
- `location.datacenter` — matches the `datacenter` field (not `data_center`)
- `components[].type` — one of: `compute`, `nvswitch`, `powershelf`,
  `torswitch`, `ums`, `cdu`
- `components[].position` — uses snake_case keys: `slot_id`, `tray_index`,
  `host_id`
- `components[].bmcs` — array of BMC entries; each entry has a `type` field
  (`"host"` or `"dpu"`), `mac` in standard `aa:bb:cc:dd:ee:ff` format, and
  optional `ip`, `user`, `password`

---

### `operation-rules-example.yaml`

Canonical reference for operation rules loaded via `flow rule create`. Covers
graceful and forceful power on/off, restart, and firmware upgrade sequences.

**Usage:**

```bash
# Load rules (skip existing)
flow rule create --from-yaml examples/operation-rules-example.yaml

# Overwrite existing rules
flow rule create --from-yaml examples/operation-rules-example.yaml --overwrite

# Validate without creating (dry-run)
flow rule create --from-yaml examples/operation-rules-example.yaml --dry-run
```

See [Operation Rules](../../../docs/operations/flow/operation-rules.md) for a full explanation of the rule schema
and available actions.
