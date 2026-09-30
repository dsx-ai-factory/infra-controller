// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	inventorymanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/manager"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
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
	orderBy        *dbquery.OrderBy
	externalOnly   bool
}

func (m *domainReadInventory) GetListOfRacks(_ context.Context, info dbquery.StringQueryInfo, _, _ *dbquery.StringQueryInfo, pagination *dbquery.Pagination, orderBy *dbquery.OrderBy, withComponents, externalOnly bool) ([]*rack.Rack, int32, error) {
	m.info, m.pagination, m.orderBy = info, pagination, orderBy
	m.withComponents, m.externalOnly = withComponents, externalOnly
	return m.racks, 7, m.err
}

func (m *domainReadInventory) GetRacksForNVLDomain(_ context.Context, id identifier.Identifier, withComponents bool) ([]*rack.Rack, error) {
	m.id = id
	m.withComponents = withComponents
	return m.racks, m.err
}

func TestFlowServerImpl_GetNVLinkDomain(t *testing.T) {
	for _, tc := range []struct {
		name, id       string
		count          int
		err            error
		code           codes.Code
		withComponents bool
	}{
		{name: "external ID", id: "Rack-01", count: 1},
		{name: "include components", id: "Rack-01", count: 1, withComponents: true},
		{name: "missing", id: "missing", code: codes.NotFound},
		{name: "ambiguous legacy membership", id: "legacy", count: 2, code: codes.FailedPrecondition},
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
			assert.Equal(t, "Rack-01", got.GetDomain().GetId())
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
			assert.True(t, m.externalOnly)
			assert.Equal(t, tc.withComponents, m.withComponents)
			assert.Equal(t, req.GetInfo().GetPatterns(), m.info.Patterns)
			assert.True(t, m.info.UseOR)
			assert.False(t, m.info.IsWildcard)
			assert.EqualValues(t, 2, m.pagination.Offset)
			assert.EqualValues(t, 1, m.pagination.Limit)
			direction := "ASC"
			if tc.order == "NAME_DESC" {
				direction = "DESC"
			}
			assert.Equal(t, direction, string(m.orderBy.Direction))
			if tc.empty {
				assert.Empty(t, got.GetDomains())
			} else {
				require.Len(t, got.GetDomains(), 1)
				assert.Equal(t, "rack-01", got.GetDomains()[0].GetId())
				assert.Equal(t, "GB300_NVL72R1_C2G4", got.GetDomains()[0].GetTopology())
			}
		})
	}
}
