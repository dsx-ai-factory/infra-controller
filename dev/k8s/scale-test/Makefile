# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

.PHONY: build test vet lint fmt-check devsite-test

build:
	go build -o bin/scale-test ./cmd/scale-test

# The race detector needs cgo and a C compiler.
test:
	go test -race -count=1 ./...

# Runs the probe client against a real probe at SCALE_TEST_PROBE_URL.
devsite-test:
	go test -tags devsite -count=1 -run TestAgainstARealProbe ./internal/probe/

vet:
	go vet ./...

lint:
	golangci-lint run ./...

fmt-check:
	@test -z "$$(gofmt -l . | tee /dev/stderr)"
