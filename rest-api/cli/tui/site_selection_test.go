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
	"os"
	"path/filepath"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestPromptRegisteredSite(t *testing.T) {
	for _, test := range []struct {
		name       string
		sites      []NamedItem
		fetchError error
		wantError  string
	}{
		{
			name: "mixed sites preserve the unfiltered cache",
			sites: []NamedItem{
				{
					ID:     "pending",
					Status: "Pending",
				},
				{
					ID:     "registered",
					Status: "Registered",
				},
				{
					ID:     "error",
					Status: "Error",
				},
				{
					ID: "unknown",
				},
			},
		},
		{
			name: "no eligible sites",
			sites: []NamedItem{{
				ID:     "pending",
				Status: "Pending",
			}},
			wantError: "no Registered site available",
		},
		{
			name:       "fetch failure",
			fetchError: fmt.Errorf("offline"),
			wantError:  "fetching site: offline",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			cache := NewCache()
			resolver := NewResolver(cache)
			resolver.RegisterFetcher("site", func(context.Context) ([]NamedItem, error) {
				return test.sites, test.fetchError
			})
			item, err := promptRegisteredSite(&Session{
				Resolver: resolver,
			}, context.Background())
			if test.wantError != "" {
				require.EqualError(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, "registered", item.ID)
			assert.Equal(t, test.sites, cache.Get("site"))
		})
	}
}

func TestRequireRegisteredSiteScope(t *testing.T) {
	for _, test := range []struct {
		name      string
		scope     string
		wantError bool
	}{
		{
			name: "unscoped selection",
		},
		{
			name:  "registered scope",
			scope: "registered",
		},
		{
			name:      "pending scope",
			scope:     "pending",
			wantError: true,
		},
		{
			name:      "inaccessible scope",
			scope:     "missing",
			wantError: true,
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			session := NewSession(nil, "acme", "")
			session.Cache.Set("site", []NamedItem{
				{
					ID:     "pending",
					Status: "Pending",
				},
				{
					ID:     "registered",
					Status: "Registered",
				},
			})
			session.Scope.SiteID = test.scope
			id, err := requireRegisteredSiteScope(session, "Select a site.")
			if test.wantError {
				require.ErrorContains(t, err, "not in Registered state")
				assert.Equal(t, test.scope, session.Scope.SiteID)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, "registered", id)
			assert.Equal(t, id, session.Scope.SiteID)
		})
	}
}

func TestSSHKeyGroupSiteItems(t *testing.T) {
	for _, test := range []struct {
		name       string
		fetchError error
		wantError  string
	}{
		{
			name: "registered additions and existing associations",
		},
		{
			name:       "site discovery fails",
			fetchError: fmt.Errorf("offline"),
			wantError:  "fetching site: offline",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			session := NewSession(nil, "acme", "")
			sites := []NamedItem{
				{
					ID:     "pending",
					Name:   "new-pending",
					Status: "Pending",
				},
				{
					ID:     "registered",
					Name:   "new-registered",
					Status: "Registered",
				},
			}
			session.Resolver.RegisterFetcher("site", func(context.Context) ([]NamedItem, error) {
				return sites, test.fetchError
			})
			var raw map[string]interface{}
			require.NoError(t, json.Unmarshal([]byte(`{"siteAssociations":[
				{"site":{"id":"existing","name":"existing-pending","status":"Pending"}},
				{"site":{"id":"registered","name":"new-registered","status":"Registered"}}
			]}`), &raw))
			items, err := sshKeyGroupSiteItems(session, context.Background(), &NamedItem{
				Raw: raw,
			})
			if test.wantError != "" {
				require.EqualError(t, err, test.wantError)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, []NamedItem{
				sites[1],
				{
					ID:     "existing",
					Name:   "existing-pending",
					Status: "Pending",
				},
			}, items)
			assert.Equal(t, sites, session.Cache.Get("site"))
		})
	}
}

