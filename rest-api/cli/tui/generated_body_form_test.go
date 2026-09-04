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

type queuedGeneratedBodyPrompter struct {
	choices        []string
	confirms       []bool
	texts          []string
	secrets        []string
	choiceOptions  [][]string
	choiceDefaults []string
}

func (p *queuedGeneratedBodyPrompter) Text(_ string, _ bool) (string, error) {
	if len(p.texts) == 0 {
		return "", fmt.Errorf("unexpected text prompt")
	}
	value := p.texts[0]
	p.texts = p.texts[1:]
	return value, nil
}

func (p *queuedGeneratedBodyPrompter) Secret(_ string, _ bool) (string, error) {
	if len(p.secrets) == 0 {
		return "", fmt.Errorf("unexpected secret prompt")
	}
	value := p.secrets[0]
	p.secrets = p.secrets[1:]
	return value, nil
}

func (p *queuedGeneratedBodyPrompter) Confirm(_ string) (bool, error) {
	if len(p.confirms) == 0 {
		return false, fmt.Errorf("unexpected confirmation prompt")
	}
	value := p.confirms[0]
	p.confirms = p.confirms[1:]
	return value, nil
}

func (p *queuedGeneratedBodyPrompter) Choice(
	_ string,
	options []string,
	defaultValue string,
) (string, error) {
	p.choiceOptions = append(p.choiceOptions, append([]string(nil), options...))
	p.choiceDefaults = append(p.choiceDefaults, defaultValue)
	if len(p.choices) == 0 {
		return "", fmt.Errorf("unexpected choice prompt")
	}
	value := p.choices[0]
	p.choices = p.choices[1:]
	return value, nil
}

func TestGeneratedBodyFormResolvesResourcesScopeAndEnums(t *testing.T) {
	resolver := NewResolver(NewCache())
	resolver.RegisterFetcher("instance-type", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "gpu-large", ID: "instance-type-1"}}, nil
	})
	resolver.RegisterFetcher("machine", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "host-one", ID: "machine-1"}}, nil
	})
	session := &Session{
		Scope:    Scope{SiteID: "site-1"},
		Resolver: resolver,
	}
	info := appcli.GeneratedCommandInfo{
		Name:           "example create",
		HasRequestBody: true,
		BodyRootType:   "object",
		BodyFormFields: []appcli.GeneratedCommandBodyFormField{
			{JSONName: "siteId", Required: true, Type: "string"},
			{JSONName: "instanceTypeId", Required: true, Type: "string"},
			{JSONName: "machineIds", Required: true, Type: "array", ItemType: "string"},
			{JSONName: "mode", Required: true, Type: "string", Enum: []string{"Fast", "Safe"}},
			{JSONName: "enabled", Type: "boolean"},
		},
	}
	prompter := &queuedGeneratedBodyPrompter{
		choices:  []string{"guided", "Fast"},
		confirms: []bool{false},
	}

	args, err := addGeneratedBodyFormWithPrompter(session, info, nil, prompter)
	require.NoError(t, err)
	require.Len(t, args, 2)
	assert.Equal(t, "--data", args[0])
	var body map[string]interface{}
	require.NoError(t, json.Unmarshal([]byte(args[1]), &body))
	assert.Equal(t, map[string]interface{}{
		"instanceTypeId": "instance-type-1",
		"machineIds":     []interface{}{"machine-1"},
		"mode":           "Fast",
		"siteId":         "site-1",
	}, body)
}

func TestGeneratedBodyFormDoesNotForceOptionalFields(t *testing.T) {
	fetches := 0
	resolver := NewResolver(NewCache())
	resolver.RegisterFetcher("rack", func(context.Context) ([]NamedItem, error) {
		fetches++
		return []NamedItem{{Name: "rack-one", ID: "rack-1"}}, nil
	})
	info := appcli.GeneratedCommandInfo{
		Name:           "example update",
		HasRequestBody: true,
		BodyRootType:   "object",
		BodyFormFields: []appcli.GeneratedCommandBodyFormField{
			{JSONName: "rackId", Type: "string"},
			{JSONName: "description", Type: "string"},
		},
	}
	prompter := &queuedGeneratedBodyPrompter{
		choices:  []string{"guided"},
		confirms: []bool{false},
		texts:    []string{""},
	}

	args, err := addGeneratedBodyFormWithPrompter(
		&Session{Resolver: resolver},
		info,
		nil,
		prompter,
	)
	require.NoError(t, err)
	assert.Empty(t, args)
	assert.Zero(t, fetches)
}

