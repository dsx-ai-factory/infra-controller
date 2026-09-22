// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simple

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// TestToStandardInstanceUpdateRequest verifies that nil slice/map fields in an
// InstanceUpdateRequest are NOT forwarded to the standard API request. Forwarding nil as an empty
// slice causes the backend to clear the corresponding attribute (the original reported bug).
func TestToStandardInstanceUpdateRequest(t *testing.T) {
	t.Run("partial update with only NVLinkInterfaces set leaves IB and DES nil", func(t *testing.T) {
		req := InstanceUpdateRequest{
			NVLinkInterfaces: []NVLinkInterfaceCreateOrUpdateRequest{
				{NVLinkLogicalPartitionID: "nvlink-partition-1"},
			},
		}

		apiReq := toStandardInstanceUpdateRequest(req)

		// NVLink must be populated
		require.NotNil(t, apiReq.NvLinkInterfaces)
		assert.Len(t, apiReq.NvLinkInterfaces, 1)

		// InfiniBand and DES must stay nil so they are omitted from the JSON body
		assert.Nil(t, apiReq.InfinibandInterfaces,
			"InfinibandInterfaces must be nil when not set, to avoid clearing existing IB interfaces on the server")
		assert.Nil(t, apiReq.DpuExtensionServiceDeployments,
			"DpuExtensionServiceDeployments must be nil when not set")

		// Ensure the standard SDK's ToMap omits the unset fields entirely
		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "nvLinkInterfaces")
		assert.NotContains(t, body, "infinibandInterfaces",
			"infinibandInterfaces must not appear in the serialized request when not provided")
		assert.NotContains(t, body, "dpuExtensionServiceDeployments",
			"dpuExtensionServiceDeployments must not appear in the serialized request when not provided")
	})

	t.Run("partial update with only InfinibandInterfaces set leaves NVLink and DES nil", func(t *testing.T) {
		req := InstanceUpdateRequest{
			InfinibandInterfaces: []InfiniBandInterfaceCreateOrUpdateRequest{
				{
					PartitionID:    "ib-partition-1",
					Device:         "mlx5_0",
					DeviceInstance: 0,
					IsPhysical:     true,
				},
			},
		}

		apiReq := toStandardInstanceUpdateRequest(req)

		require.NotNil(t, apiReq.InfinibandInterfaces)
		assert.Len(t, apiReq.InfinibandInterfaces, 1)
		assert.Nil(t, apiReq.NvLinkInterfaces,
			"NvLinkInterfaces must be nil when not set, to avoid clearing existing NVLink interfaces on the server")
		assert.Nil(t, apiReq.DpuExtensionServiceDeployments)

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "infinibandInterfaces")
		assert.NotContains(t, body, "nvLinkInterfaces")
		assert.NotContains(t, body, "dpuExtensionServiceDeployments")
	})

	t.Run("partial update with nil Labels leaves labels nil", func(t *testing.T) {
		name := "new-name"
		req := InstanceUpdateRequest{
			Name: &name,
		}

		apiReq := toStandardInstanceUpdateRequest(req)

		assert.Nil(t, apiReq.Labels,
			"Labels must be nil when not set, to avoid clearing existing labels on the server")

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.NotContains(t, body, "labels")
	})

	t.Run("explicit empty slice is forwarded as empty array to clear existing entries", func(t *testing.T) {
		req := InstanceUpdateRequest{
			// Explicitly setting to an empty (non-nil) slice signals intent to clear
			InfinibandInterfaces: []InfiniBandInterfaceCreateOrUpdateRequest{},
		}

		apiReq := toStandardInstanceUpdateRequest(req)

		// An explicit empty slice must be forwarded (non-nil) so the server clears the list
		require.NotNil(t, apiReq.InfinibandInterfaces,
			"An explicit empty InfinibandInterfaces slice must be forwarded to clear existing entries")
		assert.Empty(t, apiReq.InfinibandInterfaces)

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "infinibandInterfaces")
	})

	t.Run("full update with all slice and map fields set", func(t *testing.T) {
		name := "my-instance"
		req := InstanceUpdateRequest{
			Name:   &name,
			Labels: map[string]string{"env": "prod"},
			InfinibandInterfaces: []InfiniBandInterfaceCreateOrUpdateRequest{
				{PartitionID: "ib-1", Device: "mlx5_0", DeviceInstance: 0, IsPhysical: true},
			},
			NVLinkInterfaces: []NVLinkInterfaceCreateOrUpdateRequest{
				{NVLinkLogicalPartitionID: "nvlink-1"},
			},
			DpuExtensionServiceDeployments: []DpuExtensionServiceDeploymentRequest{},
		}

		apiReq := toStandardInstanceUpdateRequest(req)

		assert.NotNil(t, apiReq.InfinibandInterfaces)
		assert.Len(t, apiReq.InfinibandInterfaces, 1)
		assert.NotNil(t, apiReq.NvLinkInterfaces)
		assert.Len(t, apiReq.NvLinkInterfaces, 1)
		assert.NotNil(t, apiReq.DpuExtensionServiceDeployments)
		assert.Empty(t, apiReq.DpuExtensionServiceDeployments)
		assert.NotNil(t, apiReq.Labels)

		body, err := apiReq.ToMap()
		require.NoError(t, err)
		assert.Contains(t, body, "infinibandInterfaces")
		assert.Contains(t, body, "nvLinkInterfaces")
		assert.Contains(t, body, "dpuExtensionServiceDeployments")
		assert.Contains(t, body, "labels")
	})
}

