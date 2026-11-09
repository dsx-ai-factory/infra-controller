# nico-firmware

Firmware metadata loading, lookup, and artifact downloads for NICo.

## Overview

This crate owns the runtime loading logic for firmware metadata used
by NICo services. It loads firmware definitions from two sources:

- Entries embedded in the main NICo configuration file (host and
  DPU models).
- `metadata.toml` files discovered under a configured
  `firmware_directory` on disk.

Entries from both sources are merged into a single in-memory map
keyed by vendor and model, with conflict-resolution rules that let
newer on-disk metadata extend or override older entries.

## Scope

This crate is intentionally narrow:

- It depends on the firmware **data types** (`Firmware`,
  `FirmwareEntry`, `FirmwareComponentType`, ...) which live in
  `carbide-api-model`. Those types describe firmware; this crate
  loads and queries them.
- Core owns the top-level configuration file schema. `FirmwareGlobal` in
  `carbide-machine-controller` contains settings such as `firmware_directory`
  and embeds this crate's `FirmwareDownloadConfig` for downloads.
- `FirmwareDownloader` fetches Core firmware artifacts and verifies cached
  files. Its worker, HTTP deadline, and streamed-size limits are configured
  through [`firmware_global.downloads`](../api-core/src/cfg/README.md#firmwaredownloadconfig).
- `FirmwareArtifactCache` owns verified artifact bytes, bounded local storage,
  and physical readers. Core owns authenticated operation membership and
  supplies each request's retained digests.
- It does **not** perform firmware upgrades or version comparison. Those
  concerns live in `nico-api` and `nico-scout`.

## Key types

### `FirmwareConfig`

The main entry point. Constructed from a base map (populated from
`NicoConfig`) and a `firmware_directory` path. It is responsible
for loading, merging, and refreshing firmware metadata.

It exposes disk-state observation:

- `create_snapshot()` — produce a merged firmware snapshot (reads
  disk each call).
- `config_update_time()` — modification time of `firmware_directory`,
  used by callers that want to detect on-disk changes.

### `FirmwareConfigSnapshot`

An immutable point-in-time view of the merged firmware catalog.
Snapshots are created via `FirmwareConfig::create_snapshot()` and
can be queried without re-reading the disk.

It exposes lookup helpers keyed by vendor and model:

- `find(vendor, model)` — look up firmware metadata for a specific
  vendor/model pair.
- `find_fw_info_for_host(endpoint)` — look up firmware metadata for
  an explored endpoint.
- `find_fw_info_for_host_report(report)` — same, given the
  exploration report directly.
- `values()` / `into_values()` — iterate over snapshot contents.

## Loading behavior

`FirmwareConfig` is a **live view** over the firmware directory, not
a snapshot. Every call to `create_snapshot()` re-reads the directory,
parses every `metadata.toml`, and re-merges the entries on top of
the base map.

This lets operators add new firmware metadata at runtime without
restarting NICo: the next snapshot picks it up. Consumers that want
cheap in-memory lookups (or explicit snapshot semantics) should
cache a `FirmwareConfigSnapshot` and reuse it.

Merge rules, applied in oldest-to-newest directory order:

- A new vendor/model combination is inserted as-is.
- A newer `ordering` replaces the existing one (non-empty only).
- `explicit_start_needed = true` always wins.
- Per-component fields (`current_version_reported_as`,
  `preingest_upgrade_when_below`) are overwritten if the newer
  entry sets them.
- `known_firmware` entries are appended. If a newer entry marks one
  of its firmware versions as `default`, the `default` flag is
  cleared on all previously-registered entries for that component.

## Managed artifact cache

`FirmwareArtifactCache` stores SHA-256 artifacts in `digest-v1/` beneath the
configured firmware download-cache root. It preserves legacy URL directories,
unknown names, symlinks, and nonregular entries. Only one Core process may own a
namespace; sharing the directory across processes is unsupported. Each replica
has its own cache and may download the same artifact independently. Eviction can
make an artifact unavailable offline.

Capacity must be positive, no greater than 1 TiB, and at least
`firmware_global.downloads.max_download_bytes`. Core owns the configured capacity
and its default. New transfers reserve the maximum download size before source
access. Complete files, orphan partials, live readers, and active reservations
remain charged until physical cleanup settles. Verified files adopted after a
restart remain readable if the configured budget has shrunk below occupied
bytes; that pressure blocks new reservations. Each open supplies the caller's
current retained digests, which eviction protects together with readers and
active transfers.

Cache opens, checksum work, and transfers share the existing
`FirmwareDownloader` worker pool. Reader admission has a separate fixed limit
of 32, without a queue, so slow responses do not hold download workers. `Pending`
asks callers to poll again; it does not queue another transfer. A failed attempt
is reported before an explicit later poll retries it. Include the requested
digest in the retained set: once an idle entry has no bytes or retention, its
recorded failure may be dropped before any caller sees it.

Construction registers a cache owner in the supplied `JoinSet<()>` and performs
no filesystem work. Its caller must cancel the supplied token and drain the
owner set before exiting or removing the directory. Cancellation closes
admission and joins accepted open, read, download, and settlement work, including
physical blocking operations that outlive a disconnected request. Managed
cancellation uses the existing `transfer` download metric label. Worker panics
propagate through the owner. Dropping an individual request does not cancel an
accepted transfer.

Managed sources accept anonymous HTTP(S), without userinfo, query parameters,
or fragments. They never forward request credentials. Redirects are rejected as
a deliberate source security boundary: an approved origin cannot redirect a
transfer to a different destination. Managed diagnostics identify the digest
and bounded failure category without exposing source URL paths; the legacy
URL downloader keeps its existing redirect and logging behavior.

## Consumers

- `carbide-api-core`: `cfg/file.rs` constructs firmware metadata loading;
  `setup.rs` constructs the shared downloader and supervised artifact cache.
  `handlers/firmware.rs` reads catalog snapshots and
  `handlers/nic_firmware_artifact.rs` authorizes and streams managed artifacts.
- `carbide-machine-controller` and `carbide-preingestion-manager`: existing
  firmware orchestration uses catalog snapshots and the shared downloader.
- `carbide-site-explorer`: endpoint exploration uses the firmware catalog and
  its snapshots.

## Snapshot semantics

A lightweight `FirmwareConfigSnapshot` type has been introduced to
provide an immutable point-in-time view of the firmware catalog.
Snapshots are created via `FirmwareConfig::create_snapshot()` and
contain a `data: HashMap<String, Firmware>` that can be queried
without re-reading the disk. This avoids the hidden disk I/O that
would otherwise occur on each refresh and enables efficient sharing
(`Arc<FirmwareConfigSnapshot>`) across concurrent code paths.

## Future direction

`FirmwareConfig` currently fuses "loader" (disk I/O, merging) and
"catalog" (in-memory lookup) into a single type. A follow-up change
is expected to split this into:

- `FirmwareCatalog` — pure data, O(1) lookup.
- `FirmwareConfigLoader` — disk I/O, produces a `FirmwareCatalog`.

with consumers that want hot-reload holding an
`Arc<ArcSwap<FirmwareCatalog>>` refreshed by a background task. See
the enclosing nico-api refactor notes for details.

## License

Apache-2.0. See `LICENSE` at the workspace root.