func TestGeneratedBodyFormGuidesRootArrayItems(t *testing.T) {
	resolver := NewResolver(NewCache())
	resolver.RegisterFetcher("sku", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "compute-sku", ID: "sku-1"}}, nil
	})
	info := appcli.GeneratedCommandInfo{
		Name:           "expected-machine batch-create",
		HasRequestBody: true,
		BodyRootType:   "array",
		BodyFormFields: []appcli.GeneratedCommandBodyFormField{
			{JSONName: "siteId", Required: true, Type: "string"},
			{JSONName: "skuId", Type: "string"},
		},
	}
	prompter := &queuedGeneratedBodyPrompter{
		choices:  []string{"guided"},
		confirms: []bool{true, false},
	}

	args, err := addGeneratedBodyFormWithPrompter(
		&Session{Scope: Scope{SiteID: "site-1"}, Resolver: resolver},
		info,
		nil,
		prompter,
	)
	require.NoError(t, err)
	require.Len(t, args, 2)
	assert.Equal(t, "guided", prompter.choiceDefaults[0])
	var body []map[string]interface{}
	require.NoError(t, json.Unmarshal([]byte(args[1]), &body))
	assert.Equal(t, []map[string]interface{}{{
		"skuId":  "sku-1",
		"siteId": "site-1",
	}}, body)
}

func TestGeneratedBodyFormKeepsWholeJSONEscapeHatch(t *testing.T) {
	info := appcli.GeneratedCommandInfo{
		Name:           "example create",
		HasRequestBody: true,
		BodyRootType:   "object",
		BodyFormFields: []appcli.GeneratedCommandBodyFormField{{
			JSONName: "nested",
			Required: true,
			Type:     "object",
		}},
	}
	prompter := &queuedGeneratedBodyPrompter{
		choices: []string{"json"},
		texts:   []string{`{"nested":{"custom":true}}`},
	}

	args, err := addGeneratedBodyFormWithPrompter(&Session{}, info, nil, prompter)
	require.NoError(t, err)
	assert.Equal(t, []string{"--data", `{"nested":{"custom":true}}`}, args)
}

func TestGeneratedBodyFormRequiredBodyDoesNotOfferNone(t *testing.T) {
	info := generatedCommandInfoByName(t, "vpc-peering create")
	require.True(t, info.RequestBodyRequired)
	prompter := &queuedGeneratedBodyPrompter{
		choices: []string{"json"},
		texts:   []string{`{"siteId":"site-1","vpc1Id":"vpc-1","vpc2Id":"vpc-2"}`},
	}

	args, err := addGeneratedBodyFormWithPrompter(&Session{}, info, nil, prompter)
	require.NoError(t, err)
	require.Len(t, args, 2)
	assert.Equal(t, []string{"guided", "json"}, prompter.choiceOptions[0])
	assert.Equal(t, "guided", prompter.choiceDefaults[0])
}

func TestGeneratedCommandInfosExposeBodyFormSchema(t *testing.T) {
	byName := make(map[string]appcli.GeneratedCommandInfo, len(embeddedGeneratedCommandInfos))
	for _, info := range embeddedGeneratedCommandInfos {
		byName[info.Name] = info
	}

	association := byName["instance-type machine-association create"]
	assert.Equal(t, appcli.SchemaType("object"), association.BodyRootType)
	machineIDs := requireGeneratedBodyFormField(t, association, "machineIds")
	assert.True(t, machineIDs.Required)
	assert.Equal(t, appcli.SchemaType("array"), machineIDs.Type)
	assert.Equal(t, appcli.SchemaType("string"), machineIDs.ItemType)

	batch := byName["expected-machine batch-create"]
	assert.Equal(t, appcli.SchemaType("array"), batch.BodyRootType)
	rackID := requireGeneratedBodyFormField(t, batch, "rackId")
	assert.False(t, rackID.Required)
	assert.Equal(t, appcli.SchemaType("string"), rackID.Type)

	rackPower := byName["rack power-control-racks power-control-racks"]
	state := requireGeneratedBodyFormField(t, rackPower, "state")
	assert.Equal(t, []string{"on", "off", "cycle", "forceoff", "forcecycle"}, state.Enum)
}

