// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package client provides a gRPC client for interacting with the Flow service.
// This package can be imported by external modules to communicate with Flow.
//
// The client uses types from pkg/types, which can be imported independently
// for interface definitions and mocking without gRPC dependencies.
package client

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"
	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/types/known/timestamppb"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// Client is the gRPC client for interacting with the Flow service.
type Client struct {
	client pb.FlowClient
	conn   *grpc.ClientConn
}

// New creates a new Flow gRPC client. If CertConfig is set, the connection uses
// mTLS; otherwise it falls back to insecure (plaintext) transport.
func New(c Config) (*Client, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}

	var creds credentials.TransportCredentials
	if c.CertConfig.IsSet() {
		tlsConfig, err := c.CertConfig.TLSConfig(c.ServerName)
		if err != nil {
			return nil, fmt.Errorf("failed to build TLS config: %w", err)
		}
		creds = credentials.NewTLS(tlsConfig)
	} else {
		creds = insecure.NewCredentials()
	}

	dialOptions := []grpc.DialOption{grpc.WithTransportCredentials(creds)}
	if cotel.TransportEnabled() {
		dialOptions = append(dialOptions, grpc.WithStatsHandler(otelgrpc.NewClientHandler()))
	}

	conn, err := grpc.NewClient(c.Target(), dialOptions...)
	if err != nil {
		return nil, err
	}

	return &Client{
		conn:   conn,
		client: pb.NewFlowClient(conn),
	}, nil
}

// Close closes the gRPC connection
func (c *Client) Close() error {
	return c.conn.Close()
}

// CreateExpectedRack creates a new expected rack and returns its UUID.
func (c *Client) CreateExpectedRack(
	ctx context.Context,
	rack *types.Rack,
) (uuid.UUID, error) {
	rsp, err := c.client.CreateExpectedRack(
		ctx,
		&pb.CreateExpectedRackRequest{
			Rack: rackToProto(rack),
		},
	)

	if err != nil {
		return uuid.Nil, err
	}

	return uuidFromProto(rsp.GetId()), nil
}

// GetRackInfoByID retrieves rack information by its UUID.
func (c *Client) GetRackInfoByID(
	ctx context.Context,
	id uuid.UUID,
	withComponents bool,
) (*types.Rack, error) {
	rsp, err := c.client.GetRackInfoByID(
		ctx,
		&pb.GetRackInfoByIDRequest{
			Id:             uuidToProto(id),
			WithComponents: withComponents,
		},
	)

	if err != nil {
		return nil, err
	}

	return rackFromProto(rsp.Rack), nil
}

// GetRackInfoBySerial retrieves rack information by its manufacturer and
// serial number.
func (c *Client) GetRackInfoBySerial(
	ctx context.Context,
	manufacturer string,
	serial string,
	withComponents bool,
) (*types.Rack, error) {
	rsp, err := c.client.GetRackInfoBySerial(
		ctx,
		&pb.GetRackInfoBySerialRequest{
			SerialInfo: &pb.DeviceSerialInfo{
				Manufacturer: manufacturer,
				SerialNumber: serial,
			},
			WithComponents: withComponents,
		},
	)

	if err != nil {
		return nil, err
	}

	return rackFromProto(rsp.Rack), nil
}

// GetComponentInfoByID retrieves component information by its UUID.
func (c *Client) GetComponentInfoByID(
	ctx context.Context,
	id uuid.UUID,
	withRack bool,
) (*types.Component, *types.Rack, error) {
	rsp, err := c.client.GetComponentInfoByID(
		ctx,
		&pb.GetComponentInfoByIDRequest{
			Id:       uuidToProto(id),
			WithRack: withRack,
		},
	)

	if err != nil {
		return nil, nil, err
	}

	return componentFromProto(rsp.Component), rackFromProto(rsp.Rack), nil
}

// GetComponentInfoBySerial retrieves component information by its
// manufacturer and serial number.
func (c *Client) GetComponentInfoBySerial(
	ctx context.Context,
	manufacturer string,
	serial string,
	withRack bool,
) (*types.Component, *types.Rack, error) {
	rsp, err := c.client.GetComponentInfoBySerial(
		ctx,
		&pb.GetComponentInfoBySerialRequest{
			SerialInfo: &pb.DeviceSerialInfo{
				Manufacturer: manufacturer,
				SerialNumber: serial,
			},
			WithRack: withRack,
		},
	)

	if err != nil {
		return nil, nil, err
	}

	return componentFromProto(rsp.Component), rackFromProto(rsp.Rack), nil
}

