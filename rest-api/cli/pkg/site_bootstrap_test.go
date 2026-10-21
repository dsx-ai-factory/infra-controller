// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"maps"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSitePrerequisiteManifestValidation(t *testing.T) {
	valid := `
provider:
  org: provider-org
tenant:
  org: tenant-org
site:
  request:
    name: test-site
`

	tests := []struct {
		name      string
		manifest  string
		errString string
	}{
		{name: "valid", manifest: valid},
		{name: "tenant org required", manifest: strings.Replace(valid, "tenant-org", "", 1), errString: "tenant.org is required"},
		{name: "site required", manifest: strings.Replace(valid, "site:\n  request:\n    name: test-site\n", "", 1), errString: "site is required"},
		{name: "resource name required", manifest: strings.Replace(valid, "name: test-site", "description: missing-name", 1), errString: "site.request.name is required"},
		{name: "manual IP blocks are not supported", manifest: valid + "ipBlocks:\n  fabric:\n    request:\n      name: manual\n", errString: "field ipBlocks not found"},
		{name: "site IP block selector required", manifest: valid + "siteIpBlocks: {}\n", errString: "siteIpBlocks.id or siteIpBlocks.match is required"},
		{name: "unknown field", manifest: valid + "unknown: true\n", errString: "field unknown not found"},
		{name: "multiple documents", manifest: valid + "---\n{}\n", errString: "multiple YAML documents"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			manifest, err := readSitePrerequisiteManifest("-", strings.NewReader(test.manifest))
			if test.errString == "" {
				require.NoError(t, err)
				assert.Equal(t, "test-site", manifest.Site.Request["name"])
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), test.errString)
		})
	}
}

func TestSitePrerequisiteExampleManifestParses(t *testing.T) {
	manifest, err := readSitePrerequisiteManifest("../examples/site-prerequisites.yaml", nil)
	require.NoError(t, err)
	assert.Equal(t, "sjc4", manifest.Site.Request["name"])
	assert.Contains(t, manifest.Instances, "worker")
}

func TestResolveBootstrapValue(t *testing.T) {
	context := map[string]any{
		"site": map[string]any{"id": "site-1"},
		"allocations": map[string]any{
			"network": map[string]any{
				"allocationConstraints": []any{
					map[string]any{"derivedResourceId": "ipblock-derived-1"},
				},
			},
		},
	}

	tests := []struct {
		name      string
		input     any
		expected  any
		errorIs   error
		errString string
	}{
		{name: "whole value", input: "${site.id}", expected: "site-1"},
		{name: "embedded value", input: "site-${site.id}", expected: "site-site-1"},
		{name: "array traversal", input: "${allocations.network.allocationConstraints.0.derivedResourceId}", expected: "ipblock-derived-1"},
		{name: "nested object", input: map[string]any{"siteId": "${site.id}"}, expected: map[string]any{"siteId": "site-1"}},
		{name: "missing reference", input: "${vpcs.default.id}", errorIs: errBootstrapReference, errString: "does not exist"},
		{name: "invalid array index", input: "${allocations.network.allocationConstraints.4.derivedResourceId}", errorIs: errBootstrapReference, errString: "does not exist"},
		{name: "unclosed reference", input: "site-${site.id", errorIs: errBootstrapReference, errString: "malformed reference"},
		{name: "unmatched closing brace", input: "site-${site.id}}", errorIs: errBootstrapReference, errString: "malformed reference"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			actual, err := bootstrapReferences(context).resolve(test.input)
			if test.errString == "" {
				require.NoError(t, err)
				assert.Equal(t, test.expected, actual)
				return
			}
			require.Error(t, err)
			require.ErrorIs(t, err, test.errorIs)
			assert.Contains(t, err.Error(), test.errString)
		})
	}
}

