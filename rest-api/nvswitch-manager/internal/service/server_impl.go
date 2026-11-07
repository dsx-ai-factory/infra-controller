// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"fmt"
	"net"
	"time"

	"github.com/google/uuid"
	log "github.com/sirupsen/logrus"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/nvswitchmanager"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/redfish"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// NVSwitchManagerServerImpl implements the v1.NVSwitchManager gRPC service.
type NVSwitchManagerServerImpl struct {
	nsm *nvswitchmanager.NVSwitchManager
	fwm *firmwaremanager.FirmwareManager
	pb.UnimplementedNVSwitchManagerServer
}

func newServerImplementation(nsm *nvswitchmanager.NVSwitchManager, fwm *firmwaremanager.FirmwareManager) (*NVSwitchManagerServerImpl, error) {
	return &NVSwitchManagerServerImpl{
		nsm: nsm,
		fwm: fwm,
	}, nil
}

// registerNVSwitch registers an NV-Switch tray with BMC and NVOS subsystems.
func (s *NVSwitchManagerServerImpl) registerNVSwitch(
	ctx context.Context,
	req *pb.RegisterNVSwitchRequest,
) *pb.RegisterNVSwitchResponse {
	// Validate input
	if req.Bmc == nil || req.Bmc.MacAddress == "" {
		return &pb.RegisterNVSwitchResponse{
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "BMC subsystem with MAC address is required",
		}
	}
	if req.Nvos == nil || req.Nvos.MacAddress == "" {
		return &pb.RegisterNVSwitchResponse{
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "NVOS subsystem with MAC address is required",
		}
	}

	// Create BMC subsystem
	var bmcCred *credential.Credential
	if req.Bmc.Credentials != nil {
		c := credential.New(req.Bmc.Credentials.Username, req.Bmc.Credentials.Password)
		bmcCred = &c
	}

	// Create NVOS credential early so we can validate both before proceeding
	var nvosCred *credential.Credential
	if req.Nvos.Credentials != nil {
		c := credential.New(req.Nvos.Credentials.Username, req.Nvos.Credentials.Password)
		nvosCred = &c
	}

	if s.nsm.DataStoreType == nvswitchmanager.DatastoreTypeInMemory {
		if bmcCred == nil {
			return &pb.RegisterNVSwitchResponse{
				Status: pb.StatusCode_INVALID_ARGUMENT,
				Error:  "BMC credentials are required when running in in-memory mode",
			}
		}
		if nvosCred == nil {
			return &pb.RegisterNVSwitchResponse{
				Status: pb.StatusCode_INVALID_ARGUMENT,
				Error:  "NVOS credentials are required when running in in-memory mode",
			}
		}
	}

	bmcObj, err := bmc.New(req.Bmc.MacAddress, req.Bmc.IpAddress, bmcCred)
	if err != nil {
		return &pb.RegisterNVSwitchResponse{
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "Invalid BMC: " + err.Error(),
		}
	}
	// Set custom BMC port if provided (0 = default 443)
	if req.Bmc.Port > 0 {
		bmcObj.SetPort(int(req.Bmc.Port))
	}

	// Create NVOS subsystem
	nvosObj, err := nvos.New(req.Nvos.MacAddress, req.Nvos.IpAddress, nvosCred)
	if err != nil {
		return &pb.RegisterNVSwitchResponse{
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "Invalid NVOS: " + err.Error(),
		}
	}
	// Set custom NVOS port if provided (0 = default 22)
	if req.Nvos.Port > 0 {
		nvosObj.SetPort(int(req.Nvos.Port))
	}

	// Create NVSwitchTray
	tray := &nvswitch.NVSwitchTray{
		Vendor: vendor.CodeToVendor(protobuf.VendorFrom(req.Vendor)),
		RackID: req.RackId,
		BMC:    bmcObj,
		NVOS:   nvosObj,
	}

	// Register
	uuid, isNew, err := s.nsm.Register(ctx, tray)
	if err != nil {
		return &pb.RegisterNVSwitchResponse{
			Status: pb.StatusCode_INTERNAL_ERROR,
			Error:  err.Error(),
		}
	}

	log.Printf("Successfully registered NV-Switch %s (BMC MAC: %s, NVOS MAC: %s, isNew: %v)",
		uuid.String(), req.Bmc.MacAddress, req.Nvos.MacAddress, isNew)

	return &pb.RegisterNVSwitchResponse{
		Uuid:    uuid.String(),
		IsNew:   isNew,
		Created: timestamppb.New(time.Now()),
		Status:  pb.StatusCode_SUCCESS,
	}
}

