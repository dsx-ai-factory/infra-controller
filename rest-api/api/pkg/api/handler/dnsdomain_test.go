// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"cmp"
	"encoding/json"
	"net/http"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	testDomainIDA = "00000000-0000-0000-0000-00000000000a"
	testDomainIDB = "00000000-0000-0000-0000-00000000000b"
	testDomainIDC = "00000000-0000-0000-0000-00000000000c"

	testDomainIDReverseV4 = "00000000-0000-0000-0000-0000000000d4"
	testDomainIDReverseV6 = "00000000-0000-0000-0000-0000000000d6"
)

// domainTestSiteID stands for the fixture's Site ID in a test case's request.
const domainTestSiteID = "{siteId}"

func TestCreateDNSDomainHandler_Handle(t *testing.T) {
	created := &corev1.Domain{
		Id:         &corev1.DomainId{Value: testDomainIDA},
		Name:       "tenant-a.example.com",
		DefaultTtl: cutil.GetPtr(uint32(600)),
	}

	tests := []struct {
		name        string
		roles       []string
		body        any
		wantStatus  int
		wantMessage string
		wantTTL     *uint32
	}{
		{
			name:       "creates the Domain in Core and returns it",
			body:       map[string]any{"siteId": domainTestSiteID, "name": "tenant-a.example.com", "defaultTtl": 600},
			wantStatus: http.StatusCreated,
			wantTTL:    cutil.GetPtr(uint32(600)),
		},
		{
			name:       "omitted TTL is not sent to Core",
			body:       map[string]any{"siteId": domainTestSiteID, "name": "tenant-a.example.com"},
			wantStatus: http.StatusCreated,
		},
		{
			name:        "malformed body",
			body:        "invalid",
			wantStatus:  http.StatusBadRequest,
			wantMessage: "Failed to parse request data, potentially invalid structure",
		},
		{
			name:        "missing name",
			body:        map[string]any{"siteId": domainTestSiteID},
			wantStatus:  http.StatusBadRequest,
			wantMessage: "Error validating Domain create request data",
		},
		{
			name:       "Tenant Admin is rejected",
			roles:      []string{authz.TenantAdminRole},
			body:       map[string]any{"siteId": domainTestSiteID, "name": "tenant-a.example.com"},
			wantStatus: http.StatusForbidden,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newMeasuredBootHandlerFixture(t, created, tt.roles)
			handler := NewCreateDNSDomainHandler(fixture.dbSession, fixture.scp)

			body := tt.body
			if fields, ok := body.(map[string]any); ok {
				fields["siteId"] = fixture.siteID
			}
			rec := fixture.request(t, handler.Handle, http.MethodPost, "/", "", body)
			require.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())

			if tt.wantStatus != http.StatusCreated {
				assert.Empty(t, fixture.proxiedReq.FullMethod, "a rejected request must not reach Core")
				if tt.wantMessage != "" {
					var apiErr struct {
						Message string `json:"message"`
					}
					require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiErr))
					assert.Equal(t, tt.wantMessage, apiErr.Message)
				}
				return
			}

			assert.Equal(t, corev1.Forge_CreateDomain_FullMethodName, fixture.proxiedReq.FullMethod)
			var coreReq corev1.CreateDomainRequest
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
			assert.Equal(t, "tenant-a.example.com", coreReq.GetName())
			assert.Equal(t, tt.wantTTL, coreReq.DefaultTtl)

			assert.JSONEq(t, `{"id":"`+testDomainIDA+`","siteId":"`+fixture.siteID+`","name":"tenant-a.example.com","defaultTtl":600,"created":null,"updated":null}`, rec.Body.String())
		})
	}
}

