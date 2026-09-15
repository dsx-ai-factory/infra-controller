// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/middleware"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	grpcclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	swutil "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	sworkflow "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// These are deliberately recognizable test values. Assertions name the leaking
// surface without printing a request or a stored credential on failure.
const expectedPatchSecretPrefix = "patch-pw-"

type expectedPatchCoreComponent interface {
	proto.Message
	GetBmcUsername() string
	GetBmcPassword() string
	GetMetadata() *corev1.Metadata
}

type expectedPatchSnapshot struct {
	core        expectedPatchCoreComponent
	cloud       []byte
	cloudLabels cdbm.Labels
}

type expectedPatchCredentials struct {
	bmcUsername  string
	bmcPassword  string
	nvosUsername string
	nvosPassword string
}

func (s expectedPatchSnapshot) credentials() expectedPatchCredentials {
	credentials := expectedPatchCredentials{
		bmcUsername: s.core.GetBmcUsername(),
		bmcPassword: s.core.GetBmcPassword(),
	}
	switchRecord, ok := s.core.(*corev1.ExpectedSwitch)
	if ok {
		credentials.nvosUsername = switchRecord.GetNvosUsername()
		credentials.nvosPassword = switchRecord.GetNvosPassword()
	}
	return credentials
}

type expectedPatchFixture struct {
	id       uuid.UUID
	path     string
	handle   echo.HandlerFunc
	snapshot func(*testing.T) expectedPatchSnapshot
}

