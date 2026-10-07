<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# Production Docker Images

This directory contains multi-stage Dockerfiles optimized for production deployments.

## Features

### Multi-Stage Builds
- **Build Stage**: `golang:1.26.4` - Full Go toolchain for compilation
- **Runtime Stage**: `nvcr.io/nvidia/distroless/go:v4.0.8` - Minimal distroless runtime environment

### Optimizations
- Static compilation with CGO disabled
- Strip debug symbols (`-w -s` flags)
- Minimal distroless base image for reduced attack surface
- Non-root user execution for security
- Proper signal handling

### Security Improvements
- Non-root user (`nvs` from distroless)
- Minimal distroless image with no shell or package manager
- CA certificates included
- Timezone data for proper logging

## Available Images
1. **Dockerfile.nico-rest-api** - REST API server
2. **Dockerfile.nico-rest-db** - Database Migrations
3. **Dockerfile.nico-rest-workflow** - Workflow Service
4. **Dockerfile.nico-rest-site-manager** - Site Manager service
5. **Dockerfile.nico-rest-cert-manager** - Certificate Manager
6. **Dockerfile.nico-rest-site-agent** - Site Agent
7. **Dockerfile.nico-flow** - NICo Flow

## Building Images

### Build from Repository Root

All Dockerfiles must be built from the repository root as they require access to multiple modules:

```bash
cd /path/to/nico-rest-api

docker build \
  -f docker/production/Dockerfile.nico-rest-api \
  -t nico-rest-api:latest \
  .
```

### Build All Images

```bash
make docker-build
```
