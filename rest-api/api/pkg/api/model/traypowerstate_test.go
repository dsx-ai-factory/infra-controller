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
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/sdk/standard"
)

// TestAPITrayPowerState_FromProto protects the REST distinctions that protobuf
// getters erase: absent sections, explicit `Unknown`, and successful empty work.
// Successful and unavailable responses also pass through the generated SDK so
// required nullable fields cannot make a valid HTTP 200 unusable to clients.
func TestAPITrayPowerState_FromProto(t *testing.T) {
	cases := []struct {
		name      string
		mutate    func(*flowv1.TrayPowerDetails)
		check     func(*testing.T, APITrayPowerState, string)
		decodeSDK bool
	}{
		{
			// Keep every recorded field, including false, empty string and non-power context.
			name:      "complete independent sections",
			decodeSDK: true,
			check: func(t *testing.T, got APITrayPowerState, encoded string) {
				require.NotNil(t, got.ID)
				assert.Equal(t, "managed-compute", *got.ID)
				assert.Equal(t, "Compute", got.Type)
				require.NotNil(t, got.RackID)
				assert.Equal(t, "rack-inventory-id", *got.RackID)
				assert.Equal(t, cutil.GetPtr(APIPowerState("On")), got.Actual.State)
				assert.Equal(t, cutil.GetPtr(APIPowerSource("Redfish")), got.Actual.Source)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 18, 0, 2, 0, time.UTC)), got.Actual.Observed)
				assert.Nil(t, got.Actual.Error)
				assert.Equal(t, cutil.GetPtr(APIPowerPolicyState("PowerManagerDisabled")), got.Desired.State)
				assert.Equal(t, APIPowerPolicyStatus("Available"), got.Desired.Status)

				require.Len(t, got.Operations.Actions, 2)
				active, pending := got.Operations.Actions[0], got.Operations.Actions[1]
				assert.Equal(t, APIPowerActionStatus("Active"), active.Status)
				assert.Equal(t, APIPowerActionOperation("PowerOff"), active.Operation)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 58, 0, 0, time.UTC)), active.Requested)
				require.NotNil(t, active.Graceful)
				assert.False(t, *active.Graceful)
				require.NotNil(t, active.Initiator)
				assert.Empty(t, *active.Initiator)
				assert.Equal(t, APIPowerActionStatus("Pending"), pending.Status)
				assert.Equal(t, APIPowerActionOperation("ChassisReset"), pending.Operation)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 59, 30, 0, time.UTC)), pending.Requested)
				assert.Equal(t, cutil.GetPtr("Chassis.1"), pending.ChassisID)
				// Metadata Core didn't record stays null rather than becoming a zero value.
				assert.Nil(t, active.ChassisID)
				assert.Nil(t, pending.Graceful)
				assert.Nil(t, pending.Initiator)

				require.Len(t, got.Operations.Tasks, 2)
				task := got.Operations.Tasks[0]
				assert.Equal(t, "09684314-d3bd-4352-98a4-0321a517a32e", task.ID)
				assert.Equal(t, cutil.GetPtr("rack-inventory-id"), task.RackID)
				assert.Equal(t, "Pending", task.Status)
				assert.Equal(t, APIPowerTaskOperation("ColdReset"), task.Operation)
				assert.True(t, task.OverrideReadinessCheck)
				assert.Equal(t, APIPowerTaskRelevance("RackAffected"), task.Relevance)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 55, 0, 0, time.UTC)), task.Created)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 56, 0, 0, time.UTC)), task.Updated)
				assert.Nil(t, task.Started)
				assert.Equal(t, []APIPowerTaskTargetGroup{
					{Type: "CDU", ComponentIDs: []string{"cdu-inventory-id"}},
					{Type: "NVSwitch", ComponentIDs: []string{"z-switch", "a-switch", "a-switch"}},
					{Type: "PowerShelf", ComponentIDs: []string{"z-switch"}},
				}, task.Targets)
				assert.Nil(t, task.TargetsError)
				require.NotNil(t, task.WaitingContext)
				assert.Equal(t, []APITaskSummary{{ID: "02583c20-fb2a-4ee2-ae39-ed621ed7be3b", Type: "FirmwareControl", Status: "Running"}}, task.WaitingContext.ActiveConflicts)
				assert.Equal(t, []APITaskSummary{{ID: "26b012e7-fe6f-437a-a1e6-341c7bc08317", Type: "BringUp", Status: "Pending"}}, task.WaitingContext.EarlierWaiting)

				// A running task has a recorded start but no waiting context.
				running := got.Operations.Tasks[1]
				assert.Equal(t, "458746fb-4124-465a-b522-ff2a9eb99d39", running.ID)
				assert.Equal(t, cutil.GetPtr("rack-inventory-id"), running.RackID)
				assert.Equal(t, "Running", running.Status)
				assert.Equal(t, APIPowerTaskOperation("PowerOn"), running.Operation)
				assert.False(t, running.OverrideReadinessCheck)
				assert.Equal(t, APIPowerTaskRelevance("Selected"), running.Relevance)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 57, 0, 0, time.UTC)), running.Created)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 18, 0, 0, 0, time.UTC)), running.Updated)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 57, 30, 0, time.UTC)), running.Started)
				assert.Nil(t, running.WaitingContext)
				assert.Contains(t, encoded, `"graceful":false`)
				assert.Contains(t, encoded, `"initiator":""`)
				assert.Contains(t, encoded, `"observed":"2026-10-07T18:00:02Z"`)
				assert.Contains(t, encoded, `"requested":"2026-10-07T17:58:00Z"`)
				assert.Contains(t, encoded, `"created":"2026-10-07T17:55:00Z"`)
				assert.Contains(t, encoded, `"started":"2026-10-07T17:57:30Z"`)
				assert.NotContains(t, encoded, "flowComponentId")
				assert.NotContains(t, encoded, "coreComponentId")
				assert.NotContains(t, encoded, "flowRackId")
				assert.NotContains(t, encoded, "a4bf94ec-656a-46ec-9411-0da8132d346e")
				assert.NotContains(t, encoded, "438838f6-cbca-4c5d-8d70-098f741e1f47")
			},
		},
		{
			// Protobuf's zero-valued state is a successful observation when present.
			name: "explicit unknown observation",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Actual.GetValue().Outcome = &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_UNKNOWN}
			},
			check: func(t *testing.T, got APITrayPowerState, _ string) {
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
			check: func(t *testing.T, got APITrayPowerState, encoded string) {
				assert.Nil(t, got.Actual.State)
				assert.Nil(t, got.Actual.Observed)
				require.NotNil(t, got.Actual.Error)
				assert.Equal(t, APIPowerReadError("CredentialsUnavailable"), *got.Actual.Error)
				assert.Equal(t, cutil.GetPtr(APIPowerSource("Redfish")), got.Actual.Source)
				assert.Equal(t, "NVSwitch", got.Type)
				assert.Equal(t, APIPowerPolicyStatus("NotApplicable"), got.Desired.Status)
				assert.Nil(t, got.Desired.State)
				assert.NotNil(t, got.Operations.Actions)
				assert.Empty(t, got.Operations.Actions)
				assert.Nil(t, got.Operations.ActionsError)
				assert.Contains(t, encoded, `"tasks":[],"tasksError":null`)
				assert.NotContains(t, encoded, "private dependency detail")
			},
		},
		{
			// A successful policy lookup can find no policy or no managed target.
			name: "unavailable policy is not a failed lookup",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Desired.Result = &flowv1.PowerDesiredDetails_Value{Value: &corev1.ComponentPowerPolicy{}}
			},
			check: func(t *testing.T, got APITrayPowerState, _ string) {
				assert.Equal(t, APIPowerPolicyStatus("Unavailable"), got.Desired.Status)
				assert.Nil(t, got.Desired.State)
				assert.Nil(t, got.Desired.Error)
			},
		},
		{
			// An enum added by a newer service must not look like observed `Unknown`.
			name:      "unrecognized hardware state",
			decodeSDK: true,
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Actual.GetValue().Outcome = &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState(99)}
				p.Actual.GetValue().Source = cutil.GetPtr(corev1.ComponentPowerSource(99)) // A newer reader must not become a known source.
			},
			check: func(t *testing.T, got APITrayPowerState, _ string) {
				assert.Nil(t, got.Actual.State)
				assert.Nil(t, got.Actual.Observed)
				assert.Nil(t, got.Actual.Source)
				require.NotNil(t, got.Actual.Error)
				assert.Equal(t, APIPowerReadError("Unsupported"), *got.Actual.Error)
				assert.Nil(t, got.Desired.Error)
			},
		},
		{
			// Omitted old-service sections cannot become a successful zero-value response.
			name:      "older service omits sections",
			decodeSDK: true,
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.ComponentType = flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF
				p.ComponentId, p.RackExternalId, p.Actual, p.Desired, p.Operations = nil, nil, nil, nil, nil
			},
			check: func(t *testing.T, got APITrayPowerState, encoded string) {
				assert.JSONEq(t, `{"id":null,"type":"PowerShelf","rackId":null,"actual":{"state":null,"source":null,"observed":null,"error":"Unsupported"},"desired":{"status":"Unavailable","state":null,"error":"Unsupported"},"operations":{"actions":[],"actionsError":"Unsupported","tasks":[],"tasksError":"Unsupported"}}`, encoded)
			},
		},
		{
			// Unavailable identities must not hide this task or a healthy sibling.
			name:      "external identity failure retains tasks",
			decodeSDK: true,
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Operations.Tasks.Entries[0].ExternalTargets = &flowv1.PowerTaskExternalTargets{
					RackId:  "partial-rack-id",
					Targets: []*flowv1.ExternalRef{{Type: flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH, Id: "partial-switch-id"}},
					Error:   cutil.GetPtr(flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED),
				}
			},
			check: func(t *testing.T, got APITrayPowerState, encoded string) {
				require.Len(t, got.Operations.Tasks, 2)
				assert.Nil(t, got.Operations.TasksError)
				failed, healthy := got.Operations.Tasks[0], got.Operations.Tasks[1]
				assert.Equal(t, "09684314-d3bd-4352-98a4-0321a517a32e", failed.ID)
				assert.Equal(t, "Pending", failed.Status)
				assert.Equal(t, APIPowerTaskOperation("ColdReset"), failed.Operation)
				assert.True(t, failed.OverrideReadinessCheck)
				assert.Equal(t, APIPowerTaskRelevance("RackAffected"), failed.Relevance)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 55, 0, 0, time.UTC)), failed.Created)
				assert.Equal(t, cutil.GetPtr(time.Date(2026, 10, 7, 17, 56, 0, 0, time.UTC)), failed.Updated)
				assert.Nil(t, failed.Started)
				require.NotNil(t, failed.WaitingContext)
				assert.Len(t, failed.WaitingContext.ActiveConflicts, 1)
				assert.Nil(t, failed.RackID)
				assert.Nil(t, failed.Targets)
				assert.Equal(t, cutil.GetPtr(APIPowerReadError("NotLinked")), failed.TargetsError)
				assert.Equal(t, "458746fb-4124-465a-b522-ff2a9eb99d39", healthy.ID)
				assert.Equal(t, cutil.GetPtr("rack-inventory-id"), healthy.RackID)
				assert.Equal(t, []APIPowerTaskTargetGroup{{Type: "Compute", ComponentIDs: []string{"managed-compute"}}}, healthy.Targets)
				assert.Nil(t, healthy.TargetsError)
				assert.NotContains(t, encoded, "partial-rack-id")
				assert.NotContains(t, encoded, "partial-switch-id")
				assert.NotContains(t, encoded, "a4bf94ec-656a-46ec-9411-0da8132d346e")
				assert.NotContains(t, encoded, "438838f6-cbca-4c5d-8d70-098f741e1f47")
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
			check: func(t *testing.T, got APITrayPowerState, _ string) {
				require.NotNil(t, got.Actual.Error)
				require.NotNil(t, got.Desired.Error)
				require.NotNil(t, got.Operations.ActionsError)
				require.NotNil(t, got.Operations.TasksError)
				assert.Equal(t, APIPowerReadError("DeadlineExceeded"), *got.Actual.Error)
				assert.Equal(t, APIPowerReadError("NotLinked"), *got.Desired.Error)
				assert.Equal(t, APIPowerReadError("NotFound"), *got.Operations.ActionsError)
				assert.Equal(t, APIPowerReadError("LimitExceeded"), *got.Operations.TasksError)
				assert.Empty(t, got.Operations.Actions)
				assert.Empty(t, got.Operations.Tasks)
			},
		},
		{
			// A Flow lookup failure has no Core action value, but must preserve its
			// specific error and leave the other successfully read sections usable.
			name: "Flow action failure preserves independent sections",
			mutate: func(p *flowv1.TrayPowerDetails) {
				p.Operations.Actions.Result = &flowv1.PowerActionDetails_Error{Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED}
			},
			check: func(t *testing.T, got APITrayPowerState, _ string) {
				require.NotNil(t, got.Operations.ActionsError)
				assert.Equal(t, APIPowerReadError("NotLinked"), *got.Operations.ActionsError)
				assert.NotNil(t, got.Operations.Actions)
				assert.Empty(t, got.Operations.Actions)
				assert.Equal(t, cutil.GetPtr(APIPowerState("On")), got.Actual.State)
				assert.Nil(t, got.Actual.Error)
				assert.Equal(t, APIPowerPolicyStatus("Available"), got.Desired.Status)
				assert.Nil(t, got.Desired.Error)
				assert.Len(t, got.Operations.Tasks, 2)
				assert.Nil(t, got.Operations.TasksError)
			},
		},
	}
	// Every row must also satisfy the published schema. The generated SDK treats most
	// enums as plain strings, so it would accept a value that OpenAPI doesn't list.
	var document map[string]any
	require.NoError(t, yaml.Unmarshal(openapi.Spec, &document))
	compiler := jsonschema.NewCompiler()
	compiler.DefaultDraft(jsonschema.Draft2020)
	require.NoError(t, compiler.AddResource("tray-power-state.json", document))
	schema, err := compiler.Compile("tray-power-state.json#/components/schemas/TrayPowerState")
	require.NoError(t, err)

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p := testTrayPowerState()
			if tc.mutate != nil {
				tc.mutate(p)
			}

			got := APITrayPowerState{}
			got.FromProto(p)
			encoded, err := json.Marshal(got)
			require.NoError(t, err)

			tc.check(t, got, string(encoded))
			var response map[string]any
			require.NoError(t, json.Unmarshal(encoded, &response))
			require.NoError(t, schema.Validate(response))
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

				decoded, response, err := client.TrayAPI.GetTrayPowerState(context.Background(), "test-org", "managed-compute").SiteId(siteID).Execute()
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

