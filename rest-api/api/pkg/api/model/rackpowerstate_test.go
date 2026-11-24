// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/santhosh-tekuri/jsonschema/v6"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"
	"gopkg.in/yaml.v3"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// TestAPIRackPowerState_FromProto protects the rack-specific mapping and its public schema/SDK boundary.
// Shared tray tests own the leaf conversion matrix; Flow tests own summary aggregation and task deduplication.
func TestAPIRackPowerState_FromProto(t *testing.T) {
	const rackID = "rack-01"
	const privateRackID = "c3d6308c-778b-461d-9edb-d4a3c4990378"
	const privateShelfID = "438838f6-cbca-4c5d-8d70-098f741e1f47"
	const firstTaskID = "f9684314-d3bd-4352-98a4-0321a517a32e"
	const secondTaskID = "058746fb-4124-465a-b522-ff2a9eb99d39"
	const unreferencedTaskID = "658746fb-4124-465a-b522-ff2a9eb99d39"
	const privateRemovedID = "00000000-0000-4000-8000-000000000005"
	privateMembers := []string{
		"00000000-0000-4000-8000-000000000001", "00000000-0000-4000-8000-000000000002",
		"00000000-0000-4000-8000-000000000003", "00000000-0000-4000-8000-000000000004",
	}
	// Keep independent evidence and recorded selections visible so the conversion
	// cannot reconstruct the root task list from only current member references.
	newDetails := func() *flowv1.RackPowerDetails {
		base := testTrayPowerState()
		task := base.Operations.Tasks.Entries[0]
		task.Id = &flowv1.UUID{Id: firstTaskID}
		task.RackId = &flowv1.UUID{Id: privateRackID}
		task.Relevance = flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_UNSPECIFIED
		task.Targets = &flowv1.ComponentsByType{Groups: []*flowv1.ComponentsForType{{
			Type:         flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
			ComponentIds: []*flowv1.UUID{{Id: privateMembers[0]}, {Id: privateMembers[1]}},
		}}}
		task.ExternalTargets = &flowv1.PowerTaskExternalTargets{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum()}
		task.WaitingContext = &flowv1.PowerTaskWaitingContext{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED.Enum()}
		// The later task selected a shelf; current member relevance is supplied
		// separately. Its lower UUID exposes an accidental UUID-only sort.
		shelfTask := base.Operations.Tasks.Entries[1]
		shelfTask.Id = &flowv1.UUID{Id: secondTaskID}
		shelfTask.RackId = &flowv1.UUID{Id: privateRackID}
		shelfTask.Relevance = flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_UNSPECIFIED
		shelfTask.Targets = &flowv1.ComponentsByType{Groups: []*flowv1.ComponentsForType{{
			Type: flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF, ComponentIds: []*flowv1.UUID{{Id: privateShelfID}},
		}}}
		shelfTask.ExternalTargets = &flowv1.PowerTaskExternalTargets{RackId: rackID,
			Targets: []*flowv1.ExternalRef{{Type: flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF, Id: "recorded-shelf"}}}
		// A recorded selection can outlive every selected current member.
		// Keep this task at the root without adding a member reference.
		unreferencedTask := testTrayPowerState().Operations.Tasks.Entries[1]
		unreferencedTask.Id = &flowv1.UUID{Id: unreferencedTaskID}
		unreferencedTask.RackId = &flowv1.UUID{Id: privateRackID}
		unreferencedTask.Relevance = flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_UNSPECIFIED
		unreferencedTask.CreatedAt = timestamppb.New(time.Date(2026, 10, 7, 17, 57, 15, 0, time.UTC))
		unreferencedTask.Targets = &flowv1.ComponentsByType{Groups: []*flowv1.ComponentsForType{{
			Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE, ComponentIds: []*flowv1.UUID{{Id: privateRemovedID}},
		}}}
		unreferencedTask.ExternalTargets = &flowv1.PowerTaskExternalTargets{RackId: rackID,
			Targets: []*flowv1.ExternalRef{{Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE, Id: "removed-member"}}}
		details := &flowv1.RackPowerDetails{
			RackId: &flowv1.UUID{Id: privateRackID}, RackExternalId: rackID,
			Actual: &flowv1.RackPowerSummary{State: flowv1.RackPowerState_RACK_POWER_STATE_UNKNOWN,
				Coverage: &flowv1.RackPowerCoverage{Total: 4, Known: 1, Unavailable: 2, Unsupported: 1}},
			Desired: &flowv1.RackPowerSummary{State: flowv1.RackPowerState_RACK_POWER_STATE_UNKNOWN,
				Coverage: &flowv1.RackPowerCoverage{Total: 4, DisabledPolicy: 1, Unavailable: 2, Unsupported: 1}},
			Operations: &flowv1.RackPowerOperations{Tasks: &flowv1.PowerTaskDetailsList{Entries: []*flowv1.PowerTaskDetails{task, shelfTask, unreferencedTask}}},
		}
		for i, privateID := range privateMembers {
			member := &flowv1.RackTrayPowerDetails{
				InternalComponentId: &flowv1.UUID{Id: privateID}, ComponentType: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
				Operations: &flowv1.RackTrayPowerOperations{Tasks: &flowv1.PowerTaskReferenceList{}},
			}
			code := flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED
			if i == 0 {
				member.ComponentId = cutil.GetPtr("managed-compute")
				member.Actual, member.Desired, member.Operations.Actions = base.Actual, base.Desired, base.Operations.Actions
			} else {
				if i == 3 {
					member.ComponentType = flowv1.ComponentType_COMPONENT_TYPE_TORSWITCH
					code = flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED
					member.Operations.Tasks.Error = &code
				}
				member.Actual = &flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Error{Error: code}}
				member.Desired = &flowv1.PowerDesiredDetails{Result: &flowv1.PowerDesiredDetails_Error{Error: code}}
				member.Operations.Actions = &flowv1.PowerActionDetails{Result: &flowv1.PowerActionDetails_Error{Error: code}}
			}
			if i < 3 {
				if i < 2 {
					member.Operations.Tasks.Entries = append(member.Operations.Tasks.Entries, &flowv1.PowerTaskReference{
						Id: task.Id, Relevance: flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED})
				}
				member.Operations.Tasks.Entries = append(member.Operations.Tasks.Entries, &flowv1.PowerTaskReference{
					Id: shelfTask.Id, Relevance: flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED})
			}
			details.Trays = append(details.Trays, member)
			details.Operations.Actions = append(details.Operations.Actions, &flowv1.RackMemberPowerActions{
				InternalComponentId: member.InternalComponentId, ComponentId: member.ComponentId,
				ComponentType: member.ComponentType, Details: member.Operations.Actions,
			})
		}
		return details
	}
	cases := []struct {
		name  string
		input func() *flowv1.RackPowerDetails
		check func(*testing.T, APIRackPowerState)
	}{
		{
			// Unlinked components keep their positions and null IDs; their task references can differ.
			name:  "partial rack preserves component identity and task references",
			input: newDetails,
			check: func(t *testing.T, got APIRackPowerState) {
				assert.Equal(t, cutil.GetPtr(rackID), got.ID)
				assert.Equal(t, APIRackPowerCoverage{Total: 4, Known: 1, Unavailable: 2, Unsupported: 1}, got.Actual.Coverage)
				assert.Equal(t, APIRackPowerCoverage{Total: 4, DisabledPolicy: 1, Unavailable: 2, Unsupported: 1}, got.Desired.Coverage)
				assert.False(t, got.Actual.Complete)
				require.Len(t, got.Components, 4)
				wantMembers := []struct {
					ID           *string
					Type         string
					Actions      []APIPowerAction
					ActionsError *APIPowerReadError
				}{
					{ID: cutil.GetPtr("managed-compute"), Type: "Compute", Actions: []APIPowerAction{
						{Status: "Active", Operation: "PowerOff", Requested: cutil.GetPtr(time.Date(2026, 10, 7, 17, 58, 0, 0, time.UTC)),
							Initiator: cutil.GetPtr(""), Graceful: cutil.GetPtr(false)},
						{Status: "Pending", Operation: "ChassisReset", Requested: cutil.GetPtr(time.Date(2026, 10, 7, 17, 59, 30, 0, time.UTC)),
							ChassisID: cutil.GetPtr("Chassis.1")},
					}},
					{Type: "Compute", Actions: []APIPowerAction{}, ActionsError: cutil.GetPtr[APIPowerReadError]("NotLinked")},
					{Type: "Compute", Actions: []APIPowerAction{}, ActionsError: cutil.GetPtr[APIPowerReadError]("NotLinked")},
					{Type: "TORSwitch", Actions: []APIPowerAction{}, ActionsError: cutil.GetPtr[APIPowerReadError]("Unsupported")},
				}
				for i, component := range got.Components {
					assert.Equal(t, wantMembers[i].ID, component.ID)
					assert.Equal(t, wantMembers[i].Type, component.Type)
					assert.Equal(t, wantMembers[i].Actions, component.Operations.Actions)
					assert.Equal(t, wantMembers[i].ActionsError, component.Operations.ActionsError)
				}
				assert.Equal(t, APIPowerObservation{
					State: cutil.GetPtr[APIPowerState]("On"), Source: cutil.GetPtr[APIPowerSource]("Redfish"),
					Observed: cutil.GetPtr(time.Date(2026, 10, 7, 18, 0, 2, 0, time.UTC)),
				}, got.Components[0].Actual)
				assert.Equal(t, APIPowerPolicy{Status: "Available", State: cutil.GetPtr[APIPowerPolicyState]("PowerManagerDisabled")}, got.Components[0].Desired)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Components[3].Operations.TasksError)
				assert.Nil(t, got.Operations.TasksError)
				require.Len(t, got.Operations.Tasks, 3)
				assert.Equal(t, []string{firstTaskID, secondTaskID, unreferencedTaskID},
					[]string{got.Operations.Tasks[0].ID, got.Operations.Tasks[1].ID, got.Operations.Tasks[2].ID})
				assert.Equal(t, cutil.GetPtr(rackID), got.Operations.Tasks[1].RackID)
				assert.Equal(t, []APIPowerTaskTargetGroup{{Type: "PowerShelf", ComponentIDs: []string{"recorded-shelf"}}}, got.Operations.Tasks[1].Targets)
				assert.Nil(t, got.Operations.Tasks[1].TargetsError)
				assert.Equal(t, []APIPowerTaskTargetGroup{{Type: "Compute", ComponentIDs: []string{"removed-member"}}}, got.Operations.Tasks[2].Targets)
				task := got.Operations.Tasks[0]
				assert.Equal(t, APIPowerTaskRelevance("Unknown"), task.Relevance)
				assert.Equal(t, "Pending", task.Status)
				assert.NotNil(t, task.Created)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("NotLinked"), task.TargetsError)
				assert.Nil(t, task.Targets)
				require.NotNil(t, task.WaitingContext)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("LimitExceeded"), task.WaitingContext.Error)
				assert.Equal(t, []APIPowerTaskReference{
					{ID: firstTaskID, Relevance: "Selected"}, {ID: secondTaskID, Relevance: "RackAffected"},
				}, got.Components[0].Operations.Tasks)
				assert.Equal(t, got.Components[0].Operations.Tasks, got.Components[1].Operations.Tasks)
				assert.Equal(t, []APIPowerTaskReference{{ID: secondTaskID, Relevance: "RackAffected"}}, got.Components[2].Operations.Tasks)
				for _, component := range got.Components[:3] {
					assert.Nil(t, component.Operations.TasksError)
				}
			},
		},
		{
			// A newer inventory type keeps its final position while using the public `Unknown` spelling.
			name: "future member type retains position",
			input: func() *flowv1.RackPowerDetails {
				base := newDetails()
				base.Trays[3].ComponentType = flowv1.ComponentType(99)
				return base
			},
			check: func(t *testing.T, got APIRackPowerState) {
				require.Len(t, got.Components, 4)
				assert.Equal(t, "Unknown", got.Components[3].Type)
				assert.Empty(t, got.Components[3].Operations.Actions)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Components[3].Operations.ActionsError)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Components[3].Operations.TasksError)
			},
		},
		{
			// Missing messages from an older producer must not look like successful empty sections.
			name: "older service omits sections and waiting context",
			input: func() *flowv1.RackPowerDetails {
				base := newDetails()
				base.RackExternalId, base.Actual, base.Desired = "", nil, nil
				base.Operations.Tasks.Entries[0].WaitingContext = nil
				base.Operations.Actions = nil
				base.Trays[0].Actual, base.Trays[0].Desired, base.Trays[0].Operations = nil, nil, nil
				return base
			},
			check: func(t *testing.T, got APIRackPowerState) {
				assert.Nil(t, got.ID)
				assert.Equal(t, APIRackPowerSummaryState("Unknown"), got.Actual.State)
				assert.False(t, got.Actual.Complete)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Components[0].Actual.Error)
				assert.Equal(t, []APIPowerAction{}, got.Components[0].Operations.Actions)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Components[0].Operations.ActionsError)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Components[0].Operations.TasksError)
				require.Len(t, got.Operations.Tasks, 3)
				require.NotNil(t, got.Operations.Tasks[0].WaitingContext)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Operations.Tasks[0].WaitingContext.Error)
			},
		},
		{
			// Present empty lists represent a readable rack with no members or work.
			name: "successful empty rack",
			input: func() *flowv1.RackPowerDetails {
				return &flowv1.RackPowerDetails{RackExternalId: rackID,
					Actual: &flowv1.RackPowerSummary{Complete: true}, Desired: &flowv1.RackPowerSummary{Complete: true},
					Operations: &flowv1.RackPowerOperations{Tasks: &flowv1.PowerTaskDetailsList{}}}
			},
			check: func(t *testing.T, got APIRackPowerState) {
				assert.Empty(t, got.Components)
				assert.Empty(t, got.Operations.Tasks)
				assert.Nil(t, got.Operations.TasksError)
				assert.True(t, got.Actual.Complete)
			},
		},
		{
			// A supported member with no outstanding tasks has a successful empty reference list.
			name: "successful empty member references",
			input: func() *flowv1.RackPowerDetails {
				base := newDetails()
				base.Operations.Tasks.Entries = nil
				for _, member := range base.Trays {
					member.Operations.Tasks.Entries = nil
				}
				return base
			},
			check: func(t *testing.T, got APIRackPowerState) {
				assert.Empty(t, got.Operations.Tasks)
				assert.Nil(t, got.Operations.TasksError)
				require.NotEmpty(t, got.Components)
				assert.Empty(t, got.Components[0].Operations.Tasks)
				assert.Nil(t, got.Components[0].Operations.TasksError)
			},
		},
		{
			// A failed Flow task query does not invalidate independent Core action evidence.
			name: "failed task read retains member actions",
			input: func() *flowv1.RackPowerDetails {
				base := newDetails()
				code := flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED
				base.Operations.Tasks = &flowv1.PowerTaskDetailsList{Error: &code}
				// Component actions remain readable without Flow's redundant compiled copy.
				base.Operations.Actions = nil
				for _, member := range base.Trays {
					if member.ComponentType == flowv1.ComponentType_COMPONENT_TYPE_COMPUTE {
						member.Operations.Tasks = &flowv1.PowerTaskReferenceList{Error: &code}
					}
				}
				return base
			},
			check: func(t *testing.T, got APIRackPowerState) {
				assert.Empty(t, got.Operations.Tasks)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("ReadFailed"), got.Operations.TasksError)
				require.NotEmpty(t, got.Components)
				assert.NotEmpty(t, got.Components[0].Operations.Actions)
				assert.Nil(t, got.Components[0].Operations.ActionsError)
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("ReadFailed"), got.Components[0].Operations.TasksError)
			},
		},
		{
			// An omitted rack operations message cannot look like a successful empty task read.
			name:  "missing operations stay unavailable",
			input: func() *flowv1.RackPowerDetails { return &flowv1.RackPowerDetails{} },
			check: func(t *testing.T, got APIRackPowerState) {
				assert.Equal(t, cutil.GetPtr[APIPowerReadError]("Unsupported"), got.Operations.TasksError)
			},
		},
	}

	schema := testRackPowerSchema(t, "RackPowerState")
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			input := tc.input()
			got := APIRackPowerState{}

			got.FromProto(input)

			encoded, err := json.Marshal(got)
			require.NoError(t, err)
			tc.check(t, got)
			assert.NotContains(t, string(encoded), privateRackID)
			assert.NotContains(t, string(encoded), privateShelfID)
			assert.NotContains(t, string(encoded), privateRemovedID)
			for _, id := range privateMembers {
				assert.NotContains(t, string(encoded), id)
			}
			var value map[string]any
			require.NoError(t, json.Unmarshal(encoded, &value))
			require.NoError(t, schema.Validate(value))
			// Publish each component's actions without redundant root fields.
			assert.Contains(t, value, "components")
			assert.NotContains(t, value, "trays")
			operations, ok := value["operations"].(map[string]any)
			require.True(t, ok)
			assert.NotContains(t, operations, "actions")
			assert.NotContains(t, operations, "actionsError")

			// Decode the actual DTO bytes through the generated public GET operation.
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				assert.Equal(t, http.MethodGet, r.Method)
				assert.Equal(t, "/v2/org/test-org/nico/rack/rack-01/power", r.URL.Path)
				assert.Equal(t, "11111111-1111-1111-1111-111111111111", r.URL.Query().Get("siteId"))
				w.Header().Set("Content-Type", "application/json")
				_, err := w.Write(encoded)
				assert.NoError(t, err)
			}))
			t.Cleanup(server.Close)
			config := standard.NewConfiguration()
			config.Servers = standard.ServerConfigurations{{URL: server.URL}}
			client := standard.NewAPIClient(config)
			decoded, response, err := client.RackAPI.GetRackPowerState(context.Background(), "test-org", rackID).
				SiteId("11111111-1111-1111-1111-111111111111").Execute()
			require.NoError(t, err)
			require.NotNil(t, response)
			t.Cleanup(func() { _ = response.Body.Close() })
			roundTrip, err := json.Marshal(decoded)
			require.NoError(t, err)
			assert.JSONEq(t, string(encoded), string(roundTrip))
		})
	}
}

