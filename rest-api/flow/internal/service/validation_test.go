// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	inventorystore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/store"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// The list callbacks assert the query boundary and supply inventory results;
// existing inventory-store tests own SQL matching, not this service fixture.
type validationManager struct {
	*mockManager
	listRacks      func(dbquery.StringQueryInfo, *dbquery.StringQueryInfo, *dbquery.StringQueryInfo, *dbquery.Pagination, bool, bool) ([]*rack.Rack, int32, error)
	listComponents func(*dbquery.Pagination) ([]*component.Component, int32, error)
}

func (m *validationManager) GetListOfRacks(_ context.Context, name dbquery.StringQueryInfo, manufacturer, model *dbquery.StringQueryInfo, page *dbquery.Pagination, _ *dbquery.OrderBy, withComponents, externalOnly bool) ([]*rack.Rack, int32, error) {
	return m.listRacks(name, manufacturer, model, page, withComponents, externalOnly)
}

func (m *validationManager) GetListOfComponents(_ context.Context, name dbquery.StringQueryInfo, manufacturer, model *dbquery.StringQueryInfo, types []devicetypes.ComponentType, page *dbquery.Pagination, _ *dbquery.OrderBy) ([]*component.Component, int32, error) {
	return m.listComponents(page)
}

func TestFlowServerImpl_ValidateComponents(t *testing.T) {
	t.Run("invalid component ordering", func(t *testing.T) {
		_, err := (&FlowServerImpl{inventoryManager: newMockManager()}).ValidateComponents(
			t.Context(),
			&pb.ValidateComponentsRequest{OrderBy: &pb.OrderBy{
				Field:     &pb.OrderBy_RackField{RackField: pb.RackOrderByField_RACK_ORDER_BY_FIELD_MODEL},
				Direction: "ASC",
			}},
		)

		require.Error(t, err)
		assert.Equal(t, codes.InvalidArgument, status.Code(err))

	})
	rackName := func(name string) *pb.Filter {
		return &pb.Filter{Field: &pb.Filter_RackField{RackField: pb.RackFilterField_RACK_FILTER_FIELD_NAME}, QueryInfo: &pb.StringQueryInfo{Patterns: []string{name}}}
	}
	componentFilter := func(field pb.ComponentFilterField, value string) *pb.Filter {
		return &pb.Filter{Field: &pb.Filter_ComponentField{ComponentField: field}, QueryInfo: &pb.StringQueryInfo{Patterns: []string{value}}}
	}
	cases := []struct {
		name                              string
		filters                           []*pb.Filter
		target                            bool
		emptyRack                         bool
		listError                         bool
		total, missing, mismatch, matches int32
	}{
		{name: "rack name selects parent not component", filters: []*pb.Filter{rackName("test-rack")}, total: 3, missing: 1, mismatch: 2, matches: 1},
		{name: "absent rack name stays empty", filters: []*pb.Filter{rackName("absent-rack")}, emptyRack: true},
		{name: "absent tray name stays empty", filters: []*pb.Filter{componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME, "absent-tray")}},
		{name: "tray name without target", filters: []*pb.Filter{componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME, "compute-01")}, total: 1, mismatch: 1},
		{name: "tray type without target", filters: []*pb.Filter{componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE, "nvswitch")}, total: 1, mismatch: 1},
		{name: "tray manufacturer without target", filters: []*pb.Filter{componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_MANUFACTURER, "NVIDIA")}, total: 2, missing: 1, mismatch: 1},
		{name: "rack and tray filters intersect", filters: []*pb.Filter{rackName("test-rack"), componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME, "compute-01")}, total: 1, mismatch: 1},
		{name: "target and empty rack selection intersect", filters: []*pb.Filter{rackName("absent-rack")}, target: true, emptyRack: true},
		{name: "target filtered to empty", filters: []*pb.Filter{componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME, "absent-tray")}, target: true},
		{name: "rack lookup failure propagates", filters: []*pb.Filter{rackName("test-rack")}, listError: true},
		{name: "component lookup failure propagates", filters: []*pb.Filter{componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME, "compute-01")}, listError: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			base := newMockManager()
			rackID, ids := setupValidateTestData(base)
			// A healthy member makes the selected-set match count observable.
			healthy := component.Component{Info: deviceinfo.DeviceInfo{ID: uuid.New(), Name: "healthy"}, RackID: rackID}
			base.racks[rackID].Components = append(base.racks[rackID].Components, healthy)
			base.components[healthy.Info.ID] = &healthy
			outsideID := uuid.New()
			outside := &component.Component{Info: deviceinfo.DeviceInfo{ID: outsideID, Name: "test-rack"}, RackID: uuid.New()}
			base.components[outsideID] = outside
			base.drifts = append(base.drifts, inventorystore.ComponentDrift{ID: uuid.New(), ComponentID: &outsideID, DriftType: "mismatch"})
			mgr := &validationManager{mockManager: base}
			mgr.listRacks = func(name dbquery.StringQueryInfo, manufacturer, model *dbquery.StringQueryInfo, page *dbquery.Pagination, withComponents, externalOnly bool) ([]*rack.Rack, int32, error) {
				assert.Equal(t, tc.filters[0].GetQueryInfo().GetPatterns(), name.Patterns)
				assert.Nil(t, manufacturer)
				assert.Nil(t, model)
				assert.True(t, withComponents)
				assert.False(t, externalOnly)
				if tc.listError {
					return nil, 0, assert.AnError
				}
				if tc.emptyRack {
					return nil, 0, nil
				}
				return []*rack.Rack{base.racks[rackID]}, 1, nil
			}
			mgr.listComponents = func(page *dbquery.Pagination) ([]*component.Component, int32, error) {
				if tc.listError {
					return nil, 0, assert.AnError
				}
				return []*component.Component{base.components[ids[0]], base.components[ids[1]], base.components[ids[2]], &healthy, outside}, 5, nil
			}
			server := &FlowServerImpl{inventoryManager: mgr}
			req := &pb.ValidateComponentsRequest{Filters: tc.filters}
			if tc.target {
				req.TargetSpec = &pb.OperationTargetSpec{Targets: &pb.OperationTargetSpec_Racks{Racks: &pb.RackTargets{Targets: []*pb.RackTarget{{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}}}}}}
			}
			resp, err := server.ValidateComponents(context.Background(), req)
			if tc.listError {
				require.ErrorIs(t, err, assert.AnError)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.total, resp.GetTotalDiffs())
			assert.Len(t, resp.GetDiffs(), int(tc.total))
			assert.Equal(t, tc.missing, resp.GetMissingCount())
			assert.Zero(t, resp.GetUnexpectedCount())
			assert.Equal(t, tc.mismatch, resp.GetMismatchCount())
			assert.Equal(t, tc.matches, resp.GetMatchCount())
			for _, diff := range resp.GetDiffs() {
				assert.NotEqual(t, outsideID.String(), diff.GetId().GetId())
			}
			if tc.name == "rack name selects parent not component" {
				detail, detailErr := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{TargetSpec: &pb.OperationTargetSpec{Targets: &pb.OperationTargetSpec_Racks{Racks: &pb.RackTargets{Targets: []*pb.RackTarget{{Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}}}}}}}})
				require.NoError(t, detailErr)
				assert.Equal(t, detail, resp)
			}
		})
	}
	// Selection pagination must not be confused with response pagination.
	for _, byRack := range []bool{false, true} {
		t.Run(fmt.Sprintf("selection beyond first inventory page rack=%t", byRack), func(t *testing.T) {
			base := newMockManager()
			var components []*component.Component
			var racks []*rack.Rack
			for i := 0; i < dbquery.DefaultPaginationLimit+1; i++ {
				id := uuid.New()
				comp := component.Component{Info: deviceinfo.DeviceInfo{ID: id, Name: "selected"}}
				components = append(components, &comp)
				racks = append(racks, &rack.Rack{Components: []component.Component{comp}})
				if i == dbquery.DefaultPaginationLimit {
					base.drifts = append(base.drifts, inventorystore.ComponentDrift{ID: uuid.New(), ComponentID: &id, DriftType: "mismatch"})
				}
			}
			mgr := &validationManager{mockManager: base}
			calls := 0
			mgr.listRacks = func(_ dbquery.StringQueryInfo, _, _ *dbquery.StringQueryInfo, page *dbquery.Pagination, _, _ bool) ([]*rack.Rack, int32, error) {
				calls++
				end := min(page.Offset+page.Limit, len(racks))
				return racks[page.Offset:end], int32(len(racks)), nil
			}
			mgr.listComponents = func(page *dbquery.Pagination) ([]*component.Component, int32, error) {
				calls++
				end := min(page.Offset+page.Limit, len(components))
				return components[page.Offset:end], int32(len(components)), nil
			}
			filter := componentFilter(pb.ComponentFilterField_COMPONENT_FILTER_FIELD_NAME, "selected")
			if byRack {
				filter = rackName("selected-racks")
			}
			server := &FlowServerImpl{inventoryManager: mgr}
			resp, err := server.ValidateComponents(context.Background(), &pb.ValidateComponentsRequest{Filters: []*pb.Filter{filter}})
			require.NoError(t, err)
			assert.Equal(t, 2, calls)
			assert.Equal(t, int32(1), resp.GetTotalDiffs())
			assert.Equal(t, int32(dbquery.DefaultPaginationLimit), resp.GetMatchCount())
		})
	}
}