// GetListOfRacks retrieves a list of racks matching the query.
func (c *Client) GetListOfRacks(
	ctx context.Context,
	info *types.StringQueryInfo,
	pagination *types.Pagination,
	withComponents bool,
) ([]*types.Rack, int32, error) {
	filters := make([]*pb.Filter, 0)
	if info != nil {
		filters = append(filters, &pb.Filter{
			Field: &pb.Filter_RackField{
				RackField: pb.RackFilterField_RACK_FILTER_FIELD_NAME,
			},
			QueryInfo: stringQueryInfoToProto(info),
		})
	}

	rsp, err := c.client.GetListOfRacks(
		ctx,
		&pb.GetListOfRacksRequest{
			Filters:        filters,
			Pagination:     paginationToProto(pagination),
			WithComponents: withComponents,
		},
	)

	if err != nil {
		return nil, 0, err
	}

	results := make([]*types.Rack, 0, len(rsp.Racks))
	for _, rack := range rsp.Racks {
		results = append(results, rackFromProto(rack))
	}

	return results, rsp.Total, nil
}

// CreateNVLDomain creates a new NVL domain and returns its UUID.
func (c *Client) CreateNVLDomain(
	ctx context.Context,
	nvlDomain *types.NVLDomain,
) (uuid.UUID, error) {
	rsp, err := c.client.CreateNVLDomain(
		ctx,
		&pb.CreateNVLDomainRequest{NvlDomain: nvlDomainToProto(nvlDomain)},
	)

	if err != nil {
		return uuid.Nil, err
	}

	return uuidFromProto(rsp.Id), nil
}

// AttachRacksToNVLDomain attaches racks to an NVL domain.
func (c *Client) AttachRacksToNVLDomain(
	ctx context.Context,
	nvlDomainID types.Identifier,
	rackIDs []types.Identifier,
) error {
	pbRackIDs := make([]*pb.Identifier, 0, len(rackIDs))
	for _, rackID := range rackIDs {
		pbRackIDs = append(pbRackIDs, identifierToProto(&rackID))
	}

	_, err := c.client.AttachRacksToNVLDomain(
		ctx,
		&pb.AttachRacksToNVLDomainRequest{
			NvlDomainIdentifier: identifierToProto(&nvlDomainID),
			RackIdentifiers:     pbRackIDs,
		},
	)

	return err
}

// DetachRacksFromNVLDomain detaches racks from their NVL domain.
func (c *Client) DetachRacksFromNVLDomain(
	ctx context.Context,
	rackIDs []types.Identifier,
) error {
	pbRackIDs := make([]*pb.Identifier, 0, len(rackIDs))
	for _, rackID := range rackIDs {
		pbRackIDs = append(pbRackIDs, identifierToProto(&rackID))
	}

	_, err := c.client.DetachRacksFromNVLDomain(
		ctx,
		&pb.DetachRacksFromNVLDomainRequest{RackIdentifiers: pbRackIDs},
	)

	return err
}

// GetListOfNVLDomains retrieves a list of NVL domains matching the query.
func (c *Client) GetListOfNVLDomains(
	ctx context.Context,
	info *types.StringQueryInfo,
	pagination *types.Pagination,
) ([]*types.NVLDomain, int32, error) {
	rsp, err := c.client.GetListOfNVLDomains(
		ctx,
		&pb.GetListOfNVLDomainsRequest{
			Info:       stringQueryInfoToProto(info),
			Pagination: paginationToProto(pagination),
		},
	)

	if err != nil {
		return nil, 0, err
	}

	results := make([]*types.NVLDomain, 0, len(rsp.NvlDomains))
	for _, nvlDomain := range rsp.NvlDomains {
		results = append(results, nvlDomainFromProto(nvlDomain))
	}

	return results, rsp.Total, nil
}

// GetRacksForNVLDomain retrieves racks belonging to an NVL domain.
func (c *Client) GetRacksForNVLDomain(
	ctx context.Context,
	nvlDomainID types.Identifier,
) ([]*types.Rack, error) {
	rsp, err := c.client.GetRacksForNVLDomain(
		ctx,
		&pb.GetRacksForNVLDomainRequest{
			NvlDomainIdentifier: identifierToProto(&nvlDomainID),
		},
	)

	if err != nil {
		return nil, err
	}

	results := make([]*types.Rack, 0, len(rsp.Racks))
	for _, rack := range rsp.Racks {
		results = append(results, rackFromProto(rack))
	}

	return results, nil
}

// UpgradeFirmwareByRackIDs upgrades firmware for components in the given rack IDs.
func (c *Client) UpgradeFirmwareByRackIDs(
	ctx context.Context,
	rackIDs []uuid.UUID,
	componentType types.ComponentType,
	startTime, endTime *time.Time,
) (*UpgradeFirmwareResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackIDs))
	for _, id := range rackIDs {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Id{Id: uuidToProto(id)},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	req := &pb.UpgradeFirmwareRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{Targets: rackTargets},
			},
		},
	}
	if startTime != nil {
		req.StartTime = timestamppb.New(*startTime)
	}
	if endTime != nil {
		req.EndTime = timestamppb.New(*endTime)
	}

	rsp, err := c.client.UpgradeFirmware(ctx, req)
	if err != nil {
		return nil, err
	}

	return &UpgradeFirmwareResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}

