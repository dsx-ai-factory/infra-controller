// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSession_fetchDPUMachines(t *testing.T) {
	for _, test := range []struct {
		name      string
		site      string
		wantError string
	}{
		{name: "all pages in selected site", site: "site-1"},
		{name: "missing site", wantError: "siteId must be resolved before DPU machines"},
	} {
		t.Run(test.name, func(t *testing.T) {
			requests := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests++
				assert.Equal(t, "/v2/org/acme/nico/dpu", r.URL.Path)
				assert.Equal(t, test.site, r.URL.Query().Get("siteId"))
				assert.Equal(t, "100", r.URL.Query().Get("pageSize"))
				assert.Equal(t, fmt.Sprint(requests), r.URL.Query().Get("pageNumber"))
				if requests == 1 {
					machines := make([]map[string]string, 100)
					for i := range machines {
						machines[i] = map[string]string{"id": fmt.Sprintf("dpu-%d", i), "state": "Ready"}
					}
					assert.NoError(t, json.NewEncoder(w).Encode(machines))
					return
				}
				_, _ = io.WriteString(w, `[{"id":"last-dpu","labels":{"hostname":"bluefield"},"state":"Ready"}]`)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Scope.SiteID = test.site
			items, err := session.fetchDPUMachines(context.Background())
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				assert.Zero(t, requests)
				return
			}
			require.NoError(t, err)
			require.Len(t, items, 101)
			assert.Equal(t, "last-dpu", items[100].ID)
			assert.Equal(t, "bluefield", items[100].Name)
			assert.Equal(t, "Ready", items[100].Status)
			assert.Equal(t, 2, requests)
		})
	}
}

func TestSession_fetchSpectrumXPartitions(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v2/org/acme/nico/tenant/current":
			_, _ = io.WriteString(w, `{"id":"tenant-1"}`)
		case "/v2/org/acme/nico/spectrumx-partition":
			assert.Equal(t, "tenant-1", r.URL.Query().Get("tenantId"))
			assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
			_, _ = io.WriteString(w, `[
				{"id":"provider-partition","tenantId":null},
				{"id":"other-partition","tenantId":"tenant-2"},
				{"id":"partition-1","tenantId":"tenant-1","name":"training","status":"Ready"}
			]`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()
	session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
	session.Scope.SiteID = "site-1"
	session.Cache.Set("_infra_provider", []NamedItem{{Name: "acme", ID: "provider-1"}})
	items, err := session.fetchSpectrumXPartitions(context.Background())
	require.NoError(t, err)
	require.Len(t, items, 1)
	assert.Equal(t, "partition-1", items[0].ID)
	assert.Equal(t, "training", items[0].Name)
	assert.Equal(t, "Ready", items[0].Status)
}

func TestSession_fetchLabelKeys(t *testing.T) {
	for _, test := range []struct {
		name      string
		resource  string
		response  string
		wantCount int
		wantError string
	}{
		{name: "machine pages", resource: "machine", response: `["last/key"]`, wantCount: 101},
		{name: "empty result", resource: "machine", response: `[]`},
		{name: "invalid response", resource: "machine", response: `{}`, wantError: "parsing machine label keys"},
	} {
		t.Run(test.name, func(t *testing.T) {
			requests := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests++
				assert.Equal(t, "/v2/org/acme/nico/"+test.resource+"/label/key", r.URL.Path)
				assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
				assert.Equal(t, "100", r.URL.Query().Get("pageSize"))
				assert.Equal(t, fmt.Sprint(requests), r.URL.Query().Get("pageNumber"))
				if requests == 1 && test.wantCount > 100 {
					keys := make([]string, 100)
					for i := range keys {
						keys[i] = fmt.Sprintf("key-%d", i)
					}
					assert.NoError(t, json.NewEncoder(w).Encode(keys))
					return
				}
				_, _ = io.WriteString(w, test.response)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Scope.SiteID = "site-1"
			items, err := session.fetchLabelKeys(test.resource)
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, items, "nil would make the resolver cache treat an empty result as a miss")
			require.Len(t, items, test.wantCount)
			if test.wantCount > 0 {
				assert.Equal(t, NamedItem{Name: "last/key", ID: "last/key"}, items[test.wantCount-1])
			}
			assert.Equal(t, test.wantCount/100+1, requests)
		})
	}
}

