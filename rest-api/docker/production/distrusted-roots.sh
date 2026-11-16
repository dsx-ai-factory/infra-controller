#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -eu

bundle=$1

distrusted="86a1ecba089c4a8d3bbe2734c612ba341d813e043cf9e8a862cd5c57a36bbe6b
125609aa301da0a249b97a8239cb6a34216f44dcac9f3954b14292f2e8c8608f
bc4d809b15189d78db3e1d8cf4f9726a795da1643ca5f1358e1ddb0edc0d7eb3"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

awk -v dir="$work" '
  /-----BEGIN CERTIFICATE-----/ { n++; f = sprintf("%s/%06d.pem", dir, n) }
  f != "" { print > f }
  /-----END CERTIFICATE-----/ { close(f); f = "" }
' "$bundle"

kept=0
removed=0
: > "$work/bundle"
for cert in "$work"/*.pem; do
  [ -f "$cert" ] || continue
  sed '1d;$d' "$cert" | base64 -d > "$cert.der"
  [ -s "$cert.der" ] || { echo "cannot decode a certificate in $bundle" >&2; exit 1; }
  fp=$(sha256sum "$cert.der" | cut -d ' ' -f 1)
  if printf '%s\n' "$distrusted" | grep -qx "$fp"; then
    removed=$((removed + 1))
  else
    cat "$cert" >> "$work/bundle"
    kept=$((kept + 1))
  fi
done

[ "$kept" -gt 0 ] || { echo "no certificates left in $bundle" >&2; exit 1; }
cat "$work/bundle" > "$bundle"
echo "$bundle: removed $removed distrusted roots, kept $kept"
