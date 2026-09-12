// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSession_fetchTenantIPBlocks(t *testing.T) {
	providerRequests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v2/org/acme/nico/infrastructure-provider/current":
			providerRequests++
			_, _ = io.WriteString(w, `{"id":"provider-a"}`)
		case "/v2/org/acme/nico/tenant/current":
			_, _ = io.WriteString(w, `{"id":"tenant-a"}`)
		case "/v2/org/acme/nico/ipblock":
			assert.Equal(t, "site-a", r.URL.Query().Get("siteId"))
			assert.Equal(t, "tenant-a", r.URL.Query().Get("tenantId"))
			assert.Empty(t, r.URL.Query().Get("infrastructureProviderId"))
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `[{"id":"provider-id","name":"provider","siteId":"site-a","status":"Ready","tenantId":null,"protocolVersion":"IPv4"},{"id":"other-tenant-id","name":"other tenant","siteId":"site-a","status":"Ready","tenantId":"tenant-b","protocolVersion":"IPv4"},{"id":"tenant-id","name":"tenant","siteId":"site-a","status":"Ready","tenantId":"tenant-a","protocolVersion":"IPv4"}]`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	session := NewSession(client, "acme", "")
	session.Scope.SiteID = "site-a"
	ctx := context.Background()

	providerID, err := session.getInfrastructureProviderID(ctx)
	require.NoError(t, err)
	assert.Equal(t, "provider-a", providerID)

	items, tenantID, err := session.fetchTenantIPBlocks(ctx)

	require.NoError(t, err)
	assert.Equal(t, "tenant-a", tenantID)
	assert.Equal(t, 1, providerRequests)
	require.Len(t, items, 3)
	assert.Empty(t, items[0].Extra["tenantId"])
	assert.Equal(t, "tenant-b", items[1].Extra["tenantId"])
	assert.Equal(t, "tenant-a", items[2].Extra["tenantId"])
	assert.Equal(t, "IPv4", items[2].Extra["protocolVersion"])

	selectItems := buildIPBlockSelectItems(items, tenantID)
	require.Len(t, selectItems, 2)
	assert.Equal(t, "tenant-id", selectItems[0].ID)
	assert.Equal(t, ipBlockManualEntrySentinel, selectItems[1].ID)
}

type specializedRequestSnapshot struct {
	method        string
	path          string
	query         string
	authorization string
	accept        string
	contentType   string
	body          string
}

func TestSpecializedCommands_ReadOnlyRequestsPreserveSessionBehavior(t *testing.T) {
	tests := []struct {
		name           string
		command        string
		args           []string
		response       string
		expectedPath   string
		expectedQuery  []string
		expectedOutput []string
		seed           func(*Session)
	}{
		{
			name:         "singleton get",
			command:      "metadata get",
			response:     `{"version":"test-version"}`,
			expectedPath: "/v2/org/acme/custom-api/metadata",
			expectedOutput: []string{
				"metadata get",
				`"version": "test-version"`,
			},
		},
		{
			name:          "paginated resource list",
			command:       "site list",
			response:      `[{"name":"Site One","id":"site-1","status":"Ready"}]`,
			expectedPath:  "/v2/org/acme/custom-api/site",
			expectedQuery: []string{"pageNumber=1", "pageSize=100"},
			expectedOutput: []string{
				"site list",
				"Site One",
				"site-1",
			},
		},
		{
			name:         "named resource get",
			command:      "machine get",
			args:         []string{"host-one"},
			response:     `{"id":"machine-1","status":"Ready"}`,
			expectedPath: "/v2/org/acme/custom-api/machine/machine-1",
			expectedOutput: []string{
				"machine get machine-1",
				`"id": "machine-1"`,
			},
			seed: func(s *Session) {
				s.Cache.Set("machine", []NamedItem{{Name: "host-one", ID: "machine-1"}})
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var requests atomic.Int32
			var mu sync.Mutex
			var got specializedRequestSnapshot
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests.Add(1)
				body, _ := io.ReadAll(r.Body)
				mu.Lock()
				got = specializedRequestSnapshot{
					method:        r.Method,
					path:          r.URL.Path,
					query:         r.URL.RawQuery,
					authorization: r.Header.Get("Authorization"),
					accept:        r.Header.Get("Accept"),
					contentType:   r.Header.Get("Content-Type"),
					body:          string(body),
				}
				mu.Unlock()
				w.Header().Set("Content-Type", "application/json")
				_, _ = io.WriteString(w, test.response)
			}))
			defer server.Close()

			client := appcli.NewClient(server.URL, "acme", "specialized-token", nil, false)
			client.APIName = "custom-api"
			session := NewSession(client, "acme", "/tmp/nicocli.yaml")
			if test.seed != nil {
				test.seed(session)
			}

			var runErr error
			output := captureStdout(func() {
				runErr = specializedRegressionCommand(t, test.command).Run(session, test.args)
			})

			require.NoError(t, runErr)
			mu.Lock()
			gotSnapshot := got
			mu.Unlock()
			assert.Equal(t, int32(1), requests.Load())
			assert.Equal(t, http.MethodGet, gotSnapshot.method)
			assert.Equal(t, test.expectedPath, gotSnapshot.path)
			assert.Equal(t, "Bearer specialized-token", gotSnapshot.authorization)
			assert.Equal(t, "application/json", gotSnapshot.accept)
			assert.Empty(t, gotSnapshot.contentType)
			assert.Empty(t, gotSnapshot.body)
			if len(test.expectedQuery) == 0 {
				assert.Empty(t, gotSnapshot.query)
			} else {
				for _, queryPart := range test.expectedQuery {
					assert.Contains(t, gotSnapshot.query, queryPart)
				}
			}
			for _, outputPart := range test.expectedOutput {
				assert.Contains(t, output, outputPart)
			}
		})
	}
}