func TestCmdSSHKeyGroupUpdate(t *testing.T) {
	for _, test := range []struct {
		name       string
		input      string
		fetchError error
		wantBody   string
		wantError  string
	}{
		{
			name:     "keep associations",
			input:    "\n\nn\n\n",
			wantBody: `{"version":"version-1"}`,
		},
		{
			name:     "clear associations",
			input:    "\n\ny\nn\n\n",
			wantBody: `{"version":"version-1","siteIds":[]}`,
		},
		{
			name:      "cancel association update",
			input:     "\n\n",
			wantError: "input cancelled",
		},
		{
			name:       "site discovery fails without submitting update",
			input:      "\n\ny\n",
			fetchError: fmt.Errorf("offline"),
			wantError:  "fetching site: offline",
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			requests := make(chan string, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				assert.Equal(t, http.MethodPatch, request.Method)
				assert.Equal(t, "/v2/org/acme/nico/sshkeygroup/group-1", request.URL.Path)
				body, err := io.ReadAll(request.Body)
				require.NoError(t, err)
				requests <- string(body)
				w.Header().Set("Content-Type", "application/json")
				_, _ = io.WriteString(w, `{"id":"group-1","name":"group-one"}`)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			var raw map[string]interface{}
			require.NoError(t, json.Unmarshal([]byte(`{"version":"version-1","siteAssociations":[{"site":{"id":"existing","name":"existing-pending","status":"Pending"}}]}`), &raw))
			session.Cache.Set("ssh-key-group", []NamedItem{
				{
					ID:   "group-1",
					Name: "group-one",
					Raw:  raw,
				},
			})
			session.Resolver.RegisterFetcher("site", func(context.Context) ([]NamedItem, error) {
				return []NamedItem{}, test.fetchError
			})
			_, err := withStdin(t, test.input, func() (string, error) {
				return "", cmdSSHKeyGroupUpdate(session, nil)
			})
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				assert.Empty(t, requests)
				return
			}
			require.NoError(t, err)
			require.Len(t, requests, 1)
			assert.JSONEq(t, test.wantBody, <-requests)
		})
	}
}

func TestGeneratedBodySiteSelection(t *testing.T) {
	for _, test := range []struct {
		command        string
		registeredOnly bool
	}{
		{
			command:        "vpc create",
			registeredOnly: true,
		},
		{
			command:        "vpc-peering create",
			registeredOnly: true,
		},
		{
			command:        "network-security-group create",
			registeredOnly: true,
		},
		{
			command:        "operating-system create",
			registeredOnly: true,
		},
		{
			command:        "ssh-key-group create",
			registeredOnly: true,
		},
		{
			command:        "expected-machine create",
			registeredOnly: true,
		},
		{
			command:        "expected-switch create",
			registeredOnly: true,
		},
		{
			command:        "expected-power-shelf create",
			registeredOnly: true,
		},
		{
			command:        "expected-rack create",
			registeredOnly: true,
		},
		{
			command:        "expected-rack-group create",
			registeredOnly: true,
		},
		{
			command:        "expected-machine replace-all replace-all-expected-machine",
			registeredOnly: true,
		},
		{
			command:        "expected-switch replace-all replace-all-expected-switch",
			registeredOnly: true,
		},
		{
			command:        "expected-power-shelf replace-all replace-all-expected-power-shelf",
			registeredOnly: true,
		},
		{
			command:        "expected-rack replace-all",
			registeredOnly: true,
		},
		{
			command:        "expected-rack replace-all replace-all-expected-rack",
			registeredOnly: true,
		},
		{
			command:        "expected-rack-group replace-all replace-all-expected-rack-group",
			registeredOnly: true,
		},
		{
			command:        "infiniband-partition create",
			registeredOnly: true,
		},
		{
			command:        "nvlink-logical-partition create",
			registeredOnly: true,
		},
		{
			command:        "spectrumx-partition create",
			registeredOnly: true,
		},
		{
			command:        "dpu-extension-service create",
			registeredOnly: true,
		},
		{
			command: "ip-block create",
		},
		{
			command: "allocation create",
		},
		{
			command: "instance-type create",
		},
		{
			command: "expected-machine batch-create",
		},
	} {
		t.Run(test.command, func(t *testing.T) {
			info := generatedCommandInfoByName(t, test.command)
			session := NewSession(nil, "acme", "")
			sites := []NamedItem{{
				ID:     "pending",
				Status: "Pending",
			}}
			wantID := "pending"
			if test.registeredOnly {
				sites = append(sites, NamedItem{
					ID:     "registered",
					Status: "Registered",
				})
				wantID = "registered"
			}
			session.Cache.Set("site", sites)
			// Non-restricted generated forms call the live fetcher directly.
			session.Resolver.RegisterFetcher("site", func(context.Context) ([]NamedItem, error) { return sites, nil })
			var field appcli.GeneratedCommandBodyFormField
			for _, candidate := range info.BodyFormFields {
				if candidate.JSONName == "siteId" || candidate.JSONName == "siteIds" {
					field = candidate
				}
			}
			require.NotEmpty(t, field.JSONName)
			field.Required = true
			value, set, err := promptGeneratedBodyField(session, info, field, map[string]string{}, &queuedGeneratedBodyPrompter{})
			require.NoError(t, err)
			require.True(t, set)
			if field.Type == "array" {
				assert.Equal(t, []string{wantID}, value)
			} else {
				assert.Equal(t, wantID, value)
			}
			assert.Equal(t, sites, session.Cache.Get("site"))
			if test.registeredOnly && field.JSONName == "siteId" {
				session.Scope.SiteID = "pending"
				_, err = buildGeneratedBodyFormProperties(session, info, []appcli.GeneratedCommandBodyFormField{field}, map[string]string{}, &queuedGeneratedBodyPrompter{})
				require.ErrorContains(t, err, "not in Registered state")
			}
		})
	}
}

