# Upgrade Kea to 3.0 LTS

NICo pins the ISC Kea 3.0 LTS packages in [`dev/docker/kea.version`](https://github.com/dsx-ai-factory/infra-controller/blob/main/dev/docker/kea.version). Build the hook and ship the DHCP daemons from the same repository revision and package version, on both AMD64 and ARM64. A Kea 2.x hook cannot be reused with Kea 3.x. ISC lists the LTS maintenance period on its [downloads page](https://www.isc.org/download/#Kea).

The migration is tracked in [#8084](https://github.com/dsx-ai-factory/infra-controller/issues/8084). It preserves the DHCP behavior already merged on main; the additional DHCPv6 boot-stage handoff work in [#7934](https://github.com/dsx-ai-factory/infra-controller/pull/7934) is independent.

## Direct upgrade

A Kea 2.2 deployment with one IPv4 catch-all subnet and retained leases using subnet ID `1` can upgrade directly to this release. Make `1` explicit in the configuration shipped with Kea 3; a separate preparation release is unnecessary. Multi-subnet deployments must establish their own existing ID mapping; the single-subnet rule does not establish that mapping.

Kea subnet IDs identify the server's configured subnets. They do not identify Core network segments: one catch-all serves many Core segments. This migration requires neither a Core database change nor an exchange of IDs through the NICo hook.

The site release integration owner must deliver both the image and every configuration source used to deploy it, including GitOps-managed ConfigMaps rendered outside this chart. Update their source JSON or templates in the same release change as the image reference, regenerate derived manifests, and validate the resulting configuration. A chart-default change does not migrate an external ConfigMap. Keep the old image and matching configuration available for rollback.

Publishing image and configuration changes in one Git commit does not order their reconciliation. The IPv4 chart uses a fixed ConfigMap name even with `config.enabled: false`. Where Reloader is installed, the chart's annotation requests restarts on changes to that name; without Reloader, a configuration-only change does not restart the IPv4 pod. A versioned ConfigMap requires deployment tooling to patch the Deployment's ConfigMap reference and update the Reloader annotation through the chart's `annotations` values. Alternatively, that tooling must orchestrate a stopped-writer update of the fixed-name configuration and image, accounting for Reloader where installed. Both approaches must prevent either daemon version from starting against the other version's hook path. The chart does not provide this migration orchestration.

Preserve the existing lease backend, directory, filename, storage mount where present, and server identity. In particular, a custom memfile `lease-database.name` such as `/var/lib/kea/dhcp4.leases` must not silently revert to the package default `/var/lib/kea/kea-leases4.csv`. The structured chart cannot express a custom lease filename; sites using one must retain `config.keaConfigJsonRaw` or an external ConfigMap. Preserve custom fields when updating that JSON. Verify the DHCP server identifier observed by clients rather than assuming that unchanged JSON guarantees unchanged identity.

The IPv4 chart has no lease volume or values setting to add one. Its memfile lives in the container's writable layer and is lost on container restart or pod replacement. Its `RollingUpdate` strategy can also overlap old and new writers. Preserving these files requires deployment tooling outside the chart to stop the old writer, transfer the complete lease directory, and mount the retained storage into the replacement without overlapping writers. Existing `emptyDir` mounts survive a container restart but not pod replacement. The retained-storage conversion and rollback procedure below applies once that storage and writer coordination are provided; a normal chart rollout alone does not supply them.

## Configuration changes

Kea 3 requires explicit, stable subnet IDs. The structured chart emits ID `1` for the shipped IPv4 and IPv6 catch-all subnets. An IPv4 override containing a single subnet without an ID also defaults to `1`. For an existing installation, verify that this matches its effective configuration and lease records before upgrading. For multiple IPv4 subnets, supply each existing ID under `nico-dhcp.config.kea.subnet4[].id`; rendering rejects omitted, duplicate, non-integer, zero, and out-of-range IDs. Usable IDs are `1` through `4294967294`.

Read the old server's effective configuration through its existing `config-get` control channel, where configured, and compare the `subnet_id` values in retained lease records. Do not derive IDs from the new list order or renumber existing subnets. An ID is a persistent lease association, not a display index.

```yaml
nico-dhcp:
  config:
    kea:
      subnet4:
        - id: 42 # Existing subnet ID.
          subnet: "192.0.2.0/24"
          pools: ["192.0.2.10-192.0.2.20"]
```

The IPv4 catch-all pool is `0.0.0.1-255.255.255.254`. Kea 3 treats an allocator result of `0.0.0.0` as pool exhaustion before invoking the NICo lease-selection hook, so the previous zero-based pool can suppress the first offer after startup. Update equivalent raw configurations and site overrides. NICo still supplies the actual client address; the pool excludes only the unspecified and limited-broadcast addresses.

The generated IPv4 and IPv6 configurations explicitly set `"cache-threshold": 0.0`. This retains the previous disabled lease-caching behavior instead of adopting Kea 3's `0.25` default. This setting is separate from the NICo hook's API-response cache. Kea requires a JSON floating-point literal here; integer `0` is rejected.

The generated configurations also set `"multi-threading": { "enable-multi-threading": false }`, preserving the Kea 2.2 production default. Kea 3 otherwise enables multithreading and automatically sizes the worker pool. The integration tests explicitly use four workers to retain concurrent-hook coverage; that test setting does not change the chart's production default.

IPv4 configurations supplied through `config.keaConfigJsonRaw` or an external ConfigMap (`config.enabled: false`) remain operator-owned. The chart does not apply its structured defaults to them. Add explicit subnet IDs, the floating-point cache setting, and an explicit threading policy, and validate the pool, hooks, and file paths before deployment. Keep the stable IPv4 server identifier and IPv6 server DUID unchanged.

## Restricted file paths

NICo container images set `KEA_HOOKS_PATH=/usr/lib/kea/hooks`, and the chart uses `/usr/lib/kea/hooks/libdhcp.so` on both architectures. Remove old architecture-specific `hookLibraryPath` overrides when using these images. Rewrite paths in `additionalHooksLibraries` and operator-owned configurations to the approved directory while retaining the required hook entries. The pinned `isc-kea-hooks` package preserves the public hooks previously included in Debian's `kea-common`, including HA, lease commands, and statistics commands; NICo images expose them in the same approved directory. Custom hooks must be installed there; an absolute path outside it fails validation.

Both DHCP deployments use the same configured image and chart `env` map. With `dhcp.v6Enabled: false`, a custom image or `env.KEA_HOOKS_PATH` can select another directory if every configured hook path is updated together. With IPv6 enabled, retain `KEA_HOOKS_PATH=/usr/lib/kea/hooks`: the generated IPv6 configuration fixes the hook path to `/usr/lib/kea/hooks/libdhcp.so`.

Default memfiles remain under `/var/lib/kea`, and default logging goes to stdout. Custom lease, file-log, and UNIX control-socket locations must match `KEA_DHCP_DATA_DIR`, `KEA_LOG_FILE_DIR`, and `KEA_CONTROL_SOCKET_DIR`, respectively. Kea also checks the control-socket directory's permissions; use `0750`. Keep these directories narrowly scoped and writable only as required. Do not use `-X` to disable security checks.

Native integration tests set the approved hook directory to their compiled-library directory and the data directory to the actual temporary lease-file directory. The native host setup scripts add the developer to `_kea`, because ISC's daemon executables are restricted to root and that group. Start a fresh login session after setup.

The container runtime directories and lease-cleanup executable retain the package's restricted modes but are owned by root, matching NICo's daemon user. The executable retains `_kea` group access. ISC's default `_kea` ownership otherwise requires root to override filesystem permissions, which the native AppArmor profile blocks. On development hosts running Kind, rerun the host preparation's Kea AppArmor setup to permit the container PID, hook, and certificate paths. Keep the profiles enforced. A mounted lease directory must also be writable by the configured daemon user.

## Deployment and recovery

1. Record the old image, chart values, effective configurations, subnet IDs, server identifiers, package versions, and lease storage settings, including any custom filename. Check any custom hooks, control-agent clients, SQL backends, or HA peers against the [ISC upgrade notes](https://kb.isc.org/docs/en/things-to-be-aware-of-when-upgrading-to-kea-300). SQL backends require `isc-kea-mysql` or `isc-kea-pgsql`, their database hooks in the approved directory, and schema migration; they are outside the default memfile deployment. Mixed Kea 2.x/3.x HA peers are not an upgrade strategy.
2. Render the complete candidate deployment from its Helm or GitOps sources with the site's values. Inspect the configuration actually referenced by each pod, including external ConfigMaps. In the candidate image, run `kea-dhcp4 -t /path/to/kea_config.json` and `kea-dhcp4 -T /path/to/kea_config.json`; use the corresponding `kea-dhcp6` commands for IPv6. `-t` checks configuration and `-T` also loads hooks. Supply the same approved paths, certificates, and hook parameters as the deployment. The IPv6 chart's interface-address placeholder must first be resolved by its startup script. Syntax and hook loading alone do not prove API connectivity or packet delivery.
3. Schedule the DHCP restart and stop the old writer before taking a consistent backup of the entire lease directory, including LFC files, and any persisted server identifier. Existing client leases remain valid during a short outage, but new allocations and renewals require an available server. Account for the chart's rollout policy and prevent overlapping writers to a shared lease store.
4. Deploy the matching image and configuration, preserving the recorded lease storage and server identity. Kea can convert older memfiles automatically through lease-file cleanup (LFC); inspect startup and conversion errors before continuing. Conversion can rotate retained leases into `.2`, so the active lease file alone is not a complete snapshot.
5. Verify representative IPv4 and IPv6 clients through their real relays: initial allocation, renew/rebind, DNS/NTP and boot options, API allocation records, metrics, and a server restart with retained leases. Kea 3 emits separate NTP option 56 instances; NICo constructs parsed suboptions so the configured addresses survive that serialization.

Rollback requires the old image and its matching hook/configuration plus lease state readable by that version. Do not assume that replacing the image reverses a memfile or SQL schema upgrade. Stop DHCP writers and retain the post-upgrade lease state before recovery. Reconcile allocations and renewals made since the backup before restoring old state; blindly restoring an earlier lease snapshot can make active addresses appear free. Exercise this recovery procedure on a copy of the site's lease store before the rollout.

Before promoting the migration release to a site, its release integration owner must exercise the complete sequence using the matching NICo images, NICo hook, Core API, and that site's deployment mechanism: acquire a lease on Kea 2.2, upgrade with the same lease storage and explicit ID, renew that lease on Kea 3, allocate another lease, and roll back with the matching old image and configuration. Verify both clients afterward, including the renewal expiry, subnet ID, lease filename (including custom names where configured), and server identifier. IPv6-enabled sites must also preserve their existing IPv6 subnet IDs and server DUID and exercise this sequence with an IPv6 client; the IPv4 single-subnet rule alone does not establish their compatibility. Include LFC and a daemon restart so the check covers rotated files as well as the active memfile. Run this on isolated storage. A stock-Kea format rehearsal, successful fresh install, or restored pre-upgrade snapshot does not establish this integrated contract.

## Build and validation

Container builds and native setup use ISC's signed public `kea-3-0` repository and the exact package version in `dev/docker/kea.version`. The repository key is checked in with fingerprint `9DA570BB192211885E4EB280B16C44CD45514C3C`. The distro's older `kea-common` package must be removed from native development hosts so obsolete libraries cannot be selected when linking the hook.

The DHCP tests execute the compiled hook inside real Kea daemons. They cover packet options, stateful and stateless exchanges, retained leases, release/decline/expiry, and restarts. The migration also covers import of the legacy DHCPv6 memfile schema. Run them in the matching compiler image:

```bash
cargo test --locked -p carbide-dhcp --tests
```

Run the `nico-dhcp` Helm unit tests as well. Deployment validation must still exercise the site's API, certificates, storage, relay topology, and any additional hooks. See the [Kea 3.0.0 release notes](https://downloads.isc.org/isc/kea/3.0.0/Kea-3.0.0-ReleaseNotes.txt) and [ISC package documentation](https://kb.isc.org/docs/isc-kea-packages) for upstream compatibility details.