func TestSpecializedCommands_MutationsRequireConfirmation(t *testing.T) {
	tests := []struct {
		name                 string
		command              string
		args                 []string
		input                string
		status               int
		response             string
		expectedCalls        int32
		expectedMethod       string
		expectedPath         string
		expectedBody         string
		expectedPrompt       string
		expectResourceCached bool
		seed                 func(*Session)
	}{
		{
			name:           "confirmed delete sends request",
			command:        "vpc delete",
			args:           []string{"vpc-one"},
			input:          "y\n",
			status:         http.StatusNoContent,
			expectedCalls:  1,
			expectedMethod: http.MethodDelete,
			expectedPath:   "/v2/org/acme/nico/vpc/vpc-1",
			expectedPrompt: "Delete VPC vpc-one (vpc-1)?",
			seed: func(s *Session) {
				s.Cache.Set("vpc", []NamedItem{{Name: "vpc-one", ID: "vpc-1"}})
			},
		},
		{
			name:                 "cancelled delete sends no request",
			command:              "vpc delete",
			args:                 []string{"vpc-one"},
			input:                "n\n",
			status:               http.StatusNoContent,
			expectedCalls:        0,
			expectedPrompt:       "Delete VPC vpc-one (vpc-1)?",
			expectResourceCached: true,
			seed: func(s *Session) {
				s.Cache.Set("vpc", []NamedItem{{Name: "vpc-one", ID: "vpc-1"}})
			},
		},
		{
			name:           "confirmed task cancellation sends scoped body",
			command:        "rack task cancel",
			args:           []string{"task-9"},
			input:          "yes\n",
			status:         http.StatusOK,
			response:       `{"id":"task-9","status":"Cancelling"}`,
			expectedCalls:  1,
			expectedMethod: http.MethodPost,
			expectedPath:   "/v2/org/acme/nico/rack/task/task-9/cancel",
			expectedBody:   `{"siteId":"site-1"}`,
			expectedPrompt: "Cancel task task-9?",
			seed: func(s *Session) {
				s.Scope.SiteID = "site-1"
				s.Scope.SiteName = "Site One"
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var requests atomic.Int32
			var mu sync.Mutex
			var got specializedRequestSnapshot
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests.Add(1)
				body, _ := io.ReadAll(r.Body)
				mu.Lock()
				got = specializedRequestSnapshot{
					method:        r.Method,
					path:          r.URL.Path,
					authorization: r.Header.Get("Authorization"),
					contentType:   r.Header.Get("Content-Type"),
					body:          string(body),
				}
				mu.Unlock()
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(test.status)
				_, _ = io.WriteString(w, test.response)
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "specialized-token", nil, false),
				"acme",
				"",
			)
			if test.seed != nil {
				test.seed(session)
			}

			output, err := runSpecializedCommandWithInput(
				t,
				test.input,
				func() error {
					return specializedRegressionCommand(t, test.command).Run(session, test.args)
				},
			)

			require.NoError(t, err)
			mu.Lock()
			gotSnapshot := got
			mu.Unlock()
			assert.Equal(t, test.expectedCalls, requests.Load())
			assert.Contains(t, output, test.expectedPrompt)
			if test.expectedCalls > 0 {
				assert.Equal(t, test.expectedMethod, gotSnapshot.method)
				assert.Equal(t, test.expectedPath, gotSnapshot.path)
				assert.Equal(t, "Bearer specialized-token", gotSnapshot.authorization)
				if test.expectedBody == "" {
					assert.Empty(t, gotSnapshot.body)
					assert.Empty(t, gotSnapshot.contentType)
				} else {
					assert.JSONEq(t, test.expectedBody, gotSnapshot.body)
					assert.Equal(t, "application/json", gotSnapshot.contentType)
				}
			}
			if test.expectResourceCached {
				assert.NotNil(t, session.Cache.Get("vpc"))
			} else if test.command == "vpc delete" {
				assert.Nil(t, session.Cache.Get("vpc"))
			}
		})
	}
}

