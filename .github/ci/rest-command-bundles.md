# REST command bundle contract

REST has 11 standalone command names, while the production Linux images also
need `nicocli` and `nico-mcp`. Building each command in its own job repeated the
same checkout and Go setup, and it left the Docker builds with no checked bundle
they could reuse.

So, [`rest-command-manifest.json`](rest-command-manifest.json) is the one list of
REST commands this workflow may build. One job builds everything for a target,
checks each binary, and uploads one archive with the exact build record a later
job needs.

## What can be built

The source manifest is an object with exactly three required fields:

- `schema_version` is the integer `1`.
- `targets` is a non-empty array. Each entry has the non-empty strings `name`,
  `goos`, and `goarch`; `name` must be the literal `<goos>-<goarch>` value, and
  target names must be unique. The supported names are exactly `linux-amd64`,
  `linux-arm64`, and `darwin-arm64`.
- `outputs` is a non-empty array. Each entry has `name`, `package`, `target`,
  `cgo_enabled`, `ldflags`, and `output`. The command name uses lowercase
  letters, numbers, and internal hyphens. `package` starts with `./`, contains
  no empty, `.`, or `..` path components, cannot escape through a symlink, and
  names an existing directory under `--repo-root`. `target` names a declared
  target, `cgo_enabled` is a Boolean, `ldflags` is an array of non-empty
  strings, and `output` is exactly `bin/<name>`.

`cgo_enabled: true` sets `CGO_ENABLED=1`; `false` sets it to `0`. Command names
and `output` paths must be unique within one target. Every target needs at least
one output and produces one `rest-command-bundle-<target>.tar.gz` archive;
source-manifest order becomes resolved-manifest command order. Unknown fields
and unsafe paths fail validation.

Linker flags may use only `${VERSION}`, `${SHORT_SHA}`,
`${BUILD_TIME_LEGACY}`, and `${BUILD_TIME_RFC3339}`. Any other dollar form is
rejected; literal-dollar escaping is unsupported, and a backslash does not
escape a token.

The checked-in inventory has three deliberate boundaries:

- Linux amd64 and Linux arm64 each build 13 commands. The ten production image
  entrypoints are `api`, `migrations`, `sitemgr`, `workflow`, `site-agent`,
  `credsmgr`, `flow`, `psm`, `nsm`, and `nico-mcp`. They use the same settings
  and metadata fields as their production Dockerfiles. Bundle timestamps use
  canonical UTC values rather than inheriting a runner's local timezone. The
  API image also contains `nicocli`; both `nicocli` and `nico-mcp` are
  Linux-only bundle outputs.
- `mock-core` and `mock-flow` keep the previous standalone behavior. The native
  Linux amd64 builds use CGO, while the cross-compiled Linux arm64 builds do
  not. That amd64 target therefore needs a working C compiler; build metadata,
  rather than `file` wording, verifies its `CGO_ENABLED=1` setting.
- Darwin arm64 keeps the previous 11-command standalone contract. The API,
  site-agent, and flow commands keep their runtime version metadata without
  acquiring Linux static-linker flags or new CLI/MCP builds.

All three targets build `api`, `migrations`, `sitemgr`, `workflow`,
`site-agent`, `mock-core`, `mock-flow`, `credsmgr`, `flow`, `psm`, and `nsm`.
The two Linux targets also build `nicocli` and `nico-mcp`. The independent
command-contract test pins the exact package, target, CGO, linker-flag, and
output inventory; the source manifest remains the canonical machine-readable
list.

