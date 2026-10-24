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

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// TestAPITrayPowerDetails_FromProto protects the REST distinctions that protobuf
// getters erase: absent sections, explicit `Unknown`, and successful empty work.
// Successful and unavailable responses also pass through the generated SDK so
// required nullable fields cannot make a valid HTTP 200 unusable to clients.
func TestAPITrayPowerDetails_FromProto(t *testing.T) {
	cases := []struct {
		name      string
		mutate    func(*flowv1.TrayPowerDetails)
		check     func(*testing.T, APITrayPowerDetails, string)
		decodeSDK bool
	}{
		{
			// Keep every recorded field, including false, empty string and non-power context.
			name:      "complete independent sections",
			decodeSDK: true,
			check: func(t *testing.T, got APITrayPowerDetails, encoded string) {
				assert.Equal(t, "550e8400-e29b-41d4-a716-446655440000", got.FlowComponentID)
				require.NotNil(t, got.CoreComponentID)
				assert.Equal(t, "managed-compute", *got.CoreComponentID)
				assert.Equal(t, "Compute", got.Type)
				require.NotNil(t, got.FlowRackID)
				assert.Equal(t, "a4bf94ec-656a-46ec-9411-0da8132d346e", *got.FlowRackID)
				assert.Equal(t, cutil.GetPtr(APIPowerState("On")), got.Actual.State)
				assert.Equal(t, cutil.GetPtr(APIPowerSource("Redfish")), got.Actual.Source)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 18, 0, 2, 0, time.UTC)), got.Actual.ObservedAt)
				assert.Nil(t, got.Actual.Error)
				assert.Equal(t, cutil.GetPtr("PowerManagerDisabled"), got.Desired.State)
				assert.Equal(t, "Available", got.Desired.Status)
				assert.Equal(t, cutil.GetPtr(APIPowerSource("CorePowerPolicy")), got.Desired.Source)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 59, 0, 0, time.UTC)), got.Desired.UpdatedAt)

				require.Len(t, got.Operations.Actions.Entries, 2)
				active, pending := got.Operations.Actions.Entries[0], got.Operations.Actions.Entries[1]
				assert.Equal(t, "Active", active.Status)
				assert.Equal(t, "PowerOff", active.Operation)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 58, 0, 0, time.UTC)), active.RequestedAt)
				require.NotNil(t, active.Graceful)
				assert.False(t, *active.Graceful)
				require.NotNil(t, active.Initiator)
				assert.Empty(t, *active.Initiator)
				assert.Equal(t, "Pending", pending.Status)
				assert.Equal(t, "ChassisReset", pending.Operation)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 59, 30, 0, time.UTC)), pending.RequestedAt)
				assert.Equal(t, cutil.GetPtr("Chassis.1"), pending.ChassisID)

				require.Len(t, got.Operations.Tasks.Entries, 2)
				task := got.Operations.Tasks.Entries[0]
				assert.Equal(t, "09684314-d3bd-4352-98a4-0321a517a32e", task.ID)
				assert.Equal(t, "a4bf94ec-656a-46ec-9411-0da8132d346e", task.FlowRackID)
				assert.Equal(t, "Pending", task.Status)
				assert.Equal(t, "ColdReset", task.Operation)
				assert.True(t, task.OverrideReadinessCheck)
				assert.Equal(t, "RackAffected", task.Relevance)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 55, 0, 0, time.UTC)), task.CreatedAt)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 56, 0, 0, time.UTC)), task.UpdatedAt)
				assert.Nil(t, task.StartedAt)
				require.Len(t, task.Targets, 1)
				assert.Equal(t, APIPowerTaskTargets{Type: "PowerShelf", FlowComponentIDs: []string{"438838f6-cbca-4c5d-8d70-098f741e1f47"}}, task.Targets[0])
				require.NotNil(t, task.WaitingContext)
				assert.Equal(t, []APIPowerTaskBrief{{ID: "02583c20-fb2a-4ee2-ae39-ed621ed7be3b", Kind: "FirmwareControl", Status: "Running"}}, task.WaitingContext.ActiveConflicts)
				assert.Equal(t, []APIPowerTaskBrief{{ID: "26b012e7-fe6f-437a-a1e6-341c7bc08317", Kind: "BringUp", Status: "Pending"}}, task.WaitingContext.EarlierWaiting)

				// A running task has a recorded start but no waiting context.
				running := got.Operations.Tasks.Entries[1]
				assert.Equal(t, "458746fb-4124-465a-b522-ff2a9eb99d39", running.ID)
				assert.Equal(t, "a4bf94ec-656a-46ec-9411-0da8132d346e", running.FlowRackID)
				assert.Equal(t, "Running", running.Status)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 57, 0, 0, time.UTC)), running.CreatedAt)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 18, 0, 0, 0, time.UTC)), running.UpdatedAt)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 57, 30, 0, time.UTC)), running.StartedAt)
				assert.Nil(t, running.WaitingContext)
				assert.Contains(t, encoded, `"graceful":false`)
				assert.Contains(t, encoded, `"initiator":""`)
			},
		},
		{
			// Protobuf's zero-valued state is a successful observation when present.
			name: "explicit unknown observation",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Actual.GetValue().Outcome = &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_UNKNOWN}
			},
			check: func(t *testing.T, got APITrayPowerDetails, _ string) {
				require.NotNil(t, got.Actual.State)
				assert.Equal(t, APIPowerState("Unknown"), *got.Actual.State)
				assert.Nil(t, got.Actual.Error)
			},
		},
		{
			// A hardware failure must leave policy and successfully empty lists usable.
			name: "partial result and successful empty work",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Actual.GetValue().Outcome = &corev1.ComponentPowerObservation_Error{Error: &corev1.ComponentPowerError{Code: corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_CREDENTIALS_UNAVAILABLE, Message: "private dependency detail"}}
				p.Actual.GetValue().ObservedAt = nil
				p.ComponentType = flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH
				p.Desired.Result = &flowv1.PowerDesiredDetails_Value{Value: &corev1.ComponentPowerPolicy{Status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_NOT_APPLICABLE}}
				p.Operations.Actions.GetValue().Actions = nil
				p.Operations.Tasks.Entries = nil
			},
			check: func(t *testing.T, got APITrayPowerDetails, encoded string) {
				assert.Nil(t, got.Actual.State)
				assert.Nil(t, got.Actual.ObservedAt)
				require.NotNil(t, got.Actual.Error)
				assert.Equal(t, APIPowerDetailsError("CredentialsUnavailable"), *got.Actual.Error)
				assert.Equal(t, "NVSwitch", got.Type)
				assert.Equal(t, "NotApplicable", got.Desired.Status)
				assert.Nil(t, got.Desired.State)
				assert.Nil(t, got.Desired.Source)
				assert.NotNil(t, got.Operations.Actions.Entries)
				assert.Empty(t, got.Operations.Actions.Entries)
				assert.Nil(t, got.Operations.Actions.Error)
				assert.Contains(t, encoded, `"tasks":{"entries":[],"error":null}`)
				assert.NotContains(t, encoded, "private dependency detail")
			},
		},
		{
			// A successful policy lookup can find no policy or no managed target.
			name: "unavailable policy is not a failed lookup",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Desired.Result = &flowv1.PowerDesiredDetails_Value{Value: &corev1.ComponentPowerPolicy{}}
			},
			check: func(t *testing.T, got APITrayPowerDetails, _ string) {
				assert.Equal(t, "Unavailable", got.Desired.Status)
				assert.Nil(t, got.Desired.State)
				assert.Nil(t, got.Desired.Source)
				assert.Nil(t, got.Desired.Error)
			},
		},
		{
			// An enum added by a newer service must not look like observed `Unknown`.
			name: "unrecognized hardware state",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Actual.GetValue().Outcome = &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState(99)}
				p.Actual.GetValue().Source = cutil.GetPtr(corev1.ComponentPowerSource(99)) // A newer reader must not become a known source.
			},
			check: func(t *testing.T, got APITrayPowerDetails, _ string) {
				assert.Nil(t, got.Actual.State)
				assert.Nil(t, got.Actual.ObservedAt)
				assert.Nil(t, got.Actual.Source)
				require.NotNil(t, got.Actual.Error)
				assert.Equal(t, APIPowerDetailsError("Unsupported"), *got.Actual.Error)
				assert.Nil(t, got.Desired.Error)
			},
		},
		{
			// Omitted old-service sections cannot become a successful zero-value response.
			name:      "older service omits sections",
			decodeSDK: true,
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.ComponentType = flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF
				p.CoreComponentId, p.RackId, p.Actual, p.Desired, p.Operations = nil, nil, nil, nil, nil
			},
			check: func(t *testing.T, got APITrayPowerDetails, encoded string) {
				assert.JSONEq(t, `{"flowComponentId":"550e8400-e29b-41d4-a716-446655440000","coreComponentId":null,"type":"PowerShelf","flowRackId":null,"actual":{"state":null,"source":null,"observedAt":null,"error":"Unsupported"},"desired":{"status":"Unavailable","state":null,"source":null,"updatedAt":null,"error":"Unsupported"},"operations":{"actions":{"entries":[],"error":"Unsupported"},"tasks":{"entries":[],"error":"Unsupported"}}}`, encoded)
			},
		},
		{
			// Flow section failures and Core action failures have the same explicit JSON boundary.
			name: "failed sections discard unusable lists",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Actual.Result = &flowv1.PowerActualDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_DEADLINE_EXCEEDED}
				p.Desired.Result = &flowv1.PowerDesiredDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED}
				p.Operations.Actions.GetValue().Error = &corev1.ComponentPowerError{Code: corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND}
				p.Operations.Tasks.Error = cutil.GetPtr(flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED)
			},
			check: func(t *testing.T, got APITrayPowerDetails, _ string) {
				require.NotNil(t, got.Actual.Error)
				require.NotNil(t, got.Desired.Error)
				require.NotNil(t, got.Operations.Actions.Error)
				require.NotNil(t, got.Operations.Tasks.Error)
				assert.Equal(t, APIPowerDetailsError("DeadlineExceeded"), *got.Actual.Error)
				assert.Equal(t, APIPowerDetailsError("NotLinked"), *got.Desired.Error)
				assert.Equal(t, APIPowerDetailsError("NotFound"), *got.Operations.Actions.Error)
				assert.Equal(t, APIPowerDetailsError("LimitExceeded"), *got.Operations.Tasks.Error)
				assert.Empty(t, got.Operations.Actions.Entries)
				assert.Empty(t, got.Operations.Tasks.Entries)
			},
		},
		{
			// A Flow lookup failure has no Core action value, but must preserve its
			// specific error and leave the other successfully read sections usable.
			name: "Flow action failure preserves independent sections",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Operations.Actions.Result = &flowv1.PowerActionDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED}
			},
			check: func(t *testing.T, got APITrayPowerDetails, _ string) {
				require.NotNil(t, got.Operations.Actions.Error)
				assert.Equal(t, APIPowerDetailsError("NotLinked"), *got.Operations.Actions.Error)
				assert.NotNil(t, got.Operations.Actions.Entries)
				assert.Empty(t, got.Operations.Actions.Entries)
				assert.Equal(t, cutil.GetPtr(APIPowerState("On")), got.Actual.State)
				assert.Nil(t, got.Actual.Error)
				assert.Equal(t, "Available", got.Desired.Status)
				assert.Nil(t, got.Desired.Error)
				assert.Len(t, got.Operations.Tasks.Entries, 2)
				assert.Nil(t, got.Operations.Tasks.Error)
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p := testTrayPowerDetails()
			if tc.mutate != nil {
				tc.mutate(p)
			}

			got := APITrayPowerDetails{}
			got.FromProto(p)
			encoded, err := json.Marshal(got)
			require.NoError(t, err)

			tc.check(t, got, string(encoded))
			if tc.decodeSDK {
				// Use the actual server DTO bytes and generated GET decoder;
				// unmarshalling into the server model would miss SDK null guards.
				const siteID = "11111111-1111-1111-1111-111111111111"
				server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					assert.Equal(t, http.MethodGet, r.Method)
					assert.Equal(t, "/v2/org/test-org/nico/tray/managed-compute/power", r.URL.Path)
					assert.Equal(t, siteID, r.URL.Query().Get("siteId"))
					w.Header().Set("Content-Type", "application/json")
					_, err := w.Write(encoded)
					assert.NoError(t, err)
				}))
				t.Cleanup(server.Close)
				config := standard.NewConfiguration()
				config.Servers = standard.ServerConfigurations{{URL: server.URL}}
				client := standard.NewAPIClient(config)

				decoded, response, err := client.TrayAPI.GetTrayPowerDetails(context.Background(), "test-org", "managed-compute").SiteId(siteID).Execute()
				require.NoError(t, err)
				require.NotNil(t, response)
				assert.Equal(t, http.StatusOK, response.StatusCode)
				t.Cleanup(func() { _ = response.Body.Close() })
				roundTrip, err := json.Marshal(decoded)
				require.NoError(t, err)
				assert.JSONEq(t, string(encoded), string(roundTrip))
			}
		})
	}
}