// UpgradeFirmwareByRackNames upgrades firmware for components in the given rack names.
func (c *Client) UpgradeFirmwareByRackNames(
	ctx context.Context,
	rackNames []string,
	componentType types.ComponentType,
	startTime, endTime *time.Time,
) (*UpgradeFirmwareResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackNames))
	for _, name := range rackNames {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Name{Name: name},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	req := &pb.UpgradeFirmwareRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{Targets: rackTargets},
			},
		},
	}
	if startTime != nil {
		req.StartTime = timestamppb.New(*startTime)
	}
	if endTime != nil {
		req.EndTime = timestamppb.New(*endTime)
	}

	rsp, err := c.client.UpgradeFirmware(ctx, req)
	if err != nil {
		return nil, err
	}

	return &UpgradeFirmwareResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}

// UpgradeFirmwareByNVLDomainIDs upgrades firmware for components in the given NVLink domains.
func (c *Client) UpgradeFirmwareByNVLDomainIDs(
	ctx context.Context,
	domainIDs []uuid.UUID,
	componentType types.ComponentType,
	startTime, endTime *time.Time,
) (*UpgradeFirmwareResult, error) {
	targets := make([]*pb.NVLDomainTarget, 0, len(domainIDs))
	for _, id := range domainIDs {
		targets = append(targets, &pb.NVLDomainTarget{
			Identifier:     &pb.NVLDomainTarget_Id{Id: uuidToProto(id)},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	return c.upgradeFirmwareByNVLDomains(ctx, targets, startTime, endTime)
}

// UpgradeFirmwareByNVLDomainNames upgrades firmware for components in the given NVLink domains.
func (c *Client) UpgradeFirmwareByNVLDomainNames(
	ctx context.Context,
	domainNames []string,
	componentType types.ComponentType,
	startTime, endTime *time.Time,
) (*UpgradeFirmwareResult, error) {
	targets := make([]*pb.NVLDomainTarget, 0, len(domainNames))
	for _, name := range domainNames {
		targets = append(targets, &pb.NVLDomainTarget{
			Identifier:     &pb.NVLDomainTarget_Name{Name: name},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	return c.upgradeFirmwareByNVLDomains(ctx, targets, startTime, endTime)
}

func (c *Client) upgradeFirmwareByNVLDomains(
	ctx context.Context,
	targets []*pb.NVLDomainTarget,
	startTime, endTime *time.Time,
) (*UpgradeFirmwareResult, error) {
	req := &pb.UpgradeFirmwareRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_NvlDomains{
				NvlDomains: &pb.NVLDomainTargets{Targets: targets},
			},
		},
	}
	if startTime != nil {
		req.StartTime = timestamppb.New(*startTime)
	}
	if endTime != nil {
		req.EndTime = timestamppb.New(*endTime)
	}

	rsp, err := c.client.UpgradeFirmware(ctx, req)
	if err != nil {
		return nil, err
	}

	return &UpgradeFirmwareResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}

// UpgradeFirmwareByMachineIDs upgrades firmware for the given machine IDs (external component IDs).
func (c *Client) UpgradeFirmwareByMachineIDs(
	ctx context.Context,
	machineIDs []string,
	startTime, endTime *time.Time,
) (*UpgradeFirmwareResult, error) {
	compTargets := make([]*pb.ComponentTarget, 0, len(machineIDs))
	for _, machineID := range machineIDs {
		compTargets = append(compTargets, &pb.ComponentTarget{
			Identifier: &pb.ComponentTarget_External{
				External: &pb.ExternalRef{
					Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
					Id:   machineID,
				},
			},
		})
	}

	req := &pb.UpgradeFirmwareRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Components{
				Components: &pb.ComponentTargets{Targets: compTargets},
			},
		},
	}
	if startTime != nil {
		req.StartTime = timestamppb.New(*startTime)
	}
	if endTime != nil {
		req.EndTime = timestamppb.New(*endTime)
	}

	rsp, err := c.client.UpgradeFirmware(ctx, req)
	if err != nil {
		return nil, err
	}

	return &UpgradeFirmwareResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}

// PowerControlByRackIDs performs power control on components in the given rack IDs.
func (c *Client) PowerControlByRackIDs(
	ctx context.Context,
	rackIDs []uuid.UUID,
	componentType types.ComponentType,
	op types.PowerControlOp,
) (*PowerControlResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackIDs))
	for _, id := range rackIDs {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Id{Id: uuidToProto(id)},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	targetSpec := &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Racks{
			Racks: &pb.RackTargets{Targets: rackTargets},
		},
	}

	return c.executePowerControl(ctx, targetSpec, op)
}

// PowerControlByRackNames performs power control on components in the given rack names.
func (c *Client) PowerControlByRackNames(
	ctx context.Context,
	rackNames []string,
	componentType types.ComponentType,
	op types.PowerControlOp,
) (*PowerControlResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackNames))
	for _, name := range rackNames {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Name{Name: name},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	targetSpec := &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Racks{
			Racks: &pb.RackTargets{Targets: rackTargets},
		},
	}

	return c.executePowerControl(ctx, targetSpec, op)
}