func TestRunGeneratedTUICommand_RegisteredSites(t *testing.T) {
	for _, test := range []struct {
		name      string
		command   string
		args      []string
		scope     string
		fileBody  string
		wantError bool
	}{
		{
			name:      "explicit pending flag",
			args:      []string{"--site-id", "pending"},
			wantError: true,
		},
		{
			name: "site ID matching is case-insensitive",
			args: []string{"--site-id", "REGISTERED"},
		},
		{
			name:      "pending JSON",
			args:      []string{"--data", `{"siteId":"pending"}`},
			wantError: true,
		},
		{
			name:      "pending file",
			fileBody:  `{"siteId":"pending"}`,
			wantError: true,
		},
		{
			name:      "pending scope",
			scope:     "pending",
			args:      []string{"--data", `{}`},
			wantError: true,
		},
		{
			name:  "registered scope",
			scope: "registered",
			args:  []string{"--data", `{}`},
		},
		{
			name:  "explicit registered overrides pending scope",
			scope: "pending",
			args:  []string{"--data", `{"siteId":"registered"}`},
		},
		{
			name:     "registered file",
			fileBody: `{"siteId":"registered"}`,
		},
		{
			name: "opaque label is not a site",
			args: []string{"--data", `{"siteId":"registered","labels":{"siteId":"pending"}}`},
		},
		{
			name:      "nested pending site",
			command:   "expected-switch replace-all replace-all-expected-switch",
			args:      []string{"--data", `{"siteId":"registered","expectedSwitches":[{"siteId":"pending"}]}`},
			wantError: true,
		},
		{
			name:      "SSH multiple sites",
			command:   "ssh-key-group create",
			args:      []string{"--data", `{"siteIds":["registered","pending"]}`},
			wantError: true,
		},
		{
			name:    "SSH may have no associations",
			command: "ssh-key-group create",
			args:    []string{"--data", `{"name":"keys"}`},
		},
		{
			name:    "IP block allows pending",
			command: "ip-block create",
			args:    []string{"--data", `{"siteId":"pending"}`},
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			mutations := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				if request.Method == http.MethodGet {
					assert.Equal(t, "/v2/org/acme/nico/site", request.URL.Path)
					_, _ = io.WriteString(w, `[{"id":"registered","name":"registered","status":"Registered"},{"id":"pending","name":"pending","status":"Pending"}]`)
					return
				}
				mutations++
				w.WriteHeader(http.StatusCreated)
				_, _ = io.WriteString(w, `{"id":"created"}`)
			}))
			defer server.Close()
			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Scope.SiteID = test.scope
			command := test.command
			if command == "" {
				command = "expected-switch create"
			}
			args := test.args
			if test.fileBody != "" {
				path := filepath.Join(t.TempDir(), "request.json")
				require.NoError(t, os.WriteFile(path, []byte(test.fileBody), 0600))
				args = []string{"--data-file", path}
			}
			_, err := runSpecializedCommandWithInput(t, "y\n", func() error {
				return runGeneratedTUICommand(session, generatedCommandInfoByName(t, command), args)
			})
			if test.wantError {
				require.ErrorContains(t, err, "not in Registered state")
				assert.Zero(t, mutations)
			} else {
				require.NoError(t, err)
				assert.Equal(t, 1, mutations)
			}
		})
	}
}
