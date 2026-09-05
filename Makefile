#
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
# Top-level Makefile for selected Core workflows and the rest-api/ Go services.
#
# Thin discoverable entrypoint that delegates to cargo-make and
# rest-api/Makefile. Both underlying entrypoints continue to work directly;
# this file is an additive convenience layer.
#
# Run `make help` (default goal) for the inventory of targets.

SHELL := /bin/bash

.DEFAULT_GOAL := help

# =============================================================================
# Help (default goal)
# =============================================================================

.PHONY: help
help: ## Show this help and exit (default goal)
	@echo "Getting started (fresh build host):"
	@grep -E '^bootstrap:.*## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "} {printf "  %-26s %s\n", $$1, $$2}'
	@echo ""
	@echo "Container images (build from a clean clone):"
	@grep -E '^images[a-zA-Z0-9_-]*:.*## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "} {printf "  %-26s %s\n", $$1, $$2}'
	@echo ""
	@echo "Core (Rust):"
	@grep -E '^core/[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "} {printf "  %-26s %s\n", $$1, $$2}'
	@echo ""
	@echo "Rest (Go services in rest-api/):"
	@grep -E '^rest-[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "} {printf "  %-26s %s\n", $$1, $$2}'
	@echo "  rest-api/<target>          Pass any target through to rest-api/Makefile"
	@echo ""
	@echo "  cat rest-api/Makefile      See all rest-api/ targets directly"

# =============================================================================
# Core (Rust; delegate to cargo-make)
# =============================================================================

.PHONY: core/check-isolated-package-builds
core/check-isolated-package-builds: ## Check each Rust package independently with default features
	make check-isolated-package-builds -f dev/Makefile.core

.PHONY: core/tests
core/tests: ## Run Core tests with isolated PostgreSQL (TEST_ARGS="...")
	make tests -f dev/Makefile.core

# =============================================================================
# Getting started (build host setup)
# =============================================================================

.PHONY: bootstrap

bootstrap: ## Set up an Ubuntu/Debian build host: apt deps, rustup, submodules, docker, multi-arch emulation, cargo tooling (run once)
	./scripts/setup-build-host.sh

# =============================================================================
# Container images (single onboarding build)
# =============================================================================
# Build NICo container images from a clean clone. Run from the repo root on an
# Ubuntu build host (see docs/manuals/building_nico_containers.md for the host
# prerequisites: docker, mkosi, rust, cargo-make, ...). Every base image is
# public (rust / debian / golang + nvcr.io/nvidia/distroless), so no internal
# registry access is required.
#
#   make images        Build the deployable service stack: NICo Core + REST images
#   make images-arm    Build the deployable service stack for ARM64 only
#   make images-all-arm Build every ARM64 image and boot artifact natively on ARM64
#   make images-all    Build everything: the stack plus machine-validation and
#                       boot-artifact images (needs the full mkosi build host)
#   make images-core   NICo Core image (nico) only
#   make images-rest   REST service images only
#
# Images are pushed as manifests at $(IMAGE_REGISTRY)/<name>:$(IMAGE_TAG)
# containing whichever architectures NICO_ARCHES, BOOT_ARTIFACTS_ARCHES, and
# DPU_ARCHES select for that image's group (each "amd64 arm64" by default, so
# manifests are multi-arch unless you narrow them). Override IMAGE_REGISTRY and
# IMAGE_TAG to publish under your own registry/tag (defaults match rest-api/).
# Override NICO_ARCHES / BOOT_ARTIFACTS_ARCHES / DPU_ARCHES to restrict which
# architectures a given image group builds, e.g.:
#   make images-all NICO_ARCHES=amd64 DPU_ARCHES=arm64

IMAGE_REGISTRY ?= localhost:5000
IMAGE_TAG ?= latest
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo dev)
CI_COMMIT_SHORT_SHA ?= $(shell git rev-parse --short HEAD 2>/dev/null || echo unknown)
LOCAL_REGISTRY_CONTAINER ?= nico-build-registry
BOOT_ARTIFACTS_RUNTIME_IMAGE ?= docker.io/library/alpine:3.20.10@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc

