// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"github.com/NVIDIA/infra-controller/rest-api/flow/cmd"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/tracing"
)

func main() {
	// First: interceptors and handlers below capture the global propagator.
	tracing.InstallPropagator()
	// No-op unless OTEL_EXPORTER_OTLP_ENDPOINT is set.
	defer tracing.InstallExporter("flow")()
	cmd.Execute()
}
