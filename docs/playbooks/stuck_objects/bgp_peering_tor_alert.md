# Troubleshoot a BgpPeeringTor Health Alert

Use this playbook when a managed host or DPU reports `BgpPeeringTor` and the
alert identifies the `p0` or `p1` top-of-rack (ToR) uplink.

## Symptoms and Impact

- HBN reports `p0_if` or `p1_if` as `Idle` or `Active` instead of established.
- `ethtool` reports that the affected link is not detected.
- `ip link` reports `NO-CARRIER` and `state DOWN`.
- Optical diagnostics report no transmit or receive power.

The DPU cannot peer with the ToR switch over the affected link. A primary `p0`
failure can block normal PXE provisioning even when `p1` remains established.
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

## Check the Physical Links

1. Check link detection:

   ```bash
   ethtool p0 | grep -i 'link detected'
   ethtool p1 | grep -i 'link detected'
   ```

1. Check carrier state:

   ```bash
   ip link show p0
   ip link show p1
   ```

   `NO-CARRIER` and `state DOWN` indicate a physical-link failure.

1. Inspect optical power and module temperature on each affected interface:

   ```bash
   ethtool -m p0 | grep -E 'optical power|temperature'
   ethtool -m p1 | grep -E 'optical power|temperature'
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
1. Run the HBN BGP summary again. Confirm that `p0_if` and `p1_if` have nonzero
   uptime and increasing message counters.
1. Confirm that the `BgpPeeringTor` alert clears from the NICo health report:

   ```bash
   nico-admin-cli dpu health-report show <dpu-machine-id>
   nico-admin-cli machine health-report show <host-machine-id>
   ```

For additional probe variants and configuration checks, refer to
[DPU ToR Uplink Health](../../dpu-management/dpu_configuration.md#dpu-tor-uplink-health).
