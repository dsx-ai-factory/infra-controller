// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/firmwareauth"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskschedule "github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/taskschedule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestCreateTaskScheduleFirmwareAuthenticationData(t *testing.T) {
	tests := []struct {
		name               string
		withCipher         bool
		authenticationData string
	}{
		{
			name:               "encrypts authentication data before persistence",
			withCipher:         true,
			authenticationData: "schedule-token",
		},
		{name: "missing cipher allows schedule without authentication data"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rackID := uuid.New()
			componentID := uuid.New()
			inventory := newMockManager()
			inventory.components[componentID] = &component.Component{
				Info:   deviceinfo.DeviceInfo{ID: componentID},
				RackID: rackID,
				Type:   devicetypes.ComponentTypeCompute,
			}
			store := &capturingScheduleStore{id: uuid.New()}
			var cipher *secret.Cipher
			if tt.withCipher {
				cipher = newServiceTestCipher(t)
			}
			server := &FlowServerImpl{
				inventoryManager:  inventory,
				taskScheduleStore: store,
				dataCipher:        cipher,
			}
			var authenticationData *pb.FirmwareAuthenticationData
			if tt.authenticationData != "" {
				authenticationData = sharedServiceAuthenticationData(
					tt.authenticationData,
				)
			}

			_, err := server.CreateTaskSchedule(
				context.Background(),
				firmwareScheduleRequest(componentID, authenticationData),
			)

			require.NoError(t, err)
			if tt.authenticationData != "" {
				require.NotContains(
					t,
					string(store.row.OperationTemplate),
					tt.authenticationData,
				)
			}
			wrapper, err := taskschedule.WrapperFromTemplate(store.row.OperationTemplate)
			require.NoError(t, err)
			var info operations.FirmwareControlTaskInfo
			require.NoError(t, info.Unmarshal(wrapper.Info))
			if tt.authenticationData == "" {
				require.Nil(t, info.AuthenticationData)
				return
			}

			got, err := firmwareauth.DecryptFor(
				cipher,
				info.AuthenticationData,
				devicetypes.ComponentTypeCompute,
			)
			require.NoError(t, err)
			require.Equal(t, tt.authenticationData, got)
		})
	}
}

func TestCreateTaskScheduleAuthenticationDataStatusCodes(t *testing.T) {
	componentID := uuid.New()
	tests := []struct {
		name               string
		cipher             *secret.Cipher
		authenticationData *pb.FirmwareAuthenticationData
		subTargets         []string
		wantCode           codes.Code
	}{
		{
			name:               "invalid input",
			cipher:             newServiceTestCipher(t),
			authenticationData: &pb.FirmwareAuthenticationData{},
			wantCode:           codes.InvalidArgument,
		},
		{
			name:               "missing cipher",
			authenticationData: sharedServiceAuthenticationData("token"),
			wantCode:           codes.FailedPrecondition,
		},
		{
			name:               "authentication with dpu-only subtargets",
			cipher:             newServiceTestCipher(t),
			authenticationData: sharedServiceAuthenticationData("token"),
			subTargets:         []string{"dpu"},
			wantCode:           codes.InvalidArgument,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &capturingScheduleStore{id: uuid.New()}
			server := &FlowServerImpl{
				dataCipher:        tt.cipher,
				taskScheduleStore: store,
			}
			req := firmwareScheduleRequest(componentID, tt.authenticationData)
			req.Operation.GetUpgradeFirmware().SubTargets = tt.subTargets
			_, err := server.CreateTaskSchedule(
				context.Background(),
				req,
			)
			require.Equal(t, tt.wantCode, status.Code(err))
			require.Nil(t, store.row)
		})
	}
}

func firmwareScheduleRequest(
	componentID uuid.UUID,
	authenticationData *pb.FirmwareAuthenticationData,
) *pb.CreateTaskScheduleRequest {
	return &pb.CreateTaskScheduleRequest{
		Schedule: &pb.ScheduleConfig{
			Name: "firmware-schedule",
			Spec: &pb.ScheduleSpec{
				Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL,
				Spec: "1h",
			},
		},
		Operation: &pb.ScheduledOperation{
			Operation: &pb.ScheduledOperation_UpgradeFirmware{
				UpgradeFirmware: &pb.UpgradeFirmwareRequest{
					TargetSpec: &pb.OperationTargetSpec{
						Targets: &pb.OperationTargetSpec_Components{
							Components: &pb.ComponentTargets{
								Targets: []*pb.ComponentTarget{
									{
										Identifier: &pb.ComponentTarget_Id{
											Id: &pb.UUID{Id: componentID.String()},
										},
									},
								},
							},
						},
					},
					AuthenticationData: authenticationData,
				},
			},
		},
	}
}

type capturingScheduleStore struct {
	taskschedule.Store
	id  uuid.UUID
	row *dbmodel.TaskSchedule
}

func (s *capturingScheduleStore) RunInTransaction(
	ctx context.Context,
	fn func(context.Context) error,
) error {
	return fn(ctx)
}

func (s *capturingScheduleStore) Create(
	_ context.Context,
	row *dbmodel.TaskSchedule,
) (uuid.UUID, error) {
	s.row = row
	s.row.ID = s.id
	s.row.CreatedAt = time.Now()
	s.row.UpdatedAt = s.row.CreatedAt
	return s.id, nil
}