// TestAPIPowerTask_FromProto protects unavailable context and identities versus
// evaluated empty results. Malformed identities invalidate the whole selection;
// SDK round trips preserve the required nullable fields in either case.
func TestAPIPowerTask_FromProto(t *testing.T) {
	cases := []struct {
		name             string
		mutate           func(*flowv1.PowerTaskDetails)
		wantTargetsError *APIPowerReadError
		check            func(*testing.T, APIPowerTask)
	}{
		{
			// Older services can omit either metadata field while retaining UUID targets.
			// A Waiting task still reports its missing context, since null means not waiting.
			name: "absent context and external identities",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.WaitingContext, p.ExternalTargets = nil, nil
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("Unsupported")),
			check: func(t *testing.T, got APIPowerTask) {
				assert.Equal(t, &APIPowerTaskWaitingContext{
					ActiveConflicts: []APITaskSummary{}, EarlierWaiting: []APITaskSummary{}, Error: cutil.GetPtr(APIPowerReadError("Unsupported")),
				}, got.WaitingContext)
			},
		},
		{
			// Evaluated empty selections and context are affirmative answers.
			name: "evaluated empty",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.WaitingContext = &flowv1.PowerTaskWaitingContext{}
				p.Targets = &flowv1.ComponentsByType{Groups: []*flowv1.ComponentsForType{{Type: flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH}}}
				p.ExternalTargets = &flowv1.PowerTaskExternalTargets{RackId: "rack-inventory-id"}
			},
			check: func(t *testing.T, got APIPowerTask) {
				assert.Equal(t, &APIPowerTaskWaitingContext{ActiveConflicts: []APITaskSummary{}, EarlierWaiting: []APITaskSummary{}}, got.WaitingContext)
				assert.Equal(t, cutil.GetPtr("rack-inventory-id"), got.RackID)
				assert.Equal(t, []APIPowerTaskTargetGroup{}, got.Targets)
			},
		},
		{
			// Failed context cannot expose partial references as complete evidence.
			name: "unavailable context",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.WaitingContext = &flowv1.PowerTaskWaitingContext{
					Error:           cutil.GetPtr(flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED),
					ActiveConflicts: []*flowv1.TaskBrief{{Id: &flowv1.UUID{Id: "partial-active"}}},
					EarlierWaiting:  []*flowv1.TaskBrief{{Id: &flowv1.UUID{Id: "partial-waiting"}}},
				}
			},
			check: func(t *testing.T, got APIPowerTask) {
				assert.Equal(t, &APIPowerTaskWaitingContext{
					ActiveConflicts: []APITaskSummary{}, EarlierWaiting: []APITaskSummary{}, Error: cutil.GetPtr(APIPowerReadError("ReadFailed")),
				}, got.WaitingContext)
			},
		},
		{
			// Successfully mapping earlier groups must not expose a partial selection.
			name: "truncated external selection",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.ExternalTargets.Targets = p.ExternalTargets.Targets[:len(p.ExternalTargets.Targets)-1]
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
		{
			// Equal ID text does not make a shelf the selected switch.
			name: "external type differs from recorded type",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.ExternalTargets.Targets[1].Type = flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
		{
			// Target IDs alone cannot identify the task's recorded rack.
			name: "empty external rack identity",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.ExternalTargets.RackId = ""
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
		{
			// A missing final ID must discard the earlier valid groups too.
			name: "empty external component identity",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.ExternalTargets.Targets[len(p.ExternalTargets.Targets)-1].Id = ""
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
		{
			// An unusable error code is still a failed lookup, even with populated IDs.
			name: "unrecognized external target error",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.ExternalTargets.Error = cutil.GetPtr(flowv1.PowerDetailsReadError(99))
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
		{
			// Matching types still need to identify a supported inventory component.
			name: "matching unknown target types",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.Targets.Groups[0].Type = flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN
				p.ExternalTargets.Targets[0].Type = flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
		{
			// A component type the API can't name must not publish a group with an empty type.
			name: "unrecognized target types",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.Targets.Groups[0].Type = flowv1.ComponentType(99)
				p.ExternalTargets.Targets[0].Type = flowv1.ComponentType(99)
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
		{
			// Extra metadata must not add components outside the recorded selection.
			name: "surplus external targets",
			mutate: func(p *flowv1.PowerTaskDetails) {
				p.ExternalTargets.Targets = append(p.ExternalTargets.Targets, &flowv1.ExternalRef{
					Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE, Id: "unselected-compute",
				})
			},
			wantTargetsError: cutil.GetPtr(APIPowerReadError("InvalidResponse")),
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p := testTrayPowerState().Operations.Tasks.Entries[0]
			tc.mutate(p)

			got := APIPowerTask{}
			got.FromProto(p)
			assert.Equal(t, tc.wantTargetsError, got.TargetsError)
			if tc.wantTargetsError != nil {
				assert.Nil(t, got.RackID)
				assert.Nil(t, got.Targets)
			}
			if tc.check != nil {
				tc.check(t, got)
			}

			// SDK serialization must retain null versus explicitly empty selection.
			taskJSON, err := json.Marshal(got)
			require.NoError(t, err)
			var sdkTask standard.PowerTask
			require.NoError(t, json.Unmarshal(taskJSON, &sdkTask))
			roundTrip, err := json.Marshal(sdkTask)
			require.NoError(t, err)
			assert.JSONEq(t, string(taskJSON), string(roundTrip))
			var fields map[string]json.RawMessage
			require.NoError(t, json.Unmarshal(roundTrip, &fields))
			assert.Contains(t, fields, "rackId")
			assert.Contains(t, fields, "targets")
			assert.Contains(t, fields, "targetsError")
		})
	}
}

// TestAPIPowerObservation_FromProto keeps a malformed populated observation
// distinct from an omitted section supplied by an older service.
func TestAPIPowerObservation_FromProto(t *testing.T) {
	for _, tc := range []struct {
		name        string
		observation *corev1.ComponentPowerObservation
		want        APIPowerObservation
	}{
		{
			name: "populated observation omits outcome",
			observation: &corev1.ComponentPowerObservation{
				Source:     cutil.GetPtr(corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_REDFISH),
				ObservedAt: timestamppb.New(time.Date(2026, 10, 7, 18, 0, 2, 0, time.UTC)),
			},
			want: APIPowerObservation{Source: cutil.GetPtr(APIPowerSource("Redfish")), Error: cutil.GetPtr(APIPowerReadError("InvalidResponse"))},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := APIPowerObservation{}
			got.FromProto(&flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Value{Value: tc.observation}})
			assert.Equal(t, tc.want, got)
		})
	}
}

// TestAPIPowerPolicy_FromProto keeps future policy values unavailable rather
// than publishing a supported policy with a guessed state or availability.
func TestAPIPowerPolicy_FromProto(t *testing.T) {
	for _, tc := range []struct {
		name      string
		state     *corev1.PowerState
		status    corev1.ComponentPowerPolicyStatus
		wantError APIPowerReadError
	}{
		// A future policy value must not become the protobuf getter's default On.
		{name: "unknown policy state", state: cutil.GetPtr(corev1.PowerState(99)), status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE, wantError: "Unsupported"},
		// A known state is unusable when the service's availability status is unknown.
		{name: "unknown policy status", state: cutil.GetPtr(corev1.PowerState_On), status: corev1.ComponentPowerPolicyStatus(99), wantError: "Unsupported"},
		// Available requires an explicit state; omission is malformed, not disabled policy.
		{name: "available policy omits state", status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE, wantError: "InvalidResponse"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			p := testTrayPowerState().Desired
			p.GetValue().State, p.GetValue().Status = tc.state, tc.status

			got := APIPowerPolicy{}
			got.FromProto(p)
			assert.Equal(t, APIPowerPolicy{Status: "Unavailable", Error: cutil.GetPtr(tc.wantError)}, got)
		})
	}
}

// TestAPIPowerTaskWaitingContext_FromProto protects callers that reuse a context
// receiver: missing context replaces stale evidence with `Unsupported`.
func TestAPIPowerTaskWaitingContext_FromProto(t *testing.T) {
	for _, tc := range []struct {
		name  string
		input *flowv1.PowerTaskWaitingContext
	}{
		{name: "nil input reports unsupported context"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := APIPowerTaskWaitingContext{
				ActiveConflicts: []APITaskSummary{{ID: "old-active"}},
				EarlierWaiting:  []APITaskSummary{{ID: "old-waiting"}},
				Error:           cutil.GetPtr(APIPowerReadError("ReadFailed")),
			}
			got.FromProto(tc.input)
			assert.Equal(t, APIPowerTaskWaitingContext{
				ActiveConflicts: []APITaskSummary{}, EarlierWaiting: []APITaskSummary{}, Error: cutil.GetPtr(APIPowerReadError("Unsupported")),
			}, got)
		})
	}
}