The supported transition boundary is
[CICD-08](https://github.com/NVIDIA/infra-controller/issues/4582). Until that
issue changes image packaging to consume these bundles, the production
Dockerfiles contain a second copy of the Linux service build settings. Keeping
the two copies aligned is a review responsibility during this transition;
CICD-08 removes the duplicate build recipe by making packaging consume these
bundles.

## Commands

`rest_command_bundle.py` has three subcommands. Each accepts optional
`--manifest PATH`, which defaults to the adjacent
`.github/ci/rest-command-manifest.json`, and `--repo-root PATH`, which defaults
to the repository's `rest-api/` directory. Relative path arguments resolve
from the current working directory; `--repo-root` does not rebase the other
paths. Absolute paths are accepted.

- `check` has no additional flags. It validates the source manifest and package
  directories, writes no files, and prints the output and target counts.
- `build` requires `--target`, `--output-dir`, `--version`,
  `--build-timestamp`, `--candidate-sha`, and `--short-sha`; `--allow-dirty` is
  optional. It validates the checkout, compiles and inspects every command for
  one target, creates the output directory when missing, publishes the three
  bundle files, and prints build progress plus the verified output count and
  archive size. An existing output directory must be empty. The accepted
  targets are `linux-amd64`, `linux-arm64`, and `darwin-arm64`.
- `verify` requires `--target`, `--bundle`, `--checksum`,
  `--resolved-manifest`, `--version`, `--build-timestamp`, `--candidate-sha`,
  and `--short-sha`; `--allow-dirty` is optional. It reads and verifies the
  three bundle files for a declared target, uses a temporary directory for
  binary inspection, removes that directory on exit, and prints the verified
  archive path. The bundle cannot choose its expected identity.

Each subcommand rejects flags owned by another subcommand. In particular,
`check` rejects `--allow-dirty`, `build` rejects the verify-only bundle path
flags, and `verify` rejects `--output-dir`. An undeclared target fails in
`build` or `verify`; there is no target flag for `check`.

The version starts with a letter or number and otherwise accepts only letters,
numbers, periods, underscores, and hyphens. The build timestamp uses UTC
`YYYY-MM-DDTHH:MM:SSZ`. The candidate SHA is exactly 40 lowercase hexadecimal
characters, while the short SHA is its 7- to 12-character prefix. Because that
timestamp is also written into the gzip header, it must fall between
`1970-01-01T00:00:00Z` and `2106-02-07T06:28:15Z`, inclusive.

Only `build` and `verify` accept `--allow-dirty`. By default, `build` rejects
tracked or untracked changes anywhere in the repository worktree, and `verify`
rejects a resolved manifest with `source_dirty: true`. The flag permits that
state for local validation. CI does not pass it.

`build` stages the three bundle files inside the output directory, verifies
them there, and publishes the resolved manifest and checksum before atomically
renaming the archive into place. A failed build, verification, or publication
normally removes staged and partially published files so the output directory
is empty for a retry. If cleanup itself fails, the error names each path it
could not remove.

All three commands require Python 3.10 or newer. `build` also requires Git,
[Go 1.26.4 or newer](../../rest-api/go.mod), and the `file` utility; `verify` requires Go
and `file`. Building `linux-amd64` also requires a C compiler for `mock-core`
and `mock-flow`. CI checks the Python minimum, selects the module's minimum Go
1.26.4, and checks `file --version` before compiling. A contract, command, tool,
or verification failure prints `error: <message>` to standard error and exits
with status 1. Argument parsing and missing required flags print `usage:` and
exit with status 2.

## Bundle files

For a target such as `linux-amd64`, `build` writes:

- `rest-command-bundle-linux-amd64.tar.gz`;
- `rest-command-bundle-linux-amd64.tar.gz.sha256`; and
- `rest-command-bundle-linux-amd64.manifest.json`.

The UTF-8 checksum sidecar contains exactly one LF-terminated line: one
lowercase SHA-256 digest, two spaces, the archive filename, and the final LF.
The producer writes a level-9 gzip stream with no stored filename or optional
header fields and with the build timestamp as its gzip timestamp. It contains a
PAX-format tar stream ordered as `manifest.json`, `SHA256SUMS`, and then binary
paths sorted by name. Those two metadata files use mode `0644`; every
`bin/<name>` entry uses mode `0755`. All members are regular relative files with
UID and GID `0`, empty owner and group names, and the build timestamp as their
modification time.

The binary checksum file uses the same digest and two-space format, with one
LF-terminated line for every command, `/`-separated relative paths, source
inventory sorted by path, and no extra entries. The resolved and embedded
manifests use UTF-8 JSON with sorted object keys, two-space indentation, and one
final newline; non-ASCII characters are escaped. No required field is omitted.

The resolved manifest records string values for the source-manifest SHA-256,
requested candidate and short SHA, version, and build timestamp; a Boolean
`source_dirty`; the exact target object; and a non-empty command array. Each
command entry records string values for its name, package, output, import path,
Go version, `file` description, SHA-256, and VCS settings; a Boolean CGO choice;
a positive integer size; and a possibly empty array whose linker-flag entries
are non-empty strings.

The flat VCS keys are always present and non-empty. `vcs_modified` records Go's
`vcs.modified` value or `unknown` when Go omits it. `vcs_revision` records the
candidate SHA only when Go reports that exact revision; otherwise it is
`unavailable`. Verification rejects every other revision value. Go normally
records the repository revision for this module; the fallback keeps the bundle
contract explicit in environments where those settings are unavailable.

The producer and its unit test own the PAX writer choice. The standalone
verifier accepts any readable tar encoding, but still enforces the canonical
JSON bytes, gzip header and timestamp, member order, ownership, timestamps,
modes, paths, inventory, and contents described above.

## Embedded metadata

The Linux and Darwin `api` and `site-agent` builds set `metadata.Version` to
`--version` and `metadata.BuildTime` to `YYYY-MM-DD HH:MM:SS`. Their `flow`
builds set `metadata.Version` to `--version`, `metadata.BuildTime` to
`YYYY-MM-DDTHH:MM:SSZ`, and `metadata.GitCommit` to `--short-sha`. Their full Go
symbol paths live in the source manifest and are pinned by the command-contract
test. Darwin builds omit only the Linux static-linker and stripping flags.

`go version -m -json` exposes linker flags as one aggregate setting. Missing,
extra, or reordered flags fail verification, unrelated build settings are
ignored, and the last value wins if Go reports one setting key more than once.
Every resolved `-X` value must also appear in the binary. The script does not
maintain a second allowlist of `-X` destinations; the source manifest and its
independent contract test own those destinations.

## What verification checks

The verifier starts from the checked-in source manifest and the identity the
caller supplied. From there it checks:

- the archive sidecar and exact archive inventory;
- the byte-for-byte match between the external and embedded resolved manifest;
- safe paths, regular-file types, exact modes, sizes, and checksums;
- package import paths, `GOOS`, `GOARCH`, `CGO_ENABLED`, and linker flags from
  `go version -m -json`;
- physical architecture and CGO-disabled Linux static linkage from `file -b`;
  and
- every metadata value supplied through a `-X` linker flag.

Go normally records `vcs.revision` in these binaries. Before compiling, the
builder confirms that Git `HEAD` is the requested candidate and rejects a dirty
checkout unless local validation explicitly passes `--allow-dirty`. The
resolved manifest records that source check and uses `unavailable` only when Go
omits the VCS setting. The standalone verifier checks the recorded candidate,
build contract, binary metadata, checksums, and file inventory, but it cannot
cryptographically prove which source tree produced the binaries.
