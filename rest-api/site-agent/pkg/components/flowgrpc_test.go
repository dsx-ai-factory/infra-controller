// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package elektra

import (
	"context"
	"testing"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/flowgrpc"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"go.opentelemetry.io/otel"
)

// TestRlaRack - test the Flow grpc client
func TestRlaRack(t *testing.T) {
	TestInitElektra(t)
	grpcClient := testElektra.manager.API.FlowGrpc.GetGrpcClient()

	var rack *flowv1.Rack

	tcs := []struct {
		descr     string
		expectErr bool
		index     int
	}{{
		descr:     "get",
		expectErr: false,
		index:     0,
	}, {
		descr:     "list",
		expectErr: false,
		index:     0,
	},
	}
	rpcSucc := 0
	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			switch tc.descr {
			case "get":
				rackID := uuid.NewString()
				ctx := context.Background()

				// First create the rack in mock server (setup, not counted in metrics)
				createReq := &flowv1.CreateExpectedRackRequest{
					Rack: &flowv1.Rack{
						Info: &flowv1.DeviceInfo{
							Id:   &flowv1.UUID{Id: rackID},
							Name: "test-rack",
						},
					},
				}
				_, createErr := grpcClient.GrpcServiceClient().CreateExpectedRack(ctx, createReq)
				assert.Nil(t, createErr)

				// Now test GetRackInfoByID
				ctx, span := otel.Tracer("flowgrpc-test").Start(ctx, "FlowTest-GetRack")

				getRequest := &flowv1.GetRackInfoByIDRequest{
					Id: &flowv1.UUID{Id: rackID},
				}

				response, err := grpcClient.GrpcServiceClient().GetRackInfoByID(ctx, getRequest)
				span.End()
				flowgrpc.ManagerAccess.API.FlowGrpc.UpdateGrpcClientState(err)
				if err != nil {
					t.Log(err.Error())
				}
				assert.Nil(t, err)
				assert.NotNil(t, response)
				assert.NotNil(t, response.Rack)
				assert.NotNil(t, response.Rack.Info)
				assert.Equal(t, rackID, response.Rack.Info.Id.Id)
				rpcSucc++
				assert.Equal(t, 0,
					int(flowgrpc.ManagerAccess.Data.EB.Managers.FlowGrpc.State.GrpcFail.Load()))
				assert.Equal(t, rpcSucc,
					int(flowgrpc.ManagerAccess.Data.EB.Managers.FlowGrpc.State.GrpcSucc.Load()))
				rack = response.Rack
				t.Log("GRPCResponse", response)
			case "list":
				ctx := context.Background()
				ctx, span := otel.Tracer("flowgrpc-test").Start(ctx, "FlowTest-GetListOfRacks")
				listRequest := &flowv1.GetListOfRacksRequest{}
				resq, err := grpcClient.GrpcServiceClient().GetListOfRacks(ctx, listRequest)
				span.End()
				flowgrpc.ManagerAccess.API.FlowGrpc.UpdateGrpcClientState(err)
				if err != nil {
					t.Log(err.Error())
				}
				assert.Nil(t, err)
				assert.NotNil(t, resq)
				assert.NotNil(t, resq.Racks)
				// Verify that the rack we got earlier is in the list
				if rack != nil && len(resq.Racks) > 0 {
					found := false
					for _, r := range resq.Racks {
						if r.Info != nil && r.Info.Id != nil && r.Info.Id.Id == rack.Info.Id.Id {
							found = true
							break
						}
					}
					assert.True(t, found, "Previously retrieved rack should be in the list")
				}
				rpcSucc++
				assert.Equal(t, 0,
					int(flowgrpc.ManagerAccess.Data.EB.Managers.FlowGrpc.State.GrpcFail.Load()))
				assert.Equal(t, rpcSucc,
					int(flowgrpc.ManagerAccess.Data.EB.Managers.FlowGrpc.State.GrpcSucc.Load()))
				t.Log("GRPCResponse", resq)
			default:
				panic("invalid operation name")
			}
		})
	}
}
