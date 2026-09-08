// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"sort"
	"strings"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/vpcprefix"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// --- Upstream tests ---

func TestCmdSiteCreateRejectsResponseWithoutID(t *testing.T) {
	for _, response := range []string{"null", "{}"} {
		t.Run(response, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusCreated)
				_, _ = io.WriteString(w, response)
			}))
			defer server.Close()

			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			_, err := withStdin(t, "site-name\n\n\n\n\n\n\n", func() (string, error) {
				return "", cmdSiteCreate(session, nil)
			})

			require.EqualError(t, err, "parsing created site response: missing id")
		})
	}
}

func TestParseMutationResponseRequiringID(t *testing.T) {
	tests := []struct {
		name      string
		response  string
		wantID    string
		wantError string
	}{
		{name: "malformed JSON", response: "[", wantError: "parsing created site response:"},
		{name: "null", response: "null", wantError: "parsing created site response: missing id"},
		{name: "empty object", response: "{}", wantError: "parsing created site response: missing id"},
		{name: "blank id", response: `{"id":"  "}`, wantError: "parsing created site response: missing id"},
		{name: "valid object", response: `{"id":"site-1","name":"Site One"}`, wantID: "site-1"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			parsed, err := parseMutationResponseRequiringID([]byte(test.response), "created site")
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				assert.Nil(t, parsed)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.wantID, str(parsed, "id"))
		})
	}
}

