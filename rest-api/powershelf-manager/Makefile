# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

CURDATE := $(shell date +%s)

gen-pb: ## Generate protobuf files
	@echo "Starting generate protobuf"
	    @protoc --go_out=. --go_opt=paths=source_relative \
		--go-grpc_out=. --go-grpc_opt=paths=source_relative \
		internal/proto/v1/*.proto
	@echo "Successfully generated protobuf"

psm:
	mkdir -p build
	go build -C build ..

container: psm
	docker build -t nico-psm:$$(grep -v -e '^#' -e "^$$" ../VERSION)-$(CURDATE)-$$(git rev-parse HEAD) -t nico-psm:latest .

.PHONY: gen-pb psm container runcontainer test