// PowerControlByNVLDomainIDs performs power control in the given NVLink domains.
func (c *Client) PowerControlByNVLDomainIDs(
	ctx context.Context,
	domainIDs []uuid.UUID,
	componentType types.ComponentType,
	op types.PowerControlOp,
) (*PowerControlResult, error) {
	targets := make([]*pb.NVLDomainTarget, 0, len(domainIDs))
	for _, id := range domainIDs {
		targets = append(targets, &pb.NVLDomainTarget{
			Identifier:     &pb.NVLDomainTarget_Id{Id: uuidToProto(id)},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	return c.executePowerControl(ctx, nvlDomainTargetSpec(targets), op)
}

// PowerControlByNVLDomainNames performs power control in the given NVLink domains.
func (c *Client) PowerControlByNVLDomainNames(
	ctx context.Context,
	domainNames []string,
	componentType types.ComponentType,
	op types.PowerControlOp,
) (*PowerControlResult, error) {
	targets := make([]*pb.NVLDomainTarget, 0, len(domainNames))
	for _, name := range domainNames {
		targets = append(targets, &pb.NVLDomainTarget{
			Identifier:     &pb.NVLDomainTarget_Name{Name: name},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	return c.executePowerControl(ctx, nvlDomainTargetSpec(targets), op)
}

func nvlDomainTargetSpec(targets []*pb.NVLDomainTarget) *pb.OperationTargetSpec {
	return &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_NvlDomains{
			NvlDomains: &pb.NVLDomainTargets{Targets: targets},
		},
	}
}

// PowerControlByMachineIDs performs power control on the given machine IDs.
func (c *Client) PowerControlByMachineIDs(
	ctx context.Context,
	machineIDs []string,
	op types.PowerControlOp,
) (*PowerControlResult, error) {
	compTargets := make([]*pb.ComponentTarget, 0, len(machineIDs))
	for _, machineID := range machineIDs {
		compTargets = append(compTargets, &pb.ComponentTarget{
			Identifier: &pb.ComponentTarget_External{
				External: &pb.ExternalRef{
					Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
					Id:   machineID,
				},
			},
		})
	}

	targetSpec := &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Components{
			Components: &pb.ComponentTargets{Targets: compTargets},
		},
	}

	return c.executePowerControl(ctx, targetSpec, op)
}

// executePowerControl executes a power control operation with the given target spec.
func (c *Client) executePowerControl(
	ctx context.Context,
	targetSpec *pb.OperationTargetSpec,
	op types.PowerControlOp,
) (*PowerControlResult, error) {
	var rsp *pb.SubmitTaskResponse
	var err error

	pbOp := powerControlOpToProto(op)

	switch pbOp {
	case pb.PowerControlOp_POWER_CONTROL_OP_ON, pb.PowerControlOp_POWER_CONTROL_OP_FORCE_ON:
		rsp, err = c.client.PowerOnRack(ctx, &pb.PowerOnRackRequest{
			TargetSpec: targetSpec,
		})

	case pb.PowerControlOp_POWER_CONTROL_OP_OFF:
		rsp, err = c.client.PowerOffRack(ctx, &pb.PowerOffRackRequest{
			TargetSpec: targetSpec,
			Forced:     false,
		})

	case pb.PowerControlOp_POWER_CONTROL_OP_FORCE_OFF:
		rsp, err = c.client.PowerOffRack(ctx, &pb.PowerOffRackRequest{
			TargetSpec: targetSpec,
			Forced:     true,
		})

	case pb.PowerControlOp_POWER_CONTROL_OP_RESTART, pb.PowerControlOp_POWER_CONTROL_OP_WARM_RESET:
		rsp, err = c.client.PowerResetRack(ctx, &pb.PowerResetRackRequest{
			TargetSpec: targetSpec,
			Forced:     false,
		})

	case pb.PowerControlOp_POWER_CONTROL_OP_FORCE_RESTART, pb.PowerControlOp_POWER_CONTROL_OP_COLD_RESET:
		rsp, err = c.client.PowerResetRack(ctx, &pb.PowerResetRackRequest{
			TargetSpec: targetSpec,
			Forced:     true,
		})

	default:
		return nil, fmt.Errorf("unsupported power control operation: %v", op)
	}

	if err != nil {
		return nil, err
	}

	return &PowerControlResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}

// GetExpectedComponentsByRackIDs retrieves expected components from local database by rack IDs.
func (c *Client) GetExpectedComponentsByRackIDs(
	ctx context.Context,
	rackIDs []uuid.UUID,
	componentType types.ComponentType,
) (*GetExpectedComponentsResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackIDs))
	for _, id := range rackIDs {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Id{Id: uuidToProto(id)},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	// Build filters array
	filters := make([]*pb.Filter, 0)
	if componentType != types.ComponentTypeUnknown {
		// Convert ComponentType enum to string
		typeStr := componentTypeToString(componentType)
		filters = append(filters, &pb.Filter{
			Field: &pb.Filter_ComponentField{
				ComponentField: pb.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE,
			},
			QueryInfo: &pb.StringQueryInfo{
				Patterns:   []string{typeStr},
				IsWildcard: false,
				UseOr:      false,
			},
		})
	}

	rsp, err := c.client.GetComponents(
		ctx,
		&pb.GetComponentsRequest{
			TargetSpec: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_Racks{
					Racks: &pb.RackTargets{Targets: rackTargets},
				},
			},
			Filters: filters,
		},
	)
	if err != nil {
		return nil, err
	}

	return convertGetComponentsResponse(rsp), nil
}

// GetExpectedComponentsByRackNames retrieves expected components from local database by rack names.
func (c *Client) GetExpectedComponentsByRackNames(
	ctx context.Context,
	rackNames []string,
	componentType types.ComponentType,
) (*GetExpectedComponentsResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackNames))
	for _, name := range rackNames {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Name{Name: name},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	// Build filters array
	filters := make([]*pb.Filter, 0)
	if componentType != types.ComponentTypeUnknown {
		// Convert ComponentType enum to string
		typeStr := componentTypeToString(componentType)
		filters = append(filters, &pb.Filter{
			Field: &pb.Filter_ComponentField{
				ComponentField: pb.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE,
			},
			QueryInfo: &pb.StringQueryInfo{
				Patterns:   []string{typeStr},
				IsWildcard: false,
				UseOr:      false,
			},
		})
	}

	rsp, err := c.client.GetComponents(
		ctx,
		&pb.GetComponentsRequest{
			TargetSpec: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_Racks{
					Racks: &pb.RackTargets{Targets: rackTargets},
				},
			},
			Filters: filters,
		},
	)
	if err != nil {
		return nil, err
	}

	return convertGetComponentsResponse(rsp), nil
}