func (*capturingScheduleStore) CreateScopes(
	context.Context,
	[]*dbmodel.TaskScheduleScope,
) error {
	return nil
}

func (s *capturingScheduleStore) Get(
	context.Context,
	uuid.UUID,
) (*dbmodel.TaskSchedule, error) {
	return s.row, nil
}

// ─── enum converters ─────────────────────────────────────────────────────────

func TestProtoSpecTypeToModel(t *testing.T) {
	tests := []struct {
		proto pb.ScheduleSpecType
		want  dbmodel.SpecType
	}{
		{pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL, dbmodel.SpecTypeInterval},
		{pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON, dbmodel.SpecTypeCron},
		{pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_ONE_TIME, dbmodel.SpecTypeOneTime},
		{pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_UNSPECIFIED, ""},
	}
	for _, tt := range tests {
		assert.Equal(t, tt.want, protoSpecTypeToModel(tt.proto))
	}
}

func TestModelSpecTypeToProto(t *testing.T) {
	tests := []struct {
		model dbmodel.SpecType
		want  pb.ScheduleSpecType
	}{
		{dbmodel.SpecTypeInterval, pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL},
		{dbmodel.SpecTypeCron, pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON},
		{dbmodel.SpecTypeOneTime, pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_ONE_TIME},
		{"unknown", pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_UNSPECIFIED},
	}
	for _, tt := range tests {
		assert.Equal(t, tt.want, modelSpecTypeToProto(tt.model))
	}
}

func TestProtoOverlapPolicyToModel(t *testing.T) {
	tests := []struct {
		proto pb.OverlapPolicy
		want  dbmodel.OverlapPolicy
	}{
		{pb.OverlapPolicy_OVERLAP_POLICY_QUEUE, dbmodel.OverlapPolicyQueue},
		{pb.OverlapPolicy_OVERLAP_POLICY_SKIP, dbmodel.OverlapPolicySkip},
		{pb.OverlapPolicy_OVERLAP_POLICY_UNSPECIFIED, dbmodel.OverlapPolicySkip},
	}
	for _, tt := range tests {
		assert.Equal(t, tt.want, protoOverlapPolicyToModel(tt.proto))
	}
}

func TestModelOverlapPolicyToProto(t *testing.T) {
	tests := []struct {
		model dbmodel.OverlapPolicy
		want  pb.OverlapPolicy
	}{
		{dbmodel.OverlapPolicyQueue, pb.OverlapPolicy_OVERLAP_POLICY_QUEUE},
		{dbmodel.OverlapPolicySkip, pb.OverlapPolicy_OVERLAP_POLICY_SKIP},
		{"unknown", pb.OverlapPolicy_OVERLAP_POLICY_SKIP},
	}
	for _, tt := range tests {
		assert.Equal(t, tt.want, modelOverlapPolicyToProto(tt.model))
	}
}

// ─── unionSlice ──────────────────────────────────────────────────────────────

func TestUnionSlice(t *testing.T) {
	t.Run("disjoint slices", func(t *testing.T) {
		union, changed := unionSlice([]string{"a", "b"}, []string{"c", "d"})
		assert.Equal(t, []string{"a", "b", "c", "d"}, union)
		assert.True(t, changed)
	})

	t.Run("b is subset of a", func(t *testing.T) {
		union, changed := unionSlice([]string{"a", "b", "c"}, []string{"b"})
		assert.Equal(t, []string{"a", "b", "c"}, union)
		assert.False(t, changed)
	})

	t.Run("partial overlap", func(t *testing.T) {
		union, changed := unionSlice([]string{"a", "b"}, []string{"b", "c"})
		assert.Equal(t, []string{"a", "b", "c"}, union)
		assert.True(t, changed)
	})

	t.Run("a empty", func(t *testing.T) {
		union, changed := unionSlice([]string{}, []string{"x"})
		assert.Equal(t, []string{"x"}, union)
		assert.True(t, changed)
	})

	t.Run("both empty", func(t *testing.T) {
		union, changed := unionSlice([]string{}, []string{})
		assert.Empty(t, union)
		assert.False(t, changed)
	})

	t.Run("b empty", func(t *testing.T) {
		union, changed := unionSlice([]string{"a"}, []string{})
		assert.Equal(t, []string{"a"}, union)
		assert.False(t, changed)
	})

	t.Run("works with uuid type", func(t *testing.T) {
		id1 := uuid.MustParse("00000000-0000-0000-0000-000000000001")
		id2 := uuid.MustParse("00000000-0000-0000-0000-000000000002")
		union, changed := unionSlice([]uuid.UUID{id1}, []uuid.UUID{id1, id2})
		assert.Equal(t, []uuid.UUID{id1, id2}, union)
		assert.True(t, changed)
	})

	t.Run("preserves order: a first then new elements from b in b-order", func(t *testing.T) {
		union, changed := unionSlice(
			[]string{"b", "a"},
			[]string{"c", "a", "d"},
		)
		assert.Equal(t, []string{"b", "a", "c", "d"}, union)
		assert.True(t, changed)
	})
}

// ─── mergeComponentFilters ───────────────────────────────────────────────────