# Architectures to build, per image group. Each defaults to both so the
# published tags stay multi-arch unless you deliberately narrow them, e.g.:
#   make images-all NICO_ARCHES=amd64 DPU_ARCHES=arm64
NICO_ARCHES ?= amd64 arm64
BOOT_ARTIFACTS_ARCHES ?= amd64 arm64
DPU_ARCHES ?= amd64 arm64

check-arches = $(if $(strip $(1)),,$(error $(2) must not be empty))$(if $(filter-out amd64 arm64,$(1)),$(error $(2) must be a subset of "amd64 arm64", got: $(1)))

require-nico-amd64 = $(if $(filter amd64,$(NICO_ARCHES)),,$(error NICO_ARCHES must include amd64: the machine-validation-runner intermediate image is always built for amd64 and depends on the amd64 Core runtime base container that images-base only pushes when amd64 is requested; got: $(NICO_ARCHES)))

# Intermediate base containers the Core and machine-validation images build FROM.
CORE_BUILD_CONTAINER_AMD64 ?= $(IMAGE_REGISTRY)/nico-buildcontainer:$(IMAGE_TAG)-amd64
CORE_BUILD_CONTAINER_ARM64 ?= $(IMAGE_REGISTRY)/nico-buildcontainer:$(IMAGE_TAG)-arm64
CORE_RUNTIME_CONTAINER_AMD64 ?= $(IMAGE_REGISTRY)/nico-runtime-container:$(IMAGE_TAG)-amd64
CORE_RUNTIME_CONTAINER_ARM64 ?= $(IMAGE_REGISTRY)/nico-runtime-container:$(IMAGE_TAG)-arm64

.PHONY: images images-arm images-all images-all-arm images-validate images-all-validate images-registry \
        images-base images-core images-rest images-machine-validation images-machine-validation-arm images-machine-a-tron-arm \
        images-boot-artifacts images-bfb images-bfb-arm

images-validate:
	$(call check-arches,$(NICO_ARCHES),NICO_ARCHES)

images-all-validate: images-validate
	$(call check-arches,$(BOOT_ARTIFACTS_ARCHES),BOOT_ARTIFACTS_ARCHES)
	$(call check-arches,$(DPU_ARCHES),DPU_ARCHES)
	$(require-nico-amd64)

images: ## Build the deployable service stack (NICo Core + REST images)
	# Ensure validation runs before building even with parallel builds.
	$(MAKE) images-validate
	$(MAKE) images-core images-rest
	@echo ""
	@echo "Deployable multi-arch images pushed under $(IMAGE_REGISTRY) (tag: $(IMAGE_TAG)):"
	@echo "  $(IMAGE_REGISTRY)/nico:$(IMAGE_TAG)   (NICo Core)"
	@echo "  $(IMAGE_REGISTRY)/nico-rest-*:$(IMAGE_TAG)       (REST services)"

images-arm: ## Build the deployable ARM64 service stack natively on an ARM64 Docker host
	@arch="$$(docker info --format '{{.Architecture}}')"; \
		case "$$arch" in arm64|aarch64) ;; *) echo "images-arm requires an ARM64 Docker host; got $$arch" >&2; exit 1 ;; esac
	$(MAKE) images NICO_ARCHES=arm64

images-all: ## Build every image (stack + machine validation + boot artifacts; needs an mkosi build host)
	# Ensure validation runs before building even with parallel builds.
	$(MAKE) images-all-validate
	$(MAKE) images images-machine-validation images-boot-artifacts images-bfb

images-all-arm: ## Build every ARM64 image and boot artifact natively on an ARM64 host
	@arch="$$(docker info --format '{{.Architecture}}')"; \
		case "$$arch" in \
			arm64|aarch64) ;; \
			*) echo "images-all-arm requires an ARM64 Docker host; got $$arch. Run 'make images-all' for the multi-architecture compatibility build." >&2; exit 1 ;; \
		esac
	$(MAKE) images images-machine-validation-arm images-machine-a-tron-arm images-bfb-arm \
		NICO_ARCHES=arm64 DPU_ARCHES=arm64

