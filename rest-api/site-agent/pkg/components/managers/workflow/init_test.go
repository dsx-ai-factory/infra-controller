// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/conftypes"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	dto "github.com/prometheus/client_model/go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestAPI_Init verifies the health reset and compatible connection counter families.
func TestAPI_Init(t *testing.T) {
	tests := []struct {
		name               string
		namespace          string
		checkDefaultHealth bool
	}{
		{name: "default metric families", namespace: conftypes.DefaultMetricsNamespace, checkDefaultHealth: true},
		{name: "configured connection counter namespace", namespace: "custom_site_agent"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			previousRegisterer := prometheus.DefaultRegisterer
			previousGauge := CertExpirationMetric
			t.Cleanup(func() {
				prometheus.DefaultRegisterer = previousRegisterer
				CertExpirationMetric = previousGauge
			})
			registry := prometheus.NewRegistry()
			prometheus.DefaultRegisterer = registry
			data := newHealthTestManager(t, &conftypes.Config{MetricsNamespace: tt.namespace})
			state := data.Managers.Workflow.State
			if tt.checkDefaultHealth {
				state.HealthStatus.Store(uint64(computils.CompHealthy))
			}
			(&API{}).Init()
			if tt.checkDefaultHealth {
				require.Equal(t, uint64(0), state.HealthStatus.Load())

				response := httptest.NewRecorder()
				handler := promhttp.HandlerFor(registry, promhttp.HandlerOpts{})
				handler.ServeHTTP(response, httptest.NewRequest(http.MethodGet, "/metrics", nil))
				require.Equal(t, http.StatusOK, response.Code)
				body := response.Body.String()
				for _, metric := range []struct {
					name string
					kind string
				}{
					{"temporal_connection_status", "counter"},
					{"temporal_connection_health", "gauge"},
					{"temporal_connection_attempted", "counter"},
					{"temporal_connection_succeeded", "counter"},
					{"temporal_cert_expiration_timestamp_seconds", "gauge"},
				} {
					assert.Contains(t, body, "# TYPE nico_rest_site_agent_"+metric.name+" "+metric.kind+"\n")
				}
				assert.Contains(t, body, "\nnico_rest_site_agent_temporal_connection_status 0\n")
				assert.Contains(t, body, "\nnico_rest_site_agent_temporal_connection_health 0\n")
			}

			for _, observation := range []struct {
				attempted uint64
				succeeded uint64
			}{
				{attempted: 0, succeeded: 0},
				{attempted: 3, succeeded: 2},
			} {
				state.ConnectionAttempted.Store(observation.attempted)
				state.ConnectionSucc.Store(observation.succeeded)
				metrics, err := registry.Gather()
				require.NoError(t, err)
				byName := make(map[string]*dto.MetricFamily, len(metrics))
				for _, metric := range metrics {
					byName[metric.GetName()] = metric
				}
				for _, counter := range []struct {
					name string
					help string
					want uint64
				}{
					{"temporal_connection_attempted", "Temporal connections attempted by the Site Agent", observation.attempted},
					{"temporal_connection_attempted_total", "Temporal connections attempted by the Site Agent", observation.attempted},
					{"temporal_connection_succeeded", "Temporal connections succeeded by the Site Agent", observation.succeeded},
					{"temporal_connection_succeeded_total", "Temporal connections succeeded by the Site Agent", observation.succeeded},
				} {
					fullName := tt.namespace + "_" + counter.name
					metric := byName[fullName]
					if tt.namespace != conftypes.DefaultMetricsNamespace {
						assert.NotContains(t, byName, conftypes.DefaultMetricsNamespace+"_"+counter.name)
					}
					require.NotNil(t, metric, fullName)
					assert.Equal(t, dto.MetricType_COUNTER, metric.GetType(), fullName)
					assert.Equal(t, counter.help, metric.GetHelp(), fullName)
					require.Len(t, metric.GetMetric(), 1, fullName)
					assert.Empty(t, metric.GetMetric()[0].GetLabel(), fullName)
					assert.Equal(t, float64(counter.want), metric.GetMetric()[0].GetCounter().GetValue(), fullName)
				}
			}
		})
	}
}

// TestWatchCertFilesReloadsEveryProjectedSecretUpdate replays the kubelet
// atomic writer twice. Each update deletes the versioned directory that the
// `ca` and `client` symlinks resolved to, so only the mount root watch sees the
// second update.
func TestWatchCertFilesReloadsEveryProjectedSecretUpdate(t *testing.T) {
	root := t.TempDir()
	writeGeneration := func(generation int) string {
		dir := fmt.Sprintf("..generation_%d", generation)
		for _, file := range []string{"ca/ca.crt", "client/tls.crt", "client/tls.key"} {
			path := filepath.Join(root, dir, file)
			require.NoError(t, os.MkdirAll(filepath.Dir(path), 0o755))
			require.NoError(t, os.WriteFile(path, []byte(dir), 0o600))
		}
		return dir
	}

	current := writeGeneration(0)
	require.NoError(t, os.Symlink(current, filepath.Join(root, computils.KubernetesSecretDataSymlink)))
	for _, dir := range []string{"ca", "client"} {
		require.NoError(t, os.Symlink(filepath.Join(computils.KubernetesSecretDataSymlink, dir), filepath.Join(root, dir)))
	}

	caDir, clientDir := filepath.Join(root, "ca")+"/", filepath.Join(root, "client")+"/"
	files := map[string]bool{
		filepath.Join(root, "ca", "ca.crt"):      true,
		filepath.Join(root, "client", "tls.crt"): true,
		filepath.Join(root, "client", "tls.key"): true,
	}
	w, err := newCertWatcher(root, []string{caDir, clientDir})
	require.NoError(t, err)

	reloads := make(chan struct{}, 64)
	done := make(chan struct{})
	go func() {
		defer close(done)
		watchCertFiles(w, root, files, func() { reloads <- struct{}{} })
	}()
	t.Cleanup(func() {
		w.Close()
		<-done
	})

	for generation := 1; generation <= 2; generation++ {
		next := writeGeneration(generation)
		staging := filepath.Join(root, "..data_tmp")
		require.NoError(t, os.Symlink(next, staging))
		require.NoError(t, os.Rename(staging, filepath.Join(root, computils.KubernetesSecretDataSymlink)))
		require.NoError(t, os.RemoveAll(filepath.Join(root, current)))
		current = next

		select {
		case <-reloads:
		case <-time.After(5 * time.Second):
			t.Fatalf("update %d did not reload the Temporal certs", generation)
		}

		// Drain the remaining events of this update so they can't satisfy the next one.
		// The kqueue backend reports the `..data` rename more than once.
		for drained := false; !drained; {
			select {
			case <-reloads:
			case <-time.After(200 * time.Millisecond):
				drained = true
			}
		}
	}
}