// marshalFilter is a test helper that marshals a ComponentFilter and fails the
// test on error.
func marshalFilter(t *testing.T, cf *dbmodel.ComponentFilter) json.RawMessage {
	t.Helper()
	raw, err := dbmodel.MarshalComponentFilter(cf)
	require.NoError(t, err)
	return raw
}

func TestMergeComponentFilters(t *testing.T) {
	t.Run("both nil — no-op", func(t *testing.T) {
		merged, changed, err := mergeComponentFilters(nil, nil)
		require.NoError(t, err)
		assert.Nil(t, merged)
		assert.False(t, changed)
	})

	t.Run("a nil, b non-nil — a already absorbs everything, no change", func(t *testing.T) {
		b := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"Compute"},
		})
		merged, changed, err := mergeComponentFilters(nil, b)
		require.NoError(t, err)
		assert.Nil(t, merged)
		assert.False(t, changed)
	})

	t.Run("a non-nil, b nil — b absorbs a, changed", func(t *testing.T) {
		a := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"Compute"},
		})
		merged, changed, err := mergeComponentFilters(a, nil)
		require.NoError(t, err)
		assert.Nil(t, merged)
		assert.True(t, changed)
	})

	t.Run("types filters disjoint — union", func(t *testing.T) {
		a := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"Compute"},
		})
		b := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"NVSwitch"},
		})
		merged, changed, err := mergeComponentFilters(a, b)
		require.NoError(t, err)
		assert.True(t, changed)

		cf, err := dbmodel.UnmarshalComponentFilter(merged)
		require.NoError(t, err)
		assert.Equal(t, dbmodel.ComponentFilterKindTypes, cf.Kind)
		assert.ElementsMatch(t, []string{"Compute", "NVSwitch"}, cf.Types)
	})

	t.Run("types filters b subset of a — unchanged", func(t *testing.T) {
		a := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"Compute", "NVSwitch"},
		})
		b := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"Compute"},
		})
		merged, changed, err := mergeComponentFilters(a, b)
		require.NoError(t, err)
		assert.False(t, changed)

		cf, err := dbmodel.UnmarshalComponentFilter(merged)
		require.NoError(t, err)
		assert.ElementsMatch(t, []string{"Compute", "NVSwitch"}, cf.Types)
	})

	t.Run("components filters union", func(t *testing.T) {
		id1 := uuid.MustParse("00000000-0000-0000-0000-000000000001")
		id2 := uuid.MustParse("00000000-0000-0000-0000-000000000002")
		a := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:       dbmodel.ComponentFilterKindComponents,
			Components: []uuid.UUID{id1},
		})
		b := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:       dbmodel.ComponentFilterKindComponents,
			Components: []uuid.UUID{id2},
		})
		merged, changed, err := mergeComponentFilters(a, b)
		require.NoError(t, err)
		assert.True(t, changed)

		cf, err := dbmodel.UnmarshalComponentFilter(merged)
		require.NoError(t, err)
		assert.Equal(t, dbmodel.ComponentFilterKindComponents, cf.Kind)
		assert.ElementsMatch(t, []uuid.UUID{id1, id2}, cf.Components)
	})

	t.Run("incompatible kinds — error", func(t *testing.T) {
		a := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"Compute"},
		})
		b := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:       dbmodel.ComponentFilterKindComponents,
			Components: []uuid.UUID{uuid.New()},
		})
		_, _, err := mergeComponentFilters(a, b)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "kinds")
	})
}

// ─── partitionScopeChanges ───────────────────────────────────────────────────

func TestPartitionScopeChanges(t *testing.T) {
	rack1 := uuid.MustParse("00000000-0000-0000-0001-000000000001")
	rack2 := uuid.MustParse("00000000-0000-0000-0001-000000000002")
	rack3 := uuid.MustParse("00000000-0000-0000-0001-000000000003")

	typesFilter := func(t *testing.T, types ...string) json.RawMessage {
		t.Helper()
		return marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: types,
		})
	}

	t.Run("no overlap — all incoming go to toCreate", func(t *testing.T) {
		incoming := []*dbmodel.TaskScheduleScope{
			{RackID: rack1},
			{RackID: rack2},
		}
		toCreate, toMerge, err := partitionScopeChanges(incoming, nil)
		require.NoError(t, err)
		assert.Len(t, toCreate, 2)
		assert.Empty(t, toMerge)
	})

	t.Run("rack already exists with identical nil filter — no-op", func(t *testing.T) {
		existing := []*dbmodel.TaskScheduleScope{{RackID: rack1, ID: uuid.New()}}
		incoming := []*dbmodel.TaskScheduleScope{{RackID: rack1}}
		toCreate, toMerge, err := partitionScopeChanges(incoming, existing)
		require.NoError(t, err)
		assert.Empty(t, toCreate)
		assert.Empty(t, toMerge)
	})

	t.Run("rack exists, incoming expands filter — goes to toMerge", func(t *testing.T) {
		existing := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "Compute")},
		}
		incoming := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "NVSwitch")},
		}
		toCreate, toMerge, err := partitionScopeChanges(incoming, existing)
		require.NoError(t, err)
		assert.Empty(t, toCreate)
		require.Len(t, toMerge, 1)

		cf, err := dbmodel.UnmarshalComponentFilter(toMerge[0].ComponentFilter)
		require.NoError(t, err)
		assert.ElementsMatch(t, []string{"Compute", "NVSwitch"}, cf.Types)
	})

	t.Run("rack exists, incoming already covered — no-op", func(t *testing.T) {
		existing := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "Compute", "NVSwitch")},
		}
		incoming := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "Compute")},
		}
		toCreate, toMerge, err := partitionScopeChanges(incoming, existing)
		require.NoError(t, err)
		assert.Empty(t, toCreate)
		assert.Empty(t, toMerge)
	})

	t.Run("mixed: new rack and existing rack with expansion", func(t *testing.T) {
		existing := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "Compute")},
		}
		incoming := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "NVSwitch")}, // expand
			{RackID: rack2}, // new rack
			{RackID: rack3}, // new rack
		}
		toCreate, toMerge, err := partitionScopeChanges(incoming, existing)
		require.NoError(t, err)
		assert.Len(t, toCreate, 2)
		assert.Len(t, toMerge, 1)
	})

	t.Run("incompatible filter kinds in merge — propagates error", func(t *testing.T) {
		existing := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "Compute")},
		}
		incoming := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: marshalFilter(t, &dbmodel.ComponentFilter{
				Kind:       dbmodel.ComponentFilterKindComponents,
				Components: []uuid.UUID{uuid.New()},
			})},
		}
		_, _, err := partitionScopeChanges(incoming, existing)
		require.Error(t, err)
	})
}