// RegisterNVSwitches registers multiple NV-Switch trays.
func (s *NVSwitchManagerServerImpl) RegisterNVSwitches(
	ctx context.Context,
	req *pb.RegisterNVSwitchesRequest,
) (*pb.RegisterNVSwitchesResponse, error) {
	responses := make([]*pb.RegisterNVSwitchResponse, 0, len(req.RegistrationRequests))
	for _, r := range req.RegistrationRequests {
		response := s.registerNVSwitch(ctx, r)
		responses = append(responses, response)
	}

	return &pb.RegisterNVSwitchesResponse{
		Responses: responses,
	}, nil
}

// GetNVSwitches returns NV-Switch information for the specified switches.
func (s *NVSwitchManagerServerImpl) GetNVSwitches(ctx context.Context, req *pb.NVSwitchRequest) (*pb.GetNVSwitchesResponse, error) {
	responses := make([]*pb.NVSwitchTray, 0)

	if len(req.Uuids) == 0 {
		// Return all switches
		trays, err := s.nsm.List(ctx)
		if err != nil {
			return nil, err
		}
		for _, tray := range trays {
			responses = append(responses, protobuf.NVSwitchTrayTo(tray))
		}
	} else {
		// Return specific switches
		for _, uuidStr := range req.Uuids {
			uuid, err := protobuf.ParseUUID(uuidStr)
			if err != nil {
				continue // Skip invalid UUIDs
			}
			tray, err := s.nsm.Get(ctx, uuid)
			if err != nil {
				continue // Skip not found
			}
			responses = append(responses, protobuf.NVSwitchTrayTo(tray))
		}
	}

	return &pb.GetNVSwitchesResponse{
		Nvswitches: responses,
	}, nil
}

// protoToRedfishAction maps a proto PowerAction to a Redfish ResetType.
func protoToRedfishAction(action pb.PowerAction) (redfish.ResetType, error) {
	switch action {
	case pb.PowerAction_POWER_ACTION_FORCE_OFF:
		return redfish.ResetForceOff, nil
	case pb.PowerAction_POWER_ACTION_POWER_CYCLE:
		return redfish.ResetPowerCycle, nil
	case pb.PowerAction_POWER_ACTION_GRACEFUL_SHUTDOWN:
		return redfish.ResetGracefulShutdown, nil
	case pb.PowerAction_POWER_ACTION_ON:
		return redfish.ResetOn, nil
	case pb.PowerAction_POWER_ACTION_FORCE_ON:
		return redfish.ResetForceOn, nil
	case pb.PowerAction_POWER_ACTION_GRACEFUL_RESTART:
		return redfish.ResetGracefulRestart, nil
	case pb.PowerAction_POWER_ACTION_FORCE_RESTART:
		return redfish.ResetForceRestart, nil
	default:
		return "", fmt.Errorf("unsupported power action: %s", action)
	}
}

