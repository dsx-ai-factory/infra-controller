# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

.PHONY: all build test test-coverage lint fmt docker-build docker-build-latest clean run verify

TAG ?= dev

all: build

build:
	@mkdir -p bin
	go build -o bin/mat-k8s-controller ./cmd/mat-k8s-controller

test:
	go test -v ./...

test-coverage:
	go test -coverprofile=coverage.out ./...
	go tool cover -html=coverage.out -o coverage.html

lint:
	golangci-lint run ./...

fmt:
	go fmt ./...

docker-build:
	docker build -f Dockerfile -t mat-k8s-controller:$(TAG) ../../..

docker-build-latest:
	docker build -f Dockerfile -t mat-k8s-controller:latest ../../..

clean:
	rm -rf bin/ coverage.out coverage.html

run:
	go run ./cmd/mat-k8s-controller --kubeconfig=$(KUBECONFIG) --log-level=debug

verify: lint test