// ─── diffScopeChanges ────────────────────────────────────────────────────────

func TestDiffScopeChanges(t *testing.T) {
	rack1 := uuid.MustParse("00000000-0000-0000-0002-000000000001")
	rack2 := uuid.MustParse("00000000-0000-0000-0002-000000000002")
	rack3 := uuid.MustParse("00000000-0000-0000-0002-000000000003")

	typesFilter := func(t *testing.T, types ...string) json.RawMessage {
		t.Helper()
		return marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: types,
		})
	}

	t.Run("desired equals current — no changes", func(t *testing.T) {
		scopes := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ID: uuid.New()},
			{RackID: rack2, ID: uuid.New()},
		}
		toAdd, toRemove, toUpdate, err := diffScopeChanges(scopes, scopes)
		require.NoError(t, err)
		assert.Empty(t, toAdd)
		assert.Empty(t, toRemove)
		assert.Empty(t, toUpdate)
	})

	t.Run("desired is empty — all current removed", func(t *testing.T) {
		current := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ID: uuid.New()},
			{RackID: rack2, ID: uuid.New()},
		}
		toAdd, toRemove, toUpdate, err := diffScopeChanges(nil, current)
		require.NoError(t, err)
		assert.Empty(t, toAdd)
		assert.Len(t, toRemove, 2)
		assert.Empty(t, toUpdate)
	})

	t.Run("current is empty — all desired added", func(t *testing.T) {
		desired := []*dbmodel.TaskScheduleScope{
			{RackID: rack1},
			{RackID: rack2},
		}
		toAdd, toRemove, toUpdate, err := diffScopeChanges(desired, nil)
		require.NoError(t, err)
		assert.Len(t, toAdd, 2)
		assert.Empty(t, toRemove)
		assert.Empty(t, toUpdate)
	})

	t.Run("rack in both with different filter — goes to toUpdate", func(t *testing.T) {
		curID := uuid.New()
		current := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ID: curID, ComponentFilter: typesFilter(t, "Compute")},
		}
		desired := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ComponentFilter: typesFilter(t, "NVSwitch")},
		}
		toAdd, toRemove, toUpdate, err := diffScopeChanges(desired, current)
		require.NoError(t, err)
		assert.Empty(t, toAdd)
		assert.Empty(t, toRemove)
		require.Len(t, toUpdate, 1)

		// toUpdate entry carries the existing row's ID so the store knows which row to update.
		assert.Equal(t, curID, toUpdate[0].ID)

		cf, err := dbmodel.UnmarshalComponentFilter(toUpdate[0].ComponentFilter)
		require.NoError(t, err)
		assert.ElementsMatch(t, []string{"NVSwitch"}, cf.Types)
	})

	t.Run("comprehensive: add, remove, update, unchanged", func(t *testing.T) {
		id1 := uuid.New()
		id2 := uuid.New()
		id3 := uuid.New()

		current := []*dbmodel.TaskScheduleScope{
			{RackID: rack1, ID: id1}, // unchanged
			{RackID: rack2, ID: id2, ComponentFilter: typesFilter(t, "Compute")}, // will be updated
			{RackID: rack3, ID: id3}, // will be removed
		}
		desired := []*dbmodel.TaskScheduleScope{
			{RackID: rack1}, // unchanged
			{RackID: rack2, ComponentFilter: typesFilter(t, "NVSwitch")},     // update filter
			{RackID: uuid.MustParse("00000000-0000-0000-0002-000000000004")}, // new rack
		}

		toAdd, toRemove, toUpdate, err := diffScopeChanges(desired, current)
		require.NoError(t, err)
		assert.Len(t, toAdd, 1)
		assert.Len(t, toRemove, 1)
		assert.Len(t, toUpdate, 1)

		assert.Equal(t, uuid.MustParse("00000000-0000-0000-0002-000000000004"), toAdd[0].RackID)
		assert.Equal(t, id3, toRemove[0].ID)
		assert.Equal(t, id2, toUpdate[0].ID)
	})
}