// PowerControl performs a power action on the specified NV-Switch trays.
func (s *NVSwitchManagerServerImpl) PowerControl(ctx context.Context, req *pb.PowerControlRequest) (*pb.PowerControlResponse, error) {
	resetType, err := protoToRedfishAction(req.Action)
	if err != nil {
		return &pb.PowerControlResponse{
			Responses: []*pb.NVSwitchResponse{{
				Status: pb.StatusCode_INVALID_ARGUMENT,
				Error:  err.Error(),
			}},
		}, nil
	}

	responses := make([]*pb.NVSwitchResponse, 0, len(req.Uuids)+len(req.Targets))

	// Registered path: resolve UUIDs via registry + credential manager
	for _, uuidStr := range req.Uuids {
		uuid, err := protobuf.ParseUUID(uuidStr)
		if err != nil {
			responses = append(responses, &pb.NVSwitchResponse{
				Uuid:   uuidStr,
				Status: pb.StatusCode_INVALID_ARGUMENT,
				Error:  "invalid UUID: " + err.Error(),
			})
			continue
		}

		tray, err := s.nsm.Get(ctx, uuid)
		if err != nil {
			responses = append(responses, &pb.NVSwitchResponse{
				Uuid:   uuidStr,
				Status: pb.StatusCode_INTERNAL_ERROR,
				Error:  "switch not found: " + err.Error(),
			})
			continue
		}

		if err := firmwaremanager.ResetTray(ctx, tray, resetType); err != nil {
			responses = append(responses, &pb.NVSwitchResponse{
				Uuid:   uuidStr,
				Status: pb.StatusCode_INTERNAL_ERROR,
				Error:  fmt.Sprintf("%s failed: %s", resetType, err.Error()),
			})
			continue
		}

		log.Infof("%s initiated for switch %s", resetType, uuidStr)
		responses = append(responses, &pb.NVSwitchResponse{
			Uuid:   uuidStr,
			Status: pb.StatusCode_SUCCESS,
		})
	}

	// Direct path: use inline connection details, bypass registry and credential manager
	for _, target := range req.Targets {
		responses = append(responses, resetTarget(ctx, target, resetType))
	}

	return &pb.PowerControlResponse{
		Responses: responses,
	}, nil
}

// resetTarget performs a power action against an unregistered device using inline connection details.
func resetTarget(ctx context.Context, target *pb.PowerTarget, resetType redfish.ResetType) *pb.NVSwitchResponse {
	ip := net.ParseIP(target.BmcIp)
	if ip == nil {
		return &pb.NVSwitchResponse{
			BmcIp:  target.BmcIp,
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  fmt.Sprintf("invalid BMC IP: %s", target.BmcIp),
		}
	}

	if target.BmcCredentials == nil {
		return &pb.NVSwitchResponse{
			BmcIp:  target.BmcIp,
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "bmc_credentials are required for power targets",
		}
	}

	if target.BmcCredentials.Username == "" || target.BmcCredentials.Password == "" {
		return &pb.NVSwitchResponse{
			BmcIp:  target.BmcIp,
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "bmc_credentials username and password must not be empty",
		}
	}

	cred := credential.New(target.BmcCredentials.Username, target.BmcCredentials.Password)
	tray := &nvswitch.NVSwitchTray{
		BMC: &bmc.BMC{
			IP:         ip,
			Port:       int(target.BmcPort),
			Credential: &cred,
		},
	}

	if err := firmwaremanager.ResetTray(ctx, tray, resetType); err != nil {
		return &pb.NVSwitchResponse{
			BmcIp:  target.BmcIp,
			Status: pb.StatusCode_INTERNAL_ERROR,
			Error:  fmt.Sprintf("%s failed: %s", resetType, err.Error()),
		}
	}

	return &pb.NVSwitchResponse{
		BmcIp:  target.BmcIp,
		Status: pb.StatusCode_SUCCESS,
	}
}

// ============================================================================
// Firmware Management API
// ============================================================================

// ListBundles returns all available firmware bundles.
func (s *NVSwitchManagerServerImpl) ListBundles(ctx context.Context, _ *emptypb.Empty) (*pb.ListBundlesResponse, error) {
	if s.fwm == nil {
		return nil, status.Error(codes.Unavailable, "firmware manager not initialized")
	}

	versions := s.fwm.ListBundles()
	bundles := make([]*pb.FirmwareBundle, 0, len(versions))

	for _, version := range versions {
		pkg, err := s.fwm.GetBundle(version)
		if err != nil {
			continue
		}

		components := make([]*pb.ComponentInfo, 0, len(pkg.Components))
		for name, comp := range pkg.Components {
			components = append(components, &pb.ComponentInfo{
				Name:     name,
				Version:  comp.Version,
				Strategy: comp.Strategy,
			})
		}

		bundles = append(bundles, &pb.FirmwareBundle{
			Version:     pkg.Version,
			Description: pkg.Description,
			Components:  components,
		})
	}

	return &pb.ListBundlesResponse{Bundles: bundles}, nil
}

