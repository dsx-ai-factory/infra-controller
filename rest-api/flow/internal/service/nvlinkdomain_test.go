// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"github.com/google/uuid"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	inventorymanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/manager"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/nvldomain"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

type domainReadInventory struct {
	inventorymanager.Manager
	racks          []*rack.Rack
	err            error
	id             identifier.Identifier
	withComponents bool
	info           dbquery.StringQueryInfo
	pagination     *dbquery.Pagination
	descending     bool
	externalOnly   bool
	batchCalls     int
}

func (m *domainReadInventory) GetNVLDomain(_ context.Context, id identifier.Identifier) (*nvldomain.NVLDomain, error) {
	m.id = id
	if m.err != nil {
		return nil, m.err
	}
	if id.ExternalID == "missing" {
		return nil, status.Error(codes.NotFound, "missing")
	}
	cluster := uuid.MustParse("10000000-0000-0000-0000-000000000001")
	return &nvldomain.NVLDomain{Identifier: identifier.Identifier{ID: cluster, ExternalID: id.ExternalID}, NMXCClusterID: &cluster}, nil
}

func (m *domainReadInventory) GetListOfNVLDomains(_ context.Context, info dbquery.StringQueryInfo, pagination *dbquery.Pagination, options ...nvldomain.ListOptions) ([]*nvldomain.NVLDomain, int32, error) {
	m.info, m.pagination = info, pagination
	m.descending, m.externalOnly = options[0].Descending, options[0].ExternalOnly
	if len(m.racks) == 0 {
		return nil, 7, m.err
	}
	domain, _ := m.GetNVLDomain(context.Background(), identifier.Identifier{ExternalID: "group-01"})
	return []*nvldomain.NVLDomain{domain}, 7, m.err
}

func (m *domainReadInventory) GetRacksForNVLDomain(_ context.Context, id identifier.Identifier, withComponents bool) ([]*rack.Rack, error) {
	m.withComponents = withComponents
	return m.racks, m.err
}

func (m *domainReadInventory) GetRacksForNVLDomains(_ context.Context, ids []uuid.UUID, withComponents bool) (map[uuid.UUID][]*rack.Rack, error) {
	m.batchCalls++
	m.withComponents = withComponents
	members := make(map[uuid.UUID][]*rack.Rack)
	for _, id := range ids {
		members[id] = m.racks
	}
	return members, m.err
}

func TestFlowServerImpl_GetNVLinkDomain(t *testing.T) {
	for _, tc := range []struct {
		name, id       string
		count          int
		err            error
		code           codes.Code
		withComponents bool
	}{
		{name: "external ID", id: "group-01", count: 1},
		{name: "include components", id: "group-01", count: 1, withComponents: true},
		{name: "missing", id: "missing", code: codes.NotFound},
		{name: "multiple member racks", id: "group-01", count: 2},
		{name: "query failure", id: "rack", err: status.Error(codes.Internal, "unavailable"), code: codes.Internal},
		{name: "blank", id: " ", code: codes.InvalidArgument},
	} {
		t.Run(tc.name, func(t *testing.T) {
			m := &domainReadInventory{err: tc.err}
			for range tc.count {
				profile := "GB200_NVL72R1_C2G4_NVIDIA"
				m.racks = append(m.racks, &rack.Rack{ExternalID: "Rack-01", RackProfileID: &profile})
			}
			got, err := (&FlowServerImpl{inventoryManager: m}).GetNVLinkDomain(t.Context(), &pb.GetNVLinkDomainRequest{Id: tc.id, WithComponents: tc.withComponents})
			if tc.code != codes.OK {
				require.Error(t, err)
				assert.Equal(t, tc.code, status.Code(err))
				return
			}
			require.NoError(t, err)
			assert.Equal(t, identifier.Identifier{ExternalID: tc.id}, m.id)
			assert.Equal(t, tc.withComponents, m.withComponents)
			assert.Equal(t, "group-01", got.GetDomain().GetId())
			assert.Equal(t, "group-01", got.GetDomain().GetRackGroupId())
			assert.Empty(t, got.GetDomain().GetName())
			assert.NotEmpty(t, got.GetDomain().GetNmxcClusterId())
			assert.Equal(t, "GB200_NVL72R1_C2G4", got.GetDomain().GetTopology())
		})
	}
}

func TestFlowServerImpl_GetListOfNVLinkDomains(t *testing.T) {
	for _, tc := range []struct {
		name, order           string
		withComponents, empty bool
		err                   error
		invalid               bool
	}{
		{name: "default order"},
		{name: "descending with components", order: "NAME_DESC", withComponents: true},
		{name: "empty page", empty: true},
		{name: "query failure", err: status.Error(codes.Unavailable, "database unavailable")},
		{name: "invalid order", order: "OTHER_ASC", invalid: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			m := &domainReadInventory{err: tc.err}
			if !tc.empty {
				profile := "GB300_NVL72R1_C2G4_SMC_NO_POWERSHELF"
				m.racks = []*rack.Rack{{ExternalID: "rack-01", RackProfileID: &profile}}
			}
			req := &pb.GetListOfNVLinkDomainsRequest{
				Info:       &pb.StringQueryInfo{Patterns: []string{"domain-a", "domain-b"}, UseOr: true},
				Pagination: &pb.Pagination{Offset: 2, Limit: 1}, OrderBy: tc.order, WithComponents: tc.withComponents,
			}
			got, err := (&FlowServerImpl{inventoryManager: m}).GetListOfNVLinkDomains(t.Context(), req)
			if tc.invalid {
				assert.Equal(t, codes.InvalidArgument, status.Code(err))
				assert.Nil(t, m.pagination)
				return
			}
			if tc.err != nil {
				require.ErrorIs(t, err, tc.err)
				return
			}
			require.NoError(t, err)
			assert.EqualValues(t, 7, got.GetTotal())
			assert.Equal(t, 1, m.batchCalls)
			assert.True(t, m.externalOnly)
			if !tc.empty {
				assert.Equal(t, tc.withComponents, m.withComponents)
			}
			assert.Equal(t, req.GetInfo().GetPatterns(), m.info.Patterns)
			assert.True(t, m.info.UseOR)
			assert.False(t, m.info.IsWildcard)
			assert.EqualValues(t, 2, m.pagination.Offset)
			assert.EqualValues(t, 1, m.pagination.Limit)
			assert.Equal(t, tc.order == "NAME_DESC", m.descending)
			if tc.empty {
				assert.Empty(t, got.GetDomains())
			} else {
				require.Len(t, got.GetDomains(), 1)
				assert.Equal(t, "group-01", got.GetDomains()[0].GetId())
				assert.Empty(t, got.GetDomains()[0].GetName())
				assert.Equal(t, "GB300_NVL72R1_C2G4", got.GetDomains()[0].GetTopology())
			}
		})
	}
}