// ─── taskScheduleScopeToProto ────────────────────────────────────────────────

func TestTaskScheduleScopeToProto(t *testing.T) {
	schedID := uuid.New()
	rackID := uuid.New()
	scopeID := uuid.New()
	taskID := uuid.New()

	t.Run("nil component filter", func(t *testing.T) {
		s := &dbmodel.TaskScheduleScope{
			ID:         scopeID,
			ScheduleID: schedID,
			RackID:     rackID,
			CreatedAt:  time.Now(),
		}
		p, err := taskScheduleScopeToProto(s)
		require.NoError(t, err)
		assert.Nil(t, p.ComponentFilter)
		assert.Empty(t, p.LastTaskId.GetId())
	})

	t.Run("types filter populated", func(t *testing.T) {
		cf := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: []string{"Compute", "NVSwitch"},
		})
		s := &dbmodel.TaskScheduleScope{
			ID:              scopeID,
			ScheduleID:      schedID,
			RackID:          rackID,
			ComponentFilter: cf,
			CreatedAt:       time.Now(),
		}
		p, err := taskScheduleScopeToProto(s)
		require.NoError(t, err)
		typesFilter, ok := p.ComponentFilter.(*pb.TaskScheduleScope_Types)
		require.True(t, ok, "expected types filter oneof")
		assert.Len(t, typesFilter.Types.GetTypes(), 2)
	})

	t.Run("components filter populated", func(t *testing.T) {
		id1 := uuid.New()
		id2 := uuid.New()
		cf := marshalFilter(t, &dbmodel.ComponentFilter{
			Kind:       dbmodel.ComponentFilterKindComponents,
			Components: []uuid.UUID{id1, id2},
		})
		s := &dbmodel.TaskScheduleScope{
			ID:              scopeID,
			ScheduleID:      schedID,
			RackID:          rackID,
			ComponentFilter: cf,
			CreatedAt:       time.Now(),
		}
		p, err := taskScheduleScopeToProto(s)
		require.NoError(t, err)
		compsFilter, ok := p.ComponentFilter.(*pb.TaskScheduleScope_Components)
		require.True(t, ok, "expected components filter oneof")
		assert.Len(t, compsFilter.Components.GetTargets(), 2)
	})

	t.Run("LastTaskID set", func(t *testing.T) {
		s := &dbmodel.TaskScheduleScope{
			ID:         scopeID,
			ScheduleID: schedID,
			RackID:     rackID,
			LastTaskID: &taskID,
			CreatedAt:  time.Now(),
		}
		p, err := taskScheduleScopeToProto(s)
		require.NoError(t, err)
		assert.NotNil(t, p.LastTaskId)
	})

	t.Run("invalid component filter JSON — error", func(t *testing.T) {
		s := &dbmodel.TaskScheduleScope{
			ID:              scopeID,
			ScheduleID:      schedID,
			RackID:          rackID,
			ComponentFilter: json.RawMessage(`not-valid-json`),
			CreatedAt:       time.Now(),
		}
		_, err := taskScheduleScopeToProto(s)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "unmarshal component filter")
	})
}

// ─── taskScheduleToProto ─────────────────────────────────────────────────────

// makeTemplate builds a valid operation_template JSON blob for tests.
func makeTemplate(t *testing.T) json.RawMessage {
	t.Helper()
	raw, err := taskschedule.MarshalTemplate(
		taskcommon.TaskTypePowerControl,
		taskcommon.OpCodePowerControlPowerOn,
		json.RawMessage("{}"),
		taskschedule.TemplateOptions{},
	)
	require.NoError(t, err)
	return raw
}