// TestAPIRackPowerSummary_FromProto protects Flow's aggregation decisions from being
// recomputed or losing coverage categories during the REST conversion.
func TestAPIRackPowerSummary_FromProto(t *testing.T) {
	cases := []struct {
		name  string
		input *flowv1.RackPowerSummary
		want  APIRackPowerSummary
	}{
		// An absent producer summary cannot claim complete coverage.
		{name: "absent summary stays incomplete", want: APIRackPowerSummary{State: "Unknown"}},
		// Distinct actual counts expose swaps while preserving Flow's incomplete decision.
		{name: "supplied mixed decision survives incomplete coverage", input: &flowv1.RackPowerSummary{
			State:    flowv1.RackPowerState_RACK_POWER_STATE_MIXED,
			Coverage: &flowv1.RackPowerCoverage{Total: 14, Known: 2, Unknown: 3, Unavailable: 4, Unsupported: 5}},
			want: APIRackPowerSummary{State: "Mixed", Coverage: APIRackPowerCoverage{Total: 14, Known: 2, Unknown: 3, Unavailable: 4, Unsupported: 5}}},
		// Policy exclusions and unsupported inventory do not become failed observations.
		{name: "policy exclusions stay separate", input: &flowv1.RackPowerSummary{
			State: flowv1.RackPowerState_RACK_POWER_STATE_ON, Complete: true,
			Coverage: &flowv1.RackPowerCoverage{Total: 15, Known: 1, NotApplicable: 2, NoPolicy: 3, DisabledPolicy: 4, Unsupported: 5}},
			want: APIRackPowerSummary{State: "On", Complete: true,
				Coverage: APIRackPowerCoverage{Total: 15, Known: 1, NotApplicable: 2, NoPolicy: 3, DisabledPolicy: 4, Unsupported: 5}}},
		// An enum added by a newer producer must not be mistaken for a known state.
		{name: "future state stays unknown", input: &flowv1.RackPowerSummary{State: flowv1.RackPowerState(99), Complete: true},
			want: APIRackPowerSummary{State: "Unknown", Complete: true}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := APIRackPowerSummary{}

			got.FromProto(tc.input)

			assert.Equal(t, tc.want, got)
		})
	}
}