func TestGetAllDNSDomainHandler_Handle(t *testing.T) {
	// Core returns Domains in no particular order; two share a name. It also
	// returns the reverse zones it keeps for Network Segment prefixes, which
	// the list must leave out.
	coreDomains := &corev1.DomainList{Domains: []*corev1.Domain{
		{Id: &corev1.DomainId{Value: testDomainIDReverseV4}, Name: "254.254.254.169.in-addr.arpa"},
		{Id: &corev1.DomainId{Value: testDomainIDC}, Name: "b.example.com"},
		{Id: &corev1.DomainId{Value: testDomainIDB}, Name: "a.example.com"},
		{Id: &corev1.DomainId{Value: testDomainIDReverseV6}, Name: "8.b.d.0.1.0.0.2.ip6.arpa"},
		{Id: &corev1.DomainId{Value: testDomainIDA}, Name: "a.example.com"},
	}}
	const wantTotal = 3

	tests := []struct {
		name        string
		roles       []string
		query       string
		wantStatus  int
		wantIDs     []string
		wantOrderBy string
		wantPage    int
		wantCoreReq string
	}{
		{
			name:        "orders by name then ID by default",
			query:       "siteId=" + domainTestSiteID,
			wantStatus:  http.StatusOK,
			wantIDs:     []string{testDomainIDA, testDomainIDB, testDomainIDC},
			wantOrderBy: domainOrderByNameAsc,
			wantPage:    1,
		},
		{
			name:        "descending order still breaks ties by ascending ID",
			query:       "siteId=" + domainTestSiteID + "&orderBy=NAME_DESC",
			wantStatus:  http.StatusOK,
			wantIDs:     []string{testDomainIDC, testDomainIDA, testDomainIDB},
			wantOrderBy: domainOrderByNameDesc,
			wantPage:    1,
		},
		{
			name:        "descending order pages after sorting",
			query:       "siteId=" + domainTestSiteID + "&orderBy=NAME_DESC&pageSize=1&pageNumber=2",
			wantStatus:  http.StatusOK,
			wantIDs:     []string{testDomainIDA},
			wantOrderBy: domainOrderByNameDesc,
			wantPage:    2,
		},
		{
			name:        "page past the end is empty",
			query:       "siteId=" + domainTestSiteID + "&pageSize=2&pageNumber=3",
			wantStatus:  http.StatusOK,
			wantIDs:     []string{},
			wantOrderBy: domainOrderByNameAsc,
			wantPage:    3,
		},
		{name: "missing Site", query: "", wantStatus: http.StatusBadRequest},
		{
			name:        "name filter is passed to Core",
			query:       "siteId=" + domainTestSiteID + "&name=a.example.com",
			wantStatus:  http.StatusOK,
			wantIDs:     []string{testDomainIDA, testDomainIDB, testDomainIDC},
			wantOrderBy: domainOrderByNameAsc,
			wantPage:    1,
			wantCoreReq: `{"name":"a.example.com"}`,
		},
		{name: "empty name filter", query: "siteId=" + domainTestSiteID + "&name=", wantStatus: http.StatusBadRequest},
		{name: "unknown query parameter", query: "siteId=" + domainTestSiteID + "&zone=a.example.com", wantStatus: http.StatusBadRequest},
		{name: "unsupported order field", query: "siteId=" + domainTestSiteID + "&orderBy=ID_ASC", wantStatus: http.StatusBadRequest},
		{name: "Tenant Admin is rejected", roles: []string{authz.TenantAdminRole}, query: "siteId=" + domainTestSiteID, wantStatus: http.StatusForbidden},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newMeasuredBootHandlerFixture(t, coreDomains, tt.roles)
			handler := NewGetAllDNSDomainHandler(fixture.dbSession, fixture.scp)

			rec := fixture.request(t, handler.Handle, http.MethodGet, "/?"+domainQueryForSite(tt.query, fixture.siteID), "", nil)
			require.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())

			if tt.wantStatus != http.StatusOK {
				assert.Empty(t, fixture.proxiedReq.FullMethod, "a rejected request must not reach Core")
				return
			}

			assert.Equal(t, corev1.Forge_FindDomain_FullMethodName, fixture.proxiedReq.FullMethod)
			wantCoreReq := cmp.Or(tt.wantCoreReq, `{}`)
			assert.JSONEq(t, wantCoreReq, string(fixture.proxiedReq.RequestJSON), "an empty query lists every Domain")

			var resp []struct {
				ID     string `json:"id"`
				SiteID string `json:"siteId"`
			}
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
			gotIDs := make([]string, 0, len(resp))
			for _, domain := range resp {
				gotIDs = append(gotIDs, domain.ID)
				assert.Equal(t, fixture.siteID, domain.SiteID)
			}
			assert.Equal(t, tt.wantIDs, gotIDs)

			var page pagination.PageResponse
			require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &page))
			assert.Equal(t, wantTotal, page.Total, "reverse zones are not counted")
			assert.Equal(t, tt.wantPage, page.PageNumber)
			require.NotNil(t, page.OrderBy)
			assert.Equal(t, tt.wantOrderBy, *page.OrderBy)
		})
	}
}