func TestEnsureBootstrapResourceRevalidatesResolvedRequest(t *testing.T) {
	manifest := completeBootstrapTestManifest()
	resource := &bootstrapResource{Request: map[string]any{"name": "${site}"}}
	context := map[string]any{"site": map[string]any{"id": "site-1"}}
	client := NewClient("http://invalid.example", "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, new(bytes.Buffer))
	bootstrap.references = context

	_, err := bootstrap.ensureResource(bootstrap.operations.instanceType, "compute", resource)
	require.ErrorIs(t, err, errInvalidBootstrapResource)
	assert.Contains(t, err.Error(), "instanceTypes.compute.request.name is required")
}

func TestEnsureBootstrapResourcePreservesDriftAfterConflict(t *testing.T) {
	requests := []string{}
	server := httptest.NewServer(http.HandlerFunc(func(response http.ResponseWriter, request *http.Request) {
		response.Header().Set("Content-Type", "application/json")
		requests = append(requests, request.Method)

		switch request.Method {
		case http.MethodGet:
			if len(requests) == 1 {
				writeBootstrapTestJSON(response, []map[string]any{})
				return
			}
			writeBootstrapTestJSON(response, []map[string]any{{
				"id":          "site-existing",
				"name":        "test-site",
				"description": "different description",
			}})
		case http.MethodPost:
			response.WriteHeader(http.StatusConflict)
			writeBootstrapTestJSON(response, map[string]any{"message": "already exists"})
		default:
			response.WriteHeader(http.StatusMethodNotAllowed)
		}
	}))
	t.Cleanup(server.Close)

	manifest := completeBootstrapTestManifest()
	resource := &bootstrapResource{Request: map[string]any{
		"name":        "test-site",
		"description": "expected description",
	}}
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, new(bytes.Buffer))

	_, err := bootstrap.ensureResource(bootstrap.operations.instanceType, "compute", resource)
	require.ErrorIs(t, err, errBootstrapDrift)
	assert.Contains(t, err.Error(), "description is different description, want expected description")
	assert.Equal(t, []string{http.MethodGet, http.MethodPost, http.MethodGet}, requests)
}

func TestBootstrapScalarEqual(t *testing.T) {
	tests := []struct {
		name     string
		left     any
		right    any
		expected bool
	}{
		{name: "same strings", left: "true", right: "true", expected: true},
		{name: "string and boolean", left: "true", right: true, expected: false},
		{name: "string and number", left: "1", right: 1, expected: false},
		{name: "same booleans", left: true, right: true, expected: true},
		{name: "different booleans", left: true, right: false, expected: false},
		{name: "compatible integer types", left: int32(1), right: int64(1), expected: true},
		{name: "integer and JSON number", left: uint(1), right: json.Number("1"), expected: true},
		{name: "integer and decimal", left: 1, right: 1.0, expected: true},
		{name: "different numbers", left: json.Number("1.5"), right: float64(2), expected: false},
		{name: "invalid JSON numbers", left: json.Number("invalid"), right: json.Number("invalid"), expected: false},
		{name: "both nil", expected: true},
		{name: "one nil", right: "", expected: false},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.expected, bootstrapScalarEqual(test.left, test.right))
		})
	}
}

