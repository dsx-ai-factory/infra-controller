// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestBuildVPCPeeringPlan(t *testing.T) {
	tests := []struct {
		name       string
		selected   []NamedItem
		existing   []NamedItem
		wantCreate int
		wantSkip   int
	}{
		{
			name:       "three VPCs produce three peerings",
			selected:   testVPCPeeringItems(3),
			wantCreate: 3,
		},
		{
			name:       "five VPCs produce ten peerings",
			selected:   testVPCPeeringItems(5),
			wantCreate: 10,
		},
		{
			name:     "existing peering is skipped regardless of order",
			selected: testVPCPeeringItems(3),
			existing: []NamedItem{{Extra: map[string]string{
				"vpc1Id": "vpc-3",
				"vpc2Id": "vpc-1",
			}}},
			wantCreate: 2,
			wantSkip:   1,
		},
		{
			name:       "duplicate selection does not create duplicate or self peering",
			selected:   append(testVPCPeeringItems(3), testVPCPeeringItems(1)...),
			wantCreate: 3,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			plan := buildVPCPeeringPlan(test.selected, test.existing)
			assert.Len(t, plan.create, test.wantCreate)
			assert.Len(t, plan.skip, test.wantSkip)
			for _, pair := range append(plan.create, plan.skip...) {
				assert.NotEqual(t, pair.first.ID, pair.second.ID)
			}
		})
	}
}

func TestExecuteVPCPeeringPlan(t *testing.T) {
	var requests []map[string]string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		require.Equal(t, http.MethodPost, r.Method)
		require.Equal(t, "/v2/org/acme/nico/vpc-peering", r.URL.Path)
		var body map[string]string
		decodeErr := json.NewDecoder(r.Body).Decode(&body)
		require.NoError(t, decodeErr)
		requests = append(requests, body)
		if body["vpc1Id"] == "vpc-1" && body["vpc2Id"] == "vpc-3" {
			w.WriteHeader(http.StatusInternalServerError)
			_, _ = w.Write([]byte(`{"message":"peering failed"}`))
			return
		}
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"id":"peering-1"}`))
	}))
	defer server.Close()

	selected := testVPCPeeringItems(3)
	plan := buildVPCPeeringPlan(selected, []NamedItem{{Extra: map[string]string{
		"vpc1Id": "vpc-1",
		"vpc2Id": "vpc-2",
	}}})
	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	var output bytes.Buffer
	err := executeVPCPeeringPlan(session, "site-1", plan, &output)
	require.Error(t, err)
	assert.Len(t, requests, 2, "later pairs must run after a failure")
	assert.Contains(t, output.String(), "SKIPPED VPC 1 (vpc-1) <-> VPC 2 (vpc-2)")
	assert.Contains(t, output.String(), "FAILED VPC 1 (vpc-1) <-> VPC 3 (vpc-3)")
	assert.Contains(t, output.String(), "Summary: created 1, skipped 1, failed 1")
}

func TestFetchVPCPeeringsForSite(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		require.Equal(t, http.MethodGet, r.Method)
		require.Equal(t, "/v2/org/acme/nico/vpc-peering", r.URL.Path)
		assert.Equal(t, "site-1", r.URL.Query().Get("siteId"))
		assert.Empty(t, r.URL.Query().Get("vpcId"))
		_, _ = w.Write([]byte(`[{
			"id":"peering-1",
			"siteId":"site-1",
			"vpc1Id":"vpc-1",
			"vpc2Id":"vpc-2"
		}]`))
	}))
	defer server.Close()

	session := NewSession(
		appcli.NewClient(server.URL, "acme", "token", nil, false),
		"acme",
		"",
	)
	session.Scope.VpcID = "vpc-1"
	peerings, err := fetchVPCPeeringsForSite(session, "site-1")
	require.NoError(t, err)
	require.Len(t, peerings, 1)
	assert.Equal(t, "vpc-1", peerings[0].Extra["vpc1Id"])
	assert.Equal(t, "vpc-2", peerings[0].Extra["vpc2Id"])
}

func TestPromptVPCPeeringVPCs(t *testing.T) {
	session := NewSession(nil, "acme", "")
	session.Scope.VpcID = "vpc-1"
	session.Scope.VpcName = "VPC 1"
	session.Cache.Set("vpc", []NamedItem{{ID: "vpc-1"}})
	session.Resolver.RegisterFetcher("vpc", func(context.Context) ([]NamedItem, error) {
		assert.Empty(t, session.Scope.VpcID)
		assert.Empty(t, session.Scope.VpcName)
		return nil, assert.AnError
	})

	_, err := promptVPCPeeringVPCs(session, "site-1")
	require.ErrorIs(t, err, assert.AnError)
	assert.Equal(t, "vpc-1", session.Scope.VpcID)
	assert.Equal(t, "VPC 1", session.Scope.VpcName)
	assert.Nil(t, session.Cache.Get("vpc"))
}

func TestReadyVPCsForSite(t *testing.T) {
	vpcs := []NamedItem{
		{ID: "ready", Status: "Ready", Extra: map[string]string{"siteId": "site-1"}},
		{ID: "pending", Status: "Pending", Extra: map[string]string{"siteId": "site-1"}},
		{ID: "other-site", Status: "Ready", Extra: map[string]string{"siteId": "site-2"}},
	}

	assert.Equal(t, []NamedItem{vpcs[0]}, readyVPCsForSite(vpcs, "site-1"))
}

func testVPCPeeringItems(count int) []NamedItem {
	items := make([]NamedItem, count)
	for i := range count {
		id := i + 1
		items[i] = NamedItem{
			Name:  fmt.Sprintf("VPC %d", id),
			ID:    fmt.Sprintf("vpc-%d", id),
			Extra: map[string]string{"siteId": "site-1"},
		}
	}
	return items
}

func TestPrintVPCPeeringPlan(t *testing.T) {
	selected := testVPCPeeringItems(3)
	plan := buildVPCPeeringPlan(selected, nil)
	var output bytes.Buffer
	printVPCPeeringPlan(&output, selected, plan)

	assert.Contains(t, output.String(), "Selected VPCs (3)")
	assert.Contains(t, output.String(), "Peerings to create (3)")
	assert.Contains(t, output.String(), "Existing peerings to skip (0)")
	assert.Equal(t, 3, strings.Count(output.String(), "<->"))
}
