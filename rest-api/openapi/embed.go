// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package openapi embeds the OpenAPI spec so it can be imported by other
// packages without requiring a build-time copy step.
package openapi

import _ "embed"

//go:embed spec.yaml
var Spec []byte