func TestGeneratedBodyFormRealSchemaPersistsSiteBeforeVPCSelectors(t *testing.T) {
	info := generatedCommandInfoByName(t, "vpc-peering create")
	require.True(t, info.RequestBodyRequired)

	cache := NewCache()
	resolver := NewResolver(cache)
	session := &Session{Cache: cache, Resolver: resolver}
	resolver.RegisterFetcher("site", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "site-one", ID: "site-1"}}, nil
	})
	var siteScopeAtVPCFetch []string
	resolver.RegisterFetcher("vpc", func(context.Context) ([]NamedItem, error) {
		siteScopeAtVPCFetch = append(siteScopeAtVPCFetch, session.Scope.SiteID)
		return []NamedItem{{
			Name:  "vpc-one",
			ID:    "vpc-1",
			Extra: map[string]string{"siteId": "site-1"},
		}}, nil
	})

	body, err := buildGeneratedBodyFormObject(
		session,
		info,
		&queuedGeneratedBodyPrompter{},
	)
	require.NoError(t, err)
	assert.Equal(t, map[string]interface{}{
		"siteId": "site-1",
		"vpc1Id": "vpc-1",
		"vpc2Id": "vpc-1",
	}, body)
	assert.Equal(t, []string{"site-1", "site-1"}, siteScopeAtVPCFetch)
	assert.Equal(t, "site-1", session.Scope.SiteID)
	assert.Equal(t, "site-one", session.Scope.SiteName)
	assert.Empty(t, session.Scope.VpcID)
	assert.Empty(t, session.Scope.VpcName)
}

func TestGeneratedTUICommandGuidedBodyResolvesNamesToIDs(t *testing.T) {
	type request struct {
		path  string
		query string
		body  string
	}
	requests := make(chan request, 4)
	vpcFetches := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		requests <- request{path: r.URL.Path, query: r.URL.RawQuery, body: string(body)}
		w.Header().Set("Content-Type", "application/json")
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/site":
			_, _ = io.WriteString(w, `[{"id":"site-1","name":"site-one"}]`)
		case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/vpc":
			vpcFetches++
			if vpcFetches == 1 {
				_, _ = io.WriteString(w, `[{"id":"vpc-1","name":"vpc-one","siteId":"site-1","tenantId":"tenant-1"}]`)
			} else {
				_, _ = io.WriteString(w, `[{"id":"vpc-2","name":"vpc-two","siteId":"site-1","tenantId":"tenant-1"}]`)
			}
		case r.Method == http.MethodPost && r.URL.Path == "/v2/org/acme/nico/vpc-peering":
			w.WriteHeader(http.StatusCreated)
			_, _ = io.WriteString(w, `{"id":"peering-1"}`)
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
	session.Cache.Set("_tenant", []NamedItem{{Name: "acme", ID: "tenant-1"}})
	_, err := withStdin(t, "\ny\n", func() (string, error) {
		return "", runGeneratedTUICommand(
			session,
			generatedCommandInfoByName(t, "vpc-peering create"),
			nil,
		)
	})
	require.NoError(t, err)

	siteRequest := <-requests
	firstVPCRequest := <-requests
	secondVPCRequest := <-requests
	createRequest := <-requests
	assert.Equal(t, "/v2/org/acme/nico/site", siteRequest.path)
	assert.Equal(t, "/v2/org/acme/nico/vpc", firstVPCRequest.path)
	assert.Contains(t, firstVPCRequest.query, "siteId=site-1")
	assert.Equal(t, "/v2/org/acme/nico/vpc", secondVPCRequest.path)
	assert.Contains(t, secondVPCRequest.query, "siteId=site-1")
	assert.Equal(t, "/v2/org/acme/nico/vpc-peering", createRequest.path)
	assert.JSONEq(t, `{"siteId":"site-1","vpc1Id":"vpc-1","vpc2Id":"vpc-2"}`, createRequest.body)
	assert.Equal(t, "site-1", session.Scope.SiteID)
	assert.Empty(t, session.Scope.VpcID)
}