func TestGetDNSDomainHandler_Handle(t *testing.T) {
	found := &corev1.DomainList{Domains: []*corev1.Domain{
		{Id: &corev1.DomainId{Value: testDomainIDA}, Name: "tenant-a.example.com", DefaultTtl: cutil.GetPtr(uint32(600))},
	}}
	reverseZone := &corev1.DomainList{Domains: []*corev1.Domain{
		{Id: &corev1.DomainId{Value: testDomainIDReverseV4}, Name: "254.254.254.169.in-addr.arpa"},
	}}

	tests := []struct {
		name        string
		roles       []string
		coreResp    *corev1.DomainList
		query       string
		id          string
		wantStatus  int
		wantReached bool
	}{
		{name: "returns the Domain Core finds by ID", coreResp: found, query: "siteId=" + domainTestSiteID, id: testDomainIDA, wantStatus: http.StatusOK, wantReached: true},
		{name: "no match in Core is not found", coreResp: &corev1.DomainList{}, query: "siteId=" + domainTestSiteID, id: testDomainIDB, wantStatus: http.StatusNotFound, wantReached: true},
		{name: "a Network Segment reverse zone is not found", coreResp: reverseZone, query: "siteId=" + domainTestSiteID, id: testDomainIDReverseV4, wantStatus: http.StatusNotFound, wantReached: true},
		{name: "invalid ID", coreResp: found, query: "siteId=" + domainTestSiteID, id: "invalid", wantStatus: http.StatusBadRequest},
		{name: "missing Site", coreResp: found, query: "", id: testDomainIDA, wantStatus: http.StatusBadRequest},
		{name: "unknown query parameter", coreResp: found, query: "siteId=" + domainTestSiteID + "&name=tenant-a.example.com", id: testDomainIDA, wantStatus: http.StatusBadRequest},
		{name: "Tenant Admin is rejected", roles: []string{authz.TenantAdminRole}, coreResp: found, query: "siteId=" + domainTestSiteID, id: testDomainIDA, wantStatus: http.StatusForbidden},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newMeasuredBootHandlerFixture(t, tt.coreResp, tt.roles)
			handler := NewGetDNSDomainHandler(fixture.dbSession, fixture.scp)

			rec := fixture.request(t, handler.Handle, http.MethodGet, "/?"+domainQueryForSite(tt.query, fixture.siteID), tt.id, nil)
			require.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())

			if !tt.wantReached {
				assert.Empty(t, fixture.proxiedReq.FullMethod, "a rejected request must not reach Core")
				return
			}

			assert.Equal(t, corev1.Forge_FindDomain_FullMethodName, fixture.proxiedReq.FullMethod)
			var coreReq corev1.DomainSearchQuery
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
			assert.Equal(t, tt.id, coreReq.GetId().GetValue())

			if tt.wantStatus == http.StatusOK {
				assert.JSONEq(t, `{"id":"`+testDomainIDA+`","siteId":"`+fixture.siteID+`","name":"tenant-a.example.com","defaultTtl":600,"created":null,"updated":null}`, rec.Body.String())
			}
		})
	}
}

