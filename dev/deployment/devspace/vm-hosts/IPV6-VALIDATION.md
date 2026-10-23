# IPv6 VM-hosts validation

This record describes the ARM64 experiments completed before the branch was
rebased onto [PR 7564](https://github.com/dsx-ai-factory/infra-controller/pull/7564).
Their final source revision is `319f8b2ab`. They are retained as historical
evidence, not live validation of the rebased tree. The BMC backend's separate
validation is recorded in [VALIDATION.md](VALIDATION.md).

Validated on 2026-10-12 and 2026-10-13 using an ARM64 Linux VM with nested KVM. The source base
merges [PR 7078](https://github.com/dsx-ai-factory/infra-controller/pull/7078)
and [PR 6606](https://github.com/dsx-ai-factory/infra-controller/pull/6606), plus
this DevSpace extension. This is development evidence for
[IPv6 support](https://github.com/dsx-ai-factory/infra-controller/issues/84).

## Exercised

- A real ARM64 guest with 4 GiB RAM, two vCPUs, and a 24 GiB sparse disk.
- Redfish power control through standalone `bmc-mock` and its libvirt backend.
- NICo DHCPv4, firmware HTTP boot, iPXE, and real Scout execution.
- NICo DHCPv6 through the lab relay, for both UEFI's UUID DUID and Linux's
  enterprise DUID. Linux requested IA_NA without prefix delegation.
- Normal host ingestion, including BMC credential rotation, discovery, cleanup,
  a stable serial-derived machine identity, and Core `Ready` state.
- A real authenticated Scout stream and successful Scout ping.
- The complete DevSpace deployment readiness checks: Core workloads, PXE delivery,
  REST site online, host usable in REST, and a completed inventory cycle.
- Lab stop, managed-save, container recreation, normal BMC power-on, and a
  successful Scout ping after resume. Disk, identity, and rotated BMC state were
  retained.
- An interactive serial login and direct inspection of guest network state.

Observed inside Linux during the initial Scout boot:

```text
enp1s0: 10.84.30.10/24, fd42:84:30::a/128, fe80::ff:fe01:2/64
IPv6 on-link route: fd42:84:30::/64, learned through RA
IPv6 default route: via the lab router's link-local address, learned through RA
forge-scout: active
cloud-init: done, no errors or recoverable errors
```

The guest reported 3901 MiB total memory and approximately 803 MiB used after
Scout startup. This is one observation, not a minimum-memory guarantee or a
measurement of the outer VM's total footprint.

The Scout Debian package and original iPXE/qcow-imager artifacts came from upstream
revision `12146af3fefdb4c1800d90de7755563fe71aa4db`. The VM builder produced matching
Scout loader/root filesystem images with Ubuntu kernel `6.8.0-147-generic`.
The published Scout image's 64 KiB kernel was rejected by the virtual CPU;
the locally built 4 KiB kernel booted successfully.

## Automated checks

- 14 Python tests covering persistent inventory, inventory growth, domain identity, configuration,
  cleanup failure handling, image staging, and DHCPv6 relay packet contracts.
  These tests require neither KVM nor an IPv6-capable runner.
- Seven shared REST integration shell checks, including real Scout readiness and
  the VM backend's declared inventory.
- 165 `bmc-mock` tests, including zero-DPU boot options, generic AMI lockdown,
  resource persistence, and cold-power-cycle behavior. The external
  `ipmitool`/IPMI-simulator test was not run.
- All 81 DHCP tests against both Kea 2.2.0 and 2.4.1, including real multi-worker
  DHCPv6 handoff, contention, expiry, and native-exception regressions.
- Shell syntax, Python formatting, Markdown lint, and whitespace checks.
- Independent review of the initial profile implementation; its findings were
  addressed. Independent review also covered the DHCPv6 transaction guard and BMC persistence follow-up.

## Two-target follow-up

The original pair of tenant VMs ran Ubuntu 24.04 ARM64 from `/dev/vda1` and passed
the requested Internet and peer IPv6 checks on 2026-10-13. At that checkpoint,
Core reported both instances `Ready` with configurations `Synced`. These instances
were subsequently released for the reproducibility run below.

| Target | IPv4 | IPv6 | Instance ID |
| --- | --- | --- | --- |
| 1 | `10.84.30.10` | `fd42:84:30::a` | `19a64f16-b26e-492d-9d02-08948dd345ea` |
| 2 | `10.84.30.11` | `fd42:84:30::b` | `49b04a70-4496-4161-bdaa-4428de8e31a3` |

Observed from inside both tenants:

- Google HTTPS returned HTTP 200 over IPv6 and IPv4, with TLS verification
  successful (`ssl_verify_result=0`). Curl ignored local configuration and
  bypassed proxies; the reported IPv6 source addresses were the tenants' own
  `::a` and `::b` addresses.
- IPv6 peer ping passed in both directions, three replies from three requests
  each. IPv6 TCP connections to the peer's port 22 returned SSH banners in both
  directions. The first target's initial banner read timed out during the
  second target's startup; a retry after startup passed with the same timeout.
- IPv4 peer ping also passed in both directions with no packet loss.
- Packet captures show each Linux client completing DHCPv6 Solicit, Advertise,
  Request, and Reply. No tenant IP address was assigned manually. IPv6 on-link
  and default routes came from router advertisements.

This is a Core-allocated, no-DPU flat-VPC experiment using the host-inband
prefixes below and IPv6 NAT at the lab gateway. It does not validate a routed
public tenant prefix, REST/UI instance creation, or DPU forwarding. PXE image
downloads and the configured DNS service use IPv4.

The VM retains `84-tenant-validation-both-peers.json`,
`84-peer-1-to-2-tcp-retry.json`, and `84-tenant-2-final-cloud-init.json` under
`/tmp`, plus bounded DHCP packet captures. The collector returns a nonzero exit
for warnings/failures; the networking results above are the individual check
results, not a claim that cloud-init was warning-free.

The lab was grown from one target to two without replacing the first target's
identity, disk, or rotated BMC account. Both guests booted the real Scout image.
The upstream lab gateway was given return routes and outbound NAT rules for
`10.84.30.0/24` and `fd42:84:30::/64`. Certificate-verified Google HTTPS returned
HTTP 200 over both families when sourced from the outer VM's target-bridge
addresses. This is gateway evidence, not tenant-instance evidence.

Follow-up exposed two blockers:

- BMC container recreation retains accounts but resets BIOS/lockdown settings.
  The first host entered `BootConfiguring/LockHost` and exhausted its recovery
  deadline. The operator API restored full lockdown. After explicit approval,
  the original machine row was saved and a conditional transaction restarted
  only the recovery clock, with a new version and history entry. Exactly one
  row matched. NICo then completed normal LockHost verification and returned
  the target to `Ready`; the repair did not force that state.
- Firmware and Linux use different DHCPv6 DUIDs. Redirecting Kea's candidate
  address onto the previous boot-stage lease caused repeated failed allocations.
  A real-Kea regression reproduced 65,535 attempts without a usable address.
  The corrected same-MAC handoff passes all 81 DHCP tests against Kea 2.2.0
  and 2.4.1. A transaction guard covers native lease persistence, delayed
  renew/rebind requests cannot replace another boot-stage identity, and expiry
  revalidates the stored lease before notifying Core. Contending packets retry;
  packet processing is serialized across this guard, so these results do not
  establish production throughput. Tests exercise real multi-worker Kea,
  observed busy expiry and native counter balance, and recovery after a native
  packet exception. The missing Kea 2.2 development header is now bundled from
  its exact upstream release and selected automatically only when needed.
  The tested Kea 2.2 hook is deployed in the lab with four workers.

A flat VPC and the checksum-verified Ubuntu 24.04 ARM64 cloud image are registered
for the tenant test. The exact image SHA-256 is
`1d6bffe64b848468ac97f821d369a4846d983de1800ccf6b5ec8853e85cefc55`.
Its root filesystem label is `cloudimg-rootfs`, separate boot filesystem UUID
is `5b886046-3262-4eca-a390-4cd88779a3c7`, and EFI filesystem ID is `1C9E-FED1`.
These identifiers belong to this image; inspect a replacement image before
registering it. Tenant login uses a separate generated SSH key outside the
image server's directory. The second target reached `Ready` through normal
provisioning, and a tenant instance was allocated with `10.84.30.11` and
`fd42:84:30::b`. Core reports `Assigned/Ready`, but phone-home is disabled for
this test: that state alone does not prove tenant OS readiness. The guest
booted qcow-imager, downloaded the tenant image over DHCP-configured IPv4,
verified its SHA-256 digest, and wrote the image to the virtual disk.
The first attempt wrote the image and booted Ubuntu but did not produce a usable
tenant: the image's separate `/boot` filesystem was omitted from its catalog
metadata. GRUB regeneration failed, the installer returned before cloud-init
seeding, and Ubuntu had neither the requested networking nor login configuration.
The catalog registration was corrected with the filesystem UUIDs read from the
original image, then `instance update-os` and `instance reboot --custom-pxe`
requested another normal provisioning attempt. That retry completed: the
second tenant boots Ubuntu 24.04 from `/dev/vda1`, accepts its SSH key, and has
DHCP-assigned `10.84.30.11/24` and `fd42:84:30::b/128`. Its IPv6 default and
on-link routes are learned through RA. Google HTTPS returned HTTP 200 over
both families with certificate verification enabled (`ssl_verify_result=0`).
A captured DHCPv4 renewal retained its address; this capture did not contain
a DHCPv6 renewal, so it does not establish DHCPv6 renewal behavior.

The original tenant fixture reported `degraded done`, with no fatal errors and one schema warning:
the custom top-level `network` key is also retained in user-data after NICo
extracts the separate, valid network-config document. The requested user, SSH
key, DHCP settings, and DHCPv6 prefix-delegation opt-out were applied. The replacement fixture omits the custom
network key, uses NICo's default DHCPv4/DHCPv6 network document, and passes the
Ubuntu cloud-init schema checker. Clean completion is required in the lifecycle
runner.

The recovered first target then completed the same normal PXE installation.
Its guest-level checks confirmed disk boot, completed cloud-init with the same
schema warning, and working IPv4 and IPv6 Internet access.

A normal API reboot of the second tenant, concurrent with first-target imaging,
exposed a lab boot-time problem: systemd timed out waiting for the `BOOT` and
`UEFI` devices and entered emergency mode. Continuing boot from the console
resumed cloud-init after device initialization. The retained packet capture
then shows the tenant Linux DUID completing DHCPv6 Solicit, Advertise, Request,
and Reply for `fd42:84:30::b`, after the earlier firmware DUID exchanges. This
proves DHCPv6 reacquisition but not an unattended successful warm reboot.
After graceful shutdown and completion of first-target imaging, powering the
second guest on succeeded without console intervention. Its kernel boot ID
changed, its SSH host key and tenant disk persisted, cloud-init completed with
the same schema warning, and the final Internet and peer checks passed.

Nested KVM was enabled throughout. First-target image conversion took about
33 minutes. The outer VM recorded two watchdog soft-lockup reports in KVM's
`stage2_unmap_put_pte` during that target's reboot; the guest continued booting.
The trace is retained as `/tmp/84-kvm-warnings.log`. The relationship between
these reports, host resource pressure, and the boot timeout is not established.
The follow-up Python suite contains 14 passing tests, including inventory growth.

## Reproducibility follow-up

The BMC fix passed 165 tests (the external `ipmitool`/IPMI-simulator test was
not run). New regressions failed before the fixes and passed afterward. Both
live BMCs were replaced after privately backing up their account state and
migrating their observed resource settings. BIOS lockdown, host-interface
lockdown, and boot order remained unchanged after replacement and a second
restart. No machine-state database update was used for this follow-up.

Both modified Rust packages pass Clippy. The VM-profile suite passes 14 tests;
Rust formatting, Markdown lint, and whitespace checks also pass.

The lifecycle runner releases the previous instances, allocates and images each
new guest sequentially, validates both guests, reboots them through the API,
validates again, and releases them back to `Ready`. It requires distinct fresh
per-target markers, absence of a previous-generation disk sentinel, disk-root
boot, cloud-init success, and changed kernel boot IDs after reboot. It checks
certificate-verified Internet HTTPS over IPv6 and IPv4, bidirectional IPv6 ping
and TCP, and IPv4 peer ping. It stops on failure and retains diagnostics.

Two complete lifecycle cycles passed on 2026-10-13. The original instances were
first released; both hosts returned to `Ready` at 18:12:55 UTC. Each cycle then
allocated two fresh instances, validated the pair, rebooted both through the
API, validated again, and released both hosts back to `Ready`.

| Cycle | First allocation (UTC) | Both fresh tenants ready (UTC) | Final hosts Ready (UTC) | Checks |
| --- | --- | --- | --- | --- |
| 1 | 18:12:57 | 19:11:53 | 19:23:50 | 20 provisioned + 20 after reboot; all passed |
| 2 | 19:23:52 | 19:39:58 | 19:51:52 | 20 provisioned + 20 after reboot; all passed |

| Cycle | Target 1 instance | Target 2 instance |
| --- | --- | --- |
| 1 | `a9146e6f-f7f4-441f-b99b-b25bd13e964d` | `9b4d6aca-5160-4e09-aa0f-7bd309bec2ae` |
| 2 | `3ea9ac3c-d2a0-41d7-8219-c9095c49e98a` | `25621671-d0d3-4bbb-9a09-38cdced6a256` |

All four fresh installations booted from `/dev/vda1`, had the expected new
per-target marker, and lacked the previous tenant's sentinel. Cloud-init
completed without errors or recoverable errors. All four API reboots returned
with changed kernel boot IDs and retained their tenant SSH host keys. Every
Internet check returned HTTPS 200 over IPv6 and IPv4 with certificate verification
successful. IPv6 peer ping and TCP/SSH banners, and IPv4 peer ping, passed in
both directions before and after reboot. No guest-console intervention or
machine-state database repair was used during these cycles.

The first cycle included a harness correction: early SSH readiness probes saved
the installer's host key. The runner was interrupted and corrected to use a
separate candidate key during fresh installation, promoting it only after
authenticated checks establish the expected tenant disk and cloud-init state.
The same in-progress allocation continued without restarting its installation.
Independent review found no blocker in this correction. Cycle 2 ran without
further harness changes. These results establish two completed guest lifecycles,
not two uninterrupted executions of the original runner.

The packet capture contains 709 packets with zero kernel drops. Matched
request/reply pairs between tenant readiness and release retained each tenant's
allocated address:

| Cycle | Target | DHCPv6 Renew/Reply pairs | DHCPv6 Rebind/Reply pairs |
| --- | --- | --- | --- |
| 1 | 1 | 2 | 28 |
| 1 | 2 | 0 | 1 |
| 2 | 1 | 2 | 11 |
| 2 | 2 | 0 | 1 |

The second target's evidence establishes rebind-based lease refresh, not an
observed Renew exchange. Lab renewal/rebind timers were 60/120 seconds;
preferred/valid lifetimes remained 3600/7200 seconds. This made refreshes
observable without shortening the validity window during imaging. The outer
VM recorded no new watchdog soft-lockup or out-of-memory reports during the run.
Installation durations varied substantially; these are functional results,
not a performance guarantee.

### Evidence and replay

The prepared VM retains the private evidence directory
`~/.local/share/84-lifecycle-20261013-repro/`. Its `summary.json` cross-checks the
two completed-cycle events, all four 20-check result files, changed boot IDs,
and matched tenant DHCPv6 refresh replies. `final-state.json` independently
confirms two `Ready` hosts and no instances. The directory also contains
`events.jsonl`, source hashes, deployed-image identities, kernel logs, and
`dhcp6.pcap`. The run's temporary kubeconfig snapshot was removed after completion.

The corrected runner and its SiteAgent helper are retained under
`~/.local/share/84-lifecycle/`; the OS fixture and SSH key remain separately under
`~/.local/share/84-tenant-access/`. Replaying requires this prepared VM, reservation
`84`, the saved lab identities and fixtures, the tested local images, the existing
`kind-nico-vm-hosts` cluster, and the gateway routes/NAT configuration. This is
not evidence that an unmodified upstream deployment reproduces these results.

From the prepared VM, with both hosts `Ready` and no instances, run:

```bash
run_output="$HOME/.local/share/84-lifecycle-$(date -u +%Y%m%dT%H%M%SZ)"
python3 "$HOME/.local/share/84-lifecycle/runner.py" --output "$run_output"
```

`--output` is required and must name a new directory. The runner executes two
cycles and leaves the hosts unallocated at `Ready`; a failure stops the run and
retains diagnostics. It checks the VM reservation at startup and validates
machine and tenant scope before instance release. It creates a private kubeconfig snapshot in its output
directory, so that directory must not be shared wholesale. The retained runner's
SHA-256 is `8688f460971abf05723bfe84e5fe44eaed9d4e208d7dda991f2a7201f89485c8`.

## Remaining issue and validation boundaries

The requested no-DPU Internet IPv6 and cross-instance IPv6 scenarios passed
across both complete lifecycle cycles on the prepared VM, using the tested local
fixes and gateway NAT. Both hosts were left `Ready` and unallocated.

Stable zero-DPU hosts produce a recurring Site Explorer subtype-conversion error
during re-exploration: the BMC lookup in `create_zero_dpu_machine` infers
`PredictedHostMachineId` before the stable-host fast path. Existing host lifecycle
operations continue, but that exploration transaction's reconciliation is
skipped. Independent review confirmed this cause; it is not fixed in the build
used for these cycles.

The lab has no GPU, DPU, or TPM. It does not establish hardware attestation,
firmware validation, DPU forwarding, secure erasure of physical storage, or
tenant isolation on physical hardware.

## IPv6-only host lifecycle validation on 2026-10-21

The combined PR integration at `477f831b9aefaf22b428c8b7a52e31c4b2d3e6b4`
provisioned two no-DPU ARM64 guests with IPv6-only installed OS networking.
The outer control plane and gateway remained dual-stack; Internet IPv6 used
existing gateway NAT. This did not test IPv6-only stack bootstrap or DPU/HBN
forwarding.

Both guests, `fd42:84:30::a` and `fd42:84:30::b`, passed Internet HTTPS with TLS
verification, normal address selection, IPv6 DNS, bidirectional ICMP and SSH
reachability, and absence of non-loopback IPv4 addresses and routes. The same
checks passed after supported NICo instance reboots, with changed boot IDs and
both machines in `Assigned/Ready`. Root filesystem and per-instance markers
were verified separately; no failed systemd units remained.

ARP and IPv4 were blocked for both host and BMC MAC addresses through
installation and reboot. IPv6 BMC registrations, firmware/iPXE HTTP requests,
OS image downloads and DHCPv6 exchanges were observed. Only the first target
underwent release and Scout cleanup while that guard was active; the second
was already Ready. This does not establish secure erasure of virtio disks.
The temporary guard and captures were removed after validation, leaving both
IPv6-only tenants running.

This was not an unmodified default-artifact run. Additional IPv6 Core/PXE/DNS
Services, routes, AAAA records, DHCPv6 options and an IPv6 image listener were
configured. The rebuilt Scout and imager used the historical address-only
`UseDelegatedPrefix=no` policy, including Scout's generated preferred-interface
configuration. [PR 7702](https://github.com/dsx-ai-factory/infra-controller/pull/7702)
was already merged and present in the tested source; the override does not
establish that another prefix-delegation fix is necessary. Mixed IA_NA/IA_PD
behavior was not requalified by this run. The lab extension does not add that
override to production boot sources.

Each guest passed 15 functional checks before and after reboot. The two
remaining checks reported the known cloud-init user-data schema warning
covered by [PR 7728](https://github.com/dsx-ai-factory/infra-controller/pull/7728).
The exact error was the unexpected top-level `network` property. Network-config
was valid; a temporary user-data copy with only that extracted property removed
passed schema validation. All cloud-init execution-error lists were empty.
The raw nonzero validator results were retained rather than relabeled as an
unqualified pass.

Nested disk imaging was slow, and the outer kernel reported a KVM memory-unmap
soft lockup during an installer reboot. It recovered; installed-OS startup and
both supported reboots completed. This run does not establish a fix for that
performance issue.

The reusable `configure_ipv6.py` and `validate_ipv6_only.py` helpers preserve
these setup and guest-check operations on the lab-extension branch. Their
follow-up validation reapplies configuration to the retained lab and checks
both guests; it does not replace the original provisioning/guard evidence.
Machine-specific captures, credentials, instance identifiers and private lab
artifacts are not part of this branch. The IPv6 lifecycle prerequisites and
remaining manual steps are described in [the profile README](README.md).

At source revision `3ebd6b695`, the reusable setup helper was applied twice to
the retained lab: service
addresses and CoreDNS/DHCPv6 pod identities remained stable on the second run.
Both guests then passed the same 15 functional checks using the reusable
validator, which retained the two known schema/warning failures. All 30
portable VM-host tests passed with the documented PyYAML prerequisite;
independent review also passed the nine new helper/validator regression tests.