// TestAPIPowerTaskDetails_FromProto protects unavailable context versus an
// evaluated empty context, so old services cannot imply there is no nearby work.
// SDK round trips also preserve null versus explicitly empty target selections.
func TestAPIPowerTaskDetails_FromProto(t *testing.T) {
	cases := []struct {
		name        string
		context     *flowv1.PowerTaskWaitingContext
		targets     *flowv1.ComponentsByType
		want        string
		wantTargets string
	}{
		// A missing context field may come from a service that predates the field.
		{name: "absent", want: `null`, wantTargets: `null`},
		// A completed calculation with no matching tasks is a useful affirmative answer.
		{name: "evaluated empty", context: &flowv1.PowerTaskWaitingContext{}, targets: &flowv1.ComponentsByType{}, want: `{"activeConflicts":[],"earlierWaiting":[],"error":null}`, wantTargets: `[]`},
		// Failed calculations must not expose partial references as complete evidence.
		{name: "unavailable", context: &flowv1.PowerTaskWaitingContext{Error: cutil.GetPtr(flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED), ActiveConflicts: []*flowv1.TaskBrief{{Id: &flowv1.UUID{Id: "partial-active"}}}, EarlierWaiting: []*flowv1.TaskBrief{{Id: &flowv1.UUID{Id: "partial-waiting"}}}}, want: `{"activeConflicts":[],"earlierWaiting":[],"error":"ReadFailed"}`, wantTargets: `null`},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p := testTrayPowerDetails().Operations.Tasks.Entries[0]
			p.WaitingContext = tc.context
			p.Targets = tc.targets // Missing recorded targets differ from an explicitly empty selection.

			got := APIPowerTaskDetails{}
			got.FromProto(p)
			encoded, err := json.Marshal(got.WaitingContext)
			require.NoError(t, err)

			assert.JSONEq(t, tc.want, string(encoded))
			if tc.context == nil {
				// Direct reuse of the public context mapper must clear stale
				// evidence even though the enclosing task preserves absence.
				previous := APIPowerTaskWaitingContext{
					ActiveConflicts: []APIPowerTaskBrief{{ID: "old-active"}},
					EarlierWaiting:  []APIPowerTaskBrief{{ID: "old-waiting"}},
					Error:           cutil.GetPtr(APIPowerDetailsError("ReadFailed")),
				}
				previous.FromProto(nil)
				assert.Equal(t, APIPowerTaskWaitingContext{ActiveConflicts: []APIPowerTaskBrief{}, EarlierWaiting: []APIPowerTaskBrief{}}, previous)
			}

			// SDK serialization must retain the required targets key, including
			// its distinction between unavailable and explicitly empty selection.
			taskJSON, err := json.Marshal(got)
			require.NoError(t, err)
			var sdkTask standard.PowerTaskDetails
			require.NoError(t, json.Unmarshal(taskJSON, &sdkTask))
			roundTrip, err := json.Marshal(sdkTask)
			require.NoError(t, err)
			assert.JSONEq(t, string(taskJSON), string(roundTrip))
			var fields map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(roundTrip, &fields))
			require.Contains(t, fields, "targets")
			assert.JSONEq(t, tc.wantTargets, string(fields["targets"]))
		})
	}
}