func TestBootstrapResourceAPI_NormalizePrefix(t *testing.T) {
	tests := []struct {
		name     string
		category string
		resource map[string]any
		expected map[string]any
	}{
		{
			name:     "CIDR text changes but metadata stays exact",
			category: "vpcPrefixes",
			resource: map[string]any{
				"prefix":      "2001:0DB8:0000:0001:0000:0000:0000:0000/64",
				"description": "2001:0DB8:0000:0001:0000:0000:0000:0000/64",
			},
			expected: map[string]any{
				"prefix":      "2001:db8:0:1::/64",
				"description": "2001:0DB8:0000:0001:0000:0000:0000:0000/64",
			},
		},
		{
			name:     "host bits are not masked",
			category: "vpcPrefixes",
			resource: map[string]any{"prefix": "2001:0DB8:0000:0001:0000:0000:0000:0001/64"},
			expected: map[string]any{"prefix": "2001:db8:0:1::1/64"},
		},
		{
			name:     "address text changes but separate length stays exact",
			category: "siteIpBlocks",
			resource: map[string]any{
				"prefix":       "2001:0DB8:0000:0000:0000:0000:0000:0000",
				"prefixLength": 48,
			},
			expected: map[string]any{
				"prefix":       "2001:db8::",
				"prefixLength": 48,
			},
		},
		{
			name:     "unparseable prefix stays exact",
			category: "vpcPrefixes",
			resource: map[string]any{"prefix": "not-a-prefix"},
			expected: map[string]any{"prefix": "not-a-prefix"},
		},
		{
			name:     "another resource category stays exact",
			category: "instances",
			resource: map[string]any{"prefix": "2001:0DB8:0000:0001:0000:0000:0000:0000/64"},
			expected: map[string]any{"prefix": "2001:0DB8:0000:0001:0000:0000:0000:0000/64"},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			original := maps.Clone(test.resource)
			api := bootstrapResourceAPI{category: test.category}

			assert.Equal(t, test.expected, api.normalizePrefix(test.resource))
			assert.Equal(t, original, test.resource)
		})
	}
}

func TestBootstrapSitePrerequisitesCreatesAndReusesResources(t *testing.T) {
	tests := []struct {
		name string
		ipv6 bool
	}{
		{name: "IPv4 replay"},
		{name: "IPv6 replay by ID", ipv6: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			api := newBootstrapTestAPI()
			api.addSite("provider-org", "test-site")
			api.addSiteIPBlock("provider-org")
			manifest := completeBootstrapTestManifest()
			if test.ipv6 {
				manifest.SiteIPBlocks.Match["prefix"] = "2001:0DB8:0000:0000:0000:0000:0000:0000"
				manifest.SiteIPBlocks.Match["prefixLength"] = 48
				manifest.Allocations["network"].Request["allocationConstraints"].([]any)[0].(map[string]any)["constraintValue"] = 64
				manifest.VPCPrefixes["tenant"].Request["prefix"] = "2001:0DB8:0000:0001:0000:0000:0000:0000/64"
				delete(manifest.VPCPrefixes["tenant"].Request, "prefixLength")
				siteIPBlock := api.get("provider-org", "ipblock", "site-ipblock-1")
				siteIPBlock["name"] = "site-fabric-ipv6-2001-db8-48"
				siteIPBlock["prefix"] = "2001:db8::"
				siteIPBlock["prefixLength"] = 48
				api.put("provider-org", "ipblock", siteIPBlock)
			}
			server := httptest.NewServer(api)
			t.Cleanup(server.Close)

			client := NewClient(server.URL, "original-org", "token", nil, false)
			var progress bytes.Buffer
			bootstrap := newTestSiteBootstrap(t, client, manifest, &progress)

			require.NoError(t, bootstrap.apply())
			assert.Equal(t, "original-org", client.Org)
			require.NotEmpty(t, api.getOrder)
			assert.Equal(t, "provider-org/service-account/current", api.getOrder[0])
			assert.Equal(t, []string{
				"provider-org/instance/type",
				"provider-org/allocation",
				"tenant-org/vpc",
				"tenant-org/vpc-prefix",
				"tenant-org/instance",
			}, api.postOrder)
			assert.Equal(t, "provider-id", manifest.Provider.ID)
			assert.Equal(t, "tenant-id", manifest.Tenant.ID)
			assert.Equal(t, "site-1", manifest.Site.ID)
			assert.Equal(t, "site-ipblock-1", manifest.SiteIPBlocks.ID)
			assert.Equal(t, "instance-type-1", manifest.InstanceTypes["compute"].ID)
			assert.Equal(t, "allocation-1", manifest.Allocations["network"].ID)
			assert.Equal(t, "vpc-1", manifest.VPCs["tenant"].ID)
			assert.Equal(t, "vpc-prefix-1", manifest.VPCPrefixes["tenant"].ID)
			assert.Equal(t, "instance-1", manifest.Instances["worker"].ID)

			vpcPrefixRequest := api.postRequest("tenant-org/vpc-prefix")
			assert.Equal(t, "vpc-1", vpcPrefixRequest["vpcId"])
			assert.Equal(t, "tenant-ipblock-1", vpcPrefixRequest["ipBlockId"])
			if test.ipv6 {
				assert.Equal(t, "2001:0DB8:0000:0001:0000:0000:0000:0000/64", vpcPrefixRequest["prefix"])
				assert.Equal(t, "2001:db8:0:1::/64", api.get("tenant-org", "vpc-prefix", "vpc-prefix-1")["prefix"])
			}
			instanceRequest := api.postRequest("tenant-org/instance")
			assert.Equal(t, "tenant-id", instanceRequest["tenantId"])
			assert.Equal(t, "instance-type-1", instanceRequest["instanceTypeId"])
			assert.Equal(t, "vpc-1", instanceRequest["vpcId"])

			requestedPrefix := manifest.VPCPrefixes["tenant"].Request["prefix"]
			sitePrefix := manifest.SiteIPBlocks.Match["prefix"]
			firstPostCount := len(api.postOrder)
			progress.Reset()
			bootstrap = newTestSiteBootstrap(t, client, manifest, &progress)
			err := bootstrap.apply()
			assert.Len(t, api.postOrder, firstPostCount)
			assert.Equal(t, requestedPrefix, manifest.VPCPrefixes["tenant"].Request["prefix"])
			assert.Equal(t, sitePrefix, manifest.SiteIPBlocks.Match["prefix"])
			require.NoError(t, err)
			assert.Contains(t, progress.String(), "reused site test-site (site-1)")
			assert.Contains(t, progress.String(), "reused instance worker-1 (instance-1)")
		})
	}
}