func TestTaskScheduleToProto(t *testing.T) {
	now := time.Now().UTC().Truncate(time.Second)
	next := now.Add(time.Hour)
	last := now.Add(-time.Hour)

	t.Run("interval schedule with NextRunAt and no LastRunAt", func(t *testing.T) {
		row := &dbmodel.TaskSchedule{
			ID:                uuid.New(),
			Name:              "my-schedule",
			SpecType:          dbmodel.SpecTypeInterval,
			Spec:              "24h",
			Timezone:          "UTC",
			OverlapPolicy:     dbmodel.OverlapPolicySkip,
			Enabled:           true,
			NextRunAt:         &next,
			OperationTemplate: makeTemplate(t),
			CreatedAt:         now,
			UpdatedAt:         now,
		}
		p, err := taskScheduleToProto(row)
		require.NoError(t, err)
		assert.Equal(t, "my-schedule", p.Name)
		assert.True(t, p.Enabled)
		assert.Equal(t, pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL, p.Spec.Type)
		assert.Equal(t, "24h", p.Spec.Spec)
		assert.Equal(t, "UTC", p.Spec.Timezone)
		assert.Equal(t, pb.OverlapPolicy_OVERLAP_POLICY_SKIP, p.OverlapPolicy)
		assert.NotNil(t, p.NextRunAt)
		assert.Nil(t, p.LastRunAt)
		assert.Equal(t, "POWER_ON", p.OperationType)
		assert.Equal(t, "Power On", p.Description)
	})

	t.Run("one-time schedule that has fired: no NextRunAt, has LastRunAt", func(t *testing.T) {
		row := &dbmodel.TaskSchedule{
			ID:                uuid.New(),
			Name:              "one-shot",
			SpecType:          dbmodel.SpecTypeOneTime,
			Spec:              "2026-06-01T04:00:00Z",
			Timezone:          "UTC",
			OverlapPolicy:     dbmodel.OverlapPolicySkip,
			Enabled:           false,
			LastRunAt:         &last,
			OperationTemplate: makeTemplate(t),
			CreatedAt:         now,
			UpdatedAt:         now,
		}
		p, err := taskScheduleToProto(row)
		require.NoError(t, err)
		assert.False(t, p.Enabled)
		assert.Equal(t, pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_ONE_TIME, p.Spec.Type)
		assert.Nil(t, p.NextRunAt)
		assert.NotNil(t, p.LastRunAt)
	})

	t.Run("empty OperationTemplate — no op summary, no error", func(t *testing.T) {
		row := &dbmodel.TaskSchedule{
			ID:            uuid.New(),
			Name:          "no-op",
			SpecType:      dbmodel.SpecTypeCron,
			Spec:          "0 2 * * *",
			Timezone:      "UTC",
			OverlapPolicy: dbmodel.OverlapPolicySkip,
			Enabled:       true,
			CreatedAt:     now,
			UpdatedAt:     now,
		}
		p, err := taskScheduleToProto(row)
		require.NoError(t, err)
		assert.Empty(t, p.OperationType)
		assert.Empty(t, p.Description)
	})

	t.Run("queue overlap policy", func(t *testing.T) {
		row := &dbmodel.TaskSchedule{
			ID:                uuid.New(),
			Name:              "queued",
			SpecType:          dbmodel.SpecTypeCron,
			Spec:              "0 */6 * * *",
			Timezone:          "America/New_York",
			OverlapPolicy:     dbmodel.OverlapPolicyQueue,
			Enabled:           true,
			NextRunAt:         &next,
			OperationTemplate: makeTemplate(t),
			CreatedAt:         now,
			UpdatedAt:         now,
		}
		p, err := taskScheduleToProto(row)
		require.NoError(t, err)
		assert.Equal(t, pb.OverlapPolicy_OVERLAP_POLICY_QUEUE, p.OverlapPolicy)
		assert.Equal(t, "America/New_York", p.Spec.Timezone)
	})
}

// ─── buildUpdateFields ───────────────────────────────────────────────────────

// stubScheduleStore satisfies taskschedule.Store with only Get implemented.
// Embedding the interface means all other methods panic if called — which is
// intentional: a test that triggers an unexpected store call will fail loudly.
type stubScheduleStore struct {
	taskschedule.Store
	getFunc func(ctx context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error)
}

func (s *stubScheduleStore) Get(ctx context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error) {
	return s.getFunc(ctx, id)
}

// storeError returns a stub whose Get always returns err.
func storeError(err error) *stubScheduleStore {
	return &stubScheduleStore{
		getFunc: func(_ context.Context, _ uuid.UUID) (*dbmodel.TaskSchedule, error) {
			return nil, err
		},
	}
}