// TestTrayPowerState_UnmarshalJSON keeps SDK nullability separate from requiredness.
func TestTrayPowerState_UnmarshalJSON(t *testing.T) {
	details := APITrayPowerState{}
	details.FromProto(testTrayPowerState())
	encoded, err := json.Marshal(details)
	require.NoError(t, err)

	for _, tc := range []struct {
		name   string
		field  string
		remove bool
	}{
		// Nullable identity still needs a key so absence cannot imply old API support.
		{name: "missing nullable identity", field: "id", remove: true},
		// A non-nullable field must retain its stronger value requirement.
		{name: "null component type", field: "type"},
	} {
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

			var decoded standard.TrayPowerState
			assert.ErrorContains(t, decoded.UnmarshalJSON(invalid), "required property "+tc.field)
		})
	}
}

// TestPowerTask_UnmarshalJSON requires the SDK to distinguish an omitted error
// field from the explicit null returned by a successful identity lookup.
func TestPowerTask_UnmarshalJSON(t *testing.T) {
	task := APIPowerTask{}
	task.FromProto(testTrayPowerState().Operations.Tasks.Entries[0])
	encoded, err := json.Marshal(task)
	require.NoError(t, err)

	for _, tc := range []struct {
		name  string
		field string
	}{
		{name: "missing nullable target error", field: "targetsError"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var fields map[string]any
			require.NoError(t, json.Unmarshal(encoded, &fields))
			delete(fields, tc.field)
			invalid, err := json.Marshal(fields)
			require.NoError(t, err)

			var decoded standard.PowerTask
			assert.ErrorContains(t, decoded.UnmarshalJSON(invalid), "required property "+tc.field)
		})
	}
}

