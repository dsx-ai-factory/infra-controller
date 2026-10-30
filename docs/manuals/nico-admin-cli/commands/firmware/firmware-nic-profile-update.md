# `nico-admin-cli firmware nic profile update`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [profile](./firmware-nic-profile.md) › **update***

## NAME

nico-admin-cli-firmware-nic-profile-update - Replace a NIC firmware
profile definition at its observed version

## SYNOPSIS

```text
nico-admin-cli firmware nic profile update [OPTIONS] --if-version-match <VERSION> <ID>
```

## DESCRIPTION

Replace the complete NIC firmware profile config in Core. Include every
entry you want to keep; this is not a merge or patch. Core requires the
original observed --if-version-match token and advances the edit version
on success. For an unknown profile, Core returns server status
NOT_FOUND; stale versions return server status FAILED_PRECONDITION.

Core also rejects removal of hardware support required by existing site,
machine, or card selections with server status FAILED_PRECONDITION.
Tenant selections do not bind hardware and do not impose this
compatibility check. Compatibility checks precede the stored-version
check. The command preserves the server diagnostic and does not read a
fresh version or retry the update. After a connection error, inspect the
profile; the change may have committed. Keep the original token when
repeating the same request.

This changes the live definition used by existing selections, without
fetching artifacts or starting firmware updates. Use --format
ascii-table (default), csv, json, or yaml and --output PATH before the
firmware subcommand. JSON and YAML output is a one-element array
containing the stored id, config, and new version. Table and CSV output
has one row per entry.

Output defaults to stdout. --output requires a path that does not exist;
the file is created before the operation, even if the operation later
fails. --sort-by and --extended have no effect.

## OPTIONS

`--config-file <PATH> [default: -]`

Read a complete NIC firmware profile config as UTF-8 JSON. Omission or a
single hyphen (-) reads standard input until EOF; any other value is a
file path. This is request data, not a controller configuration file.
Supply the config object itself, without an enclosing id, config, or
version field. To reuse JSON output, extract [0].config as JSON. YAML
output is not accepted as input. When standard input is a terminal, the
CLI prints a reading hint to stderr.

The required entries array must be nonempty. Entries preserve order and
must have unique (part_number, psid) pairs. Each entry requires firmware
with part_number, psid, and exact version strings; image with url and
sha256; and approach set to the case-sensitive string "Scout". Part
numbers, PSIDs, and firmware versions are literal, nonempty strings
without surrounding whitespace or control characters. An older firmware
version is a valid target. The optional device_config is omitted or null
when absent, or has the same url and sha256 fields as image. Hardware
selection and resolution support at most 256 UTF-8 bytes per part number
or PSID; catalog registration accepts longer strings.

Artifact URLs must be absolute HTTP(S) URLs accessible without login,
with no userinfo, query, or fragment. Each sha256 must contain exactly
64 hexadecimal digits. URLs are normalized and digests stored lowercase.
Registration does not fetch artifacts or verify their bytes.
Unrecognized JSON fields are ignored; a misspelled device_config is
treated as absent. Omitting device_config removes it when replacing a
profile.

Unreadable input, malformed JSON, missing required fields, invalid URL
syntax, and unsupported approach names fail locally before a catalog
RPC. Core validates hardware pairs, artifact restrictions, and digests,
returning server status INVALID_ARGUMENT for invalid definitions.

Example config (replace the URL and digest with values for the actual
artifact):

```json

{ "entries": [{ "firmware": { "part_number": "900-9D3B4-00CV-TA0",
"psid": "MT_0000000884", "version": "32.43.1014" }, "image": { "url":
"https://firmware.example.com/connectx.bin", "sha256":
"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
"device_config": null, "approach": "Scout" }] }

```

`--if-version-match <VERSION>`

Original observed edit version, such as V1-T1789080000000000; keep
unchanged when repeating this request

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

`<ID>`

Case-sensitive profile ID; nonempty, without surrounding whitespace or
controls

## Examples

```sh
nico-admin-cli firmware nic profile update connectx-production --config-file ./nic-profile.json --if-version-match V1-T1789080000000000
cat ./nic-profile.json | nico-admin-cli --format json firmware nic profile update connectx-production --if-version-match V1-T1789080000000000
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