// GetExpectedComponentsByComponentIDs retrieves expected components by external component IDs.
func (c *Client) GetExpectedComponentsByComponentIDs(
	ctx context.Context,
	componentIDs []string,
	componentType types.ComponentType,
) (*GetExpectedComponentsResult, error) {
	compTargets := make([]*pb.ComponentTarget, 0, len(componentIDs))
	for _, compID := range componentIDs {
		compTargets = append(compTargets, &pb.ComponentTarget{
			Identifier: &pb.ComponentTarget_External{
				External: &pb.ExternalRef{
					Type: componentTypeToProto(componentType),
					Id:   compID,
				},
			},
		})
	}

	// Build filters array
	filters := make([]*pb.Filter, 0)
	if componentType != types.ComponentTypeUnknown {
		// Convert ComponentType enum to string
		typeStr := componentTypeToString(componentType)
		filters = append(filters, &pb.Filter{
			Field: &pb.Filter_ComponentField{
				ComponentField: pb.ComponentFilterField_COMPONENT_FILTER_FIELD_TYPE,
			},
			QueryInfo: &pb.StringQueryInfo{
				Patterns:   []string{typeStr},
				IsWildcard: false,
				UseOr:      false,
			},
		})
	}

	rsp, err := c.client.GetComponents(
		ctx,
		&pb.GetComponentsRequest{
			TargetSpec: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_Components{
					Components: &pb.ComponentTargets{Targets: compTargets},
				},
			},
			Filters: filters,
		},
	)
	if err != nil {
		return nil, err
	}

	return convertGetComponentsResponse(rsp), nil
}

// convertGetComponentsResponse converts a protobuf GetComponentsResponse into
// a GetExpectedComponentsResult.
func convertGetComponentsResponse(rsp *pb.GetComponentsResponse) *GetExpectedComponentsResult {
	components := make([]*types.Component, 0, len(rsp.Components))
	for _, c := range rsp.Components {
		components = append(components, componentFromProto(c))
	}

	return &GetExpectedComponentsResult{
		Components: components,
		Total:      int(rsp.Total),
	}
}

// ValidateComponentsByRackIDs validates expected vs actual components by rack IDs.
func (c *Client) ValidateComponentsByRackIDs(
	ctx context.Context,
	rackIDs []uuid.UUID,
	componentType types.ComponentType,
) (*ValidateComponentsResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackIDs))
	for _, id := range rackIDs {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Id{Id: uuidToProto(id)},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	rsp, err := c.client.ValidateComponents(
		ctx,
		&pb.ValidateComponentsRequest{
			TargetSpec: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_Racks{
					Racks: &pb.RackTargets{Targets: rackTargets},
				},
			},
		},
	)
	if err != nil {
		return nil, err
	}

	return convertValidateComponentsResponse(rsp), nil
}

// ValidateComponentsByRackNames validates expected vs actual components by rack names.
func (c *Client) ValidateComponentsByRackNames(
	ctx context.Context,
	rackNames []string,
	componentType types.ComponentType,
) (*ValidateComponentsResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackNames))
	for _, name := range rackNames {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier:     &pb.RackTarget_Name{Name: name},
			ComponentTypes: componentTypesFilter(componentType),
		})
	}

	rsp, err := c.client.ValidateComponents(
		ctx,
		&pb.ValidateComponentsRequest{
			TargetSpec: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_Racks{
					Racks: &pb.RackTargets{Targets: rackTargets},
				},
			},
		},
	)
	if err != nil {
		return nil, err
	}

	return convertValidateComponentsResponse(rsp), nil
}