func TestCollectAutoCreatedSSHKeyGroupIDs(t *testing.T) {
	t.Run("returns only groups matching instance naming convention", func(t *testing.T) {
		instance := standard.NewInstance()
		instance.SetName("web")
		auto := standard.NewSshKeyGroup()
		auto.SetId("skg-auto")
		auto.SetName(GetInstanceSshKeyGroupName("web"))
		manual := standard.NewSshKeyGroup()
		manual.SetId("skg-manual")
		manual.SetName("shared-ops-keys")
		instance.SetSshKeyGroups([]standard.SshKeyGroup{*auto, *manual})

		assert.Equal(t, []string{"skg-auto"}, collectAutoCreatedSSHKeyGroupIDs(instance))
	})

	t.Run("returns nil when instance has no matching groups", func(t *testing.T) {
		instance := standard.NewInstance()
		instance.SetName("web")
		manual := standard.NewSshKeyGroup()
		manual.SetId("skg-manual")
		manual.SetName("shared-ops-keys")
		instance.SetSshKeyGroups([]standard.SshKeyGroup{*manual})

		assert.Nil(t, collectAutoCreatedSSHKeyGroupIDs(instance))
	})

	t.Run("returns nil for nil instance", func(t *testing.T) {
		assert.Nil(t, collectAutoCreatedSSHKeyGroupIDs(nil))
	})
}

func TestFilterOutIDs(t *testing.T) {
	exclude := map[string]struct{}{"b": {}}
	assert.Equal(t, []string{"a", "c"}, filterOutIDs([]string{"a", "b", "c"}, exclude))
	assert.Equal(t, []string{"a"}, filterOutIDs([]string{"a"}, nil))
	assert.Nil(t, filterOutIDs(nil, exclude))
}