func TestBuildUpdateFields(t *testing.T) {
	ctx := context.Background()
	anyID := uuid.New()

	// rs with nil store — safe for paths that never call Get.
	rsNoDB := &FlowServerImpl{}

	t.Run("empty paths — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID, &pb.ScheduleConfig{}, nil, nil)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "update_mask")
	})

	t.Run("nil schedule — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID, nil, []string{"schedule.name"}, nil)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "schedule")
	})

	t.Run("unsupported path — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Name: "x"},
			[]string{"description"},
			nil,
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "unsupported")
	})

	// ── "schedule.name" path ─────────────────────────────────────────────────

	t.Run("name: empty value — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{},
			[]string{"schedule.name"},
			nil,
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), `"schedule.name"`)
	})

	t.Run("name: valid — sets Name, no NextRunAt", func(t *testing.T) {
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Name: "my-schedule"},
			[]string{"schedule.name"},
			nil,
		)
		require.NoError(t, err)
		assert.Equal(t, "my-schedule", fields.Name)
		assert.Nil(t, fields.NextRunAt)
	})

	// ── "schedule.overlap_policy" path ───────────────────────────────────────

	t.Run("overlap_policy: queue", func(t *testing.T) {
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{OverlapPolicy: pb.OverlapPolicy_OVERLAP_POLICY_QUEUE},
			[]string{"schedule.overlap_policy"},
			nil,
		)
		require.NoError(t, err)
		assert.Equal(t, dbmodel.OverlapPolicyQueue, fields.OverlapPolicy)
		assert.Nil(t, fields.NextRunAt)
	})

	t.Run("overlap_policy: unspecified defaults to skip", func(t *testing.T) {
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{},
			[]string{"schedule.overlap_policy"},
			nil,
		)
		require.NoError(t, err)
		assert.Equal(t, dbmodel.OverlapPolicySkip, fields.OverlapPolicy)
	})

	// ── "schedule.spec.timezone" path (alone) ────────────────────────────────

	t.Run("spec.timezone: nil spec — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{},
			[]string{"schedule.spec.timezone"},
			nil,
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), `"schedule.spec.timezone"`)
	})

	t.Run("spec.timezone alone on cron — recomputes NextRunAt", func(t *testing.T) {
		existing := &dbmodel.TaskSchedule{
			SpecType: dbmodel.SpecTypeCron,
			Spec:     "0 2 * * *",
			Timezone: "UTC",
		}
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Timezone: "America/New_York"}},
			[]string{"schedule.spec.timezone"},
			existing,
		)
		require.NoError(t, err)
		assert.Equal(t, "America/New_York", fields.Timezone)
		assert.NotNil(t, fields.NextRunAt)
	})

	t.Run("spec.timezone alone on interval — timezone cleared (no-op), no NextRunAt", func(t *testing.T) {
		existing := &dbmodel.TaskSchedule{
			SpecType: dbmodel.SpecTypeInterval,
			Spec:     "24h",
			Timezone: "UTC",
		}
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Timezone: "America/New_York"}},
			[]string{"schedule.spec.timezone"},
			existing,
		)
		require.NoError(t, err)
		assert.Empty(t, fields.Timezone) // interval ignores timezone; write is a no-op
		assert.Nil(t, fields.NextRunAt)
	})

	t.Run("spec.timezone alone on one-time — timezone cleared (no-op), no NextRunAt", func(t *testing.T) {
		existing := &dbmodel.TaskSchedule{
			SpecType: dbmodel.SpecTypeOneTime,
			Spec:     "2026-06-01T04:00:00Z",
			Timezone: "UTC",
		}
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Timezone: "America/New_York"}},
			[]string{"schedule.spec.timezone"},
			existing,
		)
		require.NoError(t, err)
		assert.Empty(t, fields.Timezone) // one-time ignores timezone; write is a no-op
		assert.Nil(t, fields.NextRunAt)
	})

	t.Run("spec.timezone alone on cron: empty tz defaults to UTC, recomputes NextRunAt", func(t *testing.T) {
		existing := &dbmodel.TaskSchedule{
			SpecType: dbmodel.SpecTypeCron,
			Spec:     "0 2 * * *",
			Timezone: "America/New_York",
		}
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{}}, // timezone omitted → defaults to "UTC"
			[]string{"schedule.spec.timezone"},
			existing,
		)
		require.NoError(t, err)
		assert.Equal(t, "UTC", fields.Timezone)
		assert.NotNil(t, fields.NextRunAt) // cron must recompute when timezone changes
	})

	// ── "schedule.spec" path (alone) ─────────────────────────────────────────

	t.Run("spec: nil spec — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{},
			[]string{"schedule.spec"},
			nil,
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), `"schedule.spec"`)
	})

	t.Run("spec: unspecified type — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Spec: "24h"}},
			[]string{"schedule.spec"},
			nil,
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "spec.type")
	})

	t.Run("spec: empty spec string — error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{
				Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL,
			}},
			[]string{"schedule.spec"},
			nil,
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "spec.spec")
	})

	t.Run("spec alone: uses existing timezone from locked row", func(t *testing.T) {
		existing := &dbmodel.TaskSchedule{
			SpecType: dbmodel.SpecTypeInterval,
			Spec:     "6h",
			Timezone: "America/Chicago",
		}
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{
				Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL,
				Spec: "12h",
			}},
			[]string{"schedule.spec"},
			existing,
		)
		require.NoError(t, err)
		assert.Equal(t, dbmodel.SpecTypeInterval, fields.SpecType)
		assert.Equal(t, "12h", fields.Spec)
		assert.NotNil(t, fields.NextRunAt)
	})

	t.Run("spec alone: store Get fallback error propagates when existing is nil", func(t *testing.T) {
		rs := &FlowServerImpl{taskScheduleStore: storeError(errors.New("db down"))}
		_, _, err := rs.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{
				Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL,
				Spec: "12h",
			}},
			[]string{"schedule.spec"},
			nil, // nil → falls back to Get(), which returns the error
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "db down")
	})

	// ── "schedule.spec" + "schedule.spec.timezone" together (no DB call) ──────

	t.Run("spec + spec.timezone: interval — timezone cleared, NextRunAt set, no DB call", func(t *testing.T) {
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{
				Type:     pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL,
				Spec:     "6h",
				Timezone: "UTC",
			}},
			[]string{"schedule.spec", "schedule.spec.timezone"},
			nil,
		)
		require.NoError(t, err)
		assert.Equal(t, dbmodel.SpecTypeInterval, fields.SpecType)
		assert.Equal(t, "6h", fields.Spec)
		assert.Empty(t, fields.Timezone) // interval ignores timezone; not stored
		assert.NotNil(t, fields.NextRunAt)
	})

	t.Run("spec + spec.timezone: cron — NextRunAt set, no DB call", func(t *testing.T) {
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{
				Type:     pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON,
				Spec:     "0 2 * * *",
				Timezone: "America/New_York",
			}},
			[]string{"schedule.spec", "schedule.spec.timezone"},
			nil,
		)
		require.NoError(t, err)
		assert.Equal(t, dbmodel.SpecTypeCron, fields.SpecType)
		assert.Equal(t, "America/New_York", fields.Timezone)
		assert.NotNil(t, fields.NextRunAt)
	})

	t.Run("spec + spec.timezone: invalid interval string — ComputeFirstRunAt error", func(t *testing.T) {
		_, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{Spec: &pb.ScheduleSpec{
				Type:     pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL,
				Spec:     "not-a-duration",
				Timezone: "UTC",
			}},
			[]string{"schedule.spec", "schedule.spec.timezone"},
			nil,
		)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "compute next_run_at")
	})

	// ── combined non-time paths ───────────────────────────────────────────────

	t.Run("name + overlap_policy — both set, no NextRunAt", func(t *testing.T) {
		fields, _, err := rsNoDB.buildUpdateFields(ctx, anyID,
			&pb.ScheduleConfig{
				Name:          "renamed",
				OverlapPolicy: pb.OverlapPolicy_OVERLAP_POLICY_QUEUE,
			},
			[]string{"schedule.name", "schedule.overlap_policy"},
			nil,
		)
		require.NoError(t, err)
		assert.Equal(t, "renamed", fields.Name)
		assert.Equal(t, dbmodel.OverlapPolicyQueue, fields.OverlapPolicy)
		assert.Nil(t, fields.NextRunAt)
	})
}

