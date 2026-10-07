# otelcol-contrib: updating the pinned module graph

The files `ocb-generated/go.mod` and `ocb-generated/go.sum` are
generated once by running OCB locally and committed to the repo. Both
the Docker build (`bluefield/containers/otelcol-contrib/Dockerfile`)
and the cargo-make `build-otelcol` task copy these files in before
compiling, making the dependency graph reproducible across builds.

They must be regenerated whenever:

- `otelcol_builder_config_yaml.txt` changes (new component, version bump)
- Any custom processor/receiver version (`version.go`) changes
- `otelcol_version.txt` is bumped

## Prerequisites

Install the OpenTelemetry Collector Builder (`ocb`) at the version in
`otelcol_version.txt` (currently `0.155.0`):

```sh
VERSION=$(cat bluefield/otel/otelcol_version.txt)
BASE_URL="https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/cmd%2Fbuilder%2Fv${VERSION}"
curl -sSL "${BASE_URL}/ocb_${VERSION}_linux_amd64" -o "ocb_${VERSION}_linux_amd64"
curl -sSL "${BASE_URL}/checksums.txt" | grep "ocb_${VERSION}_linux_amd64" | sha256sum -c
chmod +x "ocb_${VERSION}_linux_amd64" && sudo mv "ocb_${VERSION}_linux_amd64" /usr/local/bin/ocb
```

`update-ocb-modules.sh` will exit with an error if the installed `ocb`
version does not match `otelcol_version.txt`.

## Running the update

From the repo root:

```sh
bash bluefield/otel/update-ocb-modules.sh
```

The script substitutes version placeholders into the config template,
runs `ocb --skip-compilation` (which executes `go mod tidy` with network
access), and copies the resulting `go.mod` and `go.sum` into
`bluefield/otel/ocb-generated/`.

Verify the output before committing:

```sh
head -3 bluefield/otel/ocb-generated/go.mod
# must show: module otelcol-contrib
```

Then commit `ocb-generated/go.mod` and `ocb-generated/go.sum`. The
Docker build and cargo-make `build-otelcol` task will use the updated
module graph automatically on the next run.