// ValidateComponentsByComponentIDs validates expected vs actual components by external component IDs.
func (c *Client) ValidateComponentsByComponentIDs(
	ctx context.Context,
	componentIDs []string,
	componentType types.ComponentType,
) (*ValidateComponentsResult, error) {
	compTargets := make([]*pb.ComponentTarget, 0, len(componentIDs))
	for _, compID := range componentIDs {
		compTargets = append(compTargets, &pb.ComponentTarget{
			Identifier: &pb.ComponentTarget_External{
				External: &pb.ExternalRef{
					Type: componentTypeToProto(componentType),
					Id:   compID,
				},
			},
		})
	}

	rsp, err := c.client.ValidateComponents(
		ctx,
		&pb.ValidateComponentsRequest{
			TargetSpec: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_Components{
					Components: &pb.ComponentTargets{Targets: compTargets},
				},
			},
		},
	)
	if err != nil {
		return nil, err
	}

	return convertValidateComponentsResponse(rsp), nil
}

// convertValidateComponentsResponse converts a protobuf ValidateComponentsResponse
// into a ValidateComponentsResult.
func convertValidateComponentsResponse(rsp *pb.ValidateComponentsResponse) *ValidateComponentsResult {
	diffs := make([]*types.ComponentDiff, 0, len(rsp.Diffs))
	for _, d := range rsp.Diffs {
		diffs = append(diffs, componentDiffFromProto(d))
	}

	return &ValidateComponentsResult{
		Diffs:           diffs,
		TotalDiffs:      int(rsp.TotalDiffs),
		MissingCount:    int(rsp.MissingCount),
		UnexpectedCount: int(rsp.UnexpectedCount),
		MismatchCount:   int(rsp.MismatchCount),
		MatchCount:      int(rsp.MatchCount),
	}
}

// ListTasks lists tasks matching the query.
func (c *Client) ListTasks(
	ctx context.Context,
	rackID *uuid.UUID,
	activeOnly bool,
	pagination *types.Pagination,
) (*ListTasksResult, error) {
	req := &pb.ListTasksRequest{
		ActiveOnly: activeOnly,
		Pagination: paginationToProto(pagination),
	}

	if rackID != nil {
		req.RackId = uuidToProto(*rackID)
	}

	rsp, err := c.client.ListTasks(ctx, req)
	if err != nil {
		return nil, err
	}

	tasks := make([]*types.Task, 0, len(rsp.Tasks))
	for _, t := range rsp.Tasks {
		tasks = append(tasks, taskFromProto(t))
	}

	return &ListTasksResult{
		Tasks: tasks,
		Total: int(rsp.Total),
	}, nil
}

// GetTasksByIDs retrieves tasks by their IDs.
func (c *Client) GetTasksByIDs(
	ctx context.Context,
	taskIDs []uuid.UUID,
) ([]*types.Task, error) {
	pbIDs := make([]*pb.UUID, 0, len(taskIDs))
	for _, id := range taskIDs {
		pbIDs = append(pbIDs, uuidToProto(id))
	}

	rsp, err := c.client.GetTasksByIDs(ctx, &pb.GetTasksByIDsRequest{
		TaskIds: pbIDs,
	})
	if err != nil {
		return nil, err
	}

	tasks := make([]*types.Task, 0, len(rsp.Tasks))
	for _, t := range rsp.Tasks {
		tasks = append(tasks, taskFromProto(t))
	}

	return tasks, nil
}

// AddComponent creates a single component under an existing rack.
func (c *Client) AddComponent(
	ctx context.Context,
	comp *types.Component,
) (*types.Component, error) {
	rsp, err := c.client.AddComponent(ctx, &pb.AddComponentRequest{
		Component: componentToProto(comp),
	})
	if err != nil {
		return nil, err
	}
	return componentFromProto(rsp.Component), nil
}

// DeleteComponent soft-deletes a component by UUID.
func (c *Client) DeleteComponent(
	ctx context.Context,
	componentID uuid.UUID,
) error {
	_, err := c.client.DeleteComponent(ctx, &pb.DeleteComponentRequest{
		Id: uuidToProto(componentID),
	})
	return err
}

// DeleteRack soft-deletes a rack and all its components by UUID.
func (c *Client) DeleteRack(
	ctx context.Context,
	rackID uuid.UUID,
) error {
	_, err := c.client.DeleteRack(ctx, &pb.DeleteRackRequest{
		Id: uuidToProto(rackID),
	})
	return err
}

// PurgeRack permanently removes a soft-deleted rack and its components.
func (c *Client) PurgeRack(
	ctx context.Context,
	rackID uuid.UUID,
) error {
	_, err := c.client.PurgeRack(ctx, &pb.PurgeRackRequest{
		Id: uuidToProto(rackID),
	})
	return err
}

// PurgeComponent permanently removes a soft-deleted component.
func (c *Client) PurgeComponent(
	ctx context.Context,
	componentID uuid.UUID,
) error {
	_, err := c.client.PurgeComponent(ctx, &pb.PurgeComponentRequest{
		Id: uuidToProto(componentID),
	})
	return err
}

