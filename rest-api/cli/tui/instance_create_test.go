// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSession_fetchVPCsPreservesNetworkVirtualizationType(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		assert.Equal(t, "/v2/org/acme/nico/vpc", r.URL.Path)
		assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
		_, err := io.WriteString(w, `[{"id":"vpc-1","name":"ethernet-vpc","siteId":"site-1","tenantId":"tenant-1","status":"Ready","networkVirtualizationType":"ETHERNET_VIRTUALIZER"}]`)
		require.NoError(t, err)
	}))
	defer server.Close()

	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	session.Scope.SiteID = "site-1"
	session.Cache.Set("_tenant", []NamedItem{{Name: "acme", ID: "tenant-1"}})

	vpcs, err := session.fetchVPCs(context.Background())

	require.NoError(t, err)
	require.Len(t, vpcs, 1)
	assert.Equal(t, "ETHERNET_VIRTUALIZER", vpcs[0].Extra["networkVirtualizationType"])
	assert.Equal(t, "site-1", vpcs[0].Extra["siteId"])
	assert.Equal(t, "tenant-1", vpcs[0].Extra["tenantId"])
}

func TestInstanceNetworkConfigForVPC(t *testing.T) {
	tests := []struct {
		name               string
		virtualizationType string
		want               instanceNetworkConfig
		wantErr            string
	}{
		{
			name:               "Ethernet virtualizer uses subnets",
			virtualizationType: "ETHERNET_VIRTUALIZER",
			want: instanceNetworkConfig{
				resourceType: "subnet",
				singular:     "Subnet",
				plural:       "subnets",
				selectorKey:  "subnetId",
			},
		},
		{
			name:               "FNN uses VPC prefixes",
			virtualizationType: "FNN",
			want: instanceNetworkConfig{
				detectMultiDPU: true,
				resourceType:   "vpc-prefix",
				reuseResources: true,
				singular:       "VPC prefix",
				plural:         "VPC prefixes",
				selectorKey:    "vpcPrefixId",
			},
		},
		{
			name:               "flat VPC uses automatic networking",
			virtualizationType: "FLAT",
			want: instanceNetworkConfig{
				autoNetwork: true,
			},
		},
		{
			name:               "unknown type is rejected",
			virtualizationType: "UNKNOWN",
			wantErr:            `does not support VPC network virtualization type "UNKNOWN"`,
		},
		{
			name:    "missing type is rejected",
			wantErr: "selected VPC has no network virtualization type",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			vpc := &NamedItem{
				Extra: map[string]string{
					"networkVirtualizationType": test.virtualizationType,
				},
			}

			got, err := instanceNetworkConfigForVPC(vpc)

			if test.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), test.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}

	t.Run("missing VPC is rejected", func(t *testing.T) {
		_, err := instanceNetworkConfigForVPC(nil)

		require.Error(t, err)
		assert.Contains(t, err.Error(), "selected VPC is missing")
	})
}

func TestFetchInstanceMultiDPUCapability(t *testing.T) {
	tests := []struct {
		name         string
		responseBody string
		status       int
		want         *instanceDPUDeviceNetworkCapability
		wantErr      string
	}{
		{
			name: "finds dual DPU network capability",
			responseBody: `{
				"machineCapabilities":[
					{"type":"GPU","name":"H100","count":8},
					{"type":"Network","name":"BlueField-3","deviceType":"DPU","count":2}
				]
			}`,
			status: http.StatusOK,
			want: &instanceDPUDeviceNetworkCapability{
				name:  "BlueField-3",
				count: 2,
			},
		},
		{
			name: "ignores a single DPU",
			responseBody: `{
				"machineCapabilities":[
					{"type":"Network","name":"BlueField-3","deviceType":"DPU","count":1}
				]
			}`,
			status: http.StatusOK,
		},
		{
			name: "ignores non-DPU network capability",
			responseBody: `{
				"machineCapabilities":[
					{"type":"Network","name":"ConnectX-7","deviceType":"NVLink","count":2}
				]
			}`,
			status: http.StatusOK,
		},
		{
			name:         "reports malformed machine response",
			responseBody: `{`,
			status:       http.StatusOK,
			wantErr:      "parsing capabilities for machine machine-1",
		},
		{
			name:         "reports machine lookup failure",
			responseBody: `{"message":"machine unavailable"}`,
			status:       http.StatusInternalServerError,
			wantErr:      "fetching capabilities for machine machine-1",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, "/v2/org/acme/nico/machine/machine-1", r.URL.Path)
				w.WriteHeader(test.status)
				_, err := io.WriteString(w, test.responseBody)
				require.NoError(t, err)
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			got, err := fetchInstanceMultiDPUCapability(session, "machine-1")

			if test.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), test.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}
}

