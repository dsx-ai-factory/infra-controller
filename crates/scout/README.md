# Scout

Scout reports local hardware and executes tasks requested by Core. The Cargo
binary name is `forge-scout`.

## Read-Only NIC Firmware Preflight

The local `mlx firmware preflight` command verifies downloaded files and compares
one live NIC with an exact firmware target. It requires standalone mode and does
not register with Core. Display the complete command reference with:

```bash
forge-scout --mode standalone mlx firmware preflight --help
```

Supply the NIC's PCI address, expected part number and PSID, target firmware
version, image URL, and image SHA-256 digest. If you supply a device-config URL,
its SHA-256 digest is also required. Omitting both device-config arguments skips
that file. Digests accept exactly 64 hexadecimal digits, in either case.

Artifact sources must use HTTP or HTTPS without user information, a query, or a
fragment. Redirects are rejected. This command does not use Core credentials or
a proxy. Each present input is staged separately and checked before the device
query; equal basenames cannot overwrite each other. Temporary files are removed
when the command finishes or is cancelled by SIGINT or SIGTERM. Cleanup is not
guaranteed after SIGKILL or power loss.

Each artifact has a 600-second deadline for downloading and retries. Hashing
follows that deadline. The subsequent `mlxfwmanager` query has a 30-second
deadline. The query must return the requested PCI address, matching part number
and PSID, and a known installed firmware version. MST aliases are not resolved.

An exact version match and a different known version both return a successful
command status, with the comparison printed in the output. A newer installed
version still differs from an older exact target. Missing facts, identity
mismatches, invalid inputs, failed downloads, or checksum mismatches return an
error.

Preflight does not apply configuration, lock or unlock a device, burn firmware,
or reset hardware. It does not interpret image or device-config contents, verify
that configuration matches the running configuration, or establish that flashing
is safe. An exact firmware match is not approval to skip a future configuration
change.
