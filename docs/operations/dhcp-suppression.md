# DHCP Suppression for Suppressed BMC MAC Addresses

## Overview

NICo Core's `DiscoverDhcp` RPC can stop leasing IP addresses to a BMC whose MAC
address has been marked as suppressed, without any change to the DHCP server
binary itself — suppression is handled entirely within Core. The only current
writer of a `dhcp`-subsystem suppression row is
[rack decommissioning](../decommissioning/index.md): as the final step of
rack uningestion, host, switch, and power-shelf decommissioning each request
DHCP suppression for their BMC MAC. Other features (BMC credential rotation)
write to the same `bmc_suppressions` table under the separate `site_explorer`
subsystem and do not go through this DHCP path.

---

## Background

A row is added for a BMC MAC address to the `bmc_suppressions` table with
`subsystem = 'dhcp'` — today, this happens as part of rack decommissioning.
The existence of that row signals that the DHCP server should no longer serve
the MAC.

The current behavior per DHCP message type:

| DHCP message | Behavior |
|---|---|
| `DHCPDISCOVER` | Suppression is acknowledged (`acknowledged_at` is recorded) and the request is refused with no reply sent to the BMC |
| `DHCPREQUEST` | Suppression is acknowledged and the request is refused with no reply sent to the BMC; **no `DHCPNAK` is sent** (see [Known Limitations](#known-limitations--open-items)) |

---

## Implementation

### Where the change lives

`crates/api-core/src/dhcp/discover.rs`, inside the `DiscoverDhcp` RPC handler.

### What it does

Early in the `DiscoverDhcp` request path, after the MAC address is parsed,
the handler calls `db::bmc_suppression::acknowledge()` on the transaction it
already holds open. This function:

1. Checks for a `bmc_suppressions` row matching the MAC and
   `subsystem = 'dhcp'`
2. If one exists, atomically records `acknowledged_at` (`COALESCE`d so a
   repeated acknowledgement preserves the first timestamp)
3. Returns `true` to signal that suppression is active; it does not commit —
   the caller owns the transaction it was passed

The handler commits the transaction and returns `FailedPrecondition`
immediately, before any lease logic runs:

```rust
if db::bmc_suppression::acknowledge(
    &mut txn,
    parsed_mac,
    model::bmc_suppression::BmcSuppressionSubsystem::Dhcp,
)
.await?
{
    txn.commit().await?;
    return Err(CarbideError::FailedPrecondition(format!(
        "dhcp suppressed for bmc mac {parsed_mac}"
    )));
}
```

The `FailedPrecondition` gRPC status propagates to `dhcp-server`'s controller
mode as a `DhcpError`. In `process_packet`
(`crates/dhcp-server/src/packet_handler.rs`), the `?` on the `discover_dhcp`
call short-circuits before `create_dhcp_reply_packet` ever runs, so no
`DHCPNAK` — or any reply — is constructed. `main.rs` logs the drop
(`DhcpPacketDropped`) and returns without sending a packet. The BMC receives no
signal to release its address or return to `INIT`; see
[Known Limitations](#known-limitations--open-items).

### Atomicity

The acknowledgement and timestamp write happen inside the same transaction as the
suppression check, so there is no window between checking and recording.

---

## Decommission Workflow Integration

The [decommission workflow](../decommissioning/index.md) polls `acknowledged_at` on each BMC's `bmc_suppressions`
row (`subsystem = 'dhcp'`) to confirm that Core has observed and refused a DHCP
request from that BMC. This timestamp is written server-side, inside Core's
`DiscoverDhcp` handler, at the moment Core receives the request — before any
reply (or lack of one) reaches the BMC. It confirms Core is refusing the BMC's
DHCP traffic; it does **not** confirm the BMC received a `DHCPNAK`, released
its address, or returned to `INIT`, since no reply is currently sent at all
(see [Known Limitations](#known-limitations--open-items)).

---

## Cache Invalidation

The DHCP record cache can be invalidated independently of the suppression
mechanism to force a fresh lookup from the database. This is useful when
suppression records are added or modified outside of the normal decommission
flow.

---

## Known Limitations / Open Items

- The suppression check is currently only wired into `DiscoverDhcp`. A
  `DHCPREQUEST` that arrives without a preceding `DHCPDISCOVER` (e.g. on
  network reconnect) will still be served unless the DHCP server independently
  checks suppression status.
- No `DHCPNAK` is sent today: a `FailedPrecondition` from `DiscoverDhcp` causes
  `dhcp-server` to silently drop the packet instead of constructing a reply.
  The BMC is never told to release its address or return to `INIT` — it
  simply stops receiving responses and will not renew until its existing
  lease expires on its own timers. If an explicit `DHCPNAK` is required,
  `dhcp-server` needs to construct one on this error path instead of dropping
  the packet.
- The `bmc_suppressions` row (`subsystem = 'dhcp'`) must exist before the
  decommission workflow polls for `acknowledged_at`; the ordering is the
  caller's responsibility.