// The Rust integration test starts Core against an isolated PostgreSQL
// database, then invokes the Make target that selects this test. Temporal's
// test engine executes the production workflow and activity, including their
// data converter and secret decryption, before we reload both databases.
func TestExpectedComponentPatchCoreIntegration(t *testing.T) {
	address := os.Getenv("CORE_PATCH_TEST_ADDRESS")
	if address == "" {
		t.Skip("requires the Core test listener; run: make core/tests TEST_ARGS='-p carbide-api-core --test integration expected_component_rest_patch::test_expected_component_rest_patch -- --ignored --exact'")
	}

	coreConfig := &grpcclient.CoreGrpcClientConfig{Address: address, Secure: grpcclient.InsecureGrpc}
	coreClient, err := grpcclient.NewCoreGrpcClient(coreConfig)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, coreClient.Close()) })
	atomicClient := grpcclient.NewCoreGrpcAtomicClient(coreConfig)
	atomicClient.SwapClient(coreClient)

	dbSession := testExpectedMachineInitDB(t)
	t.Cleanup(dbSession.Close)
	for _, row := range []any{(*cdbm.ExpectedPowerShelf)(nil), (*cdbm.ExpectedSwitch)(nil)} {
		err = dbSession.DB.ResetModel(t.Context(), row)
		require.NoError(t, err)
	}
	const org = "expected-patch-integration"
	_, site := testExpectedMachineSetupTestData(t, dbSession, org)
	cfg := common.GetTestConfig()
	clientPool := sc.NewClientPool(nil)

	fixtures := make([]expectedPatchFixture, 0, 4)
	machineDAO := cdbm.NewExpectedMachineDAO(dbSession)
	for i := range 2 {
		username := fmt.Sprintf("machine-admin-%d", i)
		password := fmt.Sprintf("%smachine-%d", expectedPatchSecretPrefix, i)
		machine, createErr := machineDAO.Create(t.Context(), nil, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID:   uuid.New(),
			SiteID:              site.ID,
			BmcMacAddress:       fmt.Sprintf("02:00:59:40:00:%02x", i),
			ChassisSerialNumber: fmt.Sprintf("PATCH-MACHINE-%d", i),
			Labels:              map[string]string{"seed": "machine"},
		})
		require.NoError(t, createErr)
		expectedPatchInvokeCore(t, coreClient, corev1.Forge_AddExpectedMachine_FullMethodName,
			machine.ToProto(cdbm.ExpectedMachineCredentials{Username: &username, Password: &password}), &emptypb.Empty{})
		fixtures = append(fixtures, expectedPatchFixture{
			id:     machine.ID,
			path:   "expected-machine/" + machine.ID.String(),
			handle: NewUpdateExpectedMachineHandler(dbSession, clientPool, cfg).Handle,
			snapshot: func(t *testing.T) expectedPatchSnapshot {
				core := &corev1.ExpectedMachine{}
				expectedPatchInvokeCore(t, coreClient, corev1.Forge_GetExpectedMachine_FullMethodName,
					&corev1.ExpectedMachineRequest{Id: &corev1.UUID{Value: machine.ID.String()}}, core)
				cloud, readErr := machineDAO.Get(t.Context(), nil, machine.ID, nil, false)
				require.NoError(t, readErr)
				return expectedPatchSnapshot{core, expectedPatchJSON(t, cloud), cloud.Labels}
			},
		})
	}

	shelfDAO := cdbm.NewExpectedPowerShelfDAO(dbSession)
	shelf, err := shelfDAO.Create(t.Context(), nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(),
		SiteID:               site.ID,
		BmcMacAddress:        "02:00:59:40:01:00",
		ShelfSerialNumber:    "PATCH-SHELF",
		Labels:               map[string]string{"seed": "shelf"},
	})
	require.NoError(t, err)
	expectedPatchInvokeCore(t, coreClient, corev1.Forge_AddExpectedPowerShelf_FullMethodName,
		shelf.ToProto(cdbm.ExpectedPowerShelfCredentials{
			Username: cutil.GetPtr("shelf-admin"), Password: cutil.GetPtr(expectedPatchSecretPrefix + "shelf"),
		}), &emptypb.Empty{})
	fixtures = append(fixtures, expectedPatchFixture{
		id:     shelf.ID,
		path:   "expected-power-shelf/" + shelf.ID.String(),
		handle: NewUpdateExpectedPowerShelfHandler(dbSession, clientPool, cfg).Handle,
		snapshot: func(t *testing.T) expectedPatchSnapshot {
			core := &corev1.ExpectedPowerShelf{}
			expectedPatchInvokeCore(t, coreClient, corev1.Forge_GetExpectedPowerShelf_FullMethodName,
				&corev1.ExpectedPowerShelfRequest{ExpectedPowerShelfId: &corev1.UUID{Value: shelf.ID.String()}}, core)
			cloud, readErr := shelfDAO.Get(t.Context(), nil, shelf.ID, nil, false)
			require.NoError(t, readErr)
			return expectedPatchSnapshot{core, expectedPatchJSON(t, cloud), cloud.Labels}
		},
	})

	switchDAO := cdbm.NewExpectedSwitchDAO(dbSession)
	switchRecord, err := switchDAO.Create(t.Context(), nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID:   uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      "02:00:59:40:02:00",
		SwitchSerialNumber: "PATCH-SWITCH",
		Labels:             map[string]string{"seed": "switch"},
	})
	require.NoError(t, err)
	expectedPatchInvokeCore(t, coreClient, corev1.Forge_AddExpectedSwitch_FullMethodName,
		switchRecord.ToProto(cdbm.ExpectedSwitchCredentials{
			BmcUsername: cutil.GetPtr("switch-admin"), BmcPassword: cutil.GetPtr(expectedPatchSecretPrefix + "switch"),
			NvosUsername: cutil.GetPtr("nvos-admin"), NvosPassword: cutil.GetPtr(expectedPatchSecretPrefix + "nvos"),
		}), &emptypb.Empty{})
	fixtures = append(fixtures, expectedPatchFixture{
		id:     switchRecord.ID,
		path:   "expected-switch/" + switchRecord.ID.String(),
		handle: NewUpdateExpectedSwitchHandler(dbSession, clientPool, cfg).Handle,
		snapshot: func(t *testing.T) expectedPatchSnapshot {
			core := &corev1.ExpectedSwitch{}
			expectedPatchInvokeCore(t, coreClient, corev1.Forge_GetExpectedSwitch_FullMethodName,
				&corev1.ExpectedSwitchRequest{ExpectedSwitchId: &corev1.UUID{Value: switchRecord.ID.String()}}, core)
			cloud, readErr := switchDAO.Get(t.Context(), nil, switchRecord.ID, nil, false)
			require.NoError(t, readErr)
			return expectedPatchSnapshot{core, expectedPatchJSON(t, cloud), cloud.Labels}
		},
	})

	const batch = -1
	cases := []struct {
		name        string
		target      int
		body        string
		status      int
		labels      map[int]string
		credentials map[int]expectedPatchCredentials
	}{
		{
			name: "machine metadata preserves omitted BMC credentials", target: 0,
			body: `{"labels":{"patch":"machine"}}`, status: http.StatusOK,
			labels: map[int]string{0: "machine"},
		},
		{
			name: "shelf metadata preserves null BMC credentials", target: 2,
			body: `{"labels":{"patch":"shelf"},"defaultBmcUsername":null,"defaultBmcPassword":null}`, status: http.StatusOK,
			labels: map[int]string{2: "shelf"},
		},
		{
			name: "switch metadata preserves omitted BMC and null NVOS credentials", target: 3,
			body: `{"labels":{"patch":"switch"},"nvOsUsername":null,"nvOsPassword":null}`, status: http.StatusOK,
			labels: map[int]string{3: "switch"},
		},
		{
			name: "batch preserves distinct machine credentials", target: batch,
			body:   fmt.Sprintf(`[{"id":%q,"labels":{"patch":"batch-first"}},{"id":%q,"labels":{"patch":"batch-second"}}]`, fixtures[0].id, fixtures[1].id),
			status: http.StatusOK, labels: map[int]string{0: "batch-first", 1: "batch-second"},
		},
		{
			name: "switch BMC update preserves NVOS credentials", target: 3,
			body: `{"defaultBmcUsername":"new-switch-admin","defaultBmcPassword":"patch-pw-bmc2"}`, status: http.StatusOK,
			credentials: map[int]expectedPatchCredentials{3: {
				bmcUsername: "new-switch-admin", bmcPassword: expectedPatchSecretPrefix + "bmc2",
				nvosUsername: "nvos-admin", nvosPassword: expectedPatchSecretPrefix + "nvos",
			}},
		},
		{
			name: "switch NVOS update preserves BMC credentials", target: 3,
			body: `{"nvOsUsername":"new-nvos-admin","nvOsPassword":"patch-pw-nvos2"}`, status: http.StatusOK,
			credentials: map[int]expectedPatchCredentials{3: {
				bmcUsername: "new-switch-admin", bmcPassword: expectedPatchSecretPrefix + "bmc2",
				nvosUsername: "new-nvos-admin", nvosPassword: expectedPatchSecretPrefix + "nvos2",
			}},
		},
		{
			name: "partial machine pair rejects accompanying metadata", target: 0,
			body: `{"labels":{"patch":"rejected"},"defaultBmcUsername":"incomplete"}`, status: http.StatusBadRequest,
		},
		{
			name: "partial shelf pair rejects accompanying metadata", target: 2,
			body: `{"labels":{"patch":"rejected"},"defaultBmcUsername":"incomplete"}`, status: http.StatusBadRequest,
		},
		{
			name: "partial switch BMC pair rejects accompanying metadata", target: 3,
			body: `{"labels":{"patch":"rejected"},"defaultBmcUsername":"incomplete"}`, status: http.StatusBadRequest,
		},
		{
			name: "partial switch NVOS pair rejects accompanying metadata", target: 3,
			body: `{"labels":{"patch":"rejected"},"nvOsUsername":"incomplete"}`, status: http.StatusBadRequest,
		},
		{
			name: "partial batch pairs reject all accompanying metadata", target: batch,
			body:   fmt.Sprintf(`[{"id":%q,"labels":{"patch":"rejected"},"defaultBmcUsername":"incomplete"},{"id":%q,"labels":{"patch":"rejected"},"defaultBmcUsername":"incomplete"}]`, fixtures[0].id, fixtures[1].id),
			status: http.StatusBadRequest,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var logs bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(zerolog.SyncWriter(&logs))
			t.Cleanup(func() { log.Logger = previousLogger })

			proxyClient, dispatches := expectedPatchTemporalClient(t, atomicClient, site.ID.String())
			clientPool.IDClientMap[site.ID.String()] = proxyClient
			before := make([]expectedPatchSnapshot, len(fixtures))
			for i, fixture := range fixtures {
				before[i] = fixture.snapshot(t)
			}
			path := "expected-machine/batch"
			handle := NewUpdateExpectedMachinesHandler(dbSession, clientPool, cfg).Handle
			id := "batch"
			if tc.target != batch {
				fixture := fixtures[tc.target]
				path, handle, id = fixture.path, fixture.handle, fixture.id.String()
			}
			request := httptest.NewRequest(http.MethodPatch, "/v2/org/"+org+"/nico/"+path, strings.NewReader(tc.body))
			request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			recorder := httptest.NewRecorder()
			c := echo.New().NewContext(request, recorder)
			c.Set("user", &cdbm.User{
				StarfleetID: cutil.GetPtr("test-user"),
				OrgData:     cdbm.OrgData{org: cdbm.Org{ID: 123, Name: org, OrgType: "ENTERPRISE", Roles: []string{"FORGE_PROVIDER_ADMIN"}}},
			})
			c.SetParamNames("orgName", "id")
			c.SetParamValues(org, id)
			err = middleware.Logger()(handle)(c)
			require.NoError(t, err)
			require.Equal(t, tc.status, recorder.Code)
			expectedPatchNoSecret(t, recorder.Body.Bytes(), "REST response")
			expectedPatchNoSecret(t, logs.Bytes(), "REST and site proxy logs")
			require.True(t, bytes.Contains(logs.Bytes(), []byte("started API handler")), "REST handler logs were not captured")
			require.True(t, bytes.Contains(logs.Bytes(), []byte("HTTP request")), "REST request middleware logs were not captured")
			if tc.status == http.StatusBadRequest {
				require.Zero(t, *dispatches, "invalid credentials must be rejected before dispatch")
			} else {
				require.Equal(t, 1, *dispatches, "one PATCH must reach the real Core service")
				require.True(t, bytes.Contains(logs.Bytes(), []byte(`"Workflow":"InvokeCoreGRPC"`)), "production workflow logs were not captured")
				require.True(t, bytes.Contains(logs.Bytes(), []byte(`"Activity":"InvokeCoreGRPCOnSite"`)), "production activity logs were not captured")
			}

			for i, fixture := range fixtures {
				after := fixture.snapshot(t)
				affected := tc.target == i || (tc.target == batch && i < 2)
				if tc.status == http.StatusBadRequest || !affected {
					require.True(t, proto.Equal(before[i].core, after.core), "Core record %d changed unexpectedly", i)
					require.True(t, bytes.Equal(before[i].cloud, after.cloud), "Cloud record %d changed unexpectedly", i)
					continue
				}
				credentials := before[i].credentials()
				replacement, provided := tc.credentials[i]
				if provided {
					credentials = replacement
				}
				require.True(t, credentials == after.credentials(), "Core credential preservation failed for record %d", i)
				labels := before[i].cloudLabels
				label, provided := tc.labels[i]
				if provided {
					labels = cdbm.Labels{"patch": label}
				}
				var coreLabels cdbm.Labels
				coreLabels.FromProto(after.core.GetMetadata().GetLabels())
				require.Equal(t, labels, coreLabels, "Core must persist the requested metadata")
				require.Equal(t, labels, after.cloudLabels, "Cloud must persist the requested metadata")
			}
		})
	}
}

