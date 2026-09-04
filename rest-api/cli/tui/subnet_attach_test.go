// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"sync/atomic"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSession_fetchSubnets(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		assert.Equal(t, "/v2/org/acme/nico/subnet", r.URL.Path)
		assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
		assert.Equal(t, "vpc-1", r.URL.Query().Get("vpcId"))
		assert.Equal(t, "true", r.URL.Query().Get("includeUsageStats"))
		w.Header().Set("Content-Type", "application/json")
		_, err := io.WriteString(w, `[{"id":"subnet-1","name":"tenant-subnet","siteId":"site-1","vpcId":"vpc-1","status":"Ready","usageStats":{"acquiredIPs":2}}]`)
		require.NoError(t, err)
	}))
	defer server.Close()

	session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
	session.Scope.SiteID = "site-1"
	session.Scope.VpcID = "vpc-1"
	items, err := session.fetchSubnets(context.Background())

	require.NoError(t, err)
	require.Len(t, items, 1)
	assert.Equal(t, "2", items[0].Extra["acquiredIPs"])
}

func TestCmdSubnetAttachVPC(t *testing.T) {
	const generatedCommandName = "subnet attach-vpc-to attach-vpc-to-subnet"
	tests := []struct {
		name            string
		commandName     string
		input           string
		sourceUsage     int
		sourceMode      string
		targetMode      string
		response        string
		wantErrContains string
		wantPosts       int
	}{
		{
			name:        "concise command explicitly allows replacement after confirmation",
			input:       "y\n",
			sourceUsage: 2,
			response:    `{"id":"subnet-1","name":"tenant-subnet","vpcId":"vpc-target"}`,
			wantPosts:   1,
		},
		{
			name:        "original generated command uses guarded handler",
			commandName: generatedCommandName,
			input:       "yes\n",
			sourceUsage: 2,
			response:    `{"id":"subnet-1","name":"tenant-subnet","vpcId":"vpc-target"}`,
			wantPosts:   1,
		},
		{
			name:        "same NVUE mode remains eligible",
			input:       "y\n",
			sourceUsage: 2,
			sourceMode:  "ETHERNET_VIRTUALIZER_WITH_NVUE",
			targetMode:  "ETHERNET_VIRTUALIZER_WITH_NVUE",
			response:    `{"id":"subnet-1","name":"tenant-subnet","vpcId":"vpc-target"}`,
			wantPosts:   1,
		},
		{
			name:            "NVUE source rejects Ethernet virtualizer target",
			sourceUsage:     2,
			sourceMode:      "ETHERNET_VIRTUALIZER_WITH_NVUE",
			wantErrContains: "no Target Ready tenant Ethernet virtualizer VPC available",
		},
		{
			name:            "Ethernet virtualizer source rejects NVUE target",
			sourceUsage:     2,
			targetMode:      "ETHERNET_VIRTUALIZER_WITH_NVUE",
			wantErrContains: "no Target Ready tenant Ethernet virtualizer VPC available",
		},
		{
			name:        "cancelled confirmation sends no request",
			input:       "n\n",
			sourceUsage: 2,
		},
		{
			name:            "Subnet with workload usage is rejected",
			sourceUsage:     3,
			wantErrContains: "no eligible subnet matching",
		},
		{
			name:            "response Subnet mismatch is rejected",
			input:           "y\n",
			sourceUsage:     2,
			response:        `{"id":"subnet-other","name":"tenant-subnet","vpcId":"vpc-target"}`,
			wantErrContains: "id does not match selected Subnet",
			wantPosts:       1,
		},
		{
			name:            "response target VPC mismatch is rejected",
			input:           "y\n",
			sourceUsage:     2,
			response:        `{"id":"subnet-1","name":"tenant-subnet","vpcId":"vpc-other"}`,
			wantErrContains: "vpcId does not match selected target VPC",
			wantPosts:       1,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			sourceMode := test.sourceMode
			if sourceMode == "" {
				sourceMode = "ETHERNET_VIRTUALIZER"
			}
			targetMode := test.targetMode
			if targetMode == "" {
				targetMode = "ETHERNET_VIRTUALIZER"
			}
			var postCount atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				switch {
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/vpc":
					assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
					_, err := io.WriteString(w, `[
						{"id":"vpc-source","name":"source-vpc","siteId":"site-1","tenantId":"tenant-1","status":"Ready","networkVirtualizationType":"`+sourceMode+`"},
						{"id":"vpc-target","name":"target-vpc","siteId":"site-1","tenantId":"tenant-1","status":"Ready","networkVirtualizationType":"`+targetMode+`"}
					]`)
					require.NoError(t, err)
				case r.Method == http.MethodGet && r.URL.Path == "/v2/org/acme/nico/subnet":
					assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
					assert.Empty(t, r.URL.Query().Get("vpcId"), "Site-wide discovery must ignore the narrower VPC scope")
					assert.Equal(t, "true", r.URL.Query().Get("includeUsageStats"))
					_, err := io.WriteString(w, `[{"id":"subnet-1","name":"tenant-subnet","siteId":"site-1","vpcId":"vpc-source","status":"Ready","usageStats":{"acquiredIPs":`+strconv.Itoa(test.sourceUsage)+`}}]`)
					require.NoError(t, err)
				case r.Method == http.MethodPost && r.URL.Path == "/v2/org/acme/nico/subnet/subnet-1/attach-vpc":
					postCount.Add(1)
					var body map[string]interface{}
					require.NoError(t, json.NewDecoder(r.Body).Decode(&body))
					assert.Equal(t, map[string]interface{}{"vpcId": "vpc-target", "allowReplace": true}, body)
					w.Header().Set("Content-Type", "application/json")
					_, err := io.WriteString(w, test.response)
					require.NoError(t, err)
				default:
					http.NotFound(w, r)
				}
			}))
			defer server.Close()

			session := NewSession(appcli.NewClient(server.URL, "acme", "token", nil, false), "acme", "")
			session.Scope = Scope{SiteID: "site-1", SiteName: "Site One", VpcID: "vpc-narrow", VpcName: "Narrow VPC"}
			session.Cache.Set("_tenant", []NamedItem{{Name: "acme", ID: "tenant-1"}})

			run := cmdSubnetAttachVPC
			if test.commandName != "" {
				found := false
				for _, command := range AllCommands() {
					if command.Name == test.commandName {
						run = command.Run
						found = true
						break
					}
				}
				require.True(t, found, "generated source command must be registered")
			}
			_, err := withStdin(t, test.input, func() (string, error) {
				return "", run(session, []string{"subnet-1"})
			})

			if test.wantErrContains != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), test.wantErrContains)
			} else {
				require.NoError(t, err)
			}
			assert.Equal(t, int32(test.wantPosts), postCount.Load())
			assert.Equal(t, "site-1", session.Scope.SiteID)
			assert.Equal(t, "vpc-narrow", session.Scope.VpcID)
			assert.Nil(t, session.Cache.Get("subnet"))
			assert.Nil(t, session.Cache.Get("vpc"))
		})
	}
}