func TestInstanceManager_GetInstances(t *testing.T) {
	instanceName := "host-1"
	ipAddress := "10.0.0.5"
	query := "02c241b7-d2e3-4bf5-8384-07137f4f3410"
	explicitVpcID := "vpc-2"
	pageNumber := 2
	pageSize := 25
	orderBy := "NAME_ASC"

	tests := []struct {
		name         string
		defaultVpcID string
		filter       *InstanceFilter
		pagination   *PaginationFilter
		wantQuery    url.Values
	}{
		{
			name:         "query uses the default VPC",
			defaultVpcID: "vpc-1",
			filter:       &InstanceFilter{Query: &query},
			wantQuery: url.Values{
				"pageNumber": {"1"},
				"query":      {query},
				"siteId":     {"site-1"},
				"vpcId":      {"vpc-1"},
			},
		},
		{
			name:         "query composes with filters and pagination",
			defaultVpcID: "vpc-1",
			filter: &InstanceFilter{
				Name:      &instanceName,
				Query:     &query,
				VpcID:     &explicitVpcID,
				IPAddress: &ipAddress,
			},
			pagination: &PaginationFilter{
				PageNumber: &pageNumber,
				PageSize:   &pageSize,
				OrderBy:    &orderBy,
			},
			wantQuery: url.Values{
				"ipAddress":  {ipAddress},
				"name":       {instanceName},
				"orderBy":    {orderBy},
				"pageNumber": {"2"},
				"pageSize":   {"25"},
				"query":      {query},
				"siteId":     {"site-1"},
				"vpcId":      {explicitVpcID},
			},
		},
		{
			name:   "nil filter omits query",
			filter: nil,
			wantQuery: url.Values{
				"pageNumber": {"1"},
				"siteId":     {"site-1"},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, http.MethodGet, r.Method)
				assert.Equal(t, "/v2/org/test-org/nico/instance", r.URL.Path)
				assert.Equal(t, tt.wantQuery, r.URL.Query())

				w.Header().Set("Content-Type", "application/json")
				w.Header().Set("x-pagination", `{"pageNumber":1,"pageSize":20,"total":1}`)
				_, _ = io.WriteString(w, `[{"id":"instance-1","name":"host-1"}]`)
			}))
			defer server.Close()

			client := newSimpleTestClient(server.URL)
			client.apiMetadata.VpcID = tt.defaultVpcID
			instances, pagination, apiErr := NewInstanceManager(client).GetInstances(context.Background(), tt.filter, tt.pagination)

			require.Nil(t, apiErr)
			require.Len(t, instances, 1)
			assert.Equal(t, "instance-1", instances[0].GetId())
			require.NotNil(t, pagination)
			assert.Equal(t, 1, pagination.Total)
		})
	}
}

func TestInstanceManagerDeleteCleansUpAutoCreatedSSHKeyGroup(t *testing.T) {
	deletedSSHKeyGroups := []string{}
	listedInstances := false
	requestOrder := []string{}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/test-org/nico/instance/inst-1":
			_, _ = io.WriteString(w, `{
				"id":"inst-1",
				"name":"web",
				"sshKeyGroupIds":["skg-auto","skg-manual"],
				"sshKeyGroups":[
					{"id":"skg-auto","name":"web-ssh-key-group"},
					{"id":"skg-manual","name":"shared-ops-keys"}
				]
			}`)
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/test-org/nico/instance":
			listedInstances = true
			w.Header().Set("x-pagination", `{"pageNumber":1,"pageSize":100,"total":1}`)
			_, _ = io.WriteString(w, `[{
				"id":"inst-1",
				"name":"web",
				"sshKeyGroupIds":["skg-auto","skg-manual"],
				"sshKeyGroups":[
					{"id":"skg-auto","name":"web-ssh-key-group"},
					{"id":"skg-manual","name":"shared-ops-keys"}
				]
			}]`)
		case r.Method == http.MethodDelete && r.URL.Path == "/v2/org/test-org/nico/instance/inst-1":
			requestOrder = append(requestOrder, "instance")
			w.WriteHeader(http.StatusAccepted)
			_, _ = io.WriteString(w, `{"message":"accepted"}`)
		case r.Method == http.MethodDelete && r.URL.Path == "/v2/org/test-org/nico/sshkeygroup/skg-auto":
			deletedSSHKeyGroups = append(deletedSSHKeyGroups, "skg-auto")
			requestOrder = append(requestOrder, "skg-auto")
			w.WriteHeader(http.StatusInternalServerError)
			_, _ = io.WriteString(w, `{"message":"cleanup failed"}`)
		case r.Method == http.MethodDelete && strings.HasPrefix(r.URL.Path, "/v2/org/test-org/nico/sshkeygroup/"):
			t.Errorf("unexpected SSH Key Group delete: %s", r.URL.Path)
			http.NotFound(w, r)
		default:
			t.Errorf("unexpected request: %s %s", r.Method, r.URL.Path)
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	var logs bytes.Buffer
	logger := zerolog.New(&logs)
	client := newSimpleTestClient(server.URL)
	client.Logger = &logger
	apiErr := NewInstanceManager(client).Delete(context.Background(), "inst-1")
	require.Nil(t, apiErr)
	assert.True(t, listedInstances, "expected a site-scoped instance list to check shared SSH Key Groups")
	assert.Equal(t, []string{"skg-auto"}, deletedSSHKeyGroups)
	assert.Equal(t, []string{"instance", "skg-auto"}, requestOrder)
	assert.Contains(t, logs.String(), `"level":"warn"`)
	assert.Contains(t, logs.String(), `"sshKeyGroupId":"skg-auto"`)
}