func TestAppendScopeFlags_NoSession(t *testing.T) {
	got := appendScopeFlags(nil, []string{"machine", "list"})
	want := []string{"machine", "list"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_SiteScope_MachineList(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-123", SiteName: "pdx-dev3"}}
	got := appendScopeFlags(s, []string{"machine", "list"})
	want := []string{"machine", "list", "--site-id", "site-123"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_SiteScope_VPCList(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-123"}}
	got := appendScopeFlags(s, []string{"vpc", "list"})
	want := []string{"vpc", "list", "--site-id", "site-123"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_BothScopes_SubnetList(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-123", VpcID: "vpc-456"}}
	got := appendScopeFlags(s, []string{"subnet", "list"})
	want := []string{"subnet", "list", "--site-id", "site-123", "--vpc-id", "vpc-456"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_BothScopes_InstanceList(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-123", VpcID: "vpc-456"}}
	got := appendScopeFlags(s, []string{"instance", "list"})
	want := []string{"instance", "list", "--site-id", "site-123", "--vpc-id", "vpc-456"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_NonListAction_Ignored(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-123"}}
	got := appendScopeFlags(s, []string{"machine", "get"})
	want := []string{"machine", "get"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_UnknownResource_NoFlags(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-123"}}
	got := appendScopeFlags(s, []string{"site", "list"})
	want := []string{"site", "list"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_SinglePart_NoFlags(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-123"}}
	got := appendScopeFlags(s, []string{"help"})
	want := []string{"help"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestAppendScopeFlags_VpcOnlyScope_SubnetList(t *testing.T) {
	s := &Session{Scope: Scope{VpcID: "vpc-456"}}
	got := appendScopeFlags(s, []string{"subnet", "list"})
	want := []string{"subnet", "list", "--vpc-id", "vpc-456"}
	if !equal(got, want) {
		t.Errorf("got %v, want %v", got, want)
	}
}

func TestLogCmd_IncludesScopeFlags(t *testing.T) {
	s := &Session{
		ConfigPath: "/tmp/config.yaml",
		Scope:      Scope{SiteID: "site-123"},
	}
	output := captureStdout(func() {
		LogCmd(s, "machine", "list")
	})
	if !strings.Contains(output, "--site-id site-123") {
		t.Errorf("LogCmd output missing --site-id flag: %q", output)
	}
	if !strings.Contains(output, "--config /tmp/config.yaml") {
		t.Errorf("LogCmd output missing --config flag: %q", output)
	}
	if !strings.Contains(output, "cli") {
		t.Errorf("LogCmd output missing cli: %q", output)
	}
}

func TestLogCmd_NoScope(t *testing.T) {
	s := &Session{}
	output := captureStdout(func() {
		LogCmd(s, "machine", "list")
	})
	if strings.Contains(output, "--site-id") {
		t.Errorf("LogCmd output should not contain --site-id when no scope set: %q", output)
	}
}

func TestCmdInstanceListRendersIPAddresses(t *testing.T) {
	cache := NewCache()
	cache.Set("vpc", []NamedItem{{Name: "VPC One", ID: "vpc-1"}})
	cache.Set("site", []NamedItem{{Name: "Site One", ID: "site-1"}})
	cache.Set("instance", []NamedItem{
		{
			Name: "with-addresses", ID: "instance-1", Status: "Ready",
			Extra: map[string]string{"vpcId": "vpc-1", "siteId": "site-1"},
			Raw: map[string]interface{}{
				"interfaces": []interface{}{
					map[string]interface{}{"ipAddresses": []interface{}{"192.0.2.10"}},
					map[string]interface{}{"ipAddresses": []interface{}{"2001:db8::10"}},
				},
			},
		},
		{
			Name: "without-addresses", ID: "instance-2", Status: "Ready",
			Extra: map[string]string{"vpcId": "vpc-1", "siteId": "site-1"},
			Raw:   map[string]interface{}{"interfaces": []interface{}{}},
		},
		{
			Name: "auto-network", ID: "instance-3", Status: "Ready",
			Extra: map[string]string{"vpcId": "vpc-1", "siteId": "site-1"},
			Raw: map[string]interface{}{
				"interfaces": []interface{}{},
				"status": map[string]interface{}{
					"network": map[string]interface{}{
						"interfaces": []interface{}{
							map[string]interface{}{"ipAddresses": []interface{}{"198.51.100.10"}},
						},
					},
				},
			},
		},
	})
	session := &Session{Cache: cache}
	session.Resolver = NewResolver(cache)

	var runErr error
	output := captureStdout(func() {
		runErr = cmdInstanceList(session, nil)
	})
	require.NoError(t, runErr)

	lines := strings.Split(output, "\n")
	var header, populated, empty, autoNetwork string
	for _, line := range lines {
		switch {
		case strings.HasPrefix(line, "NAME"):
			header = line
		case strings.HasPrefix(line, "with-addresses"):
			populated = line
		case strings.HasPrefix(line, "without-addresses"):
			empty = line
		case strings.HasPrefix(line, "auto-network"):
			autoNetwork = line
		}
	}
	require.NotEmpty(t, header)
	require.NotEmpty(t, populated)
	require.NotEmpty(t, empty)
	require.NotEmpty(t, autoNetwork)

	ipAddressesColumn := strings.Index(header, "IP ADDRESSES")
	statusColumn := strings.Index(header, "STATUS")
	require.Greater(t, ipAddressesColumn, 0)
	require.Greater(t, statusColumn, ipAddressesColumn)
	assert.Equal(t, "192.0.2.10, 2001:db8::10", strings.TrimSpace(populated[ipAddressesColumn:statusColumn]))
	assert.Equal(t, "-", strings.TrimSpace(empty[ipAddressesColumn:statusColumn]))
	assert.Equal(t, "198.51.100.10", strings.TrimSpace(autoNetwork[ipAddressesColumn:statusColumn]))
}

func TestShellQuoteCLIArg(t *testing.T) {
	assert.Equal(t, `'simple'`, shellQuoteCLIArg("simple"))
	assert.Equal(t, `'tenant'\''s instance'`, shellQuoteCLIArg("tenant's instance"))
}

func TestFirstMachineIPAddress(t *testing.T) {
	tests := []struct {
		name string
		raw  interface{}
		want string
	}{
		{
			name: "first available address",
			raw: map[string]interface{}{
				"machineInterfaces": []interface{}{
					map[string]interface{}{"ipAddresses": []interface{}{"192.0.2.10", "192.0.2.11"}},
					map[string]interface{}{"ipAddresses": []interface{}{"192.0.2.12"}},
				},
			},
			want: "192.0.2.10",
		},
		{
			name: "later interface address",
			raw: map[string]interface{}{
				"machineInterfaces": []interface{}{
					map[string]interface{}{"ipAddresses": []interface{}{}},
					map[string]interface{}{"ipAddresses": []interface{}{"198.51.100.20"}},
				},
			},
			want: "198.51.100.20",
		},
		{
			name: "skips malformed entries",
			raw: map[string]interface{}{
				"machineInterfaces": []interface{}{
					"not an interface",
					map[string]interface{}{"ipAddresses": []interface{}{123, "   "}},
					map[string]interface{}{"ipAddresses": []interface{}{"203.0.113.7"}},
				},
			},
			want: "203.0.113.7",
		},
		{
			name: "no address",
			raw: map[string]interface{}{
				"machineInterfaces": []interface{}{
					map[string]interface{}{"ipAddresses": []interface{}{}},
				},
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.want, firstMachineIPAddress(test.raw))
		})
	}
}

func TestCmdMachineListRendersIPAddresses(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/v2/org/acme/nico/machine":
			_, _ = io.WriteString(w, `[
				{"id":"machine-with-ip","name":"with-ip","status":"Ready","siteId":"site-1","machineInterfaces":[{"ipAddresses":["192.0.2.10"]}]},
				{"id":"machine-without-ip","name":"without-ip","status":"Ready","siteId":"site-1","machineInterfaces":[]}
			]`)
		case "/v2/org/acme/nico/vpc", "/v2/org/acme/nico/instance":
			_, _ = io.WriteString(w, `[]`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
	var runErr error
	output := captureStdout(func() {
		runErr = cmdMachineList(session, nil)
	})
	require.NoError(t, runErr)

	lines := strings.Split(output, "\n")
	var header, populated, blank string
	for _, line := range lines {
		switch {
		case strings.HasPrefix(line, "NAME"):
			header = line
		case strings.HasPrefix(line, "machine-with-ip"):
			populated = line
		case strings.HasPrefix(line, "machine-without-ip"):
			blank = line
		}
	}
	require.NotEmpty(t, header)
	require.NotEmpty(t, populated)
	require.NotEmpty(t, blank)

	ipAddressColumn := strings.Index(header, "IP ADDRESS")
	statusColumn := strings.Index(header, "STATUS")
	require.Greater(t, ipAddressColumn, 0)
	require.Greater(t, statusColumn, ipAddressColumn)
	assert.Equal(t, "192.0.2.10", strings.TrimSpace(populated[ipAddressColumn:statusColumn]))
	assert.Empty(t, strings.TrimSpace(blank[ipAddressColumn:statusColumn]))
}

// --- VPC scope coverage tests ---

func TestAppendScopeFlags_SiteOnly(t *testing.T) {
	siteOnlyResources := []string{
		"vpc", "allocation", "ip-block", "operating-system", "ssh-key-group",
		"network-security-group", "sku", "rack", "expected-machine",
		"expected-rack", "expected-switch", "expected-power-shelf", "tray",
		"dpu-extension-service", "infiniband-partition", "nvlink-logical-partition",
	}

	s := &Session{Scope: Scope{SiteID: "site-1", VpcID: "vpc-1"}}

	for _, resource := range siteOnlyResources {
		got := appendScopeFlags(s, []string{resource, "list"})
		if !contains(got, "--site-id") {
			t.Errorf("%s list: expected --site-id flag", resource)
		}
		if contains(got, "--vpc-id") {
			t.Errorf("%s list: should not include --vpc-id flag", resource)
		}
	}
}

func TestAppendScopeFlags_SiteAndVPC(t *testing.T) {
	vpcResources := []string{"subnet", "vpc-prefix", "instance", "machine"}

	s := &Session{Scope: Scope{SiteID: "site-1", VpcID: "vpc-1"}}

	for _, resource := range vpcResources {
		got := appendScopeFlags(s, []string{resource, "list"})
		if !contains(got, "--site-id") {
			t.Errorf("%s list: expected --site-id flag", resource)
		}
		if !contains(got, "--vpc-id") {
			t.Errorf("%s list: expected --vpc-id flag", resource)
		}
	}
}

func TestAppendScopeFlags_NoScope(t *testing.T) {
	s := &Session{Scope: Scope{}}

	got := appendScopeFlags(s, []string{"machine", "list"})
	if contains(got, "--site-id") || contains(got, "--vpc-id") {
		t.Error("empty scope should not produce any flags")
	}
}

func TestAppendScopeFlags_VPCOnlyScope(t *testing.T) {
	s := &Session{Scope: Scope{VpcID: "vpc-1"}}

	got := appendScopeFlags(s, []string{"instance", "list"})
	if contains(got, "--site-id") {
		t.Error("should not include --site-id when SiteID is empty")
	}
	if !contains(got, "--vpc-id") {
		t.Error("expected --vpc-id flag")
	}
}

func TestAppendScopeFlags_NonListAction(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "site-1", VpcID: "vpc-1"}}

	got := appendScopeFlags(s, []string{"machine", "get"})
	if contains(got, "--site-id") || contains(got, "--vpc-id") {
		t.Error("get actions should not have scope flags appended")
	}
}

func TestAppendScopeFlags_UnscopedResources(t *testing.T) {
	unscopedResources := []string{"site", "audit", "ssh-key", "tenant-account"}

	s := &Session{Scope: Scope{SiteID: "site-1", VpcID: "vpc-1"}}

	for _, resource := range unscopedResources {
		got := appendScopeFlags(s, []string{resource, "list"})
		if contains(got, "--site-id") || contains(got, "--vpc-id") {
			t.Errorf("%s list: unscoped resource should not have scope flags", resource)
		}
	}
}

func TestAppendScopeFlags_CoversAllRegisteredFetchers(t *testing.T) {
	scopeFilteredFetchers := []string{
		"vpc", "subnet", "instance", "machine",
		"allocation", "ip-block", "operating-system",
		"ssh-key-group", "network-security-group",
		"sku", "rack", "expected-machine", "vpc-prefix",
		"expected-rack", "expected-switch", "expected-power-shelf", "tray",
		"dpu-extension-service", "infiniband-partition", "nvlink-logical-partition",
	}

	s := &Session{Scope: Scope{SiteID: "site-1", VpcID: "vpc-1"}}

	for _, resource := range scopeFilteredFetchers {
		got := appendScopeFlags(s, []string{resource, "list"})
		if !contains(got, "--site-id") {
			t.Errorf("%s list: scope-filtered fetcher missing from appendScopeFlags", resource)
		}
	}
}

func TestInvalidateFiltered_MatchesScopeFilteredFetchers(t *testing.T) {
	scopeFilteredFetchers := []string{
		"vpc", "subnet", "instance",
		"allocation", "machine", "ip-block", "operating-system",
		"ssh-key-group", "network-security-group",
		"vpc-prefix", "rack", "expected-machine",
		"expected-rack", "expected-switch", "expected-power-shelf", "tray", "sku",
		"dpu-extension-service", "infiniband-partition", "nvlink-logical-partition",
	}

	c := NewCache()
	for _, rt := range scopeFilteredFetchers {
		c.Set(rt, []NamedItem{{Name: rt, ID: rt}})
	}
	c.Set("site", []NamedItem{{Name: "site", ID: "site"}})
	c.Set("audit", []NamedItem{{Name: "audit", ID: "audit"}})

	c.InvalidateFiltered()

	for _, rt := range scopeFilteredFetchers {
		if got := c.Get(rt); got != nil {
			t.Errorf("InvalidateFiltered did not clear scope-filtered type %q", rt)
		}
	}
	if c.Get("site") == nil {
		t.Error("InvalidateFiltered should not clear unscoped type site")
	}
	if c.Get("audit") == nil {
		t.Error("InvalidateFiltered should not clear unscoped type audit")
	}
}

func TestAppendScopeFlags_ScopeFlagCategories_Consistent(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "s", VpcID: "v"}}

	vpcFilteredInFetchers := map[string]bool{
		"subnet": true, "instance": true, "vpc-prefix": true, "machine": true,
	}

	allScoped := []string{
		"vpc", "subnet", "instance", "machine",
		"allocation", "ip-block", "operating-system",
		"ssh-key-group", "network-security-group",
		"sku", "rack", "expected-machine", "vpc-prefix",
		"expected-rack", "expected-switch", "expected-power-shelf", "tray",
		"dpu-extension-service", "infiniband-partition", "nvlink-logical-partition",
	}

	for _, resource := range allScoped {
		got := appendScopeFlags(s, []string{resource, "list"})
		hasVpc := contains(got, "--vpc-id")
		expectVpc := vpcFilteredInFetchers[resource]
		if hasVpc != expectVpc {
			t.Errorf("%s: appendScopeFlags vpc-id=%v but fetcher expects vpc=%v", resource, hasVpc, expectVpc)
		}
	}
}

func TestAllCommands_HaveUniqueNames(t *testing.T) {
	commands := AllCommands()
	seen := map[string]bool{}
	for _, cmd := range commands {
		if seen[cmd.Name] {
			t.Errorf("duplicate command name: %s", cmd.Name)
		}
		seen[cmd.Name] = true
	}
}

func TestInvalidateFiltered_ListMatchesAppendScopeFlags(t *testing.T) {
	s := &Session{Scope: Scope{SiteID: "s"}}

	c := NewCache()
	allTypes := []string{
		"vpc", "subnet", "instance",
		"allocation", "machine", "ip-block", "operating-system",
		"ssh-key-group", "network-security-group",
		"vpc-prefix", "rack", "expected-machine",
		"expected-rack", "expected-switch", "expected-power-shelf", "tray", "sku",
		"dpu-extension-service", "infiniband-partition", "nvlink-logical-partition",
		"site", "audit", "ssh-key", "tenant-account",
	}
	for _, rt := range allTypes {
		c.Set(rt, []NamedItem{{Name: rt}})
	}
	c.InvalidateFiltered()

	var invalidated, preserved []string
	for _, rt := range allTypes {
		if c.Get(rt) == nil {
			invalidated = append(invalidated, rt)
		} else {
			preserved = append(preserved, rt)
		}
	}

	for _, rt := range invalidated {
		got := appendScopeFlags(s, []string{rt, "list"})
		if !contains(got, "--site-id") {
			t.Errorf("type %q is invalidated by InvalidateFiltered but not handled by appendScopeFlags", rt)
		}
	}

	for _, rt := range preserved {
		got := appendScopeFlags(s, []string{rt, "list"})
		if contains(got, "--site-id") || contains(got, "--vpc-id") {
			t.Errorf("type %q is preserved by InvalidateFiltered but has scope flags in appendScopeFlags", rt)
		}
	}
}

func TestReadyMachineItemsForSite_FiltersByStatusAndSite(t *testing.T) {
	machines := []NamedItem{
		{Name: "m1", ID: "1", Status: "Ready", Extra: map[string]string{"siteId": "site-a"}},
		{Name: "m2", ID: "2", Status: "ready", Extra: map[string]string{"siteId": "site-a"}},
		{Name: "m3", ID: "3", Status: "NotReady", Extra: map[string]string{"siteId": "site-a"}},
		{Name: "m4", ID: "4", Status: "Ready", Extra: map[string]string{"siteId": "site-b"}},
	}

	got := readyMachineItemsForSite(machines, "site-a")
	require.Len(t, got, 2)
	assert.Equal(t, "1", got[0].ID)
	assert.Equal(t, "2", got[1].ID)
	// Labels must surface BOTH the display name (which often falls back to
	// serial number when no friendly machine label is set) AND the full
	// machine ID, so users have something stable to copy/paste even when
	// every machine in the list shares an opaque serial-number prefix.
	assert.Contains(t, got[0].Label, "m1", "label must include display name")
	assert.Contains(t, got[0].Label, "1", "label must include machine ID")
}

func TestMachineSelectLabel(t *testing.T) {
	cases := []struct {
		name       string
		item       NamedItem
		wantSubstr []string
		wantExact  string
	}{
		{
			name:       "name and id present",
			item:       NamedItem{Name: "host-01", ID: "id-abc-123"},
			wantSubstr: []string{"host-01", "id-abc-123"},
		},
		{
			name:      "only id present (no display name resolved)",
			item:      NamedItem{Name: "", ID: "id-abc-123"},
			wantExact: "id-abc-123",
		},
		{
			name:      "only name present (defensive: should not happen in practice)",
			item:      NamedItem{Name: "host-01", ID: ""},
			wantExact: "host-01",
		},
		{
			name:       "whitespace name and id",
			item:       NamedItem{Name: "  host-01  ", ID: "  id-abc  "},
			wantSubstr: []string{"host-01", "id-abc"},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := machineSelectLabel(tc.item)
			if tc.wantExact != "" {
				assert.Equal(t, tc.wantExact, got)
				return
			}
			for _, s := range tc.wantSubstr {
				assert.Contains(t, got, s, "label must contain %q", s)
			}
		})
	}
}

func TestInstanceAttributeUpdateInputs_AttributeBody(t *testing.T) {
	cases := []struct {
		name   string
		inputs instanceAttributeUpdateInputs
		want   map[string]interface{}
	}{
		{
			name:   "empty inputs produce empty body",
			inputs: instanceAttributeUpdateInputs{},
			want:   map[string]interface{}{},
		},
		{
			name:   "name and description trimmed",
			inputs: instanceAttributeUpdateInputs{name: "  new-name  ", description: " new description "},
			want: map[string]interface{}{
				"name":        "new-name",
				"description": "new description",
			},
		},
		{
			name:   "blank name and description omitted",
			inputs: instanceAttributeUpdateInputs{name: "   ", description: ""},
			want:   map[string]interface{}{},
		},
		{
			name:   "ssh key group ids included only when non-empty",
			inputs: instanceAttributeUpdateInputs{sshKeyGroupIDs: []string{"g1", "g2"}},
			want:   map[string]interface{}{"sshKeyGroupIds": []string{"g1", "g2"}},
		},
		{
			name: "all attribute fields included without reboot fields",
			inputs: instanceAttributeUpdateInputs{
				name:           "new-name",
				description:    "new desc",
				osID:           "os-1",
				sshKeyGroupIDs: []string{"g1"},
			},
			want: map[string]interface{}{
				"name":              "new-name",
				"description":       "new desc",
				"operatingSystemId": "os-1",
				"sshKeyGroupIds":    []string{"g1"},
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := tc.inputs.attributeBody()
			// Compare via JSON round-trip so []string and []interface{} are
			// treated as equal when their contents match -- keeps the table
			// readable without forcing every test row to use interface{} slices.
			gotJSON, err := json.Marshal(got)
			require.NoError(t, err)
			wantJSON, err := json.Marshal(tc.want)
			require.NoError(t, err)
			assert.JSONEq(t, string(wantJSON), string(gotJSON))
		})
	}
}

func TestInstanceRebootInputs_RebootBody(t *testing.T) {
	cases := []struct {
		name   string
		inputs instanceRebootInputs
		want   map[string]interface{}
	}{
		{
			// A zero-value instanceRebootInputs means a plain reboot. Its body
			// must still contain triggerReboot=true so the PATCH is not a no-op.
			name:   "plain reboot",
			inputs: instanceRebootInputs{},
			want:   map[string]interface{}{"triggerReboot": true},
		},
		{
			name:   "custom ipxe only",
			inputs: instanceRebootInputs{rebootWithCustomIpxe: true},
			want: map[string]interface{}{
				"triggerReboot":        true,
				"rebootWithCustomIpxe": true,
			},
		},
		{
			name:   "apply updates only",
			inputs: instanceRebootInputs{applyUpdatesOnReboot: true},
			want: map[string]interface{}{
				"triggerReboot":        true,
				"applyUpdatesOnReboot": true,
			},
		},
		{
			name: "custom ipxe and apply updates",
			inputs: instanceRebootInputs{
				rebootWithCustomIpxe: true,
				applyUpdatesOnReboot: true,
			},
			want: map[string]interface{}{
				"triggerReboot":        true,
				"rebootWithCustomIpxe": true,
				"applyUpdatesOnReboot": true,
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, tc.inputs.rebootBody())
		})
	}
}

func TestAllCommands_HasInstanceUpdateAndReboot(t *testing.T) {
	// Regression guard: the TUI command registry must expose
	// `instance update` (so users can rename, swap OS, rotate ssh key
	// groups) and `instance reboot` as a distinct operation.
	commands := make(map[string]Command)
	for _, c := range AllCommands() {
		commands[c.Name] = c
	}
	assert.Contains(t, commands, "instance update", "TUI must expose `instance update`")
	assert.Contains(t, commands, "instance reboot", "TUI must expose `instance reboot`")
	assert.NotContains(t, commands["instance update"].Description, "reboot")
	assert.Contains(t, commands["instance reboot"].Description, "Reboot")
}

func TestSetSiteScopeFromID_UpdatesScopeAndInvalidatesFiltered(t *testing.T) {
	c := NewCache()
	c.Set("site", []NamedItem{{Name: "Site Two", ID: "site-2"}})
	c.Set("machine", []NamedItem{{Name: "m1", ID: "1"}})
	s := &Session{
		Scope:    Scope{SiteID: "site-1", SiteName: "Site One", VpcID: "vpc-1", VpcName: "VPC One"},
		Cache:    c,
		Resolver: NewResolver(c),
	}

	setSiteScopeFromID(s, "site-2")

	assert.Equal(t, "site-2", s.Scope.SiteID)
	assert.Equal(t, "Site Two", s.Scope.SiteName)
	assert.Empty(t, s.Scope.VpcID, "VPC scope must be cleared when site changes")
	assert.Empty(t, s.Scope.VpcName, "VPC name must be cleared when site changes")
	assert.Nil(t, c.Get("machine"), "filtered cache must be invalidated")
}

func TestSetSiteScopeFromID_NoChangeKeepsFilteredCache(t *testing.T) {
	c := NewCache()
	c.Set("machine", []NamedItem{{Name: "m1", ID: "1"}})
	s := &Session{
		Scope:    Scope{SiteID: "site-1", SiteName: "Site One", VpcID: "vpc-1"},
		Cache:    c,
		Resolver: NewResolver(c),
	}

	setSiteScopeFromID(s, "site-1")

	assert.NotNil(t, c.Get("machine"), "machine cache should remain when scope site does not change")
	assert.Equal(t, "vpc-1", s.Scope.VpcID, "VPC scope should remain when site does not change")
}

// --- Label support tests ---

func TestExtractLabels(t *testing.T) {
	t.Run("valid map", func(t *testing.T) {
		m := map[string]interface{}{
			"labels": map[string]interface{}{"env": "prod", "rack": "A3"},
		}
		got := extractLabels(m)
		require.Len(t, got, 2)
		assert.Equal(t, "prod", got["env"])
		assert.Equal(t, "A3", got["rack"])
	})
	t.Run("nil labels", func(t *testing.T) {
		m := map[string]interface{}{"name": "test"}
		assert.Nil(t, extractLabels(m))
	})
	t.Run("non-string values ignored", func(t *testing.T) {
		m := map[string]interface{}{
			"labels": map[string]interface{}{"env": "prod", "count": 42},
		}
		got := extractLabels(m)
		require.Len(t, got, 1)
		assert.Equal(t, "prod", got["env"])
	})
	t.Run("empty map", func(t *testing.T) {
		m := map[string]interface{}{
			"labels": map[string]interface{}{},
		}
		assert.Nil(t, extractLabels(m))
	})
}

func TestFormatLabels(t *testing.T) {
	t.Run("empty", func(t *testing.T) {
		assert.Equal(t, "", formatLabels(nil, 60))
	})
	t.Run("single", func(t *testing.T) {
		assert.Equal(t, "env=prod", formatLabels(map[string]string{"env": "prod"}, 60))
	})
	t.Run("multiple sorted", func(t *testing.T) {
		assert.Equal(t, "env=prod, rack=A3", formatLabels(map[string]string{"rack": "A3", "env": "prod"}, 60))
	})
	t.Run("truncation", func(t *testing.T) {
		got := formatLabels(map[string]string{"env": "production", "rack": "A3"}, 15)
		assert.LessOrEqual(t, len(got), 15)
		assert.True(t, strings.HasSuffix(got, "..."), "expected truncation suffix, got %q", got)
	})
	t.Run("no truncation when fits", func(t *testing.T) {
		got := formatLabels(map[string]string{"a": "b"}, 60)
		assert.False(t, strings.HasSuffix(got, "..."), "should not truncate short label: %q", got)
	})
}

func TestFilterByLabels(t *testing.T) {
	items := []NamedItem{
		{Name: "a", Labels: map[string]string{"env": "prod", "rack": "A3"}},
		{Name: "b", Labels: map[string]string{"env": "dev"}},
		{Name: "c", Labels: nil},
		{Name: "d", Labels: map[string]string{"env": "prod", "rack": "B1"}},
	}

	t.Run("no filters", func(t *testing.T) {
		assert.Len(t, filterByLabels(items, nil), 4)
	})
	t.Run("single match", func(t *testing.T) {
		got := filterByLabels(items, map[string]string{"env": "dev"})
		require.Len(t, got, 1)
		assert.Equal(t, "b", got[0].Name)
	})
	t.Run("multi-key AND", func(t *testing.T) {
		got := filterByLabels(items, map[string]string{"env": "prod", "rack": "A3"})
		require.Len(t, got, 1)
		assert.Equal(t, "a", got[0].Name)
	})
	t.Run("no match", func(t *testing.T) {
		assert.Empty(t, filterByLabels(items, map[string]string{"env": "staging"}))
	})
	t.Run("nil labels handled", func(t *testing.T) {
		got := filterByLabels(items, map[string]string{"env": "prod"})
		for _, item := range got {
			assert.NotNil(t, item.Labels, "nil-label item should not pass filter")
		}
	})
}

func TestSortByLabelKey(t *testing.T) {
	t.Run("ascending sort", func(t *testing.T) {
		items := []NamedItem{
			{Name: "c", Labels: map[string]string{"rack": "C1"}},
			{Name: "a", Labels: map[string]string{"rack": "A1"}},
			{Name: "b", Labels: map[string]string{"rack": "B1"}},
		}
		sorted := sortByLabelKey(items, "rack")
		require.Len(t, sorted, 3)
		assert.Equal(t, "a", sorted[0].Name)
		assert.Equal(t, "b", sorted[1].Name)
		assert.Equal(t, "c", sorted[2].Name)
		assert.Equal(t, "c", items[0].Name, "sortByLabelKey must not mutate the original slice")
	})
	t.Run("missing keys sort last", func(t *testing.T) {
		items := []NamedItem{
			{Name: "no-label", Labels: nil},
			{Name: "has-label", Labels: map[string]string{"rack": "A1"}},
		}
		sorted := sortByLabelKey(items, "rack")
		assert.Equal(t, "has-label", sorted[0].Name)
		assert.Equal(t, "no-label", sorted[1].Name)
	})
	t.Run("stable order for equal values", func(t *testing.T) {
		items := []NamedItem{
			{Name: "first", Labels: map[string]string{"rack": "A1"}},
			{Name: "second", Labels: map[string]string{"rack": "A1"}},
		}
		sorted := sortByLabelKey(items, "rack")
		assert.Equal(t, "first", sorted[0].Name)
		assert.Equal(t, "second", sorted[1].Name)
	})
}

func TestParseLabelArgs(t *testing.T) {
	t.Run("label and sort-label", func(t *testing.T) {
		remaining, labels, sortKey, err := parseLabelArgs([]string{"--label", "env=prod", "--sort-label", "rack", "extra"})
		require.NoError(t, err)
		assert.Equal(t, []string{"extra"}, remaining)
		assert.Equal(t, "prod", labels["env"])
		assert.Equal(t, "rack", sortKey)
	})
	t.Run("no label args", func(t *testing.T) {
		remaining, labels, sortKey, err := parseLabelArgs([]string{"foo", "bar"})
		require.NoError(t, err)
		assert.Len(t, remaining, 2)
		assert.Empty(t, labels)
		assert.Empty(t, sortKey)
	})
	t.Run("multiple labels AND", func(t *testing.T) {
		_, labels, _, err := parseLabelArgs([]string{"--label", "env=prod", "--label", "rack=A3"})
		require.NoError(t, err)
		require.Len(t, labels, 2)
		assert.Equal(t, "prod", labels["env"])
		assert.Equal(t, "A3", labels["rack"])
	})
	t.Run("label without equals", func(t *testing.T) {
		_, _, _, err := parseLabelArgs([]string{"--label", "env"})
		assert.Error(t, err)
	})
	t.Run("dangling sort-label", func(t *testing.T) {
		_, _, _, err := parseLabelArgs([]string{"--sort-label"})
		assert.Error(t, err)
	})
	t.Run("sort-label rejects another option", func(t *testing.T) {
		remaining, labels, sortKey, err := parseLabelArgs([]string{"--sort-label", "--label", "env=prod"})
		require.Error(t, err)
		assert.Nil(t, remaining)
		assert.Nil(t, labels)
		assert.Empty(t, sortKey)
	})
	t.Run("label rejects another option", func(t *testing.T) {
		remaining, labels, sortKey, err := parseLabelArgs([]string{"--label", "--sort-label", "rack"})
		require.Error(t, err)
		assert.Nil(t, remaining)
		assert.Nil(t, labels)
		assert.Empty(t, sortKey)
	})
	t.Run("dangling label flag", func(t *testing.T) {
		_, _, _, err := parseLabelArgs([]string{"--label"})
		assert.Error(t, err)
	})
	t.Run("conflicting same-key labels", func(t *testing.T) {
		_, _, _, err := parseLabelArgs([]string{"--label", "env=prod", "--label", "env=dev"})
		require.Error(t, err)
		assert.Contains(t, err.Error(), "conflicting")
	})
	t.Run("duplicate same-value labels accepted", func(t *testing.T) {
		_, labels, _, err := parseLabelArgs([]string{"--label", "env=prod", "--label", "env=prod"})
		require.NoError(t, err)
		assert.Equal(t, "prod", labels["env"])
	})
}

func TestMergeLabels(t *testing.T) {
	t.Run("both nil", func(t *testing.T) {
		got, err := mergeLabels(nil, nil)
		require.NoError(t, err)
		assert.Nil(t, got)
	})
	t.Run("conflicting scope and cmd", func(t *testing.T) {
		scope := map[string]string{"env": "dev"}
		cmd := map[string]string{"env": "prod"}
		_, err := mergeLabels(scope, cmd)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "conflicts")
	})
	t.Run("same value allowed", func(t *testing.T) {
		scope := map[string]string{"env": "prod"}
		cmd := map[string]string{"env": "prod"}
		got, err := mergeLabels(scope, cmd)
		require.NoError(t, err)
		assert.Equal(t, "prod", got["env"])
	})
	t.Run("combines unique keys", func(t *testing.T) {
		scope := map[string]string{"env": "prod"}
		cmd := map[string]string{"rack": "A3"}
		got, err := mergeLabels(scope, cmd)
		require.NoError(t, err)
		require.Len(t, got, 2)
		assert.Equal(t, "prod", got["env"])
		assert.Equal(t, "A3", got["rack"])
	})
}

func TestPrintLabelHint(t *testing.T) {
	itemsWithLabels := []NamedItem{
		{Name: "m1", Labels: map[string]string{"RackIdentifier": "H19", "ServerName": "pdx01"}},
		{Name: "m2", Labels: map[string]string{"RackIdentifier": "H20"}},
	}
	t.Run("active filter suppresses hint", func(t *testing.T) {
		var buf bytes.Buffer
		printLabelHint(&buf, itemsWithLabels, map[string]string{"RackIdentifier": "H19"})
		assert.Empty(t, buf.String())
	})
	t.Run("no labels means no hint", func(t *testing.T) {
		var buf bytes.Buffer
		printLabelHint(&buf, []NamedItem{{Name: "x"}}, nil)
		assert.Empty(t, buf.String())
	})
	t.Run("whitespace-only label keys do not trigger hint", func(t *testing.T) {
		var buf bytes.Buffer
		items := []NamedItem{{Name: "a", Labels: map[string]string{"": "blank", "  ": "spaces"}}}
		printLabelHint(&buf, items, nil)
		assert.Empty(t, buf.String(), "blank/whitespace keys must not be treated as real labels")
	})
	t.Run("hint uses placeholders not real keys", func(t *testing.T) {
		var buf bytes.Buffer
		printLabelHint(&buf, itemsWithLabels, nil)
		out := buf.String()
		assert.Contains(t, out, "--label <key>=<value>")
		assert.Contains(t, out, "--sort-label <key>")
		assert.Contains(t, out, "scope label <key>=<value>")
		assert.NotContains(t, out, "RackIdentifier", "hint must not surface real keys from the result set")
		assert.NotContains(t, out, "ServerName", "hint must not surface real keys from the result set")
		assert.NotContains(t, out, "Label keys:", "no per-result key listing should be printed")
	})
	t.Run("hint output is exactly one line", func(t *testing.T) {
		var buf bytes.Buffer
		printLabelHint(&buf, itemsWithLabels, nil)
		assert.Equal(t, 1, strings.Count(buf.String(), "\n"), "hint should be a single line")
	})
	t.Run("empty filter map still shows hint", func(t *testing.T) {
		var buf bytes.Buffer
		printLabelHint(&buf, itemsWithLabels, map[string]string{})
		assert.NotEmpty(t, buf.String(), "empty (non-nil) filter map should be treated as no filter")
	})
}

func TestInvalidateFilteredIncludesInstanceType(t *testing.T) {
	c := NewCache()
	c.Set("instance-type", []NamedItem{{Name: "it1", ID: "1"}})
	c.InvalidateFiltered()
	assert.Nil(t, c.Get("instance-type"), "instance-type cache should be invalidated by InvalidateFiltered")
}

func TestAppendScopeFlagsIncludesInstanceType(t *testing.T) {
	s := &Session{
		Scope: Scope{SiteID: "site-1"},
		Cache: NewCache(),
	}
	s.Resolver = NewResolver(s.Cache)
	got := appendScopeFlags(s, []string{"instance-type", "list"})
	assert.True(t, contains(got, "--site-id"), "instance-type should receive --site-id scope flag")
}

func TestVPCFilteringDoesNotMutateCachedSlice(t *testing.T) {
	original := []NamedItem{
		{Name: "m1", ID: "1"},
		{Name: "m2", ID: "2"},
		{Name: "m3", ID: "3"},
	}
	cached := make([]NamedItem, len(original))
	copy(cached, original)

	vpcMembers := map[string]string{"1": "vpc-a"}
	filtered := make([]NamedItem, 0, len(cached))
	for _, item := range cached {
		if _, ok := vpcMembers[item.ID]; ok {
			filtered = append(filtered, item)
		}
	}

	require.Len(t, filtered, 1)
	assert.Equal(t, "m1", filtered[0].Name)
	require.Len(t, cached, 3, "cached slice must not be truncated by filtering")
	assert.Equal(t, "m1", cached[0].Name)
	assert.Equal(t, "m2", cached[1].Name)
	assert.Equal(t, "m3", cached[2].Name)
}

func TestValidateIPv4SubnetPrefixLength(t *testing.T) {
	// Keep these client-side bounds aligned with SubnetCreateRequest in
	// openapi/spec.yaml.
	tests := []struct {
		name         string
		prefixLength int
		wantError    bool
	}{
		{name: "below minimum", prefixLength: 7, wantError: true},
		{name: "minimum", prefixLength: 8},
		{name: "maximum", prefixLength: 30},
		{name: "above maximum", prefixLength: 31, wantError: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := validateIPv4SubnetPrefixLength(test.prefixLength)
			if test.wantError {
				require.EqualError(t, err, "prefix length must be between 8 and 30")
				return
			}
			require.NoError(t, err)
		})
	}
}

func TestFilterSubnetVPCs(t *testing.T) {
	tests := []struct {
		name         string
		vpc          NamedItem
		wantIncluded bool
	}{
		{
			name: "Ready ETHERNET_VIRTUALIZER included",
			vpc: NamedItem{
				Name:   "Ethernet virtualizer VPC",
				ID:     "vpc-etv",
				Status: "Ready",
				Extra:  map[string]string{"networkVirtualizationType": "ETHERNET_VIRTUALIZER"},
			},
			wantIncluded: true,
		},
		{
			name: "Ready FNN excluded",
			vpc: NamedItem{
				Name:   "FNN VPC",
				ID:     "vpc-fnn",
				Status: "Ready",
				Extra:  map[string]string{"networkVirtualizationType": "FNN"},
			},
		},
		{
			name: "pending ETHERNET_VIRTUALIZER excluded",
			vpc: NamedItem{
				Name:   "Pending Ethernet virtualizer VPC",
				ID:     "vpc-pending",
				Status: "Pending",
				Extra:  map[string]string{"networkVirtualizationType": "ETHERNET_VIRTUALIZER"},
			},
		},
		{
			name: "Ready legacy VPC without type included",
			vpc: NamedItem{
				Name:   "Legacy VPC",
				ID:     "vpc-legacy",
				Status: "Ready",
				Extra:  map[string]string{},
			},
			wantIncluded: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := filterSubnetVPCs([]NamedItem{test.vpc})
			if !test.wantIncluded {
				assert.Empty(t, got)
				return
			}
			require.Equal(t, []NamedItem{test.vpc}, got)
		})
	}
}

func TestBuildSubnetIPBlockSelectItems(t *testing.T) {
	tests := []struct {
		name         string
		block        NamedItem
		siteID       string
		wantIncluded bool
	}{
		{
			name: "same-site current-tenant IPv4 included",
			block: NamedItem{
				Name:   "IPv4 block",
				ID:     "ipv4-same-site",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-1", "tenantId": "tenant-1", "protocolVersion": "IPv4"},
			},
			siteID:       "site-1",
			wantIncluded: true,
		},
		{
			name: "same-site current-tenant IPv6 excluded",
			block: NamedItem{
				Name:   "IPv6 block",
				ID:     "ipv6-same-site",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-1", "tenantId": "tenant-1", "protocolVersion": "IPv6"},
			},
			siteID: "site-1",
		},
		{
			name: "other-site current-tenant IPv4 excluded",
			block: NamedItem{
				Name:   "Other site IPv4 block",
				ID:     "ipv4-other-site",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-2", "tenantId": "tenant-1", "protocolVersion": "IPv4"},
			},
			siteID: "site-1",
		},
		{
			name: "provider-owned IPv4 excluded",
			block: NamedItem{
				Name:   "Provider IPv4 block",
				ID:     "ipv4-provider",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-1", "protocolVersion": "IPv4"},
			},
			siteID: "site-1",
		},
		{
			name: "other-tenant IPv4 excluded",
			block: NamedItem{
				Name:   "Other tenant IPv4 block",
				ID:     "ipv4-other-tenant",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-1", "tenantId": "tenant-2", "protocolVersion": "IPv4"},
			},
			siteID: "site-1",
		},
		{
			name: "pending current-tenant IPv4 excluded",
			block: NamedItem{
				Name:   "Pending IPv4 block",
				ID:     "ipv4-pending",
				Status: "Pending",
				Extra:  map[string]string{"siteId": "site-1", "tenantId": "tenant-1", "protocolVersion": "IPv4"},
			},
			siteID: "site-1",
		},
		{
			name: "empty site scope accepts current-tenant IPv4",
			block: NamedItem{
				Name:   "IPv4 block",
				ID:     "ipv4-without-site-scope",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-1", "tenantId": "tenant-1", "protocolVersion": "IPv4"},
			},
			wantIncluded: true,
		},
		{
			name: "missing name uses ID as label",
			block: NamedItem{
				ID:     "ipv4-unnamed",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-1", "tenantId": "tenant-1", "protocolVersion": "IPv4"},
			},
			siteID:       "site-1",
			wantIncluded: true,
		},
		{
			name: "missing ID excluded",
			block: NamedItem{
				Name:   "ID-less IPv4 block",
				Status: "Ready",
				Extra:  map[string]string{"siteId": "site-1", "tenantId": "tenant-1", "protocolVersion": "IPv4"},
			},
			siteID: "site-1",
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			items := buildSubnetIPBlockSelectItems([]NamedItem{test.block}, test.siteID, "tenant-1")
			if !test.wantIncluded {
				assert.Empty(t, items)
				return
			}
			require.Len(t, items, 1)
			label := test.block.Name
			if label == "" {
				label = test.block.ID
			}
			assert.Equal(t, SelectItem{Label: label, ID: test.block.ID}, items[0])
		})
	}
}

func TestBuildIPBlockCreateBody_UsesAPIFieldNames(t *testing.T) {
	body := buildIPBlockCreateBody(
		"ip-block-0",
		"37c3a99c-f5de-4202-8f46-2e1cc24226f6",
		"7.243.96.128",
		25,
		"IPv4",
		"DatacenterOnly",
	)

	assert.Equal(t, "ip-block-0", body["name"])
	assert.Equal(t, "37c3a99c-f5de-4202-8f46-2e1cc24226f6", body["siteId"])
	assert.Equal(t, "7.243.96.128", body["prefix"])
	assert.Equal(t, 25, body["prefixLength"])
	assert.Equal(t, "IPv4", body["protocolVersion"])
	assert.Equal(t, "DatacenterOnly", body["routingType"])

	_, hasIPVersion := body["ipVersion"]
	assert.False(t, hasIPVersion, "request must not use the legacy ipVersion field")
	_, hasUsageType := body["usageType"]
	assert.False(t, hasUsageType, "request must not use the legacy usageType field")
}

func TestValidateIPBlockPrefixLength(t *testing.T) {
	t.Run("IPv4 in range", func(t *testing.T) {
		require.NoError(t, validateIPBlockPrefixLength("IPv4", 1))
		require.NoError(t, validateIPBlockPrefixLength("IPv4", 25))
		require.NoError(t, validateIPBlockPrefixLength("IPv4", 32))
	})
	t.Run("IPv4 out of range", func(t *testing.T) {
		require.Error(t, validateIPBlockPrefixLength("IPv4", 0))
		require.Error(t, validateIPBlockPrefixLength("IPv4", 33))
	})
	t.Run("IPv6 in range", func(t *testing.T) {
		require.NoError(t, validateIPBlockPrefixLength("IPv6", 1))
		require.NoError(t, validateIPBlockPrefixLength("IPv6", 64))
		require.NoError(t, validateIPBlockPrefixLength("IPv6", 128))
	})
	t.Run("IPv6 out of range", func(t *testing.T) {
		require.Error(t, validateIPBlockPrefixLength("IPv6", 0))
		require.Error(t, validateIPBlockPrefixLength("IPv6", 129))
	})
	t.Run("unsupported protocol", func(t *testing.T) {
		require.Error(t, validateIPBlockPrefixLength("IPv5", 16))
	})
}

func TestPromptChoice(t *testing.T) {
	t.Run("exact match", func(t *testing.T) {
		got, err := withStdin(t, "IPv6\n", func() (string, error) {
			return PromptChoice("Protocol", []string{"IPv4", "IPv6"}, "IPv4")
		})
		require.NoError(t, err)
		assert.Equal(t, "IPv6", got)
	})
	t.Run("case insensitive returns canonical", func(t *testing.T) {
		got, err := withStdin(t, "datacenteronly\n", func() (string, error) {
			return PromptChoice("Routing", []string{"DatacenterOnly", "Public"}, "")
		})
		require.NoError(t, err)
		assert.Equal(t, "DatacenterOnly", got)
	})
	t.Run("empty uses default", func(t *testing.T) {
		got, err := withStdin(t, "\n", func() (string, error) {
			return PromptChoice("Protocol", []string{"IPv4", "IPv6"}, "IPv4")
		})
		require.NoError(t, err)
		assert.Equal(t, "IPv4", got)
	})
	t.Run("retries invalid input then accepts valid", func(t *testing.T) {
		got, err := withStdin(t, "nope\nIPv4\n", func() (string, error) {
			return PromptChoice("Protocol", []string{"IPv4", "IPv6"}, "")
		})
		require.NoError(t, err)
		assert.Equal(t, "IPv4", got)
	})
	t.Run("rejects default not in options", func(t *testing.T) {
		_, err := withStdin(t, "\n", func() (string, error) {
			return PromptChoice("Protocol", []string{"IPv4", "IPv6"}, "IPv5")
		})
		require.Error(t, err)
		assert.Contains(t, err.Error(), "not in allowed options")
	})
}

func TestPromptSequenceDoesNotConsumeLaterPipedLines(t *testing.T) {
	got, err := withStdin(t, "value\ny\n", func() (string, error) {
		value, promptErr := PromptText("Value", true)
		if promptErr != nil {
			return "", promptErr
		}
		confirmed, promptErr := PromptConfirm("Continue?")
		if promptErr != nil {
			return "", promptErr
		}
		return fmt.Sprintf("%s:%t", value, confirmed), nil
	})
	require.NoError(t, err)
	assert.Equal(t, "value:true", got)
}

func TestReadPromptLinePreservesNonEOFReadError(t *testing.T) {
	oldStdin := os.Stdin
	reader, writer, err := os.Pipe()
	require.NoError(t, err)
	require.NoError(t, writer.Close())
	require.NoError(t, reader.Close())
	os.Stdin = reader
	defer func() {
		os.Stdin = oldStdin
	}()

	_, err = readPromptLine()

	require.Error(t, err)
	assert.Contains(t, err.Error(), "reading input:")
	assert.NotContains(t, err.Error(), "input cancelled")
}

// withStdin pipes the provided input into os.Stdin for the duration of f,
// captures stdout so the prompt text does not leak into test output, and
// restores both when it returns. All four pipe ends are closed before
// returning so repeated test runs do not accumulate file descriptors.
func withStdin(t *testing.T, input string, f func() (string, error)) (string, error) {
	t.Helper()
	oldStdin := os.Stdin
	oldStdout := os.Stdout
	r, w, err := os.Pipe()
	require.NoError(t, err)
	sr, sw, err := os.Pipe()
	require.NoError(t, err)
	os.Stdin = r
	os.Stdout = sw
	defer func() {
		_ = r.Close()
		_ = sr.Close()
		os.Stdin = oldStdin
		os.Stdout = oldStdout
	}()
	go func() {
		defer w.Close()
		_, _ = io.WriteString(w, input)
	}()
	result, rerr := f()
	_ = sw.Close()
	_, _ = io.Copy(io.Discard, sr)
	return result, rerr
}

// --- Lifecycle and task command tests ---

func TestTaskIDFromArgsOrPrompt_ArgWins(t *testing.T) {
	got, err := taskIDFromArgsOrPrompt([]string{"  abc-123  "}, "Task ID")
	require.NoError(t, err)
	assert.Equal(t, "abc-123", got)
}

func TestTaskIDFromArgsOrPrompt_PromptsWhenNoArg(t *testing.T) {
	got, err := withStdin(t, "task-from-prompt\n", func() (string, error) {
		return taskIDFromArgsOrPrompt(nil, "Task ID")
	})
	require.NoError(t, err)
	assert.Equal(t, "task-from-prompt", got)
}

func TestTaskIDFromArgsOrPrompt_RejectsEmptyArg(t *testing.T) {
	got, err := withStdin(t, "\n", func() (string, error) {
		return taskIDFromArgsOrPrompt([]string{"   "}, "Task ID")
	})
	require.Error(t, err)
	assert.Empty(t, got)
}

func TestPrintTaskIDs_PrintsFromTaskIDsResponse(t *testing.T) {
	body := []byte(`{"taskIds":["t1","t2","t3"],"siteId":"s-1"}`)
	out := captureStdout(func() {
		_ = printTaskIDs(body, "Rack power")
	})
	assert.Contains(t, out, "Rack power started; 3 task(s):")
	assert.Contains(t, out, "t1")
	assert.Contains(t, out, "t2")
	assert.Contains(t, out, "t3")
	assert.Contains(t, out, `"taskIds"`)
}

func TestPrintTaskIDs_HandlesEmptyTaskIDs(t *testing.T) {
	body := []byte(`{"taskIds":[]}`)
	out := captureStdout(func() {
		_ = printTaskIDs(body, "Rack bringup")
	})
	assert.NotContains(t, out, "started")
	assert.Contains(t, out, `"taskIds"`)
}

func TestPowerStateChoices_MatchOpenAPI(t *testing.T) {
	expected := []string{"on", "off", "cycle", "forceoff", "forcecycle"}
	assert.Equal(t, expected, powerStateChoices,
		"powerStateChoices must match UpdatePowerStateRequest.state enum from openapi/spec.yaml")
}

func TestAllCommands_HasLifecycleAndTaskCommands(t *testing.T) {
	commands := AllCommands()
	names := make(map[string]bool, len(commands))
	for _, cmd := range commands {
		names[cmd.Name] = true
	}
	want := []string{
		"tray list", "tray get",
		"tray power", "tray firmware", "tray validate",
		"rack bringup", "rack power", "rack firmware", "rack validate",
		"rack task get", "rack task cancel",
	}
	for _, n := range want {
		assert.True(t, names[n], "expected command %q to be registered", n)
	}
}

func TestRequireSiteScope_ReturnsExistingScope(t *testing.T) {
	s := &Session{
		Scope: Scope{SiteID: "site-existing", SiteName: "existing"},
		Cache: NewCache(),
	}
	got, err := requireSiteScope(s, "should not prompt")
	require.NoError(t, err)
	assert.Equal(t, "site-existing", got)
}

// --- Machine health alert tests ---

func TestExtractBlockingAlerts_FiltersByPreventAllocations(t *testing.T) {
	raw := map[string]interface{}{
		"health": map[string]interface{}{
			"alerts": []interface{}{
				map[string]interface{}{
					"id":              "BmcExplorationFailure",
					"target":          "10.91.54.118",
					"message":         "Redfish endpoint refused connection",
					"classifications": []interface{}{"PreventAllocations"},
				},
				map[string]interface{}{
					"id":              "FanSpeed",
					"target":          "Fan1A",
					"message":         "Fan running slow",
					"classifications": []interface{}{"Informational"},
				},
				map[string]interface{}{
					"id":              "FailedValidationTest",
					"target":          "DcgmFullShort",
					"message":         "Failed validation",
					"classifications": []interface{}{"PreventAllocations", "ValidationFailure"},
				},
			},
		},
	}
	alerts := extractBlockingAlerts(raw)
	require.Len(t, alerts, 2)
	assert.Equal(t, "BmcExplorationFailure", alerts[0].ID)
	assert.Equal(t, "10.91.54.118", alerts[0].Target)
	assert.Equal(t, "FailedValidationTest", alerts[1].ID)
}

func TestExtractBlockingAlerts_EmptyHealth(t *testing.T) {
	cases := []struct {
		name string
		raw  interface{}
	}{
		{"nil", nil},
		{"non-map", "not a map"},
		{"missing health", map[string]interface{}{"id": "machine-1"}},
		{"non-map health", map[string]interface{}{"health": "broken"}},
		{"missing alerts", map[string]interface{}{"health": map[string]interface{}{}}},
		{"non-array alerts", map[string]interface{}{"health": map[string]interface{}{"alerts": "x"}}},
		{"empty alerts", map[string]interface{}{"health": map[string]interface{}{"alerts": []interface{}{}}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Empty(t, extractBlockingAlerts(tc.raw))
		})
	}
}

func TestSummarizeBlockingAlert(t *testing.T) {
	cases := []struct {
		name string
		raw  interface{}
		want string
	}{
		{
			name: "no alerts",
			raw:  map[string]interface{}{},
			want: "",
		},
		{
			name: "id and concise target",
			raw: map[string]interface{}{
				"health": map[string]interface{}{
					"alerts": []interface{}{
						map[string]interface{}{
							"id":              "BmcExplorationFailure",
							"target":          "10.91.54.118",
							"classifications": []interface{}{"PreventAllocations"},
						},
					},
				},
			},
			want: "BmcExplorationFailure 10.91.54.118",
		},
		{
			name: "id only when target empty",
			raw: map[string]interface{}{
				"health": map[string]interface{}{
					"alerts": []interface{}{
						map[string]interface{}{
							"id":              "FailedValidationTest",
							"classifications": []interface{}{"PreventAllocations"},
						},
					},
				},
			},
			want: "FailedValidationTest",
		},
		{
			name: "long target gets truncated",
			raw: map[string]interface{}{
				"health": map[string]interface{}{
					"alerts": []interface{}{
						map[string]interface{}{
							"id":              "X",
							"target":          strings.Repeat("a", 50),
							"classifications": []interface{}{"PreventAllocations"},
						},
					},
				},
			},
			want: "X " + strings.Repeat("a", 21) + "...",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, summarizeBlockingAlert(tc.raw))
		})
	}
}

func TestPrintMachineHealthSummary_PrintsBlockingAlerts(t *testing.T) {
	body := []byte(`{
		"id": "machine-1",
		"status": "Error",
		"isUsableByTenant": false,
		"health": {
			"alerts": [
				{
					"id": "BmcExplorationFailure",
					"target": "10.91.54.118",
					"message": "Failed to connect to Redfish endpoint at 10.91.54.118\nadditional context",
					"classifications": ["PreventAllocations"]
				}
			]
		}
	}`)
	var buf bytes.Buffer
	printMachineHealthSummary(&buf, body)
	out := buf.String()
	assert.Contains(t, out, "Blocking health alerts:")
	assert.Contains(t, out, "BmcExplorationFailure")
	assert.Contains(t, out, "10.91.54.118")
	assert.Contains(t, out, "PreventAllocations")
	assert.Contains(t, out, "Status: Error")
	assert.Contains(t, out, "Usable by tenant: false")
	assert.Contains(t, out, "Failed to connect")
	assert.Contains(t, out, "(...)")
}

func TestPrintMachineHealthSummary_SuppressedForHealthyMachine(t *testing.T) {
	body := []byte(`{"id": "machine-1", "status": "Ready", "health": {"alerts": []}}`)
	var buf bytes.Buffer
	printMachineHealthSummary(&buf, body)
	assert.Empty(t, buf.String())
}

func TestPrintMachineHealthSummary_SuppressedForNonPreventAllocations(t *testing.T) {
	body := []byte(`{
		"status": "Ready",
		"health": {"alerts": [
			{"id": "FanSpeed", "target": "Fan1A", "classifications": ["Informational"]}
		]}
	}`)
	var buf bytes.Buffer
	printMachineHealthSummary(&buf, body)
	assert.Empty(t, buf.String())
}

func TestShortMessage(t *testing.T) {
	cases := []struct {
		in   string
		want string
	}{
		{"", ""},
		{"single line", "single line"},
		{"  trimmed  ", "trimmed"},
		{"first\nsecond\nthird", "first (...)"},
		{"\nfirst non-empty\nsecond", "first non-empty (...)"},
		{strings.Repeat("a", 250), strings.Repeat("a", 197) + "..."},
	}
	for _, tc := range cases {
		t.Run(tc.in, func(t *testing.T) {
			assert.Equal(t, tc.want, shortMessage(tc.in))
		})
	}
}

// --- Helpers ---

func captureStdout(f func()) string {
	old := os.Stdout
	r, w, _ := os.Pipe()
	os.Stdout = w
	f()
	w.Close()
	os.Stdout = old
	var buf bytes.Buffer
	_, err := io.Copy(&buf, r)
	if err != nil {
		panic(err)
	}
	return buf.String()
}

func equal(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func contains(ss []string, target string) bool {
	i := sort.SearchStrings(ss, target)
	if i < len(ss) && ss[i] == target {
		return true
	}
	for _, s := range ss {
		if s == target {
			return true
		}
	}
	return false
}

// --- Allocation create tests ---

func TestBuildTenantSelectItems_MapsTenantIDAndAppendsManualSentinel(t *testing.T) {
	accounts := []NamedItem{
		{
			Name:   "acme",
			ID:     "account-1",
			Status: "Active",
			Extra:  map[string]string{"tenantId": "tenant-1", "tenantOrg": "acme"},
		},
		{
			Name:  "globex",
			ID:    "account-2",
			Extra: map[string]string{"tenantId": "tenant-2"},
		},
	}

	items := buildTenantSelectItems(accounts, "", "")

	require.Len(t, items, 3, "two tenants plus the manual-entry sentinel")
	assert.Equal(t, "tenant-1", items[0].ID, "select ID must be the tenantId, not the tenant-account ID")
	assert.Contains(t, items[0].Label, "acme")
	assert.Contains(t, items[0].Label, "Active", "status should be surfaced in the label")
	assert.Equal(t, "tenant-2", items[1].ID)
	assert.Equal(t, tenantManualEntrySentinel, items[2].ID)
}

func TestBuildTenantSelectItems_SkipsAccountsWithoutTenantID(t *testing.T) {
	accounts := []NamedItem{
		{Name: "pending-invite", ID: "account-1", Extra: map[string]string{"tenantId": ""}},
		{Name: "no-extra", ID: "account-2"},
	}

	items := buildTenantSelectItems(accounts, "", "")

	assert.Nil(t, items, "accounts without a tenantId must be skipped and no sentinel emitted")
}

func TestBuildTenantSelectItems_EmptyInputReturnsNil(t *testing.T) {
	assert.Nil(t, buildTenantSelectItems(nil, "", ""))
	assert.Nil(t, buildTenantSelectItems([]NamedItem{}, "", ""))
}

func TestBuildTenantSelectItems_FallsBackToTenantIDWhenNameBlank(t *testing.T) {
	accounts := []NamedItem{
		{Name: "   ", Extra: map[string]string{"tenantId": "tenant-xyz"}},
	}
	items := buildTenantSelectItems(accounts, "", "")
	require.Len(t, items, 2)
	assert.Equal(t, "tenant-xyz", items[0].Label)
}

func TestBuildTenantSelectItems_DisambiguatesDuplicateLabels(t *testing.T) {
	// Two distinct tenants with the same display name (e.g. "test-org" in
	// dev envs) must not produce visually identical picker options, because
	// the user could route the allocation to the wrong tenant.
	accounts := []NamedItem{
		{Name: "test-org", Extra: map[string]string{"tenantId": "11111111-aaaa-bbbb-cccc-1111aaaa0001"}},
		{Name: "test-org", Extra: map[string]string{"tenantId": "22222222-aaaa-bbbb-cccc-2222aaaa0002"}},
		{Name: "unique", Extra: map[string]string{"tenantId": "33333333-aaaa-bbbb-cccc-3333aaaa0003"}},
	}
	items := buildTenantSelectItems(accounts, "", "")
	require.Len(t, items, 4, "three tenants plus the manual-entry sentinel")

	labels := []string{items[0].Label, items[1].Label, items[2].Label}
	for _, l := range labels {
		count := 0
		for _, l2 := range labels {
			if l == l2 {
				count++
			}
		}
		assert.Equal(t, 1, count, "label %q must be unique after disambiguation, got %d copies", l, count)
	}

	uniqueIdx := -1
	for i, it := range items[:3] {
		if it.ID == "33333333-aaaa-bbbb-cccc-3333aaaa0003" {
			uniqueIdx = i
		}
	}
	require.NotEqual(t, -1, uniqueIdx)
	assert.Equal(t, "unique", items[uniqueIdx].Label,
		"items whose label is already unique must NOT get a disambiguating suffix")
}

func TestShortTenantID(t *testing.T) {
	assert.Equal(t, "short", shortTenantID("short"))
	assert.Equal(t, "12345678", shortTenantID("12345678"))
	assert.Equal(t, "ddccbbaa", shortTenantID("11111111-aaaa-bbbb-cccc-ddddccbbaa"),
		"long UUID must return last 8 chars only")
}

func TestBuildTenantSelectItems_SortsAlphabeticallyByLabel(t *testing.T) {
	accounts := []NamedItem{
		{Name: "zeta", Extra: map[string]string{"tenantId": "tenant-z"}},
		{Name: "alpha", Extra: map[string]string{"tenantId": "tenant-a"}},
		{Name: "mu", Extra: map[string]string{"tenantId": "tenant-m"}},
	}
	items := buildTenantSelectItems(accounts, "", "")
	require.Len(t, items, 4)
	assert.Equal(t, "alpha", items[0].Label)
	assert.Equal(t, "mu", items[1].Label)
	assert.Equal(t, "zeta", items[2].Label)
	assert.Equal(t, tenantManualEntrySentinel, items[3].ID, "manual-entry sentinel must stay last")
}

func TestBuildTenantSelectItems_SortTieBreaksByID(t *testing.T) {
	accounts := []NamedItem{
		{Name: "acme", Extra: map[string]string{"tenantId": "tenant-b"}},
		{Name: "acme", Extra: map[string]string{"tenantId": "tenant-a"}},
	}
	items := buildTenantSelectItems(accounts, "", "")
	require.Len(t, items, 3)
	assert.Equal(t, "tenant-a", items[0].ID, "equal labels must tie-break by ID")
	assert.Equal(t, "tenant-b", items[1].ID)
}

func TestBuildTenantSelectItems_DeduplicatesByTenantID(t *testing.T) {
	accounts := []NamedItem{
		{Name: "acme-prod", Extra: map[string]string{"tenantId": "tenant-1"}},
		{Name: "acme-dev", Extra: map[string]string{"tenantId": "tenant-1"}},
		{Name: "globex", Extra: map[string]string{"tenantId": "tenant-2"}},
	}

	items := buildTenantSelectItems(accounts, "", "")

	require.Len(t, items, 3, "two unique tenants plus the manual-entry sentinel")
	assert.Equal(t, "tenant-1", items[0].ID)
	assert.Equal(t, "acme-prod", items[0].Label, "first occurrence wins on dedupe")
	assert.Equal(t, "tenant-2", items[1].ID)
	assert.Equal(t, tenantManualEntrySentinel, items[2].ID)
}

func TestBuildTenantSelectItems_SelfTenantPinnedFirstWithSuffix(t *testing.T) {
	// Common dev-cluster case: caller is both Provider Admin and Tenant
	// Admin for the same org, so there are zero tenant-accounts but their
	// own tenant id is known. Must appear as the first picker entry with
	// a "(self)" suffix so the label is unambiguous.
	items := buildTenantSelectItems(nil, "11111111-2222-3333-4444-555566667777", "nico")
	require.Len(t, items, 2, "self entry plus manual-entry sentinel")
	assert.Equal(t, "11111111-2222-3333-4444-555566667777", items[0].ID)
	assert.Equal(t, "nico (self)", items[0].Label)
	assert.Equal(t, tenantManualEntrySentinel, items[1].ID)
}

func TestBuildTenantSelectItems_SelfTenantBlankOrgFallsBackToID(t *testing.T) {
	items := buildTenantSelectItems(nil, "abc-tenant-id", "   ")
	require.Len(t, items, 2)
	assert.Equal(t, "abc-tenant-id (self)", items[0].Label,
		"blank org name must fall back to the tenant id so the label is still non-empty")
}

func TestBuildTenantSelectItems_SelfTenantStaysFirstWhenAccountsSort(t *testing.T) {
	accounts := []NamedItem{
		{Name: "zeta", Extra: map[string]string{"tenantId": "tenant-z"}},
		{Name: "alpha", Extra: map[string]string{"tenantId": "tenant-a"}},
	}
	items := buildTenantSelectItems(accounts, "self-tenant", "nico")
	require.Len(t, items, 4, "self + 2 tenant-accounts + sentinel")
	assert.Equal(t, "nico (self)", items[0].Label, "self always pinned first even if it sorts after")
	assert.Equal(t, "alpha", items[1].Label, "remaining items sort alphabetically")
	assert.Equal(t, "zeta", items[2].Label)
	assert.Equal(t, tenantManualEntrySentinel, items[3].ID)
}

func TestBuildTenantSelectItems_SelfTenantDedupesWithAccount(t *testing.T) {
	// If a tenant-account already references the same tenant id as self,
	// only the self entry is shown (inserted first, so the account-row
	// copy is dropped by the dedupe map).
	accounts := []NamedItem{
		{Name: "nico", Extra: map[string]string{"tenantId": "self-tenant"}},
	}
	items := buildTenantSelectItems(accounts, "self-tenant", "nico")
	require.Len(t, items, 2, "one entry (self) plus manual-entry sentinel")
	assert.Equal(t, "nico (self)", items[0].Label)
	assert.Equal(t, "self-tenant", items[0].ID)
}

func TestBuildTenantSelectItems_EmptySelfTenantIgnored(t *testing.T) {
	items := buildTenantSelectItems(nil, "", "nico")
	assert.Nil(t, items, "no self id and no accounts means no picker")
}

func TestAllocationConstraintResourceTypes_MatchAPIValidation(t *testing.T) {
	items := allocationConstraintResourceTypes()
	ids := make([]string, len(items))
	for i, it := range items {
		ids[i] = it.ID
	}
	assert.ElementsMatch(t, []string{"IPBlock", "InstanceType"}, ids,
		"resource type IDs must match APIAllocationConstraintCreateRequest validation")
}

func TestAllocationConstraintTypes_OnlyExposesReserved(t *testing.T) {
	items := allocationConstraintTypes()
	ids := make([]string, len(items))
	for i, it := range items {
		ids[i] = it.ID
	}
	assert.Equal(t, []string{"Reserved"}, ids,
		"only Reserved is offered; OnDemand/Preemptible are accepted by the API validator "+
			"but are documented as unsupported by the current backend implementation")
}

func TestResolverResourceForAllocationResourceType(t *testing.T) {
	cases := []struct {
		resourceType string
		wantKey      string
		wantLabel    string
		wantOK       bool
	}{
		{"IPBlock", "ip-block", "IP Block", true},
		{"InstanceType", "instance-type", "Instance Type", true},
		{"Unknown", "", "", false},
		{"", "", "", false},
	}
	for _, tc := range cases {
		t.Run(tc.resourceType, func(t *testing.T) {
			key, label, ok := resolverResourceForAllocationResourceType(tc.resourceType)
			assert.Equal(t, tc.wantKey, key)
			assert.Equal(t, tc.wantLabel, label)
			assert.Equal(t, tc.wantOK, ok)
		})
	}
}

func TestBuildAllocationConstraint_ValidInput(t *testing.T) {
	got, err := buildAllocationConstraint("IPBlock", "block-1", "Reserved", "  28 ")
	require.NoError(t, err)
	assert.Equal(t, "IPBlock", got["resourceType"])
	assert.Equal(t, "block-1", got["resourceTypeId"])
	assert.Equal(t, "Reserved", got["constraintType"])
	assert.Equal(t, 28, got["constraintValue"], "value must be an int, not a string")
}

func TestBuildAllocationConstraint_RejectsNonInteger(t *testing.T) {
	_, err := buildAllocationConstraint("IPBlock", "block-1", "Reserved", "not-a-number")
	require.Error(t, err)
	assert.Contains(t, err.Error(), "integer")
}

func TestBuildAllocationConstraint_RejectsOutOfRangeIPBlockPrefix(t *testing.T) {
	_, err := buildAllocationConstraint("IPBlock", "block-1", "Reserved", "0")
	require.Error(t, err)
	assert.Contains(t, err.Error(), "prefix length")

	_, err = buildAllocationConstraint("IPBlock", "block-1", "Reserved", "33")
	require.Error(t, err)
	assert.Contains(t, err.Error(), "prefix length")
}

func TestBuildAllocationConstraint_AcceptsBoundaryIPBlockPrefix(t *testing.T) {
	_, err := buildAllocationConstraint("IPBlock", "block-1", "Reserved", "1")
	require.NoError(t, err)
	_, err = buildAllocationConstraint("IPBlock", "block-1", "Reserved", "32")
	require.NoError(t, err)
}

func TestBuildAllocationConstraint_RejectsNonPositiveInstanceTypeCount(t *testing.T) {
	_, err := buildAllocationConstraint("InstanceType", "type-1", "Reserved", "0")
	require.Error(t, err)
	assert.Contains(t, err.Error(), "at least 1")

	_, err = buildAllocationConstraint("InstanceType", "type-1", "Reserved", "-5")
	require.Error(t, err)
	assert.Contains(t, err.Error(), "at least 1")
}

func TestBuildAllocationConstraint_MarshalShape(t *testing.T) {
	c, err := buildAllocationConstraint("InstanceType", "type-1", "Reserved", "4")
	require.NoError(t, err)
	encoded, err := json.Marshal(c)
	require.NoError(t, err)
	var decoded map[string]interface{}
	require.NoError(t, json.Unmarshal(encoded, &decoded))
	assert.Equal(t, "InstanceType", decoded["resourceType"])
	assert.Equal(t, "type-1", decoded["resourceTypeId"])
	assert.Equal(t, "Reserved", decoded["constraintType"])
	assert.InDelta(t, 4, decoded["constraintValue"], 0.0001,
		"constraintValue must round-trip through JSON as a number, not a string")
}

func TestAllocationConstraintValueHint(t *testing.T) {
	assert.Contains(t, allocationConstraintValueHint("IPBlock"), "prefix")
	assert.Contains(t, allocationConstraintValueHint("InstanceType"), "machine")
	assert.NotEmpty(t, allocationConstraintValueHint("Unknown"), "unknown types still get a generic hint")
}

// --- VPC prefix create IP block picker tests (NVBug 6105076) ---

// TestValidateVPCPrefixLength rejects values outside the shared minimum and
// the maximum resolved by the caller.
func TestValidateVPCPrefixLength(t *testing.T) {
	tests := []struct {
		name          string
		maximumLength int
		prefixLength  int
		wantError     string
	}{
		{name: "IPv4 maximum", maximumLength: vpcprefix.IPv4PrefixLengthMaximum, prefixLength: 31},
		{name: "IPv4 above maximum", maximumLength: vpcprefix.IPv4PrefixLengthMaximum, prefixLength: 32, wantError: "prefix length must be between 8 and 31"},
		{name: "manual block uses request maximum", maximumLength: vpcprefix.PrefixLengthMaximum, prefixLength: 126},
		{name: "below shared minimum", maximumLength: vpcprefix.PrefixLengthMaximum, prefixLength: 7, wantError: "prefix length must be between 8 and 126"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			err := validateVPCPrefixLength(test.maximumLength, test.prefixLength)
			if test.wantError != "" {
				require.EqualError(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
		})
	}
}

// TestVPCPrefixSlaacEnabled verifies the TUI requires the selected VPC's
// address mode only when it affects a known IPv6 block.
func TestVPCPrefixSlaacEnabled(t *testing.T) {
	tests := []struct {
		name      string
		family    vpcprefix.IPFamily
		vpc       *NamedItem
		want      bool
		wantError string
	}{
		{name: "IPv6 SLAAC enabled", family: vpcprefix.IPFamilyIPv6, vpc: &NamedItem{Raw: map[string]interface{}{"slaacEnabled": true}}, want: true},
		{name: "IPv6 SLAAC disabled", family: vpcprefix.IPFamilyIPv6, vpc: &NamedItem{Raw: map[string]interface{}{"slaacEnabled": false}}},
		{name: "IPv6 mode absent", family: vpcprefix.IPFamilyIPv6, vpc: &NamedItem{Name: "vpc-one", Raw: map[string]interface{}{}}, wantError: "could not determine whether VPC \"vpc-one\" uses SLAAC"},
		{name: "IPv4 does not need SLAAC mode", family: vpcprefix.IPFamilyIPv4},
		{name: "manual block does not need SLAAC mode"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := vpcPrefixSlaacEnabled(test.family, test.vpc)
			if test.wantError != "" {
				require.EqualError(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}
}

// TestBuildIPBlockSelectItems verifies only eligible tenant IP Blocks are
// presented while manual ID entry remains available.
func TestBuildIPBlockSelectItems(t *testing.T) {
	// expectedItem captures the observable selector fields for one row.
	type expectedItem struct {
		id         string
		labelParts []string
		protocol   string
	}
	tests := []struct {
		name     string
		blocks   []NamedItem
		tenantID string
		want     []expectedItem
	}{
		{
			name: "maps blocks and appends the manual sentinel",
			blocks: []NamedItem{
				{Name: "block-a", ID: "id-a", Status: "Ready", Extra: map[string]string{"tenantId": "tenant-a", "protocolVersion": "IPv4"}},
				{Name: "block-b", ID: "id-b", Status: "ready", Extra: map[string]string{"tenantId": "tenant-a", "protocolVersion": "IPv6"}},
			},
			tenantID: "tenant-a",
			want: []expectedItem{
				{id: "id-a", labelParts: []string{"block-a", "IPv4", "Ready"}, protocol: "IPv4"},
				{id: "id-b", labelParts: []string{"block-b", "IPv6", "ready"}, protocol: "IPv6"},
				{id: ipBlockManualEntrySentinel, labelParts: []string{"Enter IP block ID manually"}},
			},
		},
		{
			name: "excludes provider, blocks that are not Ready, and other tenant blocks",
			blocks: []NamedItem{
				{Name: "provider-block", ID: "provider-id", Status: "Ready"},
				{Name: "pending-tenant-block", ID: "pending-id", Status: "Pending", Extra: map[string]string{"tenantId": "tenant-a"}},
				{Name: "ready-tenant-block", ID: "ready-id", Status: "Ready", Extra: map[string]string{"tenantId": "tenant-a"}},
				{Name: "other-tenant-block", ID: "other-tenant-id", Status: "Ready", Extra: map[string]string{"tenantId": "tenant-b"}},
			},
			tenantID: "tenant-a",
			want: []expectedItem{
				{id: "ready-id", labelParts: []string{"ready-tenant-block", "Ready"}},
				{id: ipBlockManualEntrySentinel, labelParts: []string{"Enter IP block ID manually"}},
			},
		},
		{
			name:     "empty input returns only the manual sentinel",
			tenantID: "tenant-a",
			want: []expectedItem{
				{id: ipBlockManualEntrySentinel, labelParts: []string{"Enter IP block ID manually"}},
			},
		},
		{
			name: "skips blocks without IDs and uses the ID for a blank name",
			blocks: []NamedItem{
				{Name: "no-id", ID: "  "},
				{Name: "  ", ID: "id-x", Status: "Ready", Extra: map[string]string{"tenantId": "tenant-x"}},
			},
			tenantID: "tenant-x",
			want: []expectedItem{
				{id: "id-x", labelParts: []string{"id-x", "Ready"}},
				{id: ipBlockManualEntrySentinel, labelParts: []string{"Enter IP block ID manually"}},
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			items := buildIPBlockSelectItems(test.blocks, test.tenantID)
			require.Len(t, items, len(test.want))
			for index := range items {
				assert.Equal(t, test.want[index].id, items[index].ID)
				for _, labelPart := range test.want[index].labelParts {
					assert.Contains(t, items[index].Label, labelPart)
				}
				assert.Equal(t, test.want[index].protocol, items[index].Extra["protocolVersion"])
			}
		})
	}
}