# Safe to call concurrently from multiple sibling targets (images-base,
# images-rest, images-boot-artifacts, images-bfb each invoke this themselves).
images-registry:
	@if [ "$(IMAGE_REGISTRY)" = "localhost:5000" ] && ! curl -fsS http://localhost:5000/v2/ >/dev/null 2>&1; then \
		docker start $(LOCAL_REGISTRY_CONTAINER) >/dev/null 2>&1 || \
			docker run -d --rm --name $(LOCAL_REGISTRY_CONTAINER) -p 5000:5000 registry:2 >/dev/null 2>&1 || true; \
		ready=0; \
		for i in 1 2 3 4 5 6 7 8 9 10; do \
			if curl -fsS http://localhost:5000/v2/ >/dev/null 2>&1; then ready=1; break; fi; \
			sleep 0.5; \
		done; \
		if [ "$$ready" != "1" ]; then \
			echo "images-registry: local registry at localhost:5000 did not become ready after starting $(LOCAL_REGISTRY_CONTAINER)" >&2; \
			exit 1; \
		fi; \
	fi

images-base: ## Build and push the Core base containers (NICO_ARCHES="amd64 arm64")
	$(call check-arches,$(NICO_ARCHES),NICO_ARCHES)
	$(MAKE) images-registry
	@set -e; \
	for arch in $(NICO_ARCHES); do \
		case $$arch in \
			amd64) build_file=dev/docker/Dockerfile.build-container-x86_64; build_tag=$(CORE_BUILD_CONTAINER_AMD64); \
			       runtime_file=dev/docker/Dockerfile.runtime-container-x86_64; runtime_tag=$(CORE_RUNTIME_CONTAINER_AMD64) ;; \
			arm64) build_file=dev/docker/Dockerfile.build-container-aarch64; build_tag=$(CORE_BUILD_CONTAINER_ARM64); \
			       runtime_file=dev/docker/Dockerfile.runtime-container-aarch64; runtime_tag=$(CORE_RUNTIME_CONTAINER_ARM64) ;; \
		esac; \
		docker buildx build --platform linux/$$arch --push --file $$build_file -t $$build_tag . ; \
		docker buildx build --platform linux/$$arch --push --file $$runtime_file -t $$runtime_tag . ; \
	done

images-core: ## Build the NICo Core image (nico) (NICO_ARCHES="amd64 arm64")
	$(call check-arches,$(NICO_ARCHES),NICO_ARCHES)
	$(MAKE) images-base
	@set -e; \
	tags=""; \
	for arch in $(NICO_ARCHES); do \
		case $$arch in \
			amd64) file=dev/docker/Dockerfile.release-container-sa-x86_64; \
			       buildarg="--build-arg CONTAINER_BUILD_X86_64=$(CORE_BUILD_CONTAINER_AMD64) --build-arg CONTAINER_RUNTIME_X86_64=$(CORE_RUNTIME_CONTAINER_AMD64)" ;; \
			arm64) file=dev/docker/Dockerfile.release-container-aarch64; \
			       buildarg="--build-arg CONTAINER_BUILD_AARCH64=$(CORE_BUILD_CONTAINER_ARM64) --build-arg CONTAINER_RUNTIME_AARCH64=$(CORE_RUNTIME_CONTAINER_ARM64)" ;; \
		esac; \
		tag=$(IMAGE_REGISTRY)/nico:$(IMAGE_TAG)-$$arch; \
		docker buildx build --platform linux/$$arch --push $$buildarg \
			--build-arg VERSION=$(VERSION) \
			--build-arg CI_COMMIT_SHORT_SHA=$(CI_COMMIT_SHORT_SHA) \
			--file $$file -t $$tag . ; \
		tags="$$tags $$tag"; \
	done; \
	docker buildx imagetools create -t $(IMAGE_REGISTRY)/nico:$(IMAGE_TAG) $$tags