func TestBootstrapSitePrerequisitesRecoversWhenRecordedIDIsMissing(t *testing.T) {
	api := newBootstrapTestAPI()
	api.addSite("provider-org", "test-site")
	api.addSiteIPBlock("provider-org")
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := completeBootstrapTestManifest()
	manifest.Site.ID = "site-from-another-installation"
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)

	require.NoError(t, bootstrap.apply())
	assert.Equal(t, "site-1", manifest.Site.ID)
	assert.NotContains(t, api.postOrder, "provider-org/site")
}

func TestBootstrapSitePrerequisitesRejectsMissingSite(t *testing.T) {
	api := newBootstrapTestAPI()
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := completeBootstrapTestManifest()
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)

	err := bootstrap.apply()
	require.ErrorIs(t, err, errInvalidBootstrapResource)
	assert.Contains(t, err.Error(), "required site \"test-site\" was not found")
	assert.NotContains(t, api.postOrder, "provider-org/site")
}

func TestBootstrapSitePrerequisitesRejectsExistingResourceDrift(t *testing.T) {
	api := newBootstrapTestAPI()
	api.put("provider-org", "site", map[string]any{
		"id":          "site-existing",
		"name":        "test-site",
		"description": "different description",
	})
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := completeBootstrapTestManifest()
	manifest.Site.Request["description"] = "expected description"
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)

	err := bootstrap.apply()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "existing site \"site\" does not match")
	assert.Contains(t, err.Error(), "description is different description, want expected description")
	assert.Empty(t, api.postOrder)
}

func TestNewAppIncludesSiteBootstrapCommand(t *testing.T) {
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err)

	var siteCommandFound bool
	var bootstrapCommandFound bool
	for _, command := range app.Commands {
		if command.Name != "site" {
			continue
		}
		siteCommandFound = true
		for _, subcommand := range command.Subcommands {
			if subcommand.Name == "bootstrap" {
				bootstrapCommandFound = true
				break
			}
		}
		break
	}
	assert.True(t, siteCommandFound)
	assert.True(t, bootstrapCommandFound)
}

