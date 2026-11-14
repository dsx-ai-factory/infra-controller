# Native development tools

From the repository root:

```bash
python3 dev/deployment/install-tools.py
export PATH="$HOME/.local/bin:$PATH"
```

It requires Python 3.9 or newer, curl and Internet HTTPS access. The defaults
are pinned in [`install-tools.py`](install-tools.py); these pins identify the
versions tested together by this workflow. It verifies publisher SHA256 manifests
before installing binaries. `--bin-dir PATH` selects another directory (default
`~/.local/bin`); relative paths resolve from the current directory. Optional
positional tool names select a distinct subset of `kind`, `kubectl`, `helm`,
`tilt` and `devspace`. Omission selects `kind`, `kubectl`, `helm` and `tilt`. Unknown or repeated
names fail before filesystem changes. `--help` prints the complete syntax.

Selected existing regular binaries are replaced; symlinks and other nonregular destinations are
rejected. All downloads and checks complete before any replacement. Each binary
is replaced atomically, but interruption during publication may leave a mix of
versions; rerun the same selection to finish. Failed downloads/checks preserve
existing binaries and remove staging files. The installer needs write access to
the destination and does not invoke sudo or change shell profiles, Docker,
Kubernetes contexts or workloads. Install Docker and workflow-specific host packages separately. Do not run concurrent installers against the same directory.