images-rest: ## Build the REST service images (api, workflow, site-manager, site-agent, db, cert-manager, flow, psm, nsm) (NICO_ARCHES="amd64 arm64")
	$(call check-arches,$(NICO_ARCHES),NICO_ARCHES)
	$(MAKE) images-registry
	$(MAKE) -C rest-api docker-build IMAGE_REGISTRY=$(IMAGE_REGISTRY) IMAGE_TAG=$(IMAGE_TAG) DOCKER_ARCHES="$(NICO_ARCHES)"

images-machine-validation: ## Build the machine-validation runner + config images (NICO_ARCHES="amd64 arm64"; must include amd64)
	$(call check-arches,$(NICO_ARCHES),NICO_ARCHES)
	$(require-nico-amd64)
	$(MAKE) images-base
	docker buildx build --platform linux/amd64 --load --build-arg CONTAINER_RUNTIME_X86_64=$(CORE_RUNTIME_CONTAINER_AMD64) \
		-t machine-validation-runner:$(IMAGE_TAG) \
		--file dev/docker/Dockerfile.machine-validation-runner .
	mkdir -p crates/machine-validation/images
	docker save --output crates/machine-validation/images/machine-validation-runner.tar machine-validation-runner:$(IMAGE_TAG)
	@set -e; \
	tags=""; \
	for arch in $(NICO_ARCHES); do \
		case $$arch in \
			amd64) file=dev/docker/Dockerfile.machine-validation-config; \
			       buildarg="--build-arg CONTAINER_RUNTIME_X86_64=$(CORE_RUNTIME_CONTAINER_AMD64)" ;; \
			arm64) file=dev/docker/Dockerfile.machine-validation-config-aarch64; \
			       buildarg="--build-arg CONTAINER_RUNTIME_AARCH64=$(CORE_RUNTIME_CONTAINER_ARM64)" ;; \
		esac; \
		tag=$(IMAGE_REGISTRY)/machine-validation:$(IMAGE_TAG)-$$arch; \
		docker buildx build --platform linux/$$arch --push $$buildarg -t $$tag --file $$file . ; \
		tags="$$tags $$tag"; \
	done; \
	docker buildx imagetools create -t $(IMAGE_REGISTRY)/machine-validation:$(IMAGE_TAG) $$tags

images-machine-validation-arm: ## Build the native ARM64 machine-validation runner + config image
	@arch="$$(docker info --format '{{.Architecture}}')"; \
		case "$$arch" in arm64|aarch64) ;; *) echo "images-machine-validation-arm requires an ARM64 Docker host; got $$arch" >&2; exit 1 ;; esac
	$(MAKE) images-base NICO_ARCHES=arm64
	docker buildx build --platform linux/arm64 --load --build-arg CONTAINER_RUNTIME_AARCH64=$(CORE_RUNTIME_CONTAINER_ARM64) \
		-t machine-validation-runner:$(IMAGE_TAG) \
		--file dev/docker/Dockerfile.machine-validation-runner-aarch64 .
	mkdir -p crates/machine-validation/images
	docker save --output crates/machine-validation/images/machine-validation-runner.tar machine-validation-runner:$(IMAGE_TAG)
	docker buildx build --platform linux/arm64 --push \
		--build-arg CONTAINER_RUNTIME_AARCH64=$(CORE_RUNTIME_CONTAINER_ARM64) \
		-t $(IMAGE_REGISTRY)/machine-validation:$(IMAGE_TAG)-arm64 \
		--file dev/docker/Dockerfile.machine-validation-config-aarch64 .
	docker buildx imagetools create -t $(IMAGE_REGISTRY)/machine-validation:$(IMAGE_TAG) \
		$(IMAGE_REGISTRY)/machine-validation:$(IMAGE_TAG)-arm64