// TestAPIRackPowerSummaryState_FromProto checks each public spelling against
// the published state schema, including the rack-only `Mixed` value.
func TestAPIRackPowerSummaryState_FromProto(t *testing.T) {
	schema := testRackPowerSchema(t, "RackPowerSummary/properties/state")
	cases := []struct {
		name  string
		input flowv1.RackPowerState
		want  APIRackPowerSummaryState
	}{
		{name: "unknown", input: flowv1.RackPowerState_RACK_POWER_STATE_UNKNOWN, want: "Unknown"},
		{name: "on", input: flowv1.RackPowerState_RACK_POWER_STATE_ON, want: "On"},
		{name: "off", input: flowv1.RackPowerState_RACK_POWER_STATE_OFF, want: "Off"},
		{name: "powering on", input: flowv1.RackPowerState_RACK_POWER_STATE_POWERING_ON, want: "PoweringOn"},
		{name: "powering off", input: flowv1.RackPowerState_RACK_POWER_STATE_POWERING_OFF, want: "PoweringOff"},
		{name: "paused", input: flowv1.RackPowerState_RACK_POWER_STATE_PAUSED, want: "Paused"},
		{name: "reset", input: flowv1.RackPowerState_RACK_POWER_STATE_RESET, want: "Reset"},
		{name: "mixed", input: flowv1.RackPowerState_RACK_POWER_STATE_MIXED, want: "Mixed"},
		{name: "future", input: flowv1.RackPowerState(99), want: "Unknown"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got APIRackPowerSummaryState

			got.FromProto(tc.input)

			assert.Equal(t, tc.want, got)
			require.NoError(t, schema.Validate(string(got)))
		})
	}
}