func TestResolveComponentTarget_ExternalID(t *testing.T) {
	rackID := uuid.New()
	compID := uuid.New()
	comp2ID := uuid.New()

	makeComp := func(id uuid.UUID, typ devicetypes.ComponentType) *component.Component {
		return &component.Component{
			Info:   deviceinfo.DeviceInfo{ID: id},
			Type:   typ,
			RackID: rackID,
		}
	}

	tests := []struct {
		name       string
		setup      func(*mockManager)
		ct         operation.ComponentTarget
		wantCompID uuid.UUID
		wantRackID uuid.UUID
		wantErr    string
	}{
		{
			name: "no match by type — not found",
			setup: func(m *mockManager) {
				c := makeComp(compID, devicetypes.ComponentTypeNVSwitch)
				c.ComponentID = "ext-1"
				m.components[compID] = c
			},
			ct: operation.ComponentTarget{
				External: &operation.ExternalRef{ID: "ext-1", Type: devicetypes.ComponentTypeCompute},
			},
			wantErr: "component identifier \"ext-1\" not found",
		},
		{
			name: "ambiguous — two component types share the external id",
			setup: func(m *mockManager) {
				c1 := makeComp(compID, devicetypes.ComponentTypeCompute)
				c1.ComponentID = "ext-2"
				m.components[compID] = c1
				c2 := makeComp(comp2ID, devicetypes.ComponentTypeNVSwitch)
				c2.ComponentID = "ext-2"
				m.components[comp2ID] = c2
			},
			ct: operation.ComponentTarget{
				External: &operation.ExternalRef{ID: "ext-2", Type: devicetypes.ComponentTypeUnknown},
			},
			wantErr: "component identifier \"ext-2\" is ambiguous",
		},
		{
			name: "exactly one match — success",
			setup: func(m *mockManager) {
				c1 := makeComp(compID, devicetypes.ComponentTypeCompute)
				c1.ComponentID = "ext-3"
				m.components[compID] = c1
				c2 := makeComp(comp2ID, devicetypes.ComponentTypeNVSwitch)
				c2.ComponentID = "ext-3"
				m.components[comp2ID] = c2
			},
			ct: operation.ComponentTarget{
				External: &operation.ExternalRef{ID: "ext-3", Type: devicetypes.ComponentTypeCompute},
			},
			wantCompID: compID,
			wantRackID: rackID,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mgr := newMockManager()
			tt.setup(mgr)
			rs := &FlowServerImpl{inventoryManager: mgr}

			gotComp, gotRack, err := rs.resolveComponentTarget(context.Background(), tt.ct)
			if tt.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tt.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.wantCompID, gotComp)
			assert.Equal(t, tt.wantRackID, gotRack)
		})
	}
}

func TestResolveNVLDomainScopeMaterializesRackMembership(t *testing.T) {
	mgr := newMockManager()
	domainID := uuid.New()
	rackIDs := []uuid.UUID{uuid.New(), uuid.New()}
	for _, rackID := range rackIDs {
		mgr.racks[rackID] = &rack.Rack{
			Info: deviceinfo.DeviceInfo{ID: rackID},
		}
		mgr.domainRacks[domainID] = append(mgr.domainRacks[domainID], mgr.racks[rackID])
	}

	scopes, err := (&FlowServerImpl{inventoryManager: mgr}).resolveNVLDomainScope(
		context.Background(),
		[]operation.NVLDomainTarget{
			{
				Identifier: identifier.Identifier{ID: domainID},
				ComponentTypes: []devicetypes.ComponentType{
					devicetypes.ComponentTypeCompute,
				},
			},
		},
	)
	require.NoError(t, err)
	require.Len(t, scopes, 2)
	for index, scope := range scopes {
		assert.Equal(t, rackIDs[index], scope.RackID)
		filter, err := dbmodel.UnmarshalComponentFilter(scope.ComponentFilter)
		require.NoError(t, err)
		require.NotNil(t, filter)
		assert.Equal(t, dbmodel.ComponentFilterKindTypes, filter.Kind)
		assert.Equal(t, []string{"Compute"}, filter.Types)
	}
}