images-machine-a-tron-arm: ## Build the native ARM64 machine-a-tron image
	@arch="$$(docker info --format '{{.Architecture}}')"; \
		case "$$arch" in arm64|aarch64) ;; *) echo "images-machine-a-tron-arm requires an ARM64 Docker host; got $$arch" >&2; exit 1 ;; esac
	$(MAKE) images-registry
	docker buildx build --platform linux/arm64 --push \
		-t $(IMAGE_REGISTRY)/machine-a-tron:$(IMAGE_TAG) \
		--file crates/machine-a-tron/Dockerfile.arm64 .

images-boot-artifacts: ## Build the x86 boot-artifact image (BOOT_ARTIFACTS_ARCHES="amd64 arm64"; requires mkosi + rust toolchain on the host)
	$(call check-arches,$(BOOT_ARTIFACTS_ARCHES),BOOT_ARTIFACTS_ARCHES)
	$(MAKE) images-registry
	cargo make --cwd pxe --env SA_ENABLEMENT=1 build-boot-artifacts-x86-host-sa
	@set -e; \
	tags=""; \
	for arch in $(BOOT_ARTIFACTS_ARCHES); do \
		tag=$(IMAGE_REGISTRY)/boot-artifacts-x86_64:$(IMAGE_TAG)-$$arch; \
		docker buildx build --platform linux/$$arch --push --build-arg CONTAINER_RUNTIME_X86_64=$(BOOT_ARTIFACTS_RUNTIME_IMAGE) \
			-t $$tag --file dev/docker/Dockerfile.release-artifacts-x86_64 . ; \
		tags="$$tags $$tag"; \
	done; \
	docker buildx imagetools create -t $(IMAGE_REGISTRY)/boot-artifacts-x86_64:$(IMAGE_TAG) $$tags

images-bfb: ## Build the aarch64 DPU BFB boot-artifact image (DPU_ARCHES="amd64 arm64"; cross-arch; requires mkosi + aarch64 toolchain)
	$(call check-arches,$(DPU_ARCHES),DPU_ARCHES)
	$(MAKE) images-registry
	cargo make --cwd pxe --env SA_ENABLEMENT=1 build-boot-artifacts-bfb-sa
	@set -e; \
	tags=""; \
	for arch in $(DPU_ARCHES); do \
		tag=$(IMAGE_REGISTRY)/boot-artifacts-aarch64:$(IMAGE_TAG)-$$arch; \
		docker buildx build --platform linux/$$arch --push --build-arg CONTAINER_RUNTIME_AARCH64=$(BOOT_ARTIFACTS_RUNTIME_IMAGE) \
			-t $$tag --file dev/docker/Dockerfile.release-artifacts-aarch64 . ; \
		tags="$$tags $$tag"; \
	done; \
	docker buildx imagetools create -t $(IMAGE_REGISTRY)/boot-artifacts-aarch64:$(IMAGE_TAG) $$tags