// TestRackPowerState_UnmarshalJSON keeps required keys distinct from nullable values
// at the generated SDK boundary, using one valid DTO as the positive control.
func TestRackPowerState_UnmarshalJSON(t *testing.T) {
	component := testTrayPowerState()
	details := APIRackPowerState{}
	details.FromProto(&flowv1.RackPowerDetails{
		Operations: &flowv1.RackPowerOperations{Tasks: &flowv1.PowerTaskDetailsList{}},
		Trays: []*flowv1.RackTrayPowerDetails{{
			ComponentType: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
			Actual:        component.Actual, Desired: component.Desired,
			Operations: &flowv1.RackTrayPowerOperations{
				Actions: component.Operations.Actions, Tasks: &flowv1.PowerTaskReferenceList{},
			},
		}},
	})
	encoded, err := json.Marshal(details)
	require.NoError(t, err)
	var valid standard.RackPowerState
	require.NoError(t, valid.UnmarshalJSON(encoded))
	require.Len(t, valid.Components, 1)

	for _, tc := range []struct {
		name   string
		mutate func(map[string]any)
		field  string
	}{
		// A nullable root identity still needs its key to advertise API support.
		{name: "missing nullable root identity", field: "id", mutate: func(fields map[string]any) { delete(fields, "id") }},
		// Nested read errors also require an explicit null on successful reads.
		{name: "missing nullable nested read error", field: "tasksError", mutate: func(fields map[string]any) {
			delete(fields["operations"].(map[string]any), "tasksError")
		}},
		// Renamed component models must also require an explicit action-error key.
		{name: "missing nullable component action error", field: "actionsError", mutate: func(fields map[string]any) {
			member := fields["components"].([]any)[0].(map[string]any)
			delete(member["operations"].(map[string]any), "actionsError")
		}},
		// A present null cannot stand in for the nonnullable member array.
		{name: "null member array", field: "components", mutate: func(fields map[string]any) { fields["components"] = nil }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var fields map[string]any
			require.NoError(t, json.Unmarshal(encoded, &fields))
			tc.mutate(fields)
			invalid, err := json.Marshal(fields)
			require.NoError(t, err)
			var decoded standard.RackPowerState

			err = decoded.UnmarshalJSON(invalid)

			assert.ErrorContains(t, err, "required property "+tc.field)
		})
	}
}

// testRackPowerSchema compiles the published contract once per conversion table,
// so model expectations cannot silently drift from the OpenAPI state spellings.
func testRackPowerSchema(t *testing.T, path string) *jsonschema.Schema {
	t.Helper()
	var document map[string]any
	require.NoError(t, yaml.Unmarshal(openapi.Spec, &document))
	compiler := jsonschema.NewCompiler()
	compiler.DefaultDraft(jsonschema.Draft2020)
	compiler.AssertFormat()
	require.NoError(t, compiler.AddResource("rack-power-state.json", document))

	schema, err := compiler.Compile("rack-power-state.json#/components/schemas/" + path)

	require.NoError(t, err)
	return schema
}