// QueueUpdate queues a firmware update for a specific switch and component.
func (s *NVSwitchManagerServerImpl) QueueUpdate(ctx context.Context, req *pb.QueueUpdateRequest) (*pb.QueueUpdateResponse, error) {
	if s.fwm == nil {
		return nil, status.Error(codes.Unavailable, "firmware manager not initialized")
	}

	switchUUID, err := uuid.Parse(req.SwitchUuid)
	if err != nil {
		return nil, status.Errorf(codes.InvalidArgument, "invalid switch UUID: %v", err)
	}

	// Convert proto components to domain components
	var components []nvswitch.Component
	for _, c := range req.Components {
		component := protoComponentToDomain(c)
		if component == "" {
			return nil, status.Errorf(codes.InvalidArgument, "invalid component: %v", c)
		}
		components = append(components, component)
	}

	updates, err := s.fwm.QueueUpdate(ctx, switchUUID, req.BundleVersion, components)
	if err != nil {
		return nil, status.Errorf(codes.InvalidArgument, "failed to queue update: %v", err)
	}

	// Convert to proto
	protoUpdates := make([]*pb.FirmwareUpdateInfo, len(updates))
	for i, u := range updates {
		protoUpdates[i] = firmwareUpdateToProto(u)
	}

	return &pb.QueueUpdateResponse{
		Updates: protoUpdates,
	}, nil
}

// QueueUpdates queues firmware updates for multiple switches in a single call.
func (s *NVSwitchManagerServerImpl) QueueUpdates(ctx context.Context, req *pb.QueueUpdatesRequest) (*pb.QueueUpdatesResponse, error) {
	if s.fwm == nil {
		return nil, status.Error(codes.Unavailable, "firmware manager not initialized")
	}

	// switch_uuids and targets are mutually exclusive: a request must populate
	// exactly one. Allowing both would risk queuing duplicate updates for any
	// device whose UUID is in switch_uuids and whose FirmwareTarget is in
	// targets, since the handler does not deduplicate across the two lists.
	// Empty requests are rejected as well — almost certainly a client bug.
	hasUUIDs := len(req.SwitchUuids) > 0
	hasTargets := len(req.Targets) > 0
	switch {
	case !hasUUIDs && !hasTargets:
		return nil, status.Error(codes.InvalidArgument,
			"request must populate either switch_uuids or targets")
	case hasUUIDs && hasTargets:
		return nil, status.Error(codes.InvalidArgument,
			"request must populate only one of switch_uuids or targets, not both")
	}

	// Convert proto components to domain components
	var components []nvswitch.Component
	for _, c := range req.Components {
		component := protoComponentToDomain(c)
		if component == "" {
			return nil, status.Errorf(codes.InvalidArgument, "invalid component: %v", c)
		}
		components = append(components, component)
	}

	var results []*pb.QueueUpdateResult

	// Registered path: queue updates by UUID
	for _, switchUUIDStr := range req.SwitchUuids {
		result := &pb.QueueUpdateResult{
			SwitchUuid: switchUUIDStr,
		}

		switchUUID, err := uuid.Parse(switchUUIDStr)
		if err != nil {
			result.Status = pb.StatusCode_INTERNAL_ERROR
			result.Error = fmt.Sprintf("invalid switch UUID: %v", err)
			results = append(results, result)
			continue
		}

		s.queueFirmwareUpdate(ctx, result, switchUUID, req.BundleVersion, components)
		results = append(results, result)
	}

	// Direct path: auto-register unregistered targets, then queue updates
	for _, target := range req.Targets {
		if target == nil {
			results = append(results, &pb.QueueUpdateResult{
				Status: pb.StatusCode_INVALID_ARGUMENT,
				Error:  "target is nil",
			})
			continue
		}

		result := &pb.QueueUpdateResult{
			BmcIp: target.BmcIp,
		}

		if err := validateFirmwareTarget(target); err != nil {
			result.Status = pb.StatusCode_INVALID_ARGUMENT
			result.Error = err.Error()
			results = append(results, result)
			continue
		}

		regResp := s.registerNVSwitch(ctx, firmwareTargetToRegisterRequest(target))
		if regResp.Status != pb.StatusCode_SUCCESS {
			result.Status = regResp.Status
			result.Error = fmt.Sprintf("failed to register target: %s", regResp.Error)
			results = append(results, result)
			continue
		}
		result.SwitchUuid = regResp.Uuid

		switchUUID, err := uuid.Parse(regResp.Uuid)
		if err != nil {
			result.Status = pb.StatusCode_INTERNAL_ERROR
			result.Error = fmt.Sprintf("invalid UUID from registration: %v", err)
			results = append(results, result)
			continue
		}

		s.queueFirmwareUpdate(ctx, result, switchUUID, req.BundleVersion, components)
		results = append(results, result)
	}

	return &pb.QueueUpdatesResponse{
		Results: results,
	}, nil
}

