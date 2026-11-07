# Host Naming

Every machine interface NICo manages gets a hostname, and that hostname is what [NICo's DNS](dns.md) serves -- the A/AAAA records for a machine are built from its stored hostname and current addresses. How those hostnames are derived is a site-wide choice, set once in the `nico-api` config:

```toml
# nico-api config TOML
host_naming_strategy = "serial_number"
```

If the option is omitted, NICo uses `ip_address` -- the same IP-derived names it has always produced, so existing sites need no config change.

## The Four Strategies

| Value | Example hostname | Derived from | Stable across IP changes? |
|---|---|---|---|
| `ip_address` (default) | `10-1-2-3` | The interface's current IP | No -- the name follows the IP |
| `serial_number` | `sn123abc` | The machine's hardware serial | Yes |
| `mac_address` | `0a-1b-2c-3d-4e-5f` | The interface's MAC address | Yes |
| `fun` | `wholesale-walrus` | A generated adjective-noun handle | Yes |

### `ip_address`

The historical behavior: the hostname mirrors the interface's preferred address, with dots replaced by dashes. A dual-stack interface is named after its IPv4 address (with both A and AAAA records pointing at it); an IPv6-only interface gets the fully expanded address with dashes (`2001-0db8-0000-0000-0000-0000-0000-0002`). When the IP changes, the name changes with it.

### `serial_number`

Names a machine after its hardware serial, lower-cased and sanitized to a valid DNS label.

- A machine's serial isn't visible on the network plane, so a brand-new interface starts with a temporary IP-derived name and switches to the serial name once the machine is discovered.
- The primary interface -- the DNS-visible one -- gets the bare serial. Secondary interfaces share that serial, so each gets `<serial>-<mac>` to stay unique.
- BMC interfaces keep IP-derived names: the BMC is the machine's management controller, not the machine, even though it reports the same serial.
- Junk vendor placeholders (`To Be Filled By O.E.M.`, `Default string`, all zeros, and friends) don't count as serials -- those machines keep their IP-derived names until real data shows up.

The serial name makes a promise: *the name is the serial*. If two machines report the same serial, NICo refuses to give the second one a substitute name -- naming fails with a clear error (`hostname "..." derived from serial "..." is already held by another machine's interface`) so the data problem gets fixed rather than hidden. See [Troubleshooting](#troubleshooting) below.

### `mac_address`

Names every interface -- BMC included -- after its own MAC, lower-cased with dashes. The MAC is in the very first DHCP packet, so unlike serial naming there's no temporary-name phase: the interface has its permanent name from the moment NICo sees it. Since every interface has its own MAC, there's no primary/secondary distinction to worry about.

### `fun`

Brings back the classic generated names (`wholesale-walrus`, `ornate-otter`, ...). A name is assigned once, checked for uniqueness against the site, and then kept for the life of the interface -- IP changes repoint the DNS records but never change the name. An interface that loses all of its addresses parks under a `noip-<mac>` placeholder and drops out of DNS until it has an address again.

## Switching Strategies

Switching is safe and never mass-renames a site. Whenever NICo is about to write a hostname -- an interface's first DHCP, an address gained or lost, a host reconcile -- it asks the configured strategy, which answers either *assign this name* or *keep what's stored*:

- Switching **to `fun`** only affects machines named from then on. Anything that already has a real name keeps it; the fleet adopts fun names gradually as machines come and go.
- Switching **to `ip_address`, `serial_number`, or `mac_address`** re-derives: existing machines pick up their new names bit by bit, the next time one of those naming moments comes around for them.

Either direction, DNS follows automatically -- records are computed from the stored hostname and addresses, so a renamed machine's records move with it and a kept name's records repoint on IP change.

To switch, set `host_naming_strategy` in the `nico-api` config and restart the service. The policy is read once at startup.

## Troubleshooting

**A machine's name didn't change after I switched strategies.** Expected -- names are re-derived at naming events (first DHCP, address changes, reconcile), not at config change. If you switched to `fun`, machines with real names keep them by design.

**Serial naming reports a duplicate.** Two machines are reporting the same serial (cloned DMI data, or a vendor placeholder NICo doesn't recognize yet). The error names both the hostname and the serial. Fix the serial data on one of the machines (or add the placeholder pattern to the recognized junk list); the colliding machine keeps working under its previous name in the meantime.

**A serial-named machine shows an IP-derived name.** Either the machine hasn't been discovered yet (the serial isn't known until then) or its serial is a recognized vendor placeholder. Check the machine's discovered hardware data for the serial NICo sees.