func expectedPatchInvokeCore(t *testing.T, client *grpcclient.CoreGrpcClient, method string, request, response proto.Message) {
	t.Helper()
	requestJSON, err := protojson.Marshal(request)
	require.NoError(t, err)
	ctx, cancel := context.WithTimeout(t.Context(), 20*time.Second)
	defer cancel()
	responseJSON, err := client.InvokeJSON(ctx, method, requestJSON)
	require.NoError(t, err)
	err = protojson.Unmarshal(responseJSON, response)
	require.NoError(t, err)
}

func expectedPatchJSON(t *testing.T, value any) []byte {
	t.Helper()
	encoded, err := json.Marshal(value)
	require.NoError(t, err)
	return encoded
}

func expectedPatchNoSecret(t *testing.T, value []byte, surface string) {
	t.Helper()
	require.False(t, bytes.Contains(value, []byte(expectedPatchSecretPrefix)), "%s contains a plaintext password", surface)
}

// Only Temporal's network client is replaced. The workflow and activity run
// their production implementations, and the activity calls the Rust server.
func expectedPatchTemporalClient(t *testing.T, client *grpcclient.CoreGrpcAtomicClient, siteID string) (*tmocks.Client, *int) {
	t.Helper()
	manager := activity.NewManageCoreProxy(client, siteID)
	dataConverter := swutil.NewTemporalDataConverter()
	var result grpcproxy.Response
	var workflowErr error
	dispatches := 0
	run := &tmocks.WorkflowRun{}
	run.On("Get", mock.Anything, mock.Anything).Return(func(_ context.Context, value any) error {
		if workflowErr != nil {
			return workflowErr
		}
		*value.(*grpcproxy.Response) = result
		return nil
	})
	clientMock := &tmocks.Client{}
	// Return errors through the client so the handler rolls back before a
	// test assertion can stop the subtest.
	clientMock.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Return(func(_ context.Context, _ tclient.StartWorkflowOptions, _ any, args ...any) (tclient.WorkflowRun, error) {
			dispatches++
			request := args[0].(grpcproxy.Request)
			payloads, err := dataConverter.ToPayloads(request)
			if err != nil {
				return nil, fmt.Errorf("encode Temporal workflow request: %w", err)
			}
			for _, payload := range payloads.Payloads {
				if bytes.Contains(payload.Data, []byte(expectedPatchSecretPrefix)) {
					return nil, errors.New("temporal workflow request contains a plaintext password")
				}
			}
			var suite testsuite.WorkflowTestSuite
			env := suite.NewTestWorkflowEnvironment()
			env.SetDataConverter(dataConverter)
			env.SetTestTimeout(time.Minute)
			env.RegisterWorkflow(sworkflow.InvokeCoreGRPC)
			env.RegisterActivity(manager.InvokeCoreGRPCOnSite)
			env.ExecuteWorkflow(grpcproxy.Core.WorkflowName, request)
			if !env.IsWorkflowCompleted() {
				return nil, errors.New("production proxy workflow did not complete")
			}
			workflowErr = env.GetWorkflowError()
			if workflowErr != nil {
				return run, nil
			}
			err = env.GetWorkflowResult(&result)
			if err != nil {
				return nil, fmt.Errorf("decode production proxy workflow result: %w", err)
			}
			payloads, err = dataConverter.ToPayloads(result)
			if err != nil {
				return nil, fmt.Errorf("encode Temporal workflow result: %w", err)
			}
			for _, payload := range payloads.Payloads {
				if bytes.Contains(payload.Data, []byte(expectedPatchSecretPrefix)) {
					return nil, errors.New("temporal workflow result contains a plaintext password")
				}
			}
			return run, nil
		})
	return clientMock, &dispatches
}