// queueFirmwareUpdate queues a firmware update for the given switch and populates the result.
func (s *NVSwitchManagerServerImpl) queueFirmwareUpdate(
	ctx context.Context,
	result *pb.QueueUpdateResult,
	switchUUID uuid.UUID,
	bundleVersion string,
	components []nvswitch.Component,
) {
	updates, err := s.fwm.QueueUpdate(ctx, switchUUID, bundleVersion, components)
	if err != nil {
		result.Status = pb.StatusCode_INTERNAL_ERROR
		result.Error = fmt.Sprintf("failed to queue update: %v", err)
		return
	}

	protoUpdates := make([]*pb.FirmwareUpdateInfo, len(updates))
	for i, u := range updates {
		protoUpdates[i] = firmwareUpdateToProto(u)
	}

	result.Status = pb.StatusCode_SUCCESS
	result.Updates = protoUpdates
}

// validateFirmwareTarget validates all required fields on a FirmwareTarget.
func validateFirmwareTarget(target *pb.FirmwareTarget) error {
	if net.ParseIP(target.BmcIp) == nil {
		return fmt.Errorf("invalid BMC IP: %s", target.BmcIp)
	}
	if target.BmcCredentials == nil || target.BmcCredentials.Username == "" || target.BmcCredentials.Password == "" {
		return fmt.Errorf("bmc_credentials with username and password are required")
	}
	if net.ParseIP(target.NvosIp) == nil {
		return fmt.Errorf("invalid NVOS IP: %s", target.NvosIp)
	}
	if target.NvosCredentials == nil || target.NvosCredentials.Username == "" || target.NvosCredentials.Password == "" {
		return fmt.Errorf("nvos_credentials with username and password are required")
	}
	if target.BmcMac == "" {
		return fmt.Errorf("bmc_mac is required")
	}
	if target.NvosMac == "" {
		return fmt.Errorf("nvos_mac is required")
	}
	return nil
}

// firmwareTargetToRegisterRequest converts a FirmwareTarget into a RegisterNVSwitchRequest.
func firmwareTargetToRegisterRequest(target *pb.FirmwareTarget) *pb.RegisterNVSwitchRequest {
	return &pb.RegisterNVSwitchRequest{
		Vendor: target.Vendor,
		Bmc: &pb.Subsystem{
			MacAddress:  target.BmcMac,
			IpAddress:   target.BmcIp,
			Credentials: target.BmcCredentials,
			Port:        target.BmcPort,
		},
		Nvos: &pb.Subsystem{
			MacAddress:  target.NvosMac,
			IpAddress:   target.NvosIp,
			Credentials: target.NvosCredentials,
			Port:        target.NvosPort,
		},
	}
}