func TestInstanceManagerDeleteSkipsSharedAutoCreatedSSHKeyGroup(t *testing.T) {
	deletedSSHKeyGroups := []string{}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/test-org/nico/instance/inst-1":
			_, _ = io.WriteString(w, `{
				"id":"inst-1",
				"name":"web",
				"sshKeyGroupIds":["skg-auto"],
				"sshKeyGroups":[{"id":"skg-auto","name":"web-ssh-key-group"}]
			}`)
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/test-org/nico/instance":
			w.Header().Set("x-pagination", `{"pageNumber":1,"pageSize":100,"total":2}`)
			_, _ = io.WriteString(w, `[
				{
					"id":"inst-1",
					"name":"web",
					"sshKeyGroupIds":["skg-auto"],
					"sshKeyGroups":[{"id":"skg-auto","name":"web-ssh-key-group"}]
				},
				{
					"id":"inst-2",
					"name":"db",
					"sshKeyGroupIds":["skg-auto"],
					"sshKeyGroups":[{"id":"skg-auto","name":"web-ssh-key-group"}]
				}
			]`)
		case r.Method == http.MethodDelete && r.URL.Path == "/v2/org/test-org/nico/instance/inst-1":
			w.WriteHeader(http.StatusAccepted)
			_, _ = io.WriteString(w, `{"message":"accepted"}`)
		case r.Method == http.MethodDelete && strings.HasPrefix(r.URL.Path, "/v2/org/test-org/nico/sshkeygroup/"):
			deletedSSHKeyGroups = append(deletedSSHKeyGroups, strings.TrimPrefix(r.URL.Path, "/v2/org/test-org/nico/sshkeygroup/"))
			w.WriteHeader(http.StatusAccepted)
			_, _ = io.WriteString(w, `{"message":"accepted"}`)
		default:
			t.Errorf("unexpected request: %s %s", r.Method, r.URL.Path)
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := newSimpleTestClient(server.URL)
	apiErr := NewInstanceManager(client).Delete(context.Background(), "inst-1")
	require.Nil(t, apiErr)
	assert.Empty(t, deletedSSHKeyGroups, "shared auto-created SSH Key Group must not be deleted")
}

func TestInstanceManagerDeleteWarnsWhenInstanceLookupFails(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/test-org/nico/instance/inst-1":
			http.Error(w, `{"message":"lookup failed"}`, http.StatusInternalServerError)
		case r.Method == http.MethodDelete && r.URL.Path == "/v2/org/test-org/nico/instance/inst-1":
			w.WriteHeader(http.StatusAccepted)
			_, _ = io.WriteString(w, `{"message":"accepted"}`)
		default:
			t.Errorf("unexpected request: %s %s", r.Method, r.URL.Path)
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	var logs bytes.Buffer
	logger := zerolog.New(&logs)
	client := newSimpleTestClient(server.URL)
	client.Logger = &logger

	apiErr := NewInstanceManager(client).Delete(context.Background(), "inst-1")
	require.Nil(t, apiErr)

	var logEntry map[string]interface{}
	require.NoError(t, json.Unmarshal(logs.Bytes(), &logEntry))
	assert.Equal(t, "warn", logEntry["level"])
	assert.Equal(t, "inst-1", logEntry["instanceId"])
	assert.Contains(t, logEntry["error"], "Code: 500")
	assert.Contains(t, logEntry["error"], "lookup failed")
	assert.Equal(t, "failed to get Instance; skipping SSH Key Group cleanup", logEntry["message"])
}

func newSimpleTestClient(baseURL string) *Client {
	apiConfig := standard.NewConfiguration()
	apiConfig.Servers = standard.ServerConfigurations{
		{URL: baseURL, Description: "test"},
	}
	apiConfig.SetAPIName("nico")
	return &Client{
		Config: ClientConfig{
			BaseURL: baseURL,
			Org:     "test-org",
			APIName: "nico",
			Token:   "test-token",
			Logger:  NewNoOpLogger(),
		},
		apiClient: standard.NewAPIClient(apiConfig),
		apiMetadata: ApiMetadata{
			Organization: "test-org",
			SiteID:       "site-1",
			TenantID:     "tenant-1",
		},
		Logger: NewNoOpLogger(),
	}
}