func TestUpdateDNSDomainHandler_Handle(t *testing.T) {
	updated := &corev1.Domain{
		Id:         &corev1.DomainId{Value: testDomainIDA},
		Name:       "tenant-a.example.com",
		DefaultTtl: cutil.GetPtr(uint32(900)),
	}

	tests := []struct {
		name       string
		roles      []string
		body       any
		id         string
		wantStatus int
	}{
		{name: "updates the TTL in Core and returns the Domain", body: map[string]any{"siteId": domainTestSiteID, "defaultTtl": 900}, id: testDomainIDA, wantStatus: http.StatusOK},
		{name: "malformed body", body: "invalid", id: testDomainIDA, wantStatus: http.StatusBadRequest},
		{name: "missing TTL", body: map[string]any{"siteId": domainTestSiteID}, id: testDomainIDA, wantStatus: http.StatusBadRequest},
		{name: "TTL out of range", body: map[string]any{"siteId": domainTestSiteID, "defaultTtl": 5}, id: testDomainIDA, wantStatus: http.StatusBadRequest},
		{name: "invalid ID", body: map[string]any{"siteId": domainTestSiteID, "defaultTtl": 900}, id: "invalid", wantStatus: http.StatusBadRequest},
		{name: "ID in the body does not replace the path ID", body: map[string]any{"siteId": domainTestSiteID, "defaultTtl": 900, "id": testDomainIDB}, id: testDomainIDA, wantStatus: http.StatusOK},
		{name: "Tenant Admin is rejected", roles: []string{authz.TenantAdminRole}, body: map[string]any{"siteId": domainTestSiteID, "defaultTtl": 900}, id: testDomainIDA, wantStatus: http.StatusForbidden},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newMeasuredBootHandlerFixture(t, updated, tt.roles)
			handler := NewUpdateDNSDomainHandler(fixture.dbSession, fixture.scp)

			body := tt.body
			if fields, ok := body.(map[string]any); ok {
				fields["siteId"] = fixture.siteID
			}
			rec := fixture.request(t, handler.Handle, http.MethodPatch, "/", tt.id, body)
			require.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())

			if tt.wantStatus != http.StatusOK {
				assert.Empty(t, fixture.proxiedReq.FullMethod, "a rejected request must not reach Core")
				return
			}

			assert.Equal(t, corev1.Forge_UpdateDomain_FullMethodName, fixture.proxiedReq.FullMethod)
			var coreReq corev1.UpdateDomainRequest
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
			assert.Equal(t, tt.id, coreReq.GetDomain().GetId().GetValue())
			assert.Equal(t, uint32(900), coreReq.GetDomain().GetDefaultTtl())
			assert.Empty(t, coreReq.GetDomain().GetName(), "the name is not sent, so Core keeps it")

			assert.JSONEq(t, `{"id":"`+testDomainIDA+`","siteId":"`+fixture.siteID+`","name":"tenant-a.example.com","defaultTtl":900,"created":null,"updated":null}`, rec.Body.String())
		})
	}
}

func TestDeleteDNSDomainHandler_Handle(t *testing.T) {
	tests := []struct {
		name       string
		roles      []string
		query      string
		id         string
		wantStatus int
	}{
		{name: "deletes the Domain in Core", query: "siteId=" + domainTestSiteID, id: testDomainIDA, wantStatus: http.StatusNoContent},
		{name: "invalid ID", query: "siteId=" + domainTestSiteID, id: "invalid", wantStatus: http.StatusBadRequest},
		{name: "missing Site", query: "", id: testDomainIDA, wantStatus: http.StatusBadRequest},
		{name: "unknown query parameter", query: "siteId=" + domainTestSiteID + "&force=true", id: testDomainIDA, wantStatus: http.StatusBadRequest},
		{name: "Tenant Admin is rejected", roles: []string{authz.TenantAdminRole}, query: "siteId=" + domainTestSiteID, id: testDomainIDA, wantStatus: http.StatusForbidden},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newMeasuredBootHandlerFixture(t, &corev1.DomainDeletionResult{}, tt.roles)
			handler := NewDeleteDNSDomainHandler(fixture.dbSession, fixture.scp)

			rec := fixture.request(t, handler.Handle, http.MethodDelete, "/?"+domainQueryForSite(tt.query, fixture.siteID), tt.id, nil)
			require.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())

			if tt.wantStatus != http.StatusNoContent {
				assert.Empty(t, fixture.proxiedReq.FullMethod, "a rejected request must not reach Core")
				return
			}

			assert.Empty(t, rec.Body.String())
			assert.Equal(t, corev1.Forge_DeleteDomain_FullMethodName, fixture.proxiedReq.FullMethod)
			var coreReq corev1.DomainDeletionRequest
			require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreReq))
			assert.Equal(t, tt.id, coreReq.GetId().GetValue())
		})
	}
}

// domainQueryForSite swaps the placeholder in a test case's query for the fixture's Site ID.
func domainQueryForSite(query, siteID string) string {
	return strings.ReplaceAll(query, domainTestSiteID, siteID)
}