// TestAPITrayPowerDetails_SDKRequiredFields keeps nullability separate from
// requiredness: allowing reported nulls must not admit a missing or invalid key.
func TestAPITrayPowerDetails_SDKRequiredFields(t *testing.T) {
	details := APITrayPowerDetails{}
	details.FromProto(testTrayPowerDetails())
	encoded, err := json.Marshal(details)
	require.NoError(t, err)

	cases := []struct {
		name   string
		field  string
		remove bool
	}{
		// Nullable identity still needs a key so absence cannot imply old API support.
		{name: "missing nullable identity", field: "coreComponentId", remove: true},
		// A non-nullable field must retain its stronger value requirement.
		{name: "null component type", field: "type"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var fields map[string]any
			require.NoError(t, json.Unmarshal(encoded, &fields))
			if tc.remove {
				delete(fields, tc.field)
			} else {
				fields[tc.field] = nil
			}
			invalid, err := json.Marshal(fields)
			require.NoError(t, err)

			var decoded standard.TrayPowerDetails
			assert.ErrorContains(t, json.Unmarshal(invalid, &decoded), "required property "+tc.field)
		})
	}
}

// testTrayPowerDetails supplies independent hardware, policy, maintenance and
// task records; their different operations must never be inferred from each other.
func testTrayPowerDetails() *flowv1.TrayPowerDetails {
	// Distinct times make accidental reuse of another section's timestamp visible.
	actual := &corev1.ComponentPowerObservation{
		Outcome:    &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON},
		Source:     cutil.GetPtr(corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_REDFISH),
		ObservedAt: timestamppb.New(time.Date(2026, 10, 7, 18, 0, 2, 0, time.UTC)),
	}
	desired := &corev1.ComponentPowerPolicy{
		Status:    corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE,
		State:     cutil.GetPtr(corev1.PowerState_PowerManagerDisabled),
		Source:    cutil.GetPtr(corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_CORE_POWER_POLICY),
		UpdatedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 59, 0, 0, time.UTC)),
	}
	actions := &corev1.ComponentPowerActionDetails{Actions: []*corev1.ComponentPowerAction{
		{
			Status:      corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_ACTIVE,
			Operation:   corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_POWER_OFF,
			RequestedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 58, 0, 0, time.UTC)),
			Initiator:   cutil.GetPtr(""),    // Explicit empty metadata must survive JSON conversion.
			Graceful:    cutil.GetPtr(false), // False selects forced shutdown, rather than absence.
		},
		{
			Status:      corev1.ComponentPowerActionStatus_COMPONENT_POWER_ACTION_STATUS_PENDING,
			Operation:   corev1.ComponentPowerActionOperation_COMPONENT_POWER_ACTION_OPERATION_CHASSIS_RESET,
			RequestedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 59, 30, 0, time.UTC)),
			ChassisId:   cutil.GetPtr("Chassis.1"),
		},
	}}

	// The shelf is the recorded target; this compute tray is affected without
	// becoming a fabricated target. Non-power neighbors stay brief references.
	task := &flowv1.PowerTaskDetails{
		Id:     &flowv1.UUID{Id: "09684314-d3bd-4352-98a4-0321a517a32e"},
		RackId: &flowv1.UUID{Id: "a4bf94ec-656a-46ec-9411-0da8132d346e"},
		Operation: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{
			PowerControl: &flowv1.PowerControlTaskOperation{
				Operation:              flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_COLD_RESET,
				OverrideReadinessCheck: true,
			},
		}},
		Targets: &flowv1.ComponentsByType{Groups: []*flowv1.ComponentsForType{{
			Type:         flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF,
			ComponentIds: []*flowv1.UUID{{Id: "438838f6-cbca-4c5d-8d70-098f741e1f47"}},
		}}},
		Status:    flowv1.TaskStatus_TASK_STATUS_WAITING,
		CreatedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 55, 0, 0, time.UTC)),
		UpdatedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 56, 0, 0, time.UTC)),
		Relevance: flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED,
		WaitingContext: &flowv1.PowerTaskWaitingContext{
			ActiveConflicts: []*flowv1.TaskBrief{{
				Id:     &flowv1.UUID{Id: "02583c20-fb2a-4ee2-ae39-ed621ed7be3b"},
				Kind:   flowv1.TaskKind_TASK_KIND_FIRMWARE_CONTROL,
				Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
			}},
			EarlierWaiting: []*flowv1.TaskBrief{{
				Id:     &flowv1.UUID{Id: "26b012e7-fe6f-437a-a1e6-341c7bc08317"},
				Kind:   flowv1.TaskKind_TASK_KIND_BRING_UP,
				Status: flowv1.TaskStatus_TASK_STATUS_WAITING,
			}},
		},
	}
	// Running work supplies a real start time and correctly has no waiting context.
	running := &flowv1.PowerTaskDetails{
		Id:        &flowv1.UUID{Id: "458746fb-4124-465a-b522-ff2a9eb99d39"},
		RackId:    &flowv1.UUID{Id: "a4bf94ec-656a-46ec-9411-0da8132d346e"},
		Operation: task.Operation,
		Targets:   task.Targets,
		Status:    flowv1.TaskStatus_TASK_STATUS_RUNNING,
		CreatedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 57, 0, 0, time.UTC)),
		UpdatedAt: timestamppb.New(time.Date(2026, 10, 7, 18, 0, 0, 0, time.UTC)),
		StartedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 57, 30, 0, time.UTC)),
		Relevance: flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED,
	}
	return &flowv1.TrayPowerDetails{
		FlowComponentId: &flowv1.UUID{Id: "550e8400-e29b-41d4-a716-446655440000"},
		CoreComponentId: cutil.GetPtr("managed-compute"),
		ComponentType:   flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
		RackId:          &flowv1.UUID{Id: "a4bf94ec-656a-46ec-9411-0da8132d346e"},
		Actual:          &flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Value{Value: actual}},
		Desired:         &flowv1.PowerDesiredDetails{Result: &flowv1.PowerDesiredDetails_Value{Value: desired}},
		Operations: &flowv1.PowerOperations{
			Actions: &flowv1.PowerActionDetails{Result: &flowv1.PowerActionDetails_Value{Value: actions}},
			Tasks:   &flowv1.PowerTaskDetailsList{Entries: []*flowv1.PowerTaskDetails{task, running}},
		},
	}
}
