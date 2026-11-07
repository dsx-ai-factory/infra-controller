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