func TestSession_fetchMachineChassis(t *testing.T) {
	for _, test := range []struct {
		name                 string
		machine              string
		endpointError        string
		wantError            string
		wantEndpointRequests int
	}{
		{name: "selected machine report", machine: `{"siteId":"machine-site"}`, wantEndpointRequests: 2},
		{
			name:                 "REST without machineId uses unfiltered pages",
			machine:              `{"siteId":"machine-site"}`,
			endpointError:        "Unknown query parameter specified in request: machineId",
			wantEndpointRequests: 3,
		},
		{
			name:                 "other bad request is returned",
			machine:              `{"siteId":"machine-site"}`,
			endpointError:        "Site is not in Registered state",
			wantError:            "API error 400: Site is not in Registered state",
			wantEndpointRequests: 1,
		},
		{name: "machine without site", machine: `{}`, wantError: "machine machine-1 has no siteId"},
	} {
		t.Run(test.name, func(t *testing.T) {
			pages := 0
			endpointRequests := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				switch r.URL.Path {
				case "/v2/org/acme/nico/machine/machine-1":
					_, _ = io.WriteString(w, test.machine)
				case "/v2/org/acme/nico/site-explorer/endpoint":
					endpointRequests++
					assert.Equal(t, "machine-site", r.URL.Query().Get("siteId"))
					if test.endpointError != "" && endpointRequests > 1 {
						assert.False(t, r.URL.Query().Has("machineId"))
					} else {
						assert.Equal(t, "machine-1", r.URL.Query().Get("machineId"))
					}
					if test.endpointError != "" && endpointRequests == 1 {
						w.WriteHeader(http.StatusBadRequest)
						assert.NoError(t, json.NewEncoder(w).Encode(map[string]string{"message": test.endpointError}))
						return
					}
					pages++
					assert.Equal(t, fmt.Sprint(pages), r.URL.Query().Get("pageNumber"))
					if pages == 1 {
						endpoints := make([]map[string]interface{}, 100)
						for i := range endpoints {
							endpoints[i] = map[string]interface{}{"report": map[string]interface{}{"machineId": "other-machine", "chassis": []map[string]string{{"id": "OtherChassis"}}}}
						}
						assert.NoError(t, json.NewEncoder(w).Encode(endpoints))
						return
					}
					_, _ = io.WriteString(w, `[{"report":{"machineId":"machine-1","chassis":[{"id":"Chassis_0"},{"id":"Chassis_1"}]}}]`)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Scope.SiteID = "different-active-site"
			items, err := session.fetchMachineChassis("machine-1")
			assert.Equal(t, test.wantEndpointRequests, endpointRequests)
			assert.Equal(t, "different-active-site", session.Scope.SiteID)
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				assert.Zero(t, pages)
				return
			}
			require.NoError(t, err)
			require.Len(t, items, 2)
			assert.Equal(t, "Chassis_0", items[0].ID)
			assert.Equal(t, "Chassis_1", items[1].ID)
			assert.Equal(t, 2, pages)
		})
	}
}