// PatchComponentOpts contains the optional fields for patching a component.
type PatchComponentOpts struct {
	FirmwareVersion *string
	SlotID          *int32
	TrayIndex       *int32
	HostID          *int32
	Description     *string
	RackID          *uuid.UUID
	BMCs            []types.BMC
}

// PatchComponent updates a single component's fields.
func (c *Client) PatchComponent(
	ctx context.Context,
	componentID uuid.UUID,
	opts PatchComponentOpts,
) (*types.Component, error) {
	req := &pb.PatchComponentRequest{
		Id: uuidToProto(componentID),
	}

	if opts.FirmwareVersion != nil {
		req.FirmwareVersion = opts.FirmwareVersion
	}

	if opts.SlotID != nil || opts.TrayIndex != nil || opts.HostID != nil {
		req.Position = &pb.RackPosition{}
		if opts.SlotID != nil {
			req.Position.SlotId = *opts.SlotID
		}
		if opts.TrayIndex != nil {
			req.Position.TrayIdx = *opts.TrayIndex
		}
		if opts.HostID != nil {
			req.Position.HostId = *opts.HostID
		}
	}

	if opts.Description != nil {
		req.Description = opts.Description
	}

	if opts.RackID != nil {
		req.RackId = uuidToProto(*opts.RackID)
	}

	if len(opts.BMCs) > 0 {
		req.Bmcs = make([]*pb.BMCInfo, 0, len(opts.BMCs))
		for i := range opts.BMCs {
			req.Bmcs = append(req.Bmcs, bmcToProto(&opts.BMCs[i]))
		}
	}

	rsp, err := c.client.PatchComponent(ctx, req)
	if err != nil {
		return nil, err
	}

	return componentFromProto(rsp.Component), nil
}

// ========================================
// Operation Rules Methods
// ========================================

// CreateOperationRule creates a new operation rule and returns its UUID.
func (c *Client) CreateOperationRule(
	ctx context.Context,
	name string,
	description string,
	operationType types.OperationType,
	operationCode string,
	ruleDefinitionJSON string,
	isDefault bool,
) (uuid.UUID, error) {
	rsp, err := c.client.CreateOperationRule(
		ctx,
		&pb.CreateOperationRuleRequest{
			Name:               name,
			Description:        description,
			OperationType:      operationTypeToProto(operationType),
			OperationCode:      operationCode,
			RuleDefinitionJson: ruleDefinitionJSON,
			IsDefault:          isDefault,
		},
	)
	if err != nil {
		return uuid.Nil, err
	}

	return uuidFromProto(rsp.GetId()), nil
}

// UpdateOperationRule updates an existing operation rule.
func (c *Client) UpdateOperationRule(
	ctx context.Context,
	ruleID uuid.UUID,
	name *string,
	description *string,
	ruleDefinitionJSON *string,
) error {
	req := &pb.UpdateOperationRuleRequest{
		RuleId: uuidToProto(ruleID),
	}

	if name != nil {
		req.Name = name
	}
	if description != nil {
		req.Description = description
	}
	if ruleDefinitionJSON != nil {
		req.RuleDefinitionJson = ruleDefinitionJSON
	}

	_, err := c.client.UpdateOperationRule(ctx, req)
	return err
}

// DeleteOperationRule deletes an operation rule by its ID.
func (c *Client) DeleteOperationRule(
	ctx context.Context,
	ruleID uuid.UUID,
) error {
	_, err := c.client.DeleteOperationRule(
		ctx,
		&pb.DeleteOperationRuleRequest{
			RuleId: uuidToProto(ruleID),
		},
	)
	return err
}

// SetRuleAsDefault marks a rule as the default for its operation type and code.
func (c *Client) SetRuleAsDefault(
	ctx context.Context,
	ruleID uuid.UUID,
) error {
	_, err := c.client.SetRuleAsDefault(
		ctx,
		&pb.SetRuleAsDefaultRequest{
			RuleId: uuidToProto(ruleID),
		},
	)
	return err
}

// GetOperationRule retrieves an operation rule by its ID.
func (c *Client) GetOperationRule(
	ctx context.Context,
	ruleID uuid.UUID,
) (*types.OperationRule, error) {
	rsp, err := c.client.GetOperationRule(
		ctx,
		&pb.GetOperationRuleRequest{
			RuleId: uuidToProto(ruleID),
		},
	)
	if err != nil {
		return nil, err
	}

	return operationRuleFromProto(rsp), nil
}

// ListOperationRules lists operation rules with optional filtering.
func (c *Client) ListOperationRules(
	ctx context.Context,
	operationType *types.OperationType,
	isDefault *bool,
	offset *int,
	limit *int,
) ([]*types.OperationRule, int, error) {
	req := &pb.ListOperationRulesRequest{}

	if operationType != nil {
		opType := operationTypeToProto(*operationType)
		req.OperationType = &opType
	}
	if isDefault != nil {
		req.IsDefault = isDefault
	}
	if offset != nil {
		offset32 := int32(*offset)
		req.Offset = &offset32
	}
	if limit != nil {
		limit32 := int32(*limit)
		req.Limit = &limit32
	}

	rsp, err := c.client.ListOperationRules(ctx, req)
	if err != nil {
		return nil, 0, err
	}

	rules := make([]*types.OperationRule, 0, len(rsp.Rules))
	for _, r := range rsp.Rules {
		rules = append(rules, operationRuleFromProto(r))
	}

	return rules, int(rsp.TotalCount), nil
}

