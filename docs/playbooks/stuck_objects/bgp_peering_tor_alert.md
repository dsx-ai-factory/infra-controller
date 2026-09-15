# Troubleshoot a BgpPeeringTor Health Alert

Use this playbook when a managed host or DPU reports `BgpPeeringTor`. The alert
can identify an unavailable top-of-rack (ToR) uplink or an established session
that did not negotiate the required IPv6 unicast address family.

## Symptoms and Impact

- HBN reports `p0_if` or `p1_if` as `Idle` or `Active` instead of established,
  or reports that IPv6 unicast was not negotiated on an established session.
- `ethtool` reports that the affected link is not detected.
- `ip link` reports `NO-CARRIER` and `state DOWN`.
- Optical diagnostics report no transmit or receive power.

An unavailable session prevents the DPU from peering with the ToR switch over
the affected link. A primary `p0` failure can block normal PXE provisioning
even when `p1` remains established. An address-family warning does not show
that the physical link or transport session failed.
For the health-policy behavior, refer to
[Waiting for Network Configuration and DPU Health](waiting_for_network_config.md#bgppeeringtor).

Access the DPU OS directly or through its BMC serial console. Most diagnostic
commands require root privileges.

## Check BGP State

Run `show bgp summary` in the `doca-hbn` container:

```bash
crictl exec "$(crictl ps | awk '/hbn/ {print $1; exit}')" \
  vtysh -c 'show bgp summary'
```

An affected interface remains in `Idle` or `Active`, often with `never` in the
`Up/Down` column:

```text
Neighbor        V  AS  MsgRcvd  MsgSent  Up/Down  State/PfxRcd
p0_if           4   0        0        0  never    Idle
p1_if           4   0        0        0  never    Idle
```

A healthy interface has nonzero message counters and uptime. Depending on the
FRR output format, an established session displays a received-prefix count in
`State/PfxRcd` rather than the word `Established`.

## Interpret the Alert

Read the complete alert message before applying physical remediation:

- If the session is established but the message says that IPv6 unicast was not
  negotiated, treat the alert as an address-family configuration warning.
  Inspect the IPv6 unicast summary and the HBN and ToR address-family
  configuration. Do not reseat or replace a cable unless the physical-link
  checks independently show a failure.
- If the expected session is missing or not established, continue with the
  physical-link checks for the interface named by the alert.

The number of ToR sessions required for general health comes from
`min_dpu_functioning_links`. A value of `1` allows either established session
to satisfy that minimum, but `p0` remains separately required for normal PXE.
Do not require both `p0` and `p1` unless the configured minimum is `2`.

## Check the Physical Links

Set `<interface>` to the `p0` or `p1` interface named by the alert, and check
only the affected link unless another alert identifies the other link.

1. Check link detection:

   ```bash
   ethtool <interface> | grep -i 'link detected'
   ```

1. Check carrier state:

   ```bash
   ip link show <interface>
   ```

   `NO-CARRIER` and `state DOWN` indicate a physical-link failure.

1. Inspect optical power and module temperature on each affected interface:

   ```bash
   ethtool -m <interface> | grep -E 'optical power|temperature'
   ```

   Near-zero transmit and receive power with a normal module temperature points
   to an unplugged, loose, or failed active optical cable.

## Restore the Link

1. Ask the data center operator to reseat the affected cable.
1. Replace the cable if reseating it does not restore carrier.
1. Identify both the logical interface and physical port in the request. DPU
   physical ports 1 and 2 correspond to `p0` and `p1`, respectively.
1. If the DPU reports carrier but BGP remains down, have the networking team
   verify that the corresponding ToR port is enabled and correctly configured.
1. Consider DPU replacement only after the cable and ToR port have been ruled
   out.

## Verify Recovery

1. Confirm that `ethtool` reports `Link detected: yes` and `ip link` reports
   `state UP` without `NO-CARRIER`.
1. Run the HBN BGP summary again. Confirm that the number of established ToR
   sessions satisfies `min_dpu_functioning_links`. For normal PXE, also confirm
   that `p0` is established. If the alert was an address-family warning,
   confirm that IPv6 unicast is now negotiated on the affected session.
1. Confirm that the `BgpPeeringTor` alert clears from the NICo health report:

   ```bash
   nico-admin-cli dpu health-report show <dpu-machine-id>
   nico-admin-cli machine health-report show <host-machine-id>
   ```

For additional probe variants and configuration checks, refer to
[DPU ToR Uplink Health](../../dpu-management/dpu_configuration.md#dpu-tor-uplink-health).
