// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// TestFlowServerImpl_AddPowerTaskExternalTargets protects complete recorded
// selections: inventory failures must not rewrite tasks or imply fewer targets.
func TestFlowServerImpl_AddPowerTaskExternalTargets(t *testing.T) {
	computeID, secondComputeID, switchID := uuid.UUID{15: 1}, uuid.UUID{15: 2}, uuid.UUID{15: 3}
	computeRef := &pb.ExternalRef{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, Id: "z-compute"}
	secondComputeRef := &pb.ExternalRef{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, Id: "a-compute"}
	switchRef := &pb.ExternalRef{Type: pb.ComponentType_COMPONENT_TYPE_NVSWITCH, Id: "a-switch"}
	complete := &pb.ComponentsByType{Groups: []*pb.ComponentsForType{
		{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: computeID.String()}, {Id: computeID.String()}, {Id: secondComputeID.String()}}},
		{Type: pb.ComponentType_COMPONENT_TYPE_NVSWITCH, ComponentIds: []*pb.UUID{{Id: switchID.String()}}},
	}}
	computeOnly := &pb.ComponentsByType{Groups: complete.Groups[:1]}
	singleCompute := &pb.ComponentsByType{Groups: []*pb.ComponentsForType{{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
		ComponentIds: []*pb.UUID{{Id: computeID.String()}}}}}
	computeIdentity := component.Identity{ID: computeID, Type: devicetypes.ComponentTypeCompute, ExternalID: computeRef.Id}
	secondComputeIdentity := component.Identity{ID: secondComputeID, Type: devicetypes.ComponentTypeCompute, ExternalID: secondComputeRef.Id}
	switchIdentity := component.Identity{ID: switchID, Type: devicetypes.ComponentTypeNVSwitch, ExternalID: switchRef.Id}
	selected := &pb.PowerTaskExternalTargets{RackId: "rack-inventory-id", Targets: []*pb.ExternalRef{computeRef, computeRef, secondComputeRef, switchRef}}
	computeSelected := &pb.PowerTaskExternalTargets{RackId: "rack-inventory-id", Targets: []*pb.ExternalRef{computeRef, computeRef, secondComputeRef}}
	notLinked := &pb.PowerTaskExternalTargets{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum()}
	invalid := &pb.PowerTaskExternalTargets{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum()}
	// Sixty thousand canonical UUIDs exceed the response allowance before
	// including any JSON field names or external identities.
	oversizedIDs := make([]*pb.UUID, 60000)
	for i := range oversizedIDs {
		oversizedIDs[i] = &pb.UUID{Id: computeID.String()}
	}

	for _, tc := range []struct {
		name        string
		selections  []*pb.ComponentsByType
		identities  []component.Identity
		missingRack bool
		readError   error
		want        []*pb.PowerTaskExternalTargets
		wantIDs     []uuid.UUID
	}{
		// The helper preserves even repeated input slots. Within one type, the
		// external ID sort order is deliberately opposite the existing targets.
		{name: "one batch preserves typed selection order", selections: []*pb.ComponentsByType{complete, computeOnly},
			identities: []component.Identity{switchIdentity, secondComputeIdentity, computeIdentity}, want: []*pb.PowerTaskExternalTargets{selected, computeSelected}, wantIDs: []uuid.UUID{computeID, secondComputeID, switchID}},
		// A purged target invalidates the whole external selection for that task;
		// another task whose selected compute remains linked is still useful.
		{name: "missing target does not hide a healthy task", selections: []*pb.ComponentsByType{complete, computeOnly},
			identities: []component.Identity{computeIdentity, secondComputeIdentity}, want: []*pb.PowerTaskExternalTargets{notLinked, computeSelected}, wantIDs: []uuid.UUID{computeID, secondComputeID, switchID}},
		// A retained row without its managed identifier is unavailable, just as a
		// missing row is; its UUID must never be substituted as a public ID.
		{name: "empty external identifier", selections: []*pb.ComponentsByType{singleCompute},
			identities: []component.Identity{{ID: computeID, Type: devicetypes.ComponentTypeCompute}}, want: []*pb.PowerTaskExternalTargets{notLinked}, wantIDs: []uuid.UUID{computeID}},
		// Matching text cannot turn a saved compute selection into a switch.
		{name: "inventory type differs from recorded type", selections: []*pb.ComponentsByType{singleCompute},
			identities: []component.Identity{{ID: computeID, Type: devicetypes.ComponentTypeNVSwitch, ExternalID: computeRef.Id}}, want: []*pb.PowerTaskExternalTargets{invalid}, wantIDs: []uuid.UUID{computeID}},
		// A mixed task can retain an Unknown component beside a supported tray.
		// Matching Unknown types still cannot describe a valid external target.
		{name: "unknown selected and inventory types", selections: []*pb.ComponentsByType{{Groups: []*pb.ComponentsForType{
			{Type: pb.ComponentType_COMPONENT_TYPE_UNKNOWN, ComponentIds: []*pb.UUID{{Id: computeID.String()}}},
		}}}, identities: []component.Identity{{ID: computeID, Type: devicetypes.ComponentTypeUnknown, ExternalID: computeRef.Id}},
			want: []*pb.PowerTaskExternalTargets{invalid}, wantIDs: []uuid.UUID{computeID}},
		// Component IDs alone are insufficient to identify the recorded rack.
		{name: "external rack identifier unavailable", selections: []*pb.ComponentsByType{computeOnly}, missingRack: true,
			want: []*pb.PowerTaskExternalTargets{notLinked}},
		// Raw database text is private; only its bounded failure category belongs
		// in the new result, leaving the original task metadata untouched.
		{name: "query failure retains original tasks", selections: []*pb.ComponentsByType{singleCompute},
			readError: fmt.Errorf("read failed: %w", &pgconn.PgError{Code: "42P01", Message: "sensitive database detail"}),
			want:      []*pb.PowerTaskExternalTargets{{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED.Enum()}}, wantIDs: []uuid.UUID{computeID}},
		// No tasks means no inventory read or synthesized task entries.
		{name: "empty task list"},
		// An explicitly empty saved selection is a successful empty result.
		{name: "empty recorded selection", selections: []*pb.ComponentsByType{nil},
			want: []*pb.PowerTaskExternalTargets{{RackId: "rack-inventory-id"}}},
		// Do not turn an invalid saved identifier into a successful empty lookup.
		{name: "invalid recorded UUID", selections: []*pb.ComponentsByType{{Groups: []*pb.ComponentsForType{
			{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*pb.UUID{{Id: "invalid"}}},
		}}}, want: []*pb.PowerTaskExternalTargets{invalid}},
		// The old UUID payload cannot fit by itself; avoid building a larger
		// lookup before the response-size fallback handles those original tasks.
		{name: "recorded targets already exceed response allowance", selections: []*pb.ComponentsByType{{Groups: []*pb.ComponentsForType{
			{Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: oversizedIDs},
		}}}, want: []*pb.PowerTaskExternalTargets{{Error: pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum()}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var diagnostics bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(&diagnostics)
			t.Cleanup(func() { log.Logger = previousLogger })
			inventory := &powerDetailsInventory{identityRead: func(context.Context) ([]component.Identity, error) {
				return tc.identities, tc.readError
			}}
			server := &FlowServerImpl{inventoryManager: inventory}
			entries := make([]*pb.PowerTaskDetails, len(tc.selections))
			want := make([]*pb.PowerTaskDetails, len(tc.selections))
			for i, selection := range tc.selections {
				entries[i] = &pb.PowerTaskDetails{Id: &pb.UUID{Id: (uuid.UUID{14: 1, 15: byte(i)}).String()}, Targets: selection,
					RackId: &pb.UUID{Id: (uuid.UUID{15: 3}).String()}, Status: pb.TaskStatus_TASK_STATUS_PENDING}
				want[i] = proto.Clone(entries[i]).(*pb.PowerTaskDetails)
				want[i].ExternalTargets = tc.want[i]
			}
			rackID := "rack-inventory-id"
			if tc.missingRack {
				rackID = ""
			}

			server.addPowerTaskExternalTargets(t.Context(), rackID, entries)

			require.NotContains(t, diagnostics.String(), "sensitive")
			if tc.readError != nil {
				var fields map[string]any
				require.NoError(t, json.Unmarshal(diagnostics.Bytes(), &fields))
				require.Equal(t, entries[0].GetRackId().GetId(), fields["rack_id"])
				require.Equal(t, "component_identity_read", fields["failure_stage"])
				require.Equal(t, "POWER_DETAILS_READ_ERROR_READ_FAILED", fields["error_category"])
				require.NotEmpty(t, fields["error_type"])
				require.Equal(t, "42P01", fields["sqlstate"])
			} else {
				require.Empty(t, diagnostics.String())
			}
			require.ElementsMatch(t, tc.wantIDs, inventory.identityIDs)
			if len(tc.wantIDs) == 0 {
				require.Zero(t, inventory.identityCalls)
			} else {
				require.Equal(t, 1, inventory.identityCalls)
			}
			for i, entry := range entries {
				require.True(t, proto.Equal(want[i], entry), "task %d must retain every original field", i)
				require.NotContains(t, entry.String(), "sensitive")
			}
		})
	}
}
