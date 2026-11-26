# NICo Fern Documentation

This directory contains the documentation for the NICo Fern API.

## Getting Started

To develop Fern docs make sure you have the following tools installed:

- Node.js v22 or later
- [Fern CLI](https://buildwithfern.com/learn/cli-api-reference/cli-reference/overview#install-fern-cli)
- [pnpm](https://pnpm.io/installation)

## Local Development

To start a local development server, run the following command:

```bash
fern docs dev
```

This will start a local development server at `http://localhost:3000`.

If you have trouble getting `esbuild` approved to install dependencies, you can try the following:

```bash
PNPM_CONFIG_DANGEROUSLY_ALLOW_ALL_BUILDS=true fern docs dev
```

If the command fails with `No such built-in module: node:sqlite`, set this env var before running `fern docs dev`:

```bash
export NODE_OPTIONS="--experimental-sqlite"
```

### Preview the admin CLI reference

From the repository root, generate a temporary copy of the site from the current
CLI source. This requires the admin CLI build prerequisites, Pandoc 3.14, and
uv. To reuse a compiled CLI, pass `--cli /path/to/nico-admin-cli` to the script.

```bash
site=$(uv run fern/cli-reference/preview.py) && (cd "$site" && fern docs dev)
```

Under **Documentation → Reference → CLI reference**, expand a domain and a
command family to browse its subcommands. Run the command again to preview
changes to CLI help or navigation.

The four folder entries in `fern/cli-reference/navigation.yml` set the sidebar
order to Hardware, Network, Tenant, and Admin. Commands appear alphabetically
in each folder and in the command tables on reference pages.

### CLI reference in CI

CI builds `nico-admin-cli` from the revision under test and runs its tests,
including the Pandoc conversion test. It generates the Fern site in a temporary
directory, checks the generated command pages against the binary's `--help`
tree, and runs `fern docs md check` and `fern check` there. The hand-authored
CLI overview is part of the site.

Pull request previews and publication use the validated site artifact.
Changes to CLI sources, shared Rust inputs, or documentation trigger generation.
For copy-pr-bot branches and merge groups, the reference check contributes to
`core-ci-pass` even when the other Core build jobs skip a documentation change.
The publish workflow builds a fresh artifact for the revision on `main`.
