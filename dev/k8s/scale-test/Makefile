# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

.PHONY: build test vet lint fmt-check devsite-test devcluster-test envtest-test history-acceptance

build:
	go build -o bin/scale-test ./cmd/scale-test

# The race detector needs cgo and a C compiler.
test:
	go test -race -count=1 ./...

# Runs the probe client against a real probe at SCALE_TEST_PROBE_URL, and the
# history store client against the store at SCALE_TEST_STORE_URL that scrapes
# it. Both fail, rather than skip, when their URL is unset.
devsite-test:
	go test -tags devsite -count=1 -run TestAgainstARealProbe ./internal/probe/
	go test -tags devsite -count=1 -run TestAgainstARealStore ./internal/store/

# Replays a captured real-probe producer run through the fleet-time math and
# checks every crossing, final count, process binding and the timing bound.
# Supply the capture via SCALE_TEST_HISTORY_CAPTURE (and the probe provenance
# vars) per internal/history/ACCEPTANCE.md; it fails when unset. The ordinary
# unit tests already replay the committed synthetic fixture.
history-acceptance:
	go test -tags acceptance -count=1 -run TestAcceptanceReplaysARealProbeCapture ./internal/history/

vet:
	go vet ./...

lint:
	golangci-lint run ./...

fmt-check:
	@test -z "$$(gofmt -l . | tee /dev/stderr)"

# Runs the Kubernetes adapter against a real API server in SCALE_TEST_NAMESPACE.
# It uses the pod ServiceAccount, so it only works inside a pod and fails
# elsewhere. Run it with helm test from helm/scale-test (see its README).
devcluster-test:
	go test -tags devcluster -count=1 -run TestAgainstARealCluster ./internal/cluster/

# Runs the site lock against the pod's in-cluster API server in
# SCALE_TEST_NAMESPACE. Meant to run from the runner Job (helm test).
envtest-test:
	go test -tags envtest -count=1 ./internal/lock/