func TestSiteBootstrapResolvesEmbeddedOpenAPIOperations(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)
	operations, err := newSiteBootstrapOperations(spec)
	require.NoError(t, err)

	assert.Equal(t, "get-current-service-account", operations.serviceAccount.op.OperationID)
	assert.Nil(t, operations.site.create.op)
	assert.Equal(t, "get-all-ipblock", operations.siteIPBlock.list.op.OperationID)
	assert.Nil(t, operations.siteIPBlock.create.op)
	assert.Equal(t, "create-instance", operations.instance.create.op.OperationID)
}

func TestSiteBootstrapUsesPathsFromOpenAPISpec(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)

	const originalPath = "/v2/org/{org}/nico/site"
	const replacementPath = "/custom/org/{org}/site"
	pathItem := spec.Paths[originalPath]
	delete(spec.Paths, originalPath)
	spec.Paths[replacementPath] = pathItem

	operations, err := newSiteBootstrapOperations(spec)
	require.NoError(t, err)
	assert.Equal(t, replacementPath, operations.site.list.path)
}

func TestBootstrapSitePrerequisitesUsesServiceAccountInitialization(t *testing.T) {
	api := newBootstrapTestAPI()
	api.serviceAccountEnabled = true
	api.addSite("service-org", "service-site")
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := &sitePrerequisiteManifest{
		Provider: bootstrapOrganization{Org: "service-org"},
		Tenant:   bootstrapOrganization{Org: "service-org"},
		Site:     &bootstrapResource{Request: map[string]any{"name": "service-site"}},
	}
	client := NewClient(server.URL, "service-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)

	require.NoError(t, bootstrap.apply())
	assert.Equal(t, "service-provider-id", manifest.Provider.ID)
	assert.Equal(t, "service-tenant-id", manifest.Tenant.ID)
	assert.Contains(t, api.getOrder, "service-org/service-account/current")
	assert.NotContains(t, api.getOrder, "service-org/infrastructure-provider/current")
	assert.NotContains(t, api.getOrder, "service-org/tenant/current")
}

func TestBootstrapSitePrerequisitesRejectsSplitOrganizationsInServiceAccountMode(t *testing.T) {
	api := newBootstrapTestAPI()
	api.serviceAccountEnabled = true
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := &sitePrerequisiteManifest{
		Provider: bootstrapOrganization{Org: "provider-org"},
		Tenant:   bootstrapOrganization{Org: "tenant-org"},
		Site:     &bootstrapResource{Request: map[string]any{"name": "service-site"}},
	}
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)

	err := bootstrap.apply()
	require.ErrorIs(t, err, errInvalidBootstrapManifest)
	assert.Contains(t, err.Error(), "service account mode requires provider.org and tenant.org to match")
	assert.Empty(t, api.postOrder)
}

func TestBootstrapSitePrerequisitesWaitsForAutoCreatedSiteIPBlock(t *testing.T) {
	api := newBootstrapTestAPI()
	api.addSite("provider-org", "test-site")
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := completeBootstrapTestManifest()
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)

	err := bootstrap.apply()
	require.ErrorIs(t, err, errInvalidBootstrapResource)
	assert.Contains(t, err.Error(), "wait for Site fabric-prefix inventory and rerun")
	assert.NotContains(t, api.postOrder, "provider-org/ipblock")
}

func TestDiscoverExistingResourceByIDOnly(t *testing.T) {
	api := newBootstrapTestAPI()
	api.addSiteIPBlock("provider-org")
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := completeBootstrapTestManifest()
	resource := &bootstrapExistingResource{ID: "site-ipblock-1"}
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)

	response, err := bootstrap.discoverExistingResource(bootstrap.operations.siteIPBlock, "fabric", resource)
	require.NoError(t, err)
	assert.Equal(t, "site-ipblock-1", response["id"])
	assert.Equal(t, []string{"provider-org/ipblock/site-ipblock-1"}, api.getOrder)
}