func TestPromptOperatingSystemTypePrefersTenantRole(t *testing.T) {
	var providerCalls atomic.Int32
	var tenantCalls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/v2/org/acme/nico/infrastructure-provider/current":
			providerCalls.Add(1)
			_, _ = io.WriteString(w, `{"id":"provider-1"}`)
		case "/v2/org/acme/nico/tenant/current":
			tenantCalls.Add(1)
			_, _ = io.WriteString(w, `{"id":"tenant-1"}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	session := NewSession(client, "acme", "")
	output, err := runSpecializedCommandWithInput(t, operatingSystemTypeImage+"\n", func() error {
		operatingSystemType, promptErr := promptOperatingSystemType(session, context.Background())
		assert.Equal(t, operatingSystemTypeImage, operatingSystemType)
		return promptErr
	})

	require.NoError(t, err)
	assert.Contains(t, output, "[iPXE/Image/Templated iPXE]")
	assert.Equal(t, int32(0), providerCalls.Load())
	assert.Equal(t, int32(1), tenantCalls.Load())
}

func TestPromptOperatingSystemTypeStopsOnUnexpectedTenantError(t *testing.T) {
	var providerCalls atomic.Int32
	var tenantCalls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/v2/org/acme/nico/infrastructure-provider/current":
			providerCalls.Add(1)
			_, _ = io.WriteString(w, `{"id":"provider-1"}`)
		case "/v2/org/acme/nico/tenant/current":
			tenantCalls.Add(1)
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = io.WriteString(w, `{"message":"unauthorized"}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	client := appcli.NewClient(server.URL, "acme", "token", nil, false)
	session := NewSession(client, "acme", "")
	_, err := promptOperatingSystemType(session, context.Background())

	require.Error(t, err)
	assert.ErrorContains(t, err, "determining operating system owner type")
	apiErr := &appcli.APIError{}
	require.ErrorAs(t, err, &apiErr)
	assert.Equal(t, http.StatusUnauthorized, apiErr.StatusCode)
	assert.Equal(t, int32(0), providerCalls.Load())
	assert.Equal(t, int32(1), tenantCalls.Load())
}

func TestCmdOSCreate(t *testing.T) {
	const (
		siteID         = "497f6eca-6276-4993-bfeb-53cbbbba6f08"
		templateID     = "6c2ac315-3040-4728-94eb-b66d320206c1"
		rootFSID       = "666c2eee-193d-42db-a490-4c444342bd4e"
		imageSHA       = "a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"
		createdOSReply = `{"id":"os-1","name":"created-os"}`
	)
	tests := []struct {
		name                      string
		provider                  bool
		tenantForbidden           bool
		input                     string
		expectedBody              string
		expectedSiteCalls         int32
		expectedTemplateCalls     int32
		templateRequiredParams    []string
		templateRequiredArtifacts []string
		expectedOutput            []string
		unexpectedOutput          []string
	}{
		{
			name:     "tenant raw iPXE prompts for script and shared options",
			provider: false,
			input: strings.Join([]string{
				"raw-ipxe-os",
				"Raw iPXE description",
				operatingSystemTypeIPXE,
				"#!ipxe",
				"#cloud-config",
				"y",
				"n",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"raw-ipxe-os",
				"description":"Raw iPXE description",
				"ipxeScript":"#!ipxe",
				"userData":"#cloud-config",
				"allowOverride":true,
				"phoneHomeEnabled":false
			}`,
			expectedOutput: []string{
				"[iPXE/Image/Templated iPXE]",
				"iPXE script or URL",
				"User data (optional)",
			},
			unexpectedOutput: []string{
				"Site One",
				"Image URL",
			},
		},
		{
			name:     "tenant templated iPXE selects site and template then shared options",
			provider: false,
			input: strings.Join([]string{
				"templated-ipxe-os",
				"Templated iPXE description",
				operatingSystemTypeTemplatedIPXE,
				"#cloud-config",
				"n",
				"y",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"templated-ipxe-os",
				"description":"Templated iPXE description",
				"siteIds":["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
				"ipxeTemplateId":"6c2ac315-3040-4728-94eb-b66d320206c1",
				"userData":"#cloud-config",
				"allowOverride":false,
				"phoneHomeEnabled":true
			}`,
			expectedSiteCalls:     1,
			expectedTemplateCalls: 1,
			expectedOutput: []string{
				"[iPXE/Image/Templated iPXE]",
				"Site One",
				"Ubuntu Template",
				"User data (optional)",
			},
			unexpectedOutput: []string{
				"iPXE script or URL",
				"Image URL",
			},
		},
		{
			name:     "tenant templated iPXE collects required parameters and artifacts",
			provider: false,
			input: strings.Join([]string{
				"templated-ipxe-requirements-os",
				"",
				operatingSystemTypeTemplatedIPXE,
				"",
				"console=ttyS0",
				"install",
				"https://example.com/kernel",
				imageSHA,
				authTypeBearer,
				"",
				"artifact-secret",
				artifactCacheStrategyAsNeeded,
				"https://example.com/initrd",
				"",
				authTypeNone,
				artifactCacheStrategyRemoteOnly,
				"",
				"n",
				"y",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"templated-ipxe-requirements-os",
				"siteIds":["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
				"ipxeTemplateId":"6c2ac315-3040-4728-94eb-b66d320206c1",
				"ipxeTemplateParameters":[
					{"name":"kernel_args","value":"console=ttyS0"},
					{"name":"mode","value":"install"}
				],
				"ipxeTemplateArtifacts":[
					{
						"name":"kernel",
						"url":"https://example.com/kernel",
						"sha":"a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980",
						"authType":"Bearer",
						"authToken":"artifact-secret",
						"cacheStrategy":"CacheAsNeeded"
					},
					{
						"name":"initrd",
						"url":"https://example.com/initrd",
						"cacheStrategy":"RemoteOnly"
					}
				],
				"allowOverride":false,
				"phoneHomeEnabled":true
			}`,
			expectedSiteCalls:         1,
			expectedTemplateCalls:     1,
			templateRequiredParams:    []string{"kernel_args", "mode"},
			templateRequiredArtifacts: []string{"kernel", "initrd"},
			expectedOutput: []string{
				"Value for parameter kernel_args",
				"Value for parameter mode",
				"URL for artifact kernel",
				"SHA for artifact kernel (optional)",
				"Auth type for artifact kernel",
				"Auth token for artifact kernel",
				"Cache strategy for artifact kernel",
				"URL for artifact initrd",
				"SHA for artifact initrd (optional)",
				"Auth type for artifact initrd",
				"Cache strategy for artifact initrd",
				"(required)",
				`"name":"templated-ipxe-requirements-os"`,
				`"authToken":"<redacted>"`,
				`"cacheStrategy":"CacheAsNeeded"`,
			},
			unexpectedOutput: []string{
				"Auth token for artifact initrd",
				"artifact-secret",
				"--data '<redacted>'",
			},
		},
		{
			name:            "provider defaults to templated iPXE",
			provider:        true,
			tenantForbidden: true,
			input: strings.Join([]string{
				"provider-template-os",
				"",
				"",
				"y",
				"n",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"provider-template-os",
				"siteIds":["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
				"ipxeTemplateId":"6c2ac315-3040-4728-94eb-b66d320206c1",
				"allowOverride":true,
				"phoneHomeEnabled":false
			}`,
			expectedSiteCalls:     1,
			expectedTemplateCalls: 1,
			expectedOutput: []string{
				"default for providers",
				"Site One",
				"Ubuntu Template",
				"User data (optional)",
			},
			unexpectedOutput: []string{
				"[iPXE/Image/Templated iPXE]",
				"iPXE script or URL",
				"Image URL",
			},
		},
		{
			name:     "tenant image selects root filesystem ID",
			provider: false,
			input: strings.Join([]string{
				"image-id-os",
				"Image description",
				operatingSystemTypeImage,
				"https://example.com/images/os.qcow2",
				imageSHA,
				rootFilesystemTypeID,
				rootFSID,
				authTypeNone,
				"",
				"hostname: image-host",
				"y",
				"y",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"image-id-os",
				"description":"Image description",
				"siteIds":["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
				"imageUrl":"https://example.com/images/os.qcow2",
				"imageSha":"a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980",
				"rootFsId":"666c2eee-193d-42db-a490-4c444342bd4e",
				"userData":"hostname: image-host",
				"allowOverride":true,
				"phoneHomeEnabled":true
			}`,
			expectedSiteCalls: 1,
			expectedOutput: []string{
				"Site One",
				"Image URL",
				"Image SHA",
				"Specify root filesystem by",
				"Root filesystem ID",
				"Image authentication type",
				"Image disk (optional)",
				"User data (optional)",
				"Allow override at instance creation?",
				"Enable phone home?",
			},
			unexpectedOutput: []string{
				"Root filesystem label",
				"Image auth token",
			},
		},
		{
			name:     "tenant image selects root filesystem label",
			provider: false,
			input: strings.Join([]string{
				"image-label-os",
				"",
				operatingSystemTypeImage,
				"https://example.com/images/os.qcow2",
				imageSHA,
				rootFilesystemTypeLabel,
				"rootfs",
				authTypeNone,
				"",
				"",
				"n",
				"n",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"image-label-os",
				"siteIds":["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
				"imageUrl":"https://example.com/images/os.qcow2",
				"imageSha":"a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980",
				"rootFsLabel":"rootfs",
				"allowOverride":false,
				"phoneHomeEnabled":false
			}`,
			expectedSiteCalls: 1,
			expectedOutput: []string{
				"Site One",
				"Root filesystem label",
				"Image authentication type",
				"Image disk (optional)",
				"User data (optional)",
			},
			unexpectedOutput: []string{
				"Root filesystem ID",
				"Image auth token",
			},
		},
		{
			name:     "tenant image requires Basic auth token and accepts image disk",
			provider: false,
			input: strings.Join([]string{
				"image-basic-os",
				"",
				operatingSystemTypeImage,
				"https://example.com/images/os.qcow2",
				imageSHA,
				rootFilesystemTypeID,
				rootFSID,
				authTypeBasic,
				"",
				"basic-secret",
				"/dev/sda",
				"",
				"n",
				"n",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"image-basic-os",
				"siteIds":["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
				"imageUrl":"https://example.com/images/os.qcow2",
				"imageSha":"a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980",
				"rootFsId":"666c2eee-193d-42db-a490-4c444342bd4e",
				"imageAuthType":"Basic",
				"imageAuthToken":"basic-secret",
				"imageDisk":"/dev/sda",
				"allowOverride":false,
				"phoneHomeEnabled":false
			}`,
			expectedSiteCalls: 1,
			expectedOutput: []string{
				"Image authentication type",
				"Image auth token",
				"(required)",
				"Image disk (optional)",
				`"name":"image-basic-os"`,
				`"imageAuthToken":"<redacted>"`,
				`"imageDisk":"/dev/sda"`,
			},
			unexpectedOutput: []string{
				"basic-secret",
				"--data '<redacted>'",
			},
		},
		{
			name:     "tenant image accepts Bearer auth without image disk",
			provider: false,
			input: strings.Join([]string{
				"image-bearer-os",
				"",
				operatingSystemTypeImage,
				"https://example.com/images/os.qcow2",
				imageSHA,
				rootFilesystemTypeLabel,
				"rootfs",
				authTypeBearer,
				"bearer-secret",
				"",
				"",
				"n",
				"n",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"image-bearer-os",
				"siteIds":["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
				"imageUrl":"https://example.com/images/os.qcow2",
				"imageSha":"a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980",
				"rootFsLabel":"rootfs",
				"imageAuthType":"Bearer",
				"imageAuthToken":"bearer-secret",
				"allowOverride":false,
				"phoneHomeEnabled":false
			}`,
			expectedSiteCalls: 1,
			expectedOutput: []string{
				"Image authentication type",
				"Image auth token",
				"Image disk (optional)",
				`"name":"image-bearer-os"`,
				`"imageAuthToken":"<redacted>"`,
				`"rootFsLabel":"rootfs"`,
			},
			unexpectedOutput: []string{
				"bearer-secret",
				"--data '<redacted>'",
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var providerCalls atomic.Int32
			var tenantCalls atomic.Int32
			var siteCalls atomic.Int32
			var templateCalls atomic.Int32
			var createCalls atomic.Int32
			createdBodies := make(chan string, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				switch {
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/infrastructure-provider/current":
					providerCalls.Add(1)
					if test.provider {
						_, _ = io.WriteString(w, `{"id":"provider-1"}`)
						return
					}
					w.WriteHeader(http.StatusForbidden)
					_, _ = io.WriteString(w, `{"message":"not a provider"}`)
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/tenant/current":
					tenantCalls.Add(1)
					if test.tenantForbidden {
						w.WriteHeader(http.StatusForbidden)
						_, _ = io.WriteString(w, `{"message":"not a tenant"}`)
						return
					}
					_, _ = io.WriteString(w, `{"id":"tenant-1"}`)
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/site":
					siteCalls.Add(1)
					_, _ = io.WriteString(w, `[{"id":"`+siteID+`","name":"Site One","status":"Registered"}]`)
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/ipxe-template":
					templateCalls.Add(1)
					assert.Equal(t, siteID, r.URL.Query().Get("siteId"))
					templateResponse := map[string]interface{}{
						"id":                templateID,
						"name":              "Ubuntu Template",
						"visibility":        "Public",
						"requiredParams":    test.templateRequiredParams,
						"requiredArtifacts": test.templateRequiredArtifacts,
					}
					encodeErr := json.NewEncoder(w).Encode([]map[string]interface{}{templateResponse})
					require.NoError(t, encodeErr)
				case r.Method == http.MethodPost && r.URL.Path == "/v2/org/acme/nico/operating-system":
					createCalls.Add(1)
					requestBody, readErr := io.ReadAll(r.Body)
					if readErr != nil {
						http.Error(w, readErr.Error(), http.StatusInternalServerError)
						return
					}
					createdBodies <- string(requestBody)
					w.WriteHeader(http.StatusCreated)
					_, _ = io.WriteString(w, createdOSReply)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			output, err := runSpecializedCommandWithInput(t, test.input, func() error {
				return cmdOSCreate(session, nil)
			})

			require.NoError(t, err)
			assert.Equal(t, int32(1), tenantCalls.Load())
			if test.tenantForbidden {
				assert.Equal(t, int32(1), providerCalls.Load())
			} else {
				assert.Equal(t, int32(0), providerCalls.Load())
			}
			assert.Equal(t, test.expectedSiteCalls, siteCalls.Load())
			assert.Equal(t, test.expectedTemplateCalls, templateCalls.Load())
			assert.Equal(t, int32(1), createCalls.Load())
			assert.JSONEq(t, test.expectedBody, <-createdBodies)
			assert.Contains(t, output, "Operating system created: created-os (os-1)")
			for _, expected := range test.expectedOutput {
				assert.Contains(t, output, expected)
			}
			for _, unexpected := range test.unexpectedOutput {
				assert.NotContains(t, output, unexpected)
			}
		})
	}
}

func TestCmdOSUpdatePromptsForTypeSpecificFields(t *testing.T) {
	const (
		operatingSystemID = "9c8f3d62-7c95-4d1c-9f91-924a7e0f31c4"
		templateID        = "6c2ac315-3040-4728-94eb-b66d320206c1"
	)
	tests := []struct {
		name                  string
		osType                string
		templateID            string
		input                 string
		expectedBody          string
		expectedTemplateCalls int32
		expectedOutput        []string
		unexpectedOutput      []string
	}{
		{
			name:   "raw iPXE offers script update only",
			osType: operatingSystemTypeIPXE,
			input: strings.Join([]string{
				"",
				"",
				"#!ipxe updated",
				"",
				"",
				"",
				"",
			}, "\n") + "\n",
			expectedBody: `{
				"ipxeScript":"#!ipxe updated"
			}`,
			expectedOutput: []string{
				"iPXE script or URL (optional)",
			},
			unexpectedOutput: []string{
				"Update image authentication?",
				"Update iPXE template parameters?",
			},
		},
		{
			name:   "boolean updates accept yes and no while blank keeps the field",
			osType: operatingSystemTypeIPXE,
			input: strings.Join([]string{
				"",
				"",
				"",
				"",
				"y",
				"n",
				"",
			}, "\n") + "\n",
			expectedBody: `{
				"allowOverride":true,
				"phoneHomeEnabled":false
			}`,
			expectedOutput: []string{
				"Allow override?",
				"Phone home enabled?",
				"Set active?",
				"[y/n, blank to keep]",
			},
			unexpectedOutput: []string{
				`"isActive"`,
			},
		},
		{
			name:   "image offers supported image-specific update fields",
			osType: operatingSystemTypeImage,
			input: strings.Join([]string{
				"",
				"",
				"y",
				authTypeBasic,
				"rotated-token",
				"y",
				"/dev/sdb",
				"",
				"",
				"",
				"",
			}, "\n") + "\n",
			expectedBody: `{
				"imageAuthType":"Basic",
				"imageAuthToken":"rotated-token",
				"imageDisk":"/dev/sdb"
			}`,
			expectedOutput: []string{
				"Update image authentication?",
				"Image authentication type",
				"Image auth token",
				"Update image disk?",
				"Image disk (blank to clear)",
				`"imageAuthToken":"<redacted>"`,
			},
			unexpectedOutput: []string{
				"iPXE script or URL (optional)",
				"Update iPXE template parameters?",
				"Update root filesystem?",
				"Specify root filesystem by",
				"Root filesystem ID",
				"Root filesystem label",
				"rotated-token",
				"--data '<redacted>'",
			},
		},
		{
			name:       "templated iPXE offers template parameters and artifacts",
			osType:     operatingSystemAPITypeTemplatedIPXE,
			templateID: templateID,
			input: strings.Join([]string{
				"",
				"",
				"y",
				"https://example.com/kernel",
				"y",
				"https://example.com/initrd",
				"",
				authTypeNone,
				artifactCacheStrategyRemoteOnly,
				"",
				"",
				"",
				"",
			}, "\n") + "\n",
			expectedBody: `{
				"ipxeTemplateParameters":[
					{"name":"kernel_url","value":"https://example.com/kernel"}
				],
				"ipxeTemplateArtifacts":[
					{
						"name":"initrd",
						"url":"https://example.com/initrd",
						"cacheStrategy":"RemoteOnly"
					}
				]
			}`,
			expectedTemplateCalls: 1,
			expectedOutput: []string{
				"Update iPXE template parameters?",
				"Value for parameter kernel_url",
				"Update iPXE template artifacts?",
				"URL for artifact initrd",
				"Cache strategy for artifact initrd",
			},
			unexpectedOutput: []string{
				"iPXE script or URL (optional)",
				"Update image authentication?",
			},
		},
		{
			name:       "templated iPXE updates parameters independently",
			osType:     operatingSystemAPITypeTemplatedIPXE,
			templateID: templateID,
			input: strings.Join([]string{
				"",
				"",
				"y",
				"https://example.com/kernel",
				"n",
				"",
				"",
				"",
				"",
			}, "\n") + "\n",
			expectedBody: `{
				"ipxeTemplateParameters":[
					{"name":"kernel_url","value":"https://example.com/kernel"}
				]
			}`,
			expectedTemplateCalls: 1,
			expectedOutput: []string{
				"Update iPXE template parameters?",
				"Value for parameter kernel_url",
				"Update iPXE template artifacts?",
			},
			unexpectedOutput: []string{
				"URL for artifact initrd",
				`"ipxeTemplateArtifacts"`,
			},
		},
		{
			name:       "templated iPXE updates artifacts independently",
			osType:     operatingSystemAPITypeTemplatedIPXE,
			templateID: templateID,
			input: strings.Join([]string{
				"",
				"",
				"n",
				"y",
				"https://example.com/initrd",
				"",
				authTypeNone,
				artifactCacheStrategyRemoteOnly,
				"",
				"",
				"",
				"",
			}, "\n") + "\n",
			expectedBody: `{
				"ipxeTemplateArtifacts":[
					{
						"name":"initrd",
						"url":"https://example.com/initrd",
						"cacheStrategy":"RemoteOnly"
					}
				]
			}`,
			expectedTemplateCalls: 1,
			expectedOutput: []string{
				"Update iPXE template parameters?",
				"Update iPXE template artifacts?",
				"URL for artifact initrd",
				"Cache strategy for artifact initrd",
			},
			unexpectedOutput: []string{
				"Value for parameter kernel_url",
				`"ipxeTemplateParameters"`,
			},
		},
		{
			name:       "templated iPXE skips template lookup when updates are declined",
			osType:     operatingSystemAPITypeTemplatedIPXE,
			templateID: templateID,
			input: strings.Join([]string{
				"renamed-template-os",
				"",
				"n",
				"n",
				"",
				"",
				"",
				"",
			}, "\n") + "\n",
			expectedBody: `{
				"name":"renamed-template-os"
			}`,
			expectedOutput: []string{
				"Update iPXE template parameters?",
				"Update iPXE template artifacts?",
			},
			unexpectedOutput: []string{
				"Value for parameter kernel_url",
				"URL for artifact initrd",
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var templateCalls atomic.Int32
			var updateCalls atomic.Int32
			updatedBodies := make(chan string, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				switch {
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/operating-system":
					response := map[string]interface{}{
						"id":     operatingSystemID,
						"name":   "update-os",
						"status": "Ready",
						"type":   test.osType,
					}
					if test.templateID != "" {
						response["ipxeTemplateId"] = test.templateID
					}
					encodeErr := json.NewEncoder(w).Encode([]map[string]interface{}{response})
					require.NoError(t, encodeErr)
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/ipxe-template":
					templateCalls.Add(1)
					assert.Empty(t, r.URL.Query().Get("siteId"))
					response := map[string]interface{}{
						"id":                templateID,
						"name":              "Ubuntu Template",
						"visibility":        "Public",
						"requiredParams":    []string{"kernel_url"},
						"requiredArtifacts": []string{"initrd"},
					}
					encodeErr := json.NewEncoder(w).Encode([]map[string]interface{}{response})
					require.NoError(t, encodeErr)
				case r.Method == http.MethodPatch && r.URL.Path == "/v2/org/acme/nico/operating-system/"+operatingSystemID:
					updateCalls.Add(1)
					requestBody, readErr := io.ReadAll(r.Body)
					require.NoError(t, readErr)
					updatedBodies <- string(requestBody)
					_, _ = io.WriteString(w, `{"id":"`+operatingSystemID+`","name":"update-os"}`)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()

			session := NewSession(
				appcli.NewClient(server.URL, "acme", "token", nil, false),
				"acme",
				"",
			)
			output, err := runSpecializedCommandWithInput(t, test.input, func() error {
				return cmdOSUpdate(session, []string{"update-os"})
			})

			require.NoError(t, err)
			assert.Equal(t, test.expectedTemplateCalls, templateCalls.Load())
			assert.Equal(t, int32(1), updateCalls.Load())
			assert.JSONEq(t, test.expectedBody, <-updatedBodies)
			expectedLogBody, redactErr := redactAuthTokenJSON([]byte(test.expectedBody))
			require.NoError(t, redactErr)
			assert.Contains(t, output, "--data "+shellQuoteCLIArg(string(expectedLogBody)))
			assert.Contains(t, output, "Operating system updated: update-os ("+operatingSystemID+")")
			for _, expected := range test.expectedOutput {
				assert.Contains(t, output, expected)
			}
			for _, unexpected := range test.unexpectedOutput {
				assert.NotContains(t, output, unexpected)
			}
		})
	}
}

func TestRedactAuthTokenJSON(t *testing.T) {
	original := []byte(`{
		"name":"visible-name",
		"imageAuthToken":"image-secret",
		"unrelated":"image-secret",
		"ipxeTemplateArtifacts":[
			{
				"name":"kernel",
				"url":"https://example.com/kernel",
				"authType":"Bearer",
				"authToken":"artifact-secret"
			}
		]
	}`)

	redacted, err := redactAuthTokenJSON(original)
	require.NoError(t, err)
	assert.JSONEq(t, `{
		"name":"visible-name",
		"imageAuthToken":"<redacted>",
		"unrelated":"image-secret",
		"ipxeTemplateArtifacts":[
			{
				"name":"kernel",
				"url":"https://example.com/kernel",
				"authType":"Bearer",
				"authToken":"<redacted>"
			}
		]
	}`, string(redacted))
	assert.Contains(t, string(original), `"imageAuthToken":"image-secret"`)
	assert.Contains(t, string(original), `"authToken":"artifact-secret"`)

	redacted, err = redactAuthTokenJSON([]byte(`{"name":`))
	assert.Nil(t, redacted)
	assert.Error(t, err)
}

func TestSpecializedScopeSelection_ClearsDependentScopeAndCache(t *testing.T) {
	t.Run("site selection clears VPC and filtered resources", func(t *testing.T) {
		var requests atomic.Int32
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			requests.Add(1)
			assert.Equal(t, "/v2/org/acme/nico/site", r.URL.Path)
			_, _ = io.WriteString(w, `[{"name":"Site Two","id":"site-2","status":"Ready"}]`)
		}))
		defer server.Close()

		session := NewSession(
			appcli.NewClient(server.URL, "acme", "token", nil, false),
			"acme",
			"",
		)
		session.Scope = Scope{
			SiteID:   "site-1",
			SiteName: "Site One",
			VpcID:    "vpc-1",
			VpcName:  "VPC One",
		}
		session.Cache.Set("machine", []NamedItem{{Name: "host-one", ID: "machine-1"}})

		output := captureStdout(func() {
			runScopeSet(session, "site", "Site Two")
		})

		assert.Equal(t, int32(1), requests.Load())
		assert.Equal(t, "site-2", session.Scope.SiteID)
		assert.Equal(t, "Site Two", session.Scope.SiteName)
		assert.Empty(t, session.Scope.VpcID)
		assert.Empty(t, session.Scope.VpcName)
		assert.Nil(t, session.Cache.Get("machine"))
		assert.Contains(t, output, "Scope set: site =")
		assert.Contains(t, output, "Site Two")
	})

	t.Run("VPC selection infers its site", func(t *testing.T) {
		var requests atomic.Int32
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			requests.Add(1)
			assert.Equal(t, "/v2/org/acme/nico/vpc", r.URL.Path)
			_, _ = io.WriteString(w, `[{"name":"VPC One","id":"vpc-1","siteId":"site-1"}]`)
		}))
		defer server.Close()

		session := NewSession(
			appcli.NewClient(server.URL, "acme", "token", nil, false),
			"acme",
			"",
		)
		session.Cache.Set("site", []NamedItem{{Name: "Site One", ID: "site-1"}})
		session.Cache.Set("machine", []NamedItem{{Name: "host-one", ID: "machine-1"}})

		output := captureStdout(func() {
			runScopeSet(session, "vpc", "VPC One")
		})

		assert.Equal(t, int32(1), requests.Load())
		assert.Equal(t, "site-1", session.Scope.SiteID)
		assert.Equal(t, "Site One", session.Scope.SiteName)
		assert.Equal(t, "vpc-1", session.Scope.VpcID)
		assert.Equal(t, "VPC One", session.Scope.VpcName)
		assert.Nil(t, session.Cache.Get("machine"))
		assert.Contains(t, output, "Scope set: site =")
		assert.Contains(t, output, "Scope set: vpc =")
	})
}

func TestSpecializedCommand_AuthAndSecretOutputRemainRedacted(t *testing.T) {
	t.Run("debug request headers redact bearer token", func(t *testing.T) {
		var logs bytes.Buffer
		logger := logrus.New()
		logger.SetOutput(&logs)
		logger.SetFormatter(&logrus.TextFormatter{DisableTimestamp: true, DisableColors: true})

		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			_, _ = io.WriteString(w, `{"version":"test"}`)
		}))
		defer server.Close()

		const secretToken = "token-that-must-not-be-logged"
		client := appcli.NewClient(server.URL, "acme", secretToken, logrus.NewEntry(logger), true)
		session := NewSession(client, "acme", "")

		var runErr error
		_ = captureStdout(func() {
			runErr = specializedRegressionCommand(t, "metadata get").Run(session, nil)
		})

		require.NoError(t, runErr)
		assert.NotContains(t, logs.String(), secretToken)
		assert.Contains(t, logs.String(), "Bearer <redacted>")
	})

	t.Run("env mask hides configured token", func(t *testing.T) {
		const secretToken = "environment-token-that-must-not-be-printed"
		t.Setenv("NICO_TOKEN", secretToken)

		var runErr error
		output := captureStdout(func() {
			runErr = specializedRegressionCommand(t, "env").Run(nil, []string{"--mask"})
		})

		require.NoError(t, runErr)
		assert.Contains(t, output, "NICO_TOKEN")
		assert.Contains(t, output, "REDACTED")
		assert.NotContains(t, output, secretToken)
	})
}

func TestSpecializedCommand_StructuredAPIErrorsRemainActionable(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		assert.Equal(t, "/v2/org/acme/nico/metadata", r.URL.Path)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnprocessableEntity)
		_, _ = io.WriteString(w, `{"message":"metadata unavailable","data":{"field":"siteId"}}`)
	}))
	defer server.Close()

	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	var runErr error
	output := captureStdout(func() {
		runErr = specializedRegressionCommand(t, "metadata get").Run(session, nil)
	})

	require.Error(t, runErr)
	assert.Contains(t, runErr.Error(), "getting metadata")
	assert.Contains(t, runErr.Error(), "API error 422: metadata unavailable")
	assert.Contains(t, runErr.Error(), `"field":"siteId"`)
	assert.NotContains(t, output, "metadata unavailable")
}