// GetUpdate returns the status of a specific firmware update by ID.
func (s *NVSwitchManagerServerImpl) GetUpdate(ctx context.Context, req *pb.GetUpdateRequest) (*pb.GetUpdateResponse, error) {
	if s.fwm == nil {
		return nil, status.Error(codes.Unavailable, "firmware manager not initialized")
	}

	updateID, err := uuid.Parse(req.UpdateId)
	if err != nil {
		return nil, status.Errorf(codes.InvalidArgument, "invalid update ID: %v", err)
	}

	update, err := s.fwm.GetUpdate(ctx, updateID)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "update not found: %v", err)
	}

	return &pb.GetUpdateResponse{
		Update: firmwareUpdateToProto(update),
	}, nil
}

// GetUpdatesForSwitch returns all firmware updates for a switch.
func (s *NVSwitchManagerServerImpl) GetUpdatesForSwitch(ctx context.Context, req *pb.GetUpdatesForSwitchRequest) (*pb.GetUpdatesForSwitchResponse, error) {
	if s.fwm == nil {
		return nil, status.Error(codes.Unavailable, "firmware manager not initialized")
	}

	switchUUID, err := uuid.Parse(req.SwitchUuid)
	if err != nil {
		return nil, status.Errorf(codes.InvalidArgument, "invalid switch UUID: %v", err)
	}

	updates, err := s.fwm.GetUpdatesForSwitch(ctx, switchUUID)
	if err != nil {
		return nil, status.Errorf(codes.Internal, "failed to get updates: %v", err)
	}

	protoUpdates := make([]*pb.FirmwareUpdateInfo, len(updates))
	for i, update := range updates {
		protoUpdates[i] = firmwareUpdateToProto(update)
	}

	return &pb.GetUpdatesForSwitchResponse{
		Updates: protoUpdates,
	}, nil
}

// GetAllUpdates returns all firmware updates across all switches.
func (s *NVSwitchManagerServerImpl) GetAllUpdates(ctx context.Context, req *emptypb.Empty) (*pb.GetAllUpdatesResponse, error) {
	if s.fwm == nil {
		return nil, status.Error(codes.Unavailable, "firmware manager not initialized")
	}

	updates, err := s.fwm.GetAllUpdates(ctx)
	if err != nil {
		return nil, status.Errorf(codes.Internal, "failed to get updates: %v", err)
	}

	protoUpdates := make([]*pb.FirmwareUpdateInfo, len(updates))
	for i, update := range updates {
		protoUpdates[i] = firmwareUpdateToProto(update)
	}

	return &pb.GetAllUpdatesResponse{
		Updates: protoUpdates,
	}, nil
}

// CancelUpdate cancels an in-progress firmware update.
func (s *NVSwitchManagerServerImpl) CancelUpdate(ctx context.Context, req *pb.CancelUpdateRequest) (*pb.CancelUpdateResponse, error) {
	if s.fwm == nil {
		return nil, status.Error(codes.Unavailable, "firmware manager not initialized")
	}

	updateID, err := uuid.Parse(req.UpdateId)
	if err != nil {
		return nil, status.Errorf(codes.InvalidArgument, "invalid update ID: %v", err)
	}

	err = s.fwm.CancelUpdate(ctx, updateID)
	if err != nil {
		return &pb.CancelUpdateResponse{
			Success: false,
			Message: err.Error(),
		}, nil
	}

	return &pb.CancelUpdateResponse{
		Success: true,
		Message: "update cancelled",
	}, nil
}

// Helper functions for proto conversion

func protoComponentToDomain(c pb.NVSwitchComponent) nvswitch.Component {
	switch c {
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_BMC:
		return nvswitch.BMC
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_CPLD:
		return nvswitch.CPLD
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_BIOS:
		return nvswitch.BIOS
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_NVOS:
		return nvswitch.NVOS
	default:
		return ""
	}
}