func TestDiscoverExistingResourceFallsBackFromStaleIDToMatch(t *testing.T) {
	api := newBootstrapTestAPI()
	api.addSiteIPBlock("provider-org")
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	manifest := completeBootstrapTestManifest()
	resource := manifest.SiteIPBlocks
	resource.ID = "stale-site-ipblock"
	client := NewClient(server.URL, "provider-org", "token", nil, false)
	bootstrap := newTestSiteBootstrap(t, client, manifest, nil)
	bootstrap.references["site"] = map[string]any{"id": "site-1"}

	response, err := bootstrap.discoverExistingResource(bootstrap.operations.siteIPBlock, "fabric", resource)
	require.NoError(t, err)
	assert.Equal(t, "site-ipblock-1", response["id"])
	assert.Equal(t, "site-ipblock-1", resource.ID)
	assert.Equal(t, []string{
		"provider-org/ipblock/stale-site-ipblock",
		"provider-org/ipblock",
	}, api.getOrder)
}

func TestSiteBootstrapCommandWritesReplayableManifest(t *testing.T) {
	api := newBootstrapTestAPI()
	api.addSite("provider-org", "command-site")
	server := httptest.NewServer(api)
	t.Cleanup(server.Close)

	input := `
provider:
  org: provider-org
tenant:
  org: tenant-org
site:
  request:
    name: command-site
`
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err)
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	app.Reader = strings.NewReader(input)
	app.Writer = &stdout
	app.ErrWriter = &stderr

	err = app.Run([]string{
		"nicocli",
		"--base-url", server.URL,
		"--token", "test-token",
		"site", "bootstrap",
		"--file", "-",
		"--output-file", "-",
	})
	require.NoError(t, err)
	assert.Contains(t, stderr.String(), "reused site command-site (site-1)")

	resolved, err := readSitePrerequisiteManifest("-", strings.NewReader(stdout.String()))
	require.NoError(t, err)
	assert.Equal(t, "provider-org", resolved.Provider.Org)
	assert.Equal(t, "provider-id", resolved.Provider.ID)
	assert.Equal(t, "tenant-id", resolved.Tenant.ID)
	assert.Equal(t, "site-1", resolved.Site.ID)
}

func completeBootstrapTestManifest() *sitePrerequisiteManifest {
	return &sitePrerequisiteManifest{
		Provider: bootstrapOrganization{Org: "provider-org"},
		Tenant:   bootstrapOrganization{Org: "tenant-org"},
		Site: &bootstrapResource{Request: map[string]any{
			"name": "test-site",
		}},
		SiteIPBlocks: &bootstrapExistingResource{Match: map[string]any{
			"siteId":       "${site.id}",
			"routingType":  "DatacenterOnly",
			"prefix":       "10.0.0.0",
			"prefixLength": 16,
		}},
		InstanceTypes: map[string]*bootstrapResource{
			"compute": {Request: map[string]any{
				"name":                "compute-large",
				"siteId":              "${site.id}",
				"machineCapabilities": []any{},
			}},
		},
		Allocations: map[string]*bootstrapResource{
			"network": {Request: map[string]any{
				"name":     "tenant-network",
				"tenantId": "${tenant.id}",
				"siteId":   "${site.id}",
				"allocationConstraints": []any{
					map[string]any{
						"resourceType":    "IPBlock",
						"resourceTypeId":  "${siteIpBlocks.id}",
						"constraintType":  "OnDemand",
						"constraintValue": 24,
					},
				},
			}},
		},
		VPCs: map[string]*bootstrapResource{
			"tenant": {Request: map[string]any{
				"name":   "tenant-vpc",
				"siteId": "${site.id}",
			}},
		},
		VPCPrefixes: map[string]*bootstrapResource{
			"tenant": {Request: map[string]any{
				"name":         "tenant-prefix",
				"vpcId":        "${vpcs.tenant.id}",
				"ipBlockId":    "${allocations.network.allocationConstraints.0.derivedResourceId}",
				"prefixLength": 24,
			}},
		},
		Instances: map[string]*bootstrapResource{
			"worker": {Request: map[string]any{
				"name":           "worker-1",
				"tenantId":       "${tenant.id}",
				"instanceTypeId": "${instanceTypes.compute.id}",
				"vpcId":          "${vpcs.tenant.id}",
				"interfaces": []any{
					map[string]any{
						"vpcPrefixId": "${vpcPrefixes.tenant.id}",
						"isPhysical":  true,
					},
				},
			}},
		},
	}
}