func TestCmdInstanceUpdate_SendsAttributeOnlyPatch(t *testing.T) {
	var requestCount atomic.Int32
	requests := make(chan specializedRequestSnapshot, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestNumber := requestCount.Add(1)
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		snapshot := specializedRequestSnapshot{
			method: r.Method,
			path:   r.URL.Path,
			body:   string(body),
		}
		if requestNumber == 1 {
			requests <- snapshot
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"id":"instance-1","name":"new-name"}`)
	}))
	defer server.Close()

	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	session.Cache.Set("instance", []NamedItem{{Name: "instance-one", ID: "instance-1"}})
	session.Cache.Set("operating-system", []NamedItem{})

	output, runErr := runSpecializedCommandWithInput(t, "new-name\n\nn\n", func() error {
		return specializedRegressionCommand(t, "instance update").Run(session, []string{"instance-1"})
	})

	require.NoError(t, runErr)
	request := <-requests
	assert.Equal(t, int32(1), requestCount.Load())
	assert.Equal(t, http.MethodPatch, request.method)
	assert.Equal(t, "/v2/org/acme/nico/instance/instance-1", request.path)
	assert.JSONEq(t, `{"name":"new-name"}`, request.body)
	assert.Contains(t, output, "Instance updated: new-name (instance-1)")
}

func TestCmdVPCCreate(t *testing.T) {
	tests := []struct {
		name                   string
		routingProfileResponse string
		createResponse         string
		input                  string
		expectedBody           string
		expectedLog            string
		unexpectedLog          string
		expectedConfirmation   string
	}{
		{
			name:                   "sends a selected alternative profile",
			routingProfileResponse: `{"defaultRoutingProfile":"external","permittedRoutingProfiles":["external","internal"]}`,
			createResponse:         `{"id":"vpc-1","name":"profile-vpc","routingProfile":"internal"}`,
			input:                  "profile-vpc\n\ninternal\n",
			expectedBody:           `{"name":"profile-vpc","routingProfile":"internal","siteId":"site-1"}`,
			expectedLog:            "--routing-profile internal",
			expectedConfirmation:   "routing profile: internal",
		},
		{
			name:                   "reports the Core-resolved profile when the inherited default changes",
			routingProfileResponse: `{"defaultRoutingProfile":"external","permittedRoutingProfiles":["external"]}`,
			createResponse:         `{"id":"vpc-1","name":"profile-vpc","routingProfile":"internal"}`,
			input:                  "profile-vpc\n\n\n",
			expectedBody:           `{"name":"profile-vpc","siteId":"site-1"}`,
			unexpectedLog:          "--routing-profile",
			expectedConfirmation:   "routing profile: internal",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var mu sync.Mutex
			requests := []specializedRequestSnapshot{}
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				body, err := io.ReadAll(r.Body)
				require.NoError(t, err)
				mu.Lock()
				requests = append(requests, specializedRequestSnapshot{
					method: r.Method,
					path:   r.URL.Path,
					query:  r.URL.RawQuery,
					body:   string(body),
				})
				mu.Unlock()

				w.Header().Set("Content-Type", "application/json")
				switch r.URL.Path {
				case "/v2/org/acme/nico/tenant/current/routing-profile":
					_, _ = io.WriteString(w, test.routingProfileResponse)
				case "/v2/org/acme/nico/vpc":
					w.WriteHeader(http.StatusCreated)
					_, _ = io.WriteString(w, test.createResponse)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()

			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Cache.Set("site", []NamedItem{{
				Name: "native-site",
				ID:   "site-1",
				Raw:  map[string]interface{}{"capabilities": map[string]interface{}{"nativeNetworking": true}},
			}})

			output, runErr := runSpecializedCommandWithInput(t, test.input, func() error {
				return specializedRegressionCommand(t, "vpc create").Run(session, nil)
			})

			require.NoError(t, runErr)
			mu.Lock()
			got := append([]specializedRequestSnapshot(nil), requests...)
			mu.Unlock()
			require.Len(t, got, 2)
			assert.Equal(t, http.MethodGet, got[0].method)
			assert.Equal(t, "/v2/org/acme/nico/tenant/current/routing-profile", got[0].path)
			assert.Equal(t, "siteId=site-1", got[0].query)
			assert.Equal(t, http.MethodPost, got[1].method)
			assert.Equal(t, "/v2/org/acme/nico/vpc", got[1].path)
			assert.JSONEq(t, test.expectedBody, got[1].body)
			assert.Contains(t, output, "Routing profile (external (tenant default))")
			if test.expectedLog != "" {
				assert.Contains(t, output, test.expectedLog)
			}
			if test.unexpectedLog != "" {
				assert.NotContains(t, output, test.unexpectedLog)
			}
			assert.Contains(t, output, test.expectedConfirmation)
		})
	}
}

func specializedRegressionCommand(t *testing.T, name string) Command {
	t.Helper()
	for _, command := range AllCommands() {
		if command.Name == name {
			return command
		}
	}
	t.Fatalf("command %q not found", name)
	return Command{}
}

func runSpecializedCommandWithInput(t *testing.T, input string, run func() error) (string, error) {
	t.Helper()

	oldStdin := os.Stdin
	oldStdout := os.Stdout
	stdinReader, stdinWriter, err := os.Pipe()
	require.NoError(t, err)
	stdoutReader, stdoutWriter, err := os.Pipe()
	require.NoError(t, err)
	os.Stdin = stdinReader
	os.Stdout = stdoutWriter
	defer func() {
		_ = stdinReader.Close()
		_ = stdoutReader.Close()
		os.Stdin = oldStdin
		os.Stdout = oldStdout
	}()

	go func() {
		defer stdinWriter.Close()
		_, _ = io.WriteString(stdinWriter, input)
	}()

	var output bytes.Buffer
	readDone := make(chan error, 1)
	go func() {
		_, copyErr := io.Copy(&output, stdoutReader)
		readDone <- copyErr
	}()

	runErr := run()
	_ = stdoutWriter.Close()
	readErr := <-readDone
	require.NoError(t, readErr)
	return strings.TrimSpace(output.String()), runErr
}
