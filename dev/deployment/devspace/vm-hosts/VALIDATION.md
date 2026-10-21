# DevSpace BMC and VM validation

## Rebase validation: 2026-10-20

The branch was rebased onto PR 7078 revision `081bde1c9`. The generated Redfish
BIOS API was updated, and the persistence regression now explicitly saves and
reloads state without the executable-script harness removed upstream. It checks
lockdown, host-interface settings, boot order and the one-shot boot override;
it does not exercise libvirt actor subprocess orchestration.

On this revision, 17 VM-host Python tests, 21 DevSpace Python tests (one additional
test skipped), the REST setup shell suite, and 177 BMC tests passed. The external
IPMI simulator test failed because its executable was absent on the development
host and was excluded from the successful package run. Formatting and Markdown
lint passed. Full-stack deployment and the full workspace suite were not rerun.

## Historical deployment scope and source

The 2026-10-13 validation uses two native x86_64 KVM targets and a dedicated,
IPv4-only kind cluster. Each target has 4 GiB RAM, two vCPUs, and a 24 GiB
sparse disk. No machine-a-tron process supplies target discovery or Scout
responses. The target bridge uses `10.84.30.0/24`; no IPv6 gateway setup is
required.

The feature depends only on [PR 7078](https://github.com/dsx-ai-factory/infra-controller/pull/7078),
rebased onto its `081bde1c9` revision on 2026-10-20. The vfkit commits from
PR 6606 and the experimental DHCPv6 changes are not included.

The deployment evidence below was collected before this rebase. Its initial
DevSpace prerequisite was `ff9515de3`, and the feature tree was `b49ac02b3`.
The original ARM64 dual-stack experiment does not establish live validation
of this rebased tree.
Core, bmc-mock, Scout, iPXE, and the qcow imager were built from this checkout.
The native Scout Debian package is `0.0.0-319f8b2ab-vm`; its VM boot profiles
use `linux-image-virtual`. The pinned mkosi revision is
`52448a27f6f869c108352ed82fcfb9be633703fb`, and iPXE starts at
`bbd7821bd42da5456ee068a471ef73d525ea26a1` with the repository's patches.
Payload provenance is retained in `build-info.json` beside the boot artifacts.

## Completed checks

- Seventeen portable Python tests pass on both development and native test hosts.
  Coverage includes native firmware selection, x86 option ROM settings,
  persistent identity and resource sizes, IPv4 forwarding, generated Helm
  values, both Scout staging profiles, rollout timeout handling, and resuming only owned
  saved guests before exposing their BMC endpoints. The restart regressions
  also check save-before-network-removal and scoped resume after network setup;
  both failed against the previous implementation and pass with the fix.
- The REST setup shell tests pass, including real Scout readiness for VM targets
  and the absence of a Scout requirement for assigned tenant hosts.
- Native bmc-mock tests pass: 163 library tests and two binary tests. The external
  `ipmitool` simulator test was excluded.
- Native Core, Scout, boot artifacts, BMC and libvirt images build successfully.
- DevSpace deployment completes. Both targets pass actual Scout discovery,
  BMC password rotation, lockdown, cleanup and reboot. Both reach Core `Ready`,
  appear in REST inventory, and respond to Scout stream pings.
- Black, Markdown lint and `git diff --check` pass. Independent review found
  timeout, saved-resource redeployment and guest-resume issues; all were fixed
  and reviewed.

## Tenant validation

Both guests completed unattended disk installation and cloud-init. All 32 checks
passed across initial provisioning and API-triggered reboot: authenticated SSH,
disk root, cloud-init, addresses, routes, boot IDs, direct IPv4 HTTPS egress and
bidirectional peer ICMP/TCP connectivity. Both guests obtained new boot IDs after
reboot. Both instances were then released through NICo, and both hosts returned
to Ready with no remaining instances. The fixture uses an official
Ubuntu Noble amd64 cloud image with SHA-256:

```text
6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2
```

The image was served only on the lab bridge. A temporary firewall rule permitted
lab clients to reach that test server. The server, rule, and test image
registrations were removed after validation. Image download and digest handling exposed
an existing mismatch: the CLI example uses a `sha256:` prefix, while
`disk_imaging.sh` expects the bare hexadecimal digest. The fixture now uses the
bare digest. The image has separate `/boot` and EFI filesystems; the fixture
supplies `bootfs_id=fb8b4264-587e-4a60-a103-a1d39552f189` and
`efifs_id=103C-DA09`, read from the image itself. This split does not change the
production imaging contract.

The run completed at 23:32 UTC. HTTPS returned status 200 with certificate
verification enabled on both guests before and after reboot. Core
`Assigned/Ready` alone was not considered proof of tenant boot.

## Stop and redeploy

The first restart attempt exposed a network ordering race: guests resumed before
DHCP and service routes were restored, and guests still finishing release boot
fell back to their tenant disks. The helper now saves guests before removing
networking, restores saved guests paused before exposing BMC endpoints, and
resumes them after routes, DNS and DHCP are ready.

The corrected run completed at 23:49 UTC. Both targets passed Core Ready, REST
inventory and real Scout pings. Persisted credentials still authenticated;
boot, BIOS and host-interface settings and domain UUIDs matched the baseline.
Console kernel-start counts were unchanged, proving memory resume rather than a
cold boot. Both targets remain available with no tenant instances allocated.
The private credential comparison file was removed after the assertions passed.

Source hashes for the 24 changed non-documentation files matched between the
checkout and test host. Runtime image identities, boot artifact hashes, lifecycle
results and deployment logs are retained with the private test evidence outside
Git.

## Limits

The initial x86 domain definitions were updated in place to enable the iPXE
option ROM. Unit coverage checks freshly generated definitions, but this run
must not be described as an untouched cold deployment of the final definitions.
OVMF still attempts PXE fallbacks before HTTP boot, adding boot latency.

Site exploration logs also show a stable host ID being passed where a predicted
host ID is expected after discovery. In this run it did not prevent Ready or
Scout connectivity; it is not fixed by this profile.

This validation does not cover the full workspace test suite, IPv6, live ARM64
execution of this split, DPU behavior, or physical hardware isolation.