func newTestSiteBootstrap(t *testing.T, client *Client, manifest *sitePrerequisiteManifest, progress io.Writer) *siteBootstrap {
	t.Helper()

	spec, err := ParseSpec(openapi.Spec)
	require.NoError(t, err)
	bootstrap, err := newSiteBootstrap(spec, client, manifest, progress)
	require.NoError(t, err)
	return bootstrap
}

type bootstrapTestAPI struct {
	resources             map[string]map[string]map[string]any
	postOrder             []string
	getOrder              []string
	postBodies            map[string][]map[string]any
	nextIDByKey           map[string]int
	serviceAccountEnabled bool
}

func newBootstrapTestAPI() *bootstrapTestAPI {
	return &bootstrapTestAPI{
		resources:   map[string]map[string]map[string]any{},
		postBodies:  map[string][]map[string]any{},
		nextIDByKey: map[string]int{},
	}
}

func (api *bootstrapTestAPI) ServeHTTP(response http.ResponseWriter, request *http.Request) {
	response.Header().Set("Content-Type", "application/json")
	parts := strings.Split(strings.TrimPrefix(request.URL.Path, "/v2/org/"), "/nico/")
	if len(parts) != 2 {
		http.NotFound(response, request)
		return
	}
	org, resourcePath := parts[0], parts[1]
	if request.Method == http.MethodGet {
		api.getOrder = append(api.getOrder, org+"/"+resourcePath)
	}

	if request.Method == http.MethodGet && resourcePath == "service-account/current" {
		payload := map[string]any{"enabled": api.serviceAccountEnabled}
		if api.serviceAccountEnabled {
			payload["infrastructureProviderId"] = "service-provider-id"
			payload["tenantId"] = "service-tenant-id"
		}
		writeBootstrapTestJSON(response, payload)
		return
	}

	if request.Method == http.MethodGet && resourcePath == "infrastructure-provider/current" {
		writeBootstrapTestJSON(response, map[string]any{"id": "provider-id", "org": org})
		return
	}
	if request.Method == http.MethodGet && resourcePath == "tenant/current" {
		writeBootstrapTestJSON(response, map[string]any{"id": "tenant-id", "org": org})
		return
	}

	collection, id := splitBootstrapTestResourcePath(resourcePath)
	if collection == "" {
		http.NotFound(response, request)
		return
	}

	switch request.Method {
	case http.MethodGet:
		if id != "" {
			item := api.get(org, collection, id)
			if item == nil {
				response.WriteHeader(http.StatusNotFound)
				writeBootstrapTestJSON(response, map[string]any{"message": "not found"})
				return
			}
			writeBootstrapTestJSON(response, item)
			return
		}
		name := request.URL.Query().Get("query")
		items := []map[string]any{}
		for _, item := range api.collection(org, collection) {
			if name == "" || strings.Contains(fmt.Sprint(item["name"]), name) {
				items = append(items, item)
			}
		}
		writeBootstrapTestJSON(response, items)
	case http.MethodPost:
		var body map[string]any
		if err := json.NewDecoder(request.Body).Decode(&body); err != nil {
			http.Error(response, fmt.Sprintf("invalid request: %v", err), http.StatusBadRequest)
			return
		}
		key := org + "/" + collection
		api.nextIDByKey[key]++
		prefix := strings.ReplaceAll(collection, "/", "-")
		id := fmt.Sprintf("%s-%d", prefix, api.nextIDByKey[key])
		item := cloneBootstrapTestMap(body)
		item["id"] = id
		if collection == "vpc-prefix" {
			cidr, ok := item["prefix"].(string)
			if ok {
				parsed, err := netip.ParsePrefix(cidr)
				if err != nil {
					http.Error(response, fmt.Sprintf("invalid prefix: %v", err), http.StatusBadRequest)
					return
				}
				item["prefix"] = parsed.String()
			}
		}
		if collection == "allocation" {
			constraints, _ := item["allocationConstraints"].([]any)
			for index, rawConstraint := range constraints {
				constraint, _ := rawConstraint.(map[string]any)
				constraint["id"] = fmt.Sprintf("constraint-%d", index+1)
				constraint["allocationId"] = id
				if constraint["resourceType"] == "IPBlock" {
					constraint["derivedResourceId"] = "tenant-ipblock-1"
				}
			}
		}
		api.put(org, collection, item)
		api.postOrder = append(api.postOrder, key)
		api.postBodies[key] = append(api.postBodies[key], cloneBootstrapTestMap(body))
		response.WriteHeader(http.StatusCreated)
		writeBootstrapTestJSON(response, item)
	default:
		response.WriteHeader(http.StatusMethodNotAllowed)
	}
}