func TestGeneratedBodyFormRealSchemaResolvesNestedServiceID(t *testing.T) {
	info := generatedCommandInfoByName(t, "instance batch-create")
	deployments := requireGeneratedBodyFormField(t, info, "dpuExtensionServiceDeployments")
	require.Equal(t, appcli.SchemaType("object"), deployments.ItemType)
	require.NotEmpty(t, deployments.Properties)

	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("dpu-extension-service", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "telemetry", ID: "service-1"}}, nil
	})
	prompter := &queuedGeneratedBodyPrompter{
		choices:  []string{"guided"},
		confirms: []bool{true, false},
		texts:    []string{""},
	}

	value, set, err := promptGeneratedBodyField(
		&Session{Cache: cache, Resolver: resolver},
		info,
		deployments,
		map[string]string{"siteId": "site-1"},
		prompter,
	)
	require.NoError(t, err)
	require.True(t, set)
	assert.Equal(t, []map[string]interface{}{{
		"dpuExtensionServiceId": "service-1",
	}}, value)
}

func TestGeneratedBodyFormRealSchemaUsesCanonicalAcronymResource(t *testing.T) {
	info := generatedCommandInfoByName(t, "vpc create")
	partition := requireGeneratedBodyFormField(t, info, "nvLinkLogicalPartitionId")

	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("nvlink-logical-partition", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "nvlink-partition-one", ID: "partition-1"}}, nil
	})
	prompter := &queuedGeneratedBodyPrompter{confirms: []bool{true}}

	value, set, err := promptGeneratedBodyField(
		&Session{Cache: cache, Resolver: resolver},
		info,
		partition,
		map[string]string{"siteId": "site-1"},
		prompter,
	)
	require.NoError(t, err)
	assert.True(t, set)
	assert.Equal(t, "partition-1", value)
}

func TestGeneratedBodyFormRealSchemaResolvesNestedInfiniBandPartition(t *testing.T) {
	info := generatedCommandInfoByName(t, "instance batch-create")
	interfaces := requireGeneratedBodyFormField(t, info, "infinibandInterfaces")
	partition := requireNestedGeneratedBodyFormField(t, interfaces, "partitionId")

	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("infiniband-partition", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "ib-partition-one", ID: "ib-partition-1"}}, nil
	})
	prompter := &queuedGeneratedBodyPrompter{confirms: []bool{true}}

	value, set, err := promptGeneratedBodyField(
		&Session{Cache: cache, Resolver: resolver},
		info,
		partition,
		map[string]string{"siteId": "site-1"},
		prompter,
	)
	require.NoError(t, err)
	assert.True(t, set)
	assert.Equal(t, "ib-partition-1", value)
}

func TestGeneratedBodyFormExactVpcIDPersistsScope(t *testing.T) {
	info := generatedCommandInfoByName(t, "instance batch-create")
	vpcID := requireGeneratedBodyFormField(t, info, "vpcId")

	cache := NewCache()
	cache.Set("site", []NamedItem{{Name: "site-one", ID: "site-1"}})
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("vpc", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{
			Name:  "vpc-one",
			ID:    "vpc-1",
			Extra: map[string]string{"siteId": "site-1"},
		}}, nil
	})
	session := &Session{Cache: cache, Resolver: resolver}

	value, set, err := promptGeneratedBodyField(
		session,
		info,
		vpcID,
		map[string]string{"siteId": "site-1"},
		&queuedGeneratedBodyPrompter{},
	)
	require.NoError(t, err)
	assert.True(t, set)
	assert.Equal(t, "vpc-1", value)
	assert.Equal(t, "site-1", session.Scope.SiteID)
	assert.Equal(t, "site-one", session.Scope.SiteName)
	assert.Equal(t, "vpc-1", session.Scope.VpcID)
	assert.Equal(t, "vpc-one", session.Scope.VpcName)
}