func TestPromptVirtualFunctionID(t *testing.T) {
	tests := []struct {
		name  string
		input string
		used  map[int]bool
		want  int
	}{
		{
			name:  "blank is retried",
			input: "\n0\n",
			want:  0,
		},
		{
			name:  "zero is accepted",
			input: "0\n",
			want:  0,
		},
		{
			name:  "fifteen is accepted",
			input: "15\n",
			want:  15,
		},
		{
			name:  "invalid values are retried",
			input: "not-a-number\n-1\n16\n7\n",
			want:  7,
		},
		{
			name:  "duplicate IDs are retried",
			input: "3\n4\n",
			used: map[int]bool{
				3: true,
			},
			want: 4,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			used := test.used
			if used == nil {
				used = make(map[int]bool)
			}
			var got int
			_, err := withStdin(t, test.input, func() (string, error) {
				var promptErr error
				got, promptErr = promptVirtualFunctionID("Virtual function ID", used)
				return "", promptErr
			})

			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}
}

func TestDeviceVirtualFunctionIDsExhausted(t *testing.T) {
	vfIDs := deviceVirtualFunctionIDs{
		used: make(map[int]bool),
	}
	for virtualFunctionID := virtualFunctionIDMinimum; virtualFunctionID < virtualFunctionIDMaximum; virtualFunctionID++ {
		vfIDs.used[virtualFunctionID] = true
	}
	assert.False(t, vfIDs.exhausted())

	vfIDs.used[virtualFunctionIDMaximum] = true
	assert.True(t, vfIDs.exhausted())
}

func TestPromptMultiDPUInstanceInterfaces(t *testing.T) {
	cache := NewCache()
	resolver := NewResolver(cache)
	session := &Session{
		Cache:    cache,
		Resolver: resolver,
	}
	networkConfig := instanceNetworkConfig{
		dpuCapability: &instanceDPUDeviceNetworkCapability{
			name:  "dual-dpu-network",
			count: 2,
		},
		plural:      "VPC prefixes",
		selectorKey: "vpcPrefixId",
		singular:    "VPC prefix",
	}

	t.Run("reuses one prefix across physical interfaces on multiple DPUs", func(t *testing.T) {
		readyItems := []NamedItem{
			{
				ID:     "prefix-1",
				Name:   "prefix-one",
				Status: "Ready",
			},
		}

		var got []map[string]interface{}
		_, err := withStdin(t, "n\ny\nn\n", func() (string, error) {
			var promptErr error
			got, promptErr = promptMultiDPUInstanceInterfaces(session, networkConfig, readyItems)
			return "", promptErr
		})

		require.NoError(t, err)
		assert.Equal(t, []map[string]interface{}{
			{
				"device":         "dual-dpu-network",
				"deviceInstance": 0,
				"isPhysical":     true,
				"vpcPrefixId":    "prefix-1",
			},
			{
				"device":         "dual-dpu-network",
				"deviceInstance": 1,
				"isPhysical":     true,
				"vpcPrefixId":    "prefix-1",
			},
		}, got)
	})
}

func TestSession_tenantHasTargetedInstanceCreationAtSite(t *testing.T) {
	tests := []struct {
		name          string
		scopeVPCID    string
		siteID        string
		siteResponse  string
		siteStatus    int
		accountStatus int
		capabilities  []interface{}
		want          bool
		wantErr       string
	}{
		{
			name:   "enabled by account default",
			siteID: "site-1",
			capabilities: []interface{}{
				map[string]interface{}{
					"targetedInstanceCreation": true,
				},
			},
			want: true,
		},
		{
			name:   "disabled by account default",
			siteID: "site-1",
			capabilities: []interface{}{
				map[string]interface{}{
					"targetedInstanceCreation": false,
				},
			},
		},
		{
			name:   "site override enables disabled default",
			siteID: "site-1",
			capabilities: []interface{}{
				map[string]interface{}{
					"targetedInstanceCreation": false,
				},
				map[string]interface{}{
					"siteIds": []interface{}{
						"site-1",
					},
					"targetedInstanceCreation": true,
				},
			},
			want: true,
		},
		{
			name:   "site override disables enabled default",
			siteID: "site-1",
			capabilities: []interface{}{
				map[string]interface{}{
					"targetedInstanceCreation": true,
				},
				map[string]interface{}{
					"siteIds": []interface{}{
						"site-1",
					},
					"targetedInstanceCreation": false,
				},
			},
		},
		{
			name:   "unrelated site override preserves default",
			siteID: "site-1",
			capabilities: []interface{}{
				map[string]interface{}{
					"targetedInstanceCreation": true,
				},
				map[string]interface{}{
					"siteIds": []interface{}{
						"site-2",
					},
					"targetedInstanceCreation": false,
				},
			},
			want: true,
		},
		{
			name:    "missing site ID in site scope",
			wantErr: "site ID is missing",
		},
		{
			name:       "missing site ID in VPC scope",
			scopeVPCID: "vpc-1",
			wantErr:    "selected VPC has no site ID",
		},
		{
			name:       "site lookup failure in site scope",
			siteID:     "site-1",
			siteStatus: http.StatusServiceUnavailable,
			wantErr:    "fetching site: API error 503: unavailable",
		},
		{
			name:       "site lookup failure in VPC scope",
			scopeVPCID: "vpc-1",
			siteID:     "site-1",
			siteStatus: http.StatusServiceUnavailable,
			wantErr:    "fetching selected VPC site: API error 503: unavailable",
		},
		{
			name:         "invalid site response in site scope",
			siteID:       "site-1",
			siteResponse: "{",
			wantErr:      "parsing site: unexpected end of JSON input",
		},
		{
			name:         "invalid site response in VPC scope",
			scopeVPCID:   "vpc-1",
			siteID:       "site-1",
			siteResponse: "{",
			wantErr:      "parsing selected VPC site: unexpected end of JSON input",
		},
		{
			name:         "missing provider ID in site scope",
			siteID:       "site-1",
			siteResponse: `{}`,
			wantErr:      "site has no infrastructure provider ID",
		},
		{
			name:         "missing provider ID in VPC scope",
			scopeVPCID:   "vpc-1",
			siteID:       "site-1",
			siteResponse: `{}`,
			wantErr:      "selected VPC site has no infrastructure provider ID",
		},
		{
			name:          "account lookup failure in site scope",
			siteID:        "site-1",
			accountStatus: http.StatusServiceUnavailable,
			wantErr:       "fetching tenant accounts for site: API error 503: unavailable",
		},
		{
			name:          "account lookup failure in VPC scope",
			scopeVPCID:    "vpc-1",
			siteID:        "site-1",
			accountStatus: http.StatusServiceUnavailable,
			wantErr:       "fetching tenant accounts for selected VPC site: API error 503: unavailable",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				switch request.URL.Path {
				case "/v2/org/acme/nico/tenant/current":
					_, err := io.WriteString(w, `{"id":"tenant-1"}`)
					require.NoError(t, err)
				case "/v2/org/acme/nico/site/site-1":
					if test.siteStatus != 0 {
						w.WriteHeader(test.siteStatus)
						_, err := io.WriteString(w, `{"message":"unavailable"}`)
						require.NoError(t, err)
						return
					}
					siteResponse := test.siteResponse
					if siteResponse == "" {
						siteResponse = `{"infrastructureProviderId":"provider-1"}`
					}
					_, err := io.WriteString(w, siteResponse)
					require.NoError(t, err)
				case "/v2/org/acme/nico/tenant/account":
					assert.Equal(t, "provider-1", request.URL.Query().Get("infrastructureProviderId"))
					assert.Equal(t, "tenant-1", request.URL.Query().Get("tenantId"))
					if test.accountStatus != 0 {
						w.WriteHeader(test.accountStatus)
						_, err := io.WriteString(w, `{"message":"unavailable"}`)
						require.NoError(t, err)
						return
					}
					err := json.NewEncoder(w).Encode([]map[string]interface{}{
						{
							"status":                   "Ready",
							"infrastructureProviderId": "provider-1",
							"tenantId":                 "tenant-1",
							"siteCapabilities":         test.capabilities,
						},
					})
					require.NoError(t, err)
				default:
					http.NotFound(w, request)
				}
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			session.Scope.VpcID = test.scopeVPCID

			got, err := session.tenantHasTargetedInstanceCreationAtSite(context.Background(), test.siteID)

			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				assert.False(t, got)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}
}

func TestPromptInstanceInterfaces(t *testing.T) {
	tests := []struct {
		name               string
		virtualizationType string
		resourceType       string
		items              []NamedItem
		responseStatus     int
		input              string
		want               []map[string]interface{}
		wantErr            string
	}{
		{
			name:               "Ethernet virtualizer selects a Ready subnet",
			virtualizationType: "ETHERNET_VIRTUALIZER",
			resourceType:       "subnet",
			items: []NamedItem{
				{
					Name:   "tenant-subnet",
					ID:     "subnet-1",
					Status: "Ready",
				},
			},
			input: "n\n",
			want: []map[string]interface{}{
				{
					"isPhysical": true,
					"subnetId":   "subnet-1",
				},
			},
		},
		{
			name:               "FNN selects a Ready VPC prefix",
			virtualizationType: "FNN",
			resourceType:       "vpc-prefix",
			items: []NamedItem{
				{
					Name:   "tenant-prefix",
					ID:     "vpc-prefix-1",
					Status: "Ready",
				},
			},
			input: "n\n",
			want: []map[string]interface{}{
				{
					"isPhysical":  true,
					"vpcPrefixId": "vpc-prefix-1",
				},
			},
		},
		{
			name:               "device-less FNN reuses a Ready VPC prefix",
			virtualizationType: "FNN",
			resourceType:       "vpc-prefix",
			items: []NamedItem{
				{
					Name:   "tenant-prefix",
					ID:     "vpc-prefix-1",
					Status: "Ready",
				},
			},
			input: "y\n3\nn\n",
			want: []map[string]interface{}{
				{
					"isPhysical":  true,
					"vpcPrefixId": "vpc-prefix-1",
				},
				{
					"isPhysical":        false,
					"virtualFunctionId": 3,
					"vpcPrefixId":       "vpc-prefix-1",
				},
			},
		},
		{
			name:               "missing Ready subnet returns a local error",
			virtualizationType: "ETHERNET_VIRTUALIZER",
			resourceType:       "subnet",
			wantErr:            "no Ready subnets available for selected VPC",
		},
		{
			name:               "empty Ready VPC prefix response returns a local error",
			virtualizationType: "FNN",
			resourceType:       "vpc-prefix",
			wantErr:            "no Ready VPC prefixes available for selected VPC",
		},
		{
			name:               "VPC prefix lookup failure returns a local error",
			virtualizationType: "FNN",
			resourceType:       "vpc-prefix",
			responseStatus:     http.StatusServiceUnavailable,
			wantErr:            "listing VPC prefixes for selected VPC",
		},
		{
			name:               "flat VPC does not fetch explicit interface resources",
			virtualizationType: "FLAT",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			requestCount := 0
			var requestOrderBy string
			var requestPath string
			var requestStatus string
			var requestSiteID string
			var requestVPCID string
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				requestCount++
				requestOrderBy = request.URL.Query().Get("orderBy")
				requestPath = request.URL.Path
				requestStatus = request.URL.Query().Get("status")
				requestSiteID = request.URL.Query().Get("siteId")
				requestVPCID = request.URL.Query().Get("vpcId")
				if test.responseStatus != 0 {
					w.WriteHeader(test.responseStatus)
					_, writeErr := io.WriteString(w, `{"message":"API unavailable"}`)
					require.NoError(t, writeErr)
					return
				}
				resources := make([]map[string]interface{}, len(test.items))
				for i, item := range test.items {
					resources[i] = map[string]interface{}{
						"id":     item.ID,
						"name":   item.Name,
						"status": item.Status,
					}
				}
				writeErr := json.NewEncoder(w).Encode(resources)
				require.NoError(t, writeErr)
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			session.Scope.SiteID = "site-1"
			session.Scope.VpcID = "vpc-1"
			vpc := &NamedItem{
				Extra: map[string]string{
					"networkVirtualizationType": test.virtualizationType,
				},
			}
			networkConfig, configErr := instanceNetworkConfigForVPC(vpc)
			require.NoError(t, configErr)
			var got []map[string]interface{}

			_, err := withStdin(t, test.input, func() (string, error) {
				var promptErr error
				got, promptErr = promptInstanceInterfaces(session, networkConfig)
				return "", promptErr
			})

			if test.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), test.wantErr)
			} else {
				require.NoError(t, err)
				assert.Equal(t, test.want, got)
			}
			if test.resourceType == "" {
				assert.Zero(t, requestCount)
			} else {
				assert.Equal(t, 1, requestCount)
				assert.Equal(t, "NAME_ASC", requestOrderBy)
				assert.Equal(t, "/v2/org/acme/nico/"+test.resourceType, requestPath)
				assert.Equal(t, "Ready", requestStatus)
				assert.Equal(t, "site-1", requestSiteID)
				assert.Equal(t, "vpc-1", requestVPCID)
			}
		})
	}
}

func TestFetchReadyInstanceNetworkResourcesOmitsStatusFromPickerItems(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		assert.Equal(t, "NAME_ASC", request.URL.Query().Get("orderBy"))
		assert.Equal(t, "Ready", request.URL.Query().Get("status"))
		_, err := io.WriteString(w, `[
			{"id":"subnet-1","name":"subnet-one","status":"Ready"},
			{"id":"subnet-2","name":"subnet-two","status":"Ready"}
		]`)
		require.NoError(t, err)
	}))
	defer server.Close()

	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	items, err := fetchReadyInstanceNetworkResources(session, instanceNetworkConfig{
		resourceType: "subnet",
	})

	require.NoError(t, err)
	require.Len(t, items, 2)
	assert.Empty(t, items[0].Status)
	assert.Empty(t, items[1].Status)
}