func splitBootstrapTestResourcePath(resourcePath string) (string, string) {
	for _, collection := range []string{"instance/type", "vpc-prefix", "ipblock", "allocation", "instance", "site", "vpc"} {
		if resourcePath == collection {
			return collection, ""
		}
		if strings.HasPrefix(resourcePath, collection+"/") {
			return collection, strings.TrimPrefix(resourcePath, collection+"/")
		}
	}
	return "", ""
}

func (api *bootstrapTestAPI) collection(org, collection string) map[string]map[string]any {
	key := org + "/" + collection
	if api.resources[key] == nil {
		api.resources[key] = map[string]map[string]any{}
	}
	return api.resources[key]
}

func (api *bootstrapTestAPI) put(org, collection string, item map[string]any) {
	api.collection(org, collection)[fmt.Sprint(item["id"])] = cloneBootstrapTestMap(item)
}

func (api *bootstrapTestAPI) addSite(org, name string) {
	api.put(org, "site", map[string]any{
		"id":   "site-1",
		"name": name,
	})
}

func (api *bootstrapTestAPI) addSiteIPBlock(org string) {
	api.put(org, "ipblock", map[string]any{
		"id":           "site-ipblock-1",
		"name":         "site-fabric-ipv4-10-0-0-0-16",
		"siteId":       "site-1",
		"routingType":  "DatacenterOnly",
		"prefix":       "10.0.0.0",
		"prefixLength": 16,
	})
}

func (api *bootstrapTestAPI) get(org, collection, id string) map[string]any {
	item := api.collection(org, collection)[id]
	if item == nil {
		return nil
	}
	return cloneBootstrapTestMap(item)
}

func (api *bootstrapTestAPI) postRequest(key string) map[string]any {
	requests := api.postBodies[key]
	if len(requests) == 0 {
		return nil
	}
	return requests[len(requests)-1]
}

func cloneBootstrapTestMap(value map[string]any) map[string]any {
	data, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	var cloned map[string]any
	if err := json.Unmarshal(data, &cloned); err != nil {
		panic(err)
	}
	return cloned
}

func writeBootstrapTestJSON(response http.ResponseWriter, value any) {
	if err := json.NewEncoder(response).Encode(value); err != nil {
		panic(err)
	}
}