// testTrayPowerState supplies independent hardware, policy, maintenance and
// task records; their different operations must never be inferred from each other.
func testTrayPowerState() *flowv1.TrayPowerDetails {
	// Distinct times make accidental reuse of another section's timestamp visible.
	actual := &corev1.ComponentPowerObservation{
		Outcome:    &corev1.ComponentPowerObservation_State{State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON},
		Source:     cutil.GetPtr(corev1.ComponentPowerSource_COMPONENT_POWER_SOURCE_REDFISH),
		ObservedAt: timestamppb.New(time.Date(2026, 10, 7, 18, 0, 2, 0, time.UTC)),
	}
	desired := &corev1.ComponentPowerPolicy{
		Status: corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE,
		State:  cutil.GetPtr(corev1.PowerState_PowerManagerDisabled),
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

	// The selected shelf affects this compute tray without selecting it. Mixed
	// types, repeated IDs and nonlexical external ID order must all survive.
	task := &flowv1.PowerTaskDetails{
		Id:     &flowv1.UUID{Id: "09684314-d3bd-4352-98a4-0321a517a32e"},
		RackId: &flowv1.UUID{Id: "a4bf94ec-656a-46ec-9411-0da8132d346e"},
		Operation: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{
			PowerControl: &flowv1.PowerControlTaskOperation{
				Operation:              flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_COLD_RESET,
				OverrideReadinessCheck: true,
			},
		}},
		Targets: &flowv1.ComponentsByType{Groups: []*flowv1.ComponentsForType{
			{Type: flowv1.ComponentType_COMPONENT_TYPE_CDU, ComponentIds: []*flowv1.UUID{{Id: "89276be0-443f-44f5-ab20-3641dd51ba02"}}},
			{Type: flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH, ComponentIds: []*flowv1.UUID{
				{Id: "13516535-7d14-4cbc-a6db-cad4f11cc2bc"},
				{Id: "269c0f83-a014-4e54-8e4b-f813e41604bd"},
				{Id: "393677de-03fd-4bc5-9ba3-b9a390fb93ba"},
			}},
			{Type: flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF, ComponentIds: []*flowv1.UUID{{Id: "438838f6-cbca-4c5d-8d70-098f741e1f47"}}},
		}},
		ExternalTargets: &flowv1.PowerTaskExternalTargets{
			RackId: "rack-inventory-id",
			Targets: []*flowv1.ExternalRef{
				{Type: flowv1.ComponentType_COMPONENT_TYPE_CDU, Id: "cdu-inventory-id"},
				{Type: flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH, Id: "z-switch"},
				{Type: flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH, Id: "a-switch"},
				{Type: flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH, Id: "a-switch"},
				{Type: flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF, Id: "z-switch"},
			},
		},
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
		Id:     &flowv1.UUID{Id: "458746fb-4124-465a-b522-ff2a9eb99d39"},
		RackId: &flowv1.UUID{Id: "a4bf94ec-656a-46ec-9411-0da8132d346e"},
		Operation: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{
			PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON},
		}},
		Targets: &flowv1.ComponentsByType{Groups: []*flowv1.ComponentsForType{{
			Type:         flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
			ComponentIds: []*flowv1.UUID{{Id: "550e8400-e29b-41d4-a716-446655440000"}},
		}}},
		ExternalTargets: &flowv1.PowerTaskExternalTargets{
			RackId:  "rack-inventory-id",
			Targets: []*flowv1.ExternalRef{{Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE, Id: "managed-compute"}},
		},
		Status:    flowv1.TaskStatus_TASK_STATUS_RUNNING,
		CreatedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 57, 0, 0, time.UTC)),
		UpdatedAt: timestamppb.New(time.Date(2026, 10, 7, 18, 0, 0, 0, time.UTC)),
		StartedAt: timestamppb.New(time.Date(2026, 10, 7, 17, 57, 30, 0, time.UTC)),
		Relevance: flowv1.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED,
	}
	return &flowv1.TrayPowerDetails{
		ComponentId:    cutil.GetPtr("managed-compute"),
		ComponentType:  flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
		RackId:         &flowv1.UUID{Id: "a4bf94ec-656a-46ec-9411-0da8132d346e"},
		RackExternalId: cutil.GetPtr("rack-inventory-id"),
		Actual:         &flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Value{Value: actual}},
		Desired:        &flowv1.PowerDesiredDetails{Result: &flowv1.PowerDesiredDetails_Value{Value: desired}},
		Operations: &flowv1.PowerOperations{
			Actions: &flowv1.PowerActionDetails{Result: &flowv1.PowerActionDetails_Value{Value: actions}},
			Tasks:   &flowv1.PowerTaskDetailsList{Entries: []*flowv1.PowerTaskDetails{task, running}},
		},
	}
}