images-bfb-arm: ## Build the aarch64 DPU BFB boot-artifact image in a native ARM64 build container
	@arch="$$(docker info --format '{{.Architecture}}')"; \
		case "$$arch" in arm64|aarch64) ;; *) echo "images-bfb-arm requires an ARM64 Docker host; got $$arch" >&2; exit 1 ;; esac
	$(MAKE) images-registry
	docker buildx build --builder default --load -t carbide-pxe-builder -f dev/docker/Dockerfile.pxe-build-container dev/docker
	docker buildx build --builder default --load -t carbide-pxe-builder-aarch64 -f dev/docker/Dockerfile.pxe-build-container-aarch64 dev/docker
	@set -e; \
		cargo_home="$${CARGO_HOME:-$$HOME/.cargo}"; \
		sccache_home="$${SCCACHE_DIR:-$$HOME/.sccache}"; \
		mkdir -p "$$cargo_home" "$$sccache_home"; \
		docker run --rm --privileged \
			-v /var/run/docker.sock:/var/run/docker.sock \
			-v "$(CURDIR)":"$(CURDIR)" \
			-v /dev/null:"$(CURDIR)/pxe/mkosi.profiles/scout-oss-aarch64/mkosi.extra/etc/apt/sources.list.d/forge.list":ro \
			-v "$$cargo_home":"$$cargo_home" \
			-v "$$sccache_home":"$$sccache_home" \
			-w "$(CURDIR)" \
			-e CARGO_HOME="$$cargo_home" \
			-e SCCACHE_DIR="$$sccache_home" \
			-e VERSION="$(VERSION)" \
			carbide-pxe-builder-aarch64 \
			sh -c 'git config --global --add safe.directory "$(CURDIR)" && git config --global --add safe.directory "$(CURDIR)/pxe/ipxe/upstream" && cargo make --cwd pxe --env SA_ENABLEMENT=1 build-boot-artifacts-bfb-sa'
	docker buildx build --platform linux/arm64 --push --build-arg CONTAINER_RUNTIME_AARCH64=$(BOOT_ARTIFACTS_RUNTIME_IMAGE) \
		-t $(IMAGE_REGISTRY)/boot-artifacts-aarch64:$(IMAGE_TAG)-arm64 \
		--file dev/docker/Dockerfile.release-artifacts-aarch64 .
	docker buildx imagetools create -t $(IMAGE_REGISTRY)/boot-artifacts-aarch64:$(IMAGE_TAG) \
		$(IMAGE_REGISTRY)/boot-artifacts-aarch64:$(IMAGE_TAG)-arm64

# =============================================================================
# Rest (delegate to rest-api/Makefile)
# =============================================================================

.PHONY: rest-build rest-test rest-lint rest-fmt rest-clean \
        rest-docker-build rest-docker-build-local rest-helm-lint \
        rest-kind-reset

rest-build: ## Build all rest-api Go binaries into rest-api/build/binaries/
	$(MAKE) -C rest-api build

rest-test: ## Run all rest-api unit tests (auto-manages postgres + mock servers)
	$(MAKE) -C rest-api test

rest-lint: ## Lint rest-api: go vet + golangci-lint + revive
	$(MAKE) -C rest-api lint-go

rest-fmt: ## go fmt check on rest-api (fails if tree changed)
	$(MAKE) -C rest-api fmt-go

rest-clean: ## Tear down test postgres, mocks, kind, and remove rest build artifacts
	$(MAKE) -C rest-api clean

rest-docker-build: ## Build production docker images for rest services
	$(MAKE) -C rest-api docker-build

rest-docker-build-local: ## Build local-dev docker images for rest services
	$(MAKE) -C rest-api docker-build-local

rest-helm-lint: ## helm lint the rest umbrella and site-agent charts
	$(MAKE) -C rest-api helm-lint

rest-kind-reset: ## Spin up the local kind dev cluster: cluster + cert-manager + postgres + temporal + keycloak + helm app deploy (~10 min)
	$(MAKE) -C rest-api kind-reset

# Pattern-rule escape hatch: pass ANY target through to rest-api/Makefile.
# Usage:
#   make rest-api/test-api
#   make rest-api/kind-reset
#   make rest-api/generate-sdk
rest-api/%:
	$(MAKE) -C rest-api $*

proto-breaking:
	@echo "Checking for proto breaking changes..."
	@if ! command -v buf >/dev/null 2>&1; then \
		echo "buf is not installed. Please install buf: https://buf.build/docs/installation"; \
		exit 1; \
	fi
	buf breaking crates/rpc/proto --against 'https://github.com/NVIDIA/infra-controller.git#branch=main,subdir=crates/rpc/proto'

openapi-breaking:
	@echo "Checking for openapi breaking changes..."
	@if ! command -v oasdiff >/dev/null 2>&1; then \
		echo "oasdiff is not installed. Please install oasdiff: https://github.com/oasdiff/oasdiff"; \
		exit 1; \
	fi
	oasdiff breaking <(git show origin/main:rest-api/openapi/spec.yaml) rest-api/openapi/spec.yaml --fail-on ERR
