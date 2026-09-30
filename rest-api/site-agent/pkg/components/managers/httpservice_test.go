// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"net/http"
	"net/http/httptest"
	"testing"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/stretchr/testify/assert"
)

func TestNewStatusServeMux(t *testing.T) {
	statusPaths := []string{
		computils.SiteStatus,
		computils.VPCStatus,
		computils.SubnetStatus,
		computils.InstanceStatus,
		computils.MachineStatus,
	}
	mux := newStatusServeMux()
	for _, path := range statusPaths {
		t.Run("registers "+path, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodGet, path, nil)
			_, pattern := mux.Handler(request)
			assert.Equal(t, path, pattern)
		})
	}

	excludedPaths := []string{"/metrics", "/unknown"}
	for _, path := range excludedPaths {
		t.Run("excludes "+path, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodGet, path, nil)
			_, pattern := mux.Handler(request)
			assert.Empty(t, pattern)
		})
	}
}

func TestNewMetricsServeMux(t *testing.T) {
	mux := newMetricsServeMux()
	t.Run("registers metrics", func(t *testing.T) {
		request := httptest.NewRequest(http.MethodGet, "/metrics", nil)
		_, pattern := mux.Handler(request)
		assert.Equal(t, "/metrics", pattern)
	})

	excludedPaths := []string{
		computils.SiteStatus,
		computils.VPCStatus,
		computils.SubnetStatus,
		computils.InstanceStatus,
		computils.MachineStatus,
		"/unknown",
	}
	for _, path := range excludedPaths {
		t.Run("excludes "+path, func(t *testing.T) {
			request := httptest.NewRequest(http.MethodGet, path, nil)
			_, pattern := mux.Handler(request)
			assert.Empty(t, pattern)
		})
	}
}