func TestGeneratedBodyFormExpectedInventoryRackIDRemainsAuthored(t *testing.T) {
	info := generatedCommandInfoByName(t, "expected-machine create")
	rack := requireGeneratedBodyFormField(t, info, "rackId")

	fetches := 0
	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("rack", func(context.Context) ([]NamedItem, error) {
		fetches++
		return []NamedItem{{Name: "rack-resource", ID: "rack-uuid"}}, nil
	})
	prompter := &queuedGeneratedBodyPrompter{texts: []string{"rack-01"}}

	value, set, err := promptGeneratedBodyField(
		&Session{Cache: cache, Resolver: resolver},
		info,
		rack,
		map[string]string{"siteId": "site-1"},
		prompter,
	)
	require.NoError(t, err)
	assert.True(t, set)
	assert.Equal(t, "rack-01", value)
	assert.Zero(t, fetches)
}

func TestGeneratedBodyFormBatchUpdateIDSelectsExpectedMachine(t *testing.T) {
	info := generatedCommandInfoByName(t, "expected-machine batch-update")
	id := requireGeneratedBodyFormField(t, info, "id")
	require.False(t, id.Required, "the source schema omits required; form policy repairs it")

	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("expected-machine", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "expected-host-one", ID: "expected-machine-1"}}, nil
	})

	value, set, err := promptGeneratedBodyField(
		&Session{Cache: cache, Resolver: resolver},
		info,
		id,
		map[string]string{"siteId": "site-1"},
		&queuedGeneratedBodyPrompter{},
	)
	require.NoError(t, err)
	assert.True(t, set)
	assert.Equal(t, "expected-machine-1", value)
}

func TestGeneratedBodyFormAllocationResourceTypeChoosesResolver(t *testing.T) {
	info := generatedCommandInfoByName(t, "allocation create")
	constraints := requireGeneratedBodyFormField(t, info, "allocationConstraints")
	resourceTypeID := requireNestedGeneratedBodyFormField(t, constraints, "resourceTypeId")

	cache := NewCache()
	resolver := NewResolver(cache)
	resolver.RegisterFetcher("instance-type", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "gpu-large", ID: "instance-type-1"}}, nil
	})

	value, set, err := promptGeneratedBodyField(
		&Session{Cache: cache, Resolver: resolver},
		info,
		resourceTypeID,
		map[string]string{"resourceType": "InstanceType"},
		&queuedGeneratedBodyPrompter{},
	)
	require.NoError(t, err)
	assert.True(t, set)
	assert.Equal(t, "instance-type-1", value)
}

func TestGeneratedBodyFormBooleanEnumProducesJSONBoolean(t *testing.T) {
	info := generatedCommandInfoByName(t, "machine update")
	onlineRepair := requireGeneratedBodyFormField(t, info, "onlineRepair")
	acknowledgments := requireNestedGeneratedBodyFormField(t, onlineRepair, "acknowledgments")
	acceptRisk := requireNestedGeneratedBodyFormField(
		t,
		acknowledgments,
		"acceptDataCorruptionRisk",
	)
	require.Equal(t, appcli.SchemaType("boolean"), acceptRisk.Type)
	require.Equal(t, []string{"true"}, acceptRisk.Enum)

	value, set, err := promptGeneratedBodyField(
		&Session{},
		info,
		acceptRisk,
		nil,
		&queuedGeneratedBodyPrompter{choices: []string{"true"}},
	)
	require.NoError(t, err)
	assert.True(t, set)
	assert.Equal(t, true, value)
}

func TestGeneratedBodyFormTrayFilterResourceMappings(t *testing.T) {
	info := generatedCommandInfoByName(t, "tray power-control-trays power-control-trays")
	filter := requireGeneratedBodyFormField(t, info, "filter")

	cache := NewCache()
	resolver := NewResolver(cache)
	for _, resourceType := range []string{"tray", "tray-component", "rack"} {
		resourceType := resourceType
		resolver.RegisterFetcher(resourceType, func(context.Context) ([]NamedItem, error) {
			return []NamedItem{{Name: resourceType, ID: resourceType + "-1"}}, nil
		})
	}
	session := &Session{Cache: cache, Resolver: resolver}
	for _, testCase := range []struct {
		field        string
		resourceType string
	}{
		{field: "ids", resourceType: "tray"},
		{field: "componentIds", resourceType: "tray-component"},
		{field: "rackId", resourceType: "rack"},
	} {
		field := requireNestedGeneratedBodyFormField(t, filter, testCase.field)
		descriptor, supported := generatedBodyFieldResourceDescriptor(
			session,
			info,
			field.JSONName,
			nil,
		)
		assert.True(t, supported, testCase.field)
		assert.Equal(t, testCase.resourceType, descriptor.ResourceType, testCase.field)
	}
}