// AssociateRuleWithRack associates an operation rule with a specific rack.
func (c *Client) AssociateRuleWithRack(
	ctx context.Context,
	rackID uuid.UUID,
	ruleID uuid.UUID,
) error {
	_, err := c.client.AssociateRuleWithRack(
		ctx,
		&pb.AssociateRuleWithRackRequest{
			RackId: uuidToProto(rackID),
			RuleId: uuidToProto(ruleID),
		},
	)
	return err
}

// DisassociateRuleFromRack removes a rule association from a rack.
func (c *Client) DisassociateRuleFromRack(
	ctx context.Context,
	rackID uuid.UUID,
	operationType types.OperationType,
	operationCode string,
) error {
	_, err := c.client.DisassociateRuleFromRack(
		ctx,
		&pb.DisassociateRuleFromRackRequest{
			RackId:        uuidToProto(rackID),
			OperationType: operationTypeToProto(operationType),
			OperationCode: operationCode,
		},
	)
	return err
}

// GetRackRuleAssociation retrieves the rule associated with a rack for a specific operation.
func (c *Client) GetRackRuleAssociation(
	ctx context.Context,
	rackID uuid.UUID,
	operationType types.OperationType,
	operationCode string,
) (uuid.UUID, error) {
	rsp, err := c.client.GetRackRuleAssociation(
		ctx,
		&pb.GetRackRuleAssociationRequest{
			RackId:        uuidToProto(rackID),
			OperationType: operationTypeToProto(operationType),
			OperationCode: operationCode,
		},
	)
	if err != nil {
		return uuid.Nil, err
	}

	return uuidFromProto(rsp.GetRuleId()), nil
}

// ListRackRuleAssociations lists all rule associations for a specific rack.
func (c *Client) ListRackRuleAssociations(
	ctx context.Context,
	rackID uuid.UUID,
) ([]*types.RackRuleAssociation, error) {
	rsp, err := c.client.ListRackRuleAssociations(
		ctx,
		&pb.ListRackRuleAssociationsRequest{
			RackId: uuidToProto(rackID),
		},
	)
	if err != nil {
		return nil, err
	}

	associations := make([]*types.RackRuleAssociation, 0, len(rsp.Associations))
	for _, a := range rsp.Associations {
		associations = append(associations, rackRuleAssociationFromProto(a))
	}

	return associations, nil
}

// IngestRackByRackIDs submits an ingestion task for the given rack IDs.
func (c *Client) IngestRackByRackIDs(
	ctx context.Context,
	rackIDs []uuid.UUID,
	description string,
) (*IngestRackResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackIDs))
	for _, id := range rackIDs {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier: &pb.RackTarget_Id{Id: uuidToProto(id)},
		})
	}

	rsp, err := c.client.IngestRack(ctx, &pb.IngestRackRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{Targets: rackTargets},
			},
		},
		Description: description,
	})
	if err != nil {
		return nil, err
	}

	return &IngestRackResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}

// IngestRackByRackNames submits an ingestion task for the given rack names.
func (c *Client) IngestRackByRackNames(
	ctx context.Context,
	rackNames []string,
	description string,
) (*IngestRackResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackNames))
	for _, name := range rackNames {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier: &pb.RackTarget_Name{Name: name},
		})
	}

	rsp, err := c.client.IngestRack(ctx, &pb.IngestRackRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{Targets: rackTargets},
			},
		},
		Description: description,
	})
	if err != nil {
		return nil, err
	}

	return &IngestRackResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}

// DecommissionRack submits a decommission task for the given rack IDs.
// The decommission workflow enforces strict component ordering:
// Compute (stage 1) → NVSwitch (stage 2) → PowerShelf (stage 3).
func (c *Client) DecommissionRack(
	ctx context.Context,
	rackIDs []uuid.UUID,
	description string,
) (*DecommissionRackResult, error) {
	rackTargets := make([]*pb.RackTarget, 0, len(rackIDs))
	for _, id := range rackIDs {
		rackTargets = append(rackTargets, &pb.RackTarget{
			Identifier: &pb.RackTarget_Id{Id: uuidToProto(id)},
		})
	}

	rsp, err := c.client.DecommissionRack(ctx, &pb.DecommissionRackRequest{
		TargetSpec: &pb.OperationTargetSpec{
			Targets: &pb.OperationTargetSpec_Racks{
				Racks: &pb.RackTargets{Targets: rackTargets},
			},
		},
		Description: description,
	})
	if err != nil {
		return nil, err
	}

	return &DecommissionRackResult{
		TaskIDs: uuidsFromProto(rsp.GetTaskIds()),
	}, nil
}