func domainComponentToProto(c nvswitch.Component) pb.NVSwitchComponent {
	switch c {
	case nvswitch.BMC:
		return pb.NVSwitchComponent_NVSWITCH_COMPONENT_BMC
	case nvswitch.CPLD:
		return pb.NVSwitchComponent_NVSWITCH_COMPONENT_CPLD
	case nvswitch.BIOS:
		return pb.NVSwitchComponent_NVSWITCH_COMPONENT_BIOS
	case nvswitch.NVOS:
		return pb.NVSwitchComponent_NVSWITCH_COMPONENT_NVOS
	default:
		return pb.NVSwitchComponent_NVSWITCH_COMPONENT_UNKNOWN
	}
}

func domainStrategyToProto(s firmwaremanager.Strategy) pb.UpdateStrategy {
	switch s {
	case firmwaremanager.StrategyScript:
		return pb.UpdateStrategy_UPDATE_STRATEGY_SCRIPT
	case firmwaremanager.StrategySSH:
		return pb.UpdateStrategy_UPDATE_STRATEGY_SSH
	case firmwaremanager.StrategyRedfish:
		return pb.UpdateStrategy_UPDATE_STRATEGY_REDFISH
	default:
		return pb.UpdateStrategy_UPDATE_STRATEGY_UNKNOWN
	}
}

func domainStateToProto(s firmwaremanager.UpdateState) pb.UpdateState {
	switch s {
	case firmwaremanager.StateQueued:
		return pb.UpdateState_UPDATE_STATE_QUEUED
	case firmwaremanager.StatePowerCycle:
		return pb.UpdateState_UPDATE_STATE_POWER_CYCLE
	case firmwaremanager.StateWaitReachable:
		return pb.UpdateState_UPDATE_STATE_WAIT_REACHABLE
	case firmwaremanager.StateCopy:
		return pb.UpdateState_UPDATE_STATE_COPY
	case firmwaremanager.StateUpload:
		return pb.UpdateState_UPDATE_STATE_UPLOAD
	case firmwaremanager.StateInstall:
		return pb.UpdateState_UPDATE_STATE_INSTALL
	case firmwaremanager.StatePollCompletion:
		return pb.UpdateState_UPDATE_STATE_POLL_COMPLETION
	case firmwaremanager.StateVerify:
		return pb.UpdateState_UPDATE_STATE_VERIFY
	case firmwaremanager.StateCleanup:
		return pb.UpdateState_UPDATE_STATE_CLEANUP
	case firmwaremanager.StateCompleted:
		return pb.UpdateState_UPDATE_STATE_COMPLETED
	case firmwaremanager.StateFailed:
		return pb.UpdateState_UPDATE_STATE_FAILED
	case firmwaremanager.StateCancelled:
		return pb.UpdateState_UPDATE_STATE_CANCELLED
	default:
		return pb.UpdateState_UPDATE_STATE_UNKNOWN
	}
}

func firmwareUpdateToProto(update *firmwaremanager.FirmwareUpdate) *pb.FirmwareUpdateInfo {
	info := &pb.FirmwareUpdateInfo{
		Id:            update.ID.String(),
		SwitchUuid:    update.SwitchUUID.String(),
		Component:     domainComponentToProto(update.Component),
		BundleVersion: update.BundleVersion,
		Strategy:      domainStrategyToProto(update.Strategy),
		State:         domainStateToProto(update.State),
		VersionFrom:   update.VersionFrom,
		VersionTo:     update.VersionTo,
		VersionActual: update.VersionActual,
		ErrorMessage:  update.ErrorMessage,
		CreatedAt:     timestamppb.New(update.CreatedAt),
		UpdatedAt:     timestamppb.New(update.UpdatedAt),
		SequenceOrder: int32(update.SequenceOrder),
	}

	// Set optional sequencing fields
	if update.BundleUpdateID != nil {
		info.BundleUpdateId = update.BundleUpdateID.String()
	}
	if update.PredecessorID != nil {
		info.PredecessorId = update.PredecessorID.String()
	}

	return info
}