func TestGeneratedBodyFormRuleSelectorIsSupported(t *testing.T) {
	info := generatedCommandInfoByName(t, "tray power-control-trays power-control-trays")
	field := requireGeneratedBodyFormField(t, info, "ruleId")
	session := &Session{Resolver: NewResolver(NewCache())}

	descriptor, supported := generatedBodyFieldResourceDescriptor(
		session,
		info,
		field.JSONName,
		nil,
	)

	assert.True(t, supported)
	assert.Equal(t, "rule", descriptor.ResourceType)
}

func TestGeneratedBodyFormRealSchemaUnscopedTrayFlowUsesSelectedSite(t *testing.T) {
	info := generatedCommandInfoByName(t, "tray power-control-trays power-control-trays")
	cache := NewCache()
	resolver := NewResolver(cache)
	session := &Session{Cache: cache, Resolver: resolver}
	resolver.RegisterFetcher("site", func(context.Context) ([]NamedItem, error) {
		return []NamedItem{{Name: "site-one", ID: "site-1"}}, nil
	})
	var siteScopeAtTrayFetch string
	resolver.RegisterFetcher("tray", func(context.Context) ([]NamedItem, error) {
		siteScopeAtTrayFetch = session.Scope.SiteID
		return []NamedItem{{Name: "tray-one", ID: "tray-1"}}, nil
	})
	for _, resourceType := range []string{"tray-component", "rack", "rule"} {
		resourceType := resourceType
		resolver.RegisterFetcher(resourceType, func(context.Context) ([]NamedItem, error) {
			return []NamedItem{{Name: resourceType, ID: resourceType + "-1"}}, nil
		})
	}
	prompter := &queuedGeneratedBodyPrompter{
		choices: []string{"guided", "on"},
		confirms: []bool{
			false, // filter.componentIds
			true,  // filter.ids
			false, // filter.rackId
			false, // filter.type
			false, // overrideReadinessCheck
			false, // ruleId
		},
		texts: []string{
			"", // filter.rackName
			"", // filter.slotId
		},
	}

	body, err := buildGeneratedBodyFormObject(session, info, prompter)
	require.NoError(t, err)
	assert.Equal(t, map[string]interface{}{
		"filter": map[string]interface{}{
			"ids": []string{"tray-1"},
		},
		"siteId": "site-1",
		"state":  "on",
	}, body)
	assert.Equal(t, "site-1", siteScopeAtTrayFetch)
	assert.Equal(t, "site-1", session.Scope.SiteID)
	assert.Empty(t, session.Scope.VpcID)
}

func requireGeneratedBodyFormField(
	t *testing.T,
	info appcli.GeneratedCommandInfo,
	name string,
) appcli.GeneratedCommandBodyFormField {
	t.Helper()
	for _, field := range info.BodyFormFields {
		if field.JSONName == name {
			return field
		}
	}
	require.FailNow(t, "generated body form field not found", "%s: %s", info.Name, name)
	return appcli.GeneratedCommandBodyFormField{}
}

func requireNestedGeneratedBodyFormField(
	t *testing.T,
	parent appcli.GeneratedCommandBodyFormField,
	name string,
) appcli.GeneratedCommandBodyFormField {
	t.Helper()
	for _, field := range parent.Properties {
		if field.JSONName == name {
			return field
		}
	}
	require.FailNow(t, "nested generated body form field not found", "%s: %s", parent.JSONName, name)
	return appcli.GeneratedCommandBodyFormField{}
}

func generatedCommandInfoByName(t *testing.T, name string) appcli.GeneratedCommandInfo {
	t.Helper()
	for _, info := range embeddedGeneratedCommandInfos {
		if info.Name == name {
			return info
		}
	}
	require.FailNow(t, "generated command info not found", name)
	return appcli.GeneratedCommandInfo{}
}