func TestRunGeneratedTUICommand_ResourceSelectors(t *testing.T) {
	for _, test := range []struct {
		name            string
		command         string
		method          string
		path            string
		args            []string
		scopeSiteID     string
		wantSiteIDQuery string
		responses       map[string]string
	}{
		{
			name: "DPU", command: "dpu-machine get", method: http.MethodGet,
			path:        "/v2/org/acme/nico/dpu/dpu-1",
			scopeSiteID: "site-1", wantSiteIDQuery: "site-1",
			responses: map[string]string{"/v2/org/acme/nico/dpu": `[{"id":"dpu-1"}]`},
		},
		{
			name: "explicit DPU ID", command: "dpu-machine get", method: http.MethodGet,
			path:        "/v2/org/acme/nico/dpu/dpu-1",
			args:        []string{"--site-id", "site-1", "dpu-1"},
			scopeSiteID: "old-site", wantSiteIDQuery: "site-1",
			responses: map[string]string{"/v2/org/acme/nico/dpu": `[{"id":"dpu-1"}]`},
		},
		{
			name: "SpectrumX", command: "spectrumx-partition get", method: http.MethodGet,
			path:        "/v2/org/acme/nico/spectrumx-partition/partition-1",
			scopeSiteID: "site-1",
			responses: map[string]string{
				"/v2/org/acme/nico/tenant/current":      `{"id":"tenant-1"}`,
				"/v2/org/acme/nico/spectrumx-partition": `[{"id":"partition-1","name":"training","tenantId":"tenant-1"}]`,
			},
		},
		{
			name: "explicit SpectrumX ID", command: "spectrumx-partition get", method: http.MethodGet,
			path:        "/v2/org/acme/nico/spectrumx-partition/partition-1",
			args:        []string{"partition-1"},
			scopeSiteID: "site-1",
			responses: map[string]string{
				"/v2/org/acme/nico/tenant/current":      `{"id":"tenant-1"}`,
				"/v2/org/acme/nico/spectrumx-partition": `[{"id":"partition-1","name":"training","tenantId":"tenant-1"}]`,
			},
		},
		{
			name: "chassis reset", command: "machine reset-machine-chassis reset-machine-chassis", method: http.MethodPatch,
			path:        "/v2/org/acme/nico/machine/machine-1/chassis/Chassis_0/reset",
			scopeSiteID: "site-1",
			responses: map[string]string{
				"/v2/org/acme/nico/machine/machine-1":      `{"siteId":"site-1"}`,
				"/v2/org/acme/nico/site-explorer/endpoint": `[{"report":{"machineId":"machine-1","chassis":[{"id":"Chassis_0"}]}}]`,
			},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			executed := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				if test.wantSiteIDQuery != "" {
					assert.Equal(t, test.wantSiteIDQuery, r.URL.Query().Get("siteId"))
				}
				if r.URL.Path == test.path {
					executed++
					assert.Equal(t, test.method, r.Method)
					_, _ = io.WriteString(w, `{}`)
					return
				}
				response, ok := test.responses[r.URL.Path]
				assert.True(t, ok, "unexpected request: %s", r.URL)
				_, _ = io.WriteString(w, response)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Scope.SiteID = test.scopeSiteID
			session.Cache.Set("machine", []NamedItem{{Name: "host", ID: "machine-1"}})
			_, err := withStdin(t, "y\n", func() (string, error) {
				var runErr error
				output := captureStdout(func() {
					runErr = requireTUICommand(t, test.command).Run(session, test.args)
				})
				return output, runErr
			})
			require.NoError(t, err)
			assert.Equal(t, 1, executed)
		})
	}
}

func TestResolveGeneratedPathParameters_ExplicitDiscoveryValues(t *testing.T) {
	for _, test := range []struct {
		command string
		args    []string
	}{
		{command: "machine label-values list", args: []string{" Mixed/Case "}},
		{command: "expected-machine label-values list", args: []string{" Mixed/Case "}},
		{command: "machine reset-machine-chassis reset-machine-chassis", args: []string{"machine-1", "Chassis_0"}},
	} {
		t.Run(test.command, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				t.Errorf("explicit key or chassis ID should not need discovery: %s", r.URL)
				http.NotFound(w, r)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Cache.Set("machine", []NamedItem{{Name: "host", ID: "machine-1"}})
			got, err := resolveGeneratedPathParameters(session, requireGeneratedInfo(t, test.command), test.args)
			require.NoError(t, err)
			assert.Equal(t, test.args, got)
		})
	}
}
