#!/usr/bin/env bash
# Regenerate bluefield/otel/ocb-generated/go.mod and go.sum.
#
# Run this whenever otelcol_builder_config_yaml.txt changes (version bump,
# new component, custom module update). After it completes, commit the updated
# files in ocb-generated
#
# Requires: ocb (opentelemetry-collector-builder) on PATH.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v ocb &>/dev/null; then
    echo "error: ocb not found. Install opentelemetry-collector-builder" >&2
    exit 1
fi

VERSION=$(cat "$SCRIPT_DIR/otelcol_version.txt")

OCB_VERSION=$(ocb version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
if [ "$OCB_VERSION" != "$VERSION" ]; then
    echo "error: installed ocb version '$OCB_VERSION' does not match otelcol_version.txt ('$VERSION')" >&2
    echo "       See bluefield/otel/README.md for install instructions." >&2
    exit 1
fi
FILERESOURCE_VERSION=$(bash "$SCRIPT_DIR/get_module_version.sh" "$SCRIPT_DIR/fileresourceprocessor")
TELEMETRYSTATS_VERSION=$(bash "$SCRIPT_DIR/get_module_version.sh" "$SCRIPT_DIR/telemetrystatsprocessor")
PUNTSTATS_VERSION=$(bash "$SCRIPT_DIR/get_module_version.sh" "$SCRIPT_DIR/puntstatsreceiver")

echo "otelcol:              $VERSION"
echo "fileresourceprocessor: $FILERESOURCE_VERSION"
echo "telemetrystatsprocessor: $TELEMETRYSTATS_VERSION"
echo "puntstatsreceiver:    $PUNTSTATS_VERSION"

WORKDIR=$(mktemp -d)
trap 'rm -rf "$WORKDIR"' EXIT

# Custom modules must be siblings of ocb-build/ to match the replace
# directives in the OCB config (../fileresourceprocessor etc.).
cp -r "$SCRIPT_DIR/fileresourceprocessor"   "$WORKDIR/"
cp -r "$SCRIPT_DIR/telemetrystatsprocessor" "$WORKDIR/"
cp -r "$SCRIPT_DIR/puntstatsreceiver"       "$WORKDIR/"

sed \
    -e "s/\${VERSION}/$VERSION/g" \
    -e "s/\${FILERESOURCE_VERSION}/$FILERESOURCE_VERSION/g" \
    -e "s/\${TELEMETRYSTATS_VERSION}/$TELEMETRYSTATS_VERSION/g" \
    -e "s/\${PUNTSTATS_VERSION}/$PUNTSTATS_VERSION/g" \
    "$SCRIPT_DIR/otelcol_builder_config_yaml.txt" > "$WORKDIR/ocb_config.yaml"

cd "$WORKDIR"
ocb --config ocb_config.yaml --skip-compilation

# Raise the minimum version of modules with known advisories. OCB's go mod
# tidy may resolve older versions transitively; this bumps only these minimums
# without changing any collector component version.
cd ocb-build
go get \
    github.com/klauspost/compress@v1.18.7 \
    go.opentelemetry.io/contrib/bridges/otelzap@v0.20.0 \
    go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploggrpc@v0.21.0 \
    go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploghttp@v0.21.0 \
    go.opentelemetry.io/otel/exporters/otlp/otlptrace@v1.45.0 \
    go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc@v1.45.0 \
    go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracehttp@v1.45.0 \
    go.opentelemetry.io/otel/exporters/stdout/stdoutlog@v0.21.0 \
    go.opentelemetry.io/otel/sdk@v1.45.0 \
    go.opentelemetry.io/otel/sdk/log@v0.21.0 \
    golang.org/x/crypto@v0.57.0 \
    golang.org/x/mod@v0.41.0 \
    golang.org/x/net@v0.60.0 \
    golang.org/x/text@v0.42.0 \
    google.golang.org/grpc@v1.83.2
cd ..

if ! grep -q "^module otelcol-contrib" ocb-build/go.mod; then
    echo "error: ocb-build/go.mod has unexpected module name:" >&2
    head -3 ocb-build/go.mod >&2
    echo "Expected: module otelcol-contrib" >&2
    exit 1
fi

mkdir -p "$SCRIPT_DIR/ocb-generated"
cp ocb-build/go.mod ocb-build/go.sum "$SCRIPT_DIR/ocb-generated/"

echo ""
echo "Updated:"
echo "  bluefield/otel/ocb-generated/go.mod"
echo "  bluefield/otel/ocb-generated/go.sum"
echo ""
echo "Next steps:"
echo "  1. git add bluefield/otel/ocb-generated/"
echo "  2. git commit -m Updating otelcol-contrib to ${VERSION}"
