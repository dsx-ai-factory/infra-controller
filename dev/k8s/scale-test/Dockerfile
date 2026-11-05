# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Acceptance image: the current scale-test suite binaries and nothing else. It
# is built from the repository root so later suites can add sibling modules.
# Base images are digest-pinned for reproducible rebuilds (bump the tag AND the
# digest together: docker buildx imagetools inspect <ref> | head -1).
FROM golang:1.27.3@sha256:f20d83a83e18dbd2a4714b8ffd4b30b287b6243962dd75608e86abcc63ba4efc AS build
WORKDIR /src/dev/k8s/scale-test
# The inventory suite imports the Forge stubs through the module's
# ../../../rest-api replace, so that sibling module is kept in the build.
COPY rest-api/ /src/rest-api/
COPY dev/k8s/scale-test/go.mod dev/k8s/scale-test/go.sum ./
RUN go mod download
COPY dev/k8s/scale-test/ ./
# Precompiled, statically linked test binaries for the distroless runtime. The
# Jobs select one test each with an anchored -test.run, so the unit tests never
# run there. Each binary must list its exact fixture, or the build fails.
RUN CGO_ENABLED=0 go test -c -tags devsite     -o /out/probe.test     ./internal/probe/ \
 && CGO_ENABLED=0 go test -c -tags devcluster  -o /out/cluster.test   ./internal/cluster/ \
 && CGO_ENABLED=0 go test -c -tags acceptance  -o /out/inventory.test ./harness/ \
 && CGO_ENABLED=0 go test -c -tags acceptance  -o /out/history.test   ./internal/history/ \
 && CGO_ENABLED=0 go test -c -tags envtest     -o /out/lock.test      ./internal/lock/ \
 && CGO_ENABLED=0 go test -c -tags devsite     -o /out/store.test     ./internal/store/ \
 && /out/cluster.test   -test.list '^TestAgainstARealCluster$' | grep -qx TestAgainstARealCluster \
 && /out/probe.test     -test.list '^TestAgainstARealProbe$'   | grep -qx TestAgainstARealProbe \
 && /out/inventory.test -test.list '^TestAcceptance$'          | grep -qx TestAcceptance \
 && /out/history.test   -test.list '^TestAcceptanceReplaysARealProbeCapture$' | grep -qx TestAcceptanceReplaysARealProbeCapture \
 && /out/lock.test      -test.list '^TestEnvLock$'             | grep -qx TestEnvLock \
 && /out/lock.test      -test.list '^TestEnvCleanupIsBounded$' | grep -qx TestEnvCleanupIsBounded \
 && /out/store.test     -test.list '^TestAgainstARealStore$'    | grep -qx TestAgainstARealStore \
 && /out/store.test     -test.list '^TestControlledTransition$' | grep -qx TestControlledTransition

FROM gcr.io/distroless/static:nonroot@sha256:f7f8f729987ad0fdf6b05eeeae94b26e6a0f613bdf46feea7fc40f7bd72953e6
COPY --from=build /out/probe.test     /tests/probe.test
COPY --from=build /out/cluster.test   /tests/cluster.test
COPY --from=build /out/inventory.test /tests/inventory.test
COPY --from=build /out/history.test   /tests/history.test
COPY --from=build /out/lock.test      /tests/lock.test
COPY --from=build /out/store.test     /tests/store.test
USER 65532:65532
