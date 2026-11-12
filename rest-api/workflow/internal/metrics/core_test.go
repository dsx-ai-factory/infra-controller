// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"strings"
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/metadata"
)

func TestNewCoreMetrics(t *testing.T) {
	previousVersion := metadata.Version
	metadata.Version = "v2.3.0-rc.10"
	t.Cleanup(func() {
		metadata.Version = previousVersion
	})

	registry := prometheus.NewRegistry()
	NewCoreMetrics(registry, "nico_rest_workflow", "site")

	err := testutil.GatherAndCompare(registry, strings.NewReader(`
# HELP nico_rest_workflow_info Information about the Cloud/Site worker
# TYPE nico_rest_workflow_info gauge
nico_rest_workflow_info{namespace="site",version="v2.3.0-rc.10"} 1
`), "nico_rest_workflow_info")
	require.NoError(t, err)
}
