<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# Security Policy: NVIDIA Infra Controller REST API

The repository-wide security policy in [`../SECURITY.md`](../SECURITY.md) is
canonical for this `rest-api/` tree. It contains the reporting policy,
architecture context, threat model, critical security assumptions, deployment
hardening checklist, scope, and dependency-security guidance for NICo REST and
its supporting services.

REST-specific surfaces covered by the root policy include:

- Go/Echo REST API routes under `/v*/org/:orgName/<apiName>/...`.
- JWT, JWKS, Keycloak, service-account, organization, and role processing.
- Temporal cloud and site workflows, site-agent communication, and Core gRPC
  proxying.
- PostgreSQL/Bun persistence, audit logging, OpenTelemetry, Prometheus, and
  Sentry integration.
- REST Helm charts, local development deployment files, OpenAPI contracts, and
  generated SDKs.

Report potential vulnerabilities through NVIDIA PSIRT as described in the root
policy. Do not open public issues or pull requests with vulnerability details.
