// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"fmt"
	"net"
	"time"

	log "github.com/sirupsen/logrus"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	pb "github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/powershelfmanager"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// PowershelfManagerServerImpl implements the v1.PowershelfManager gRPC service by delegating to a PowershelfManager instance.
type PowershelfManagerServerImpl struct {
	psm *powershelfmanager.PowershelfManager
	pb.UnimplementedPowershelfManagerServer
}

func newServerImplementation(psm *powershelfmanager.PowershelfManager) (*PowershelfManagerServerImpl, error) {
	return &PowershelfManagerServerImpl{
		psm: psm,
	}, nil
}

// registerPowershelf registers a powershelf by its PMC MAC/IP/Vendor and optionally persists its credentials. Returns creation timestamp on success.
func (s *PowershelfManagerServerImpl) registerPowershelf(
	ctx context.Context,
	req *pb.RegisterPowershelfRequest,
) *pb.RegisterPowershelfResponse {
	var cred *credential.Credential
	if req.PmcCredentials != nil {
		pmcCred := credential.New(req.PmcCredentials.Username, req.PmcCredentials.Password)
		cred = &pmcCred
	}

	if cred == nil && s.psm.DataStoreType == powershelfmanager.DatastoreTypeInMemory {
		return &pb.RegisterPowershelfResponse{
			PmcMacAddress: req.PmcMacAddress,
			IsNew:         true,
			Created:       timestamppb.New(time.Now()),
			Status:        pb.StatusCode_INVALID_ARGUMENT,
			Error:         "pmc_credentials are required when running in in-memory mode",
		}
	}

	pmc, err := pmc.New(req.PmcMacAddress, req.PmcIpAddress, protobuf.PMCVendorFrom(req.PmcVendor), cred)
	if err != nil {
		return &pb.RegisterPowershelfResponse{
			PmcMacAddress: req.PmcMacAddress,
			IsNew:         true,
			Created:       timestamppb.New(time.Now()),
			Status:        pb.StatusCode_INVALID_ARGUMENT,
			Error:         err.Error(),
		}
	}

	err = s.psm.RegisterPmc(ctx, pmc)
	if err != nil {
		return &pb.RegisterPowershelfResponse{
			PmcMacAddress: req.PmcMacAddress,
			IsNew:         true,
			Created:       timestamppb.New(time.Now()),
			Status:        pb.StatusCode_INTERNAL_ERROR,
			Error:         err.Error(),
		}
	}

	log.Printf("Successfully registered PMC %v with IP %v", req.PmcMacAddress, req.PmcIpAddress)

	return &pb.RegisterPowershelfResponse{
		PmcMacAddress: req.PmcMacAddress,
		IsNew:         true,
		Created:       timestamppb.New(time.Now()),
		Status:        pb.StatusCode_SUCCESS,
		Error:         "",
	}
}

// registerPowershelf registers a powershelf by its PMC MAC/IP/Vendor and persists its credentials. Returns creation timestamp on success.
func (s *PowershelfManagerServerImpl) RegisterPowershelves(
	ctx context.Context,
	req *pb.RegisterPowershelvesRequest,
) (*pb.RegisterPowershelvesResponse, error) {
	responses := make([]*pb.RegisterPowershelfResponse, 0, len(req.RegistrationRequests))
	for _, req := range req.RegistrationRequests {
		response := s.registerPowershelf(ctx, req)
		responses = append(responses, response)
	}

	return &pb.RegisterPowershelvesResponse{
		Responses: responses,
	}, nil
}

func (s *PowershelfManagerServerImpl) GetPowershelves(ctx context.Context, req *pb.PowershelfRequest) (*pb.GetPowershelvesResponse, error) {
	responses := make([]*pb.PowerShelf, 0, len(req.PmcMacs))
	if len(req.PmcMacs) == 0 {
		powershelves, err := s.psm.GetAllPowershelves(ctx)
		if err != nil {
			return nil, err
		}

		for _, powershelf := range powershelves {
			responses = append(responses, protobuf.PowershelfTo(powershelf))
		}
	} else {
		pmcs := make([]net.HardwareAddr, 0, len(req.PmcMacs))
		for _, mac := range req.PmcMacs {
			pmc, err := net.ParseMAC(mac)
			if err != nil {
				return nil, err
			}
			pmcs = append(pmcs, pmc)
		}

		powershelves, err := s.psm.GetPowershelves(ctx, pmcs)
		if err != nil {
			return nil, err
		}

		for _, powershelf := range powershelves {
			responses = append(responses, protobuf.PowershelfTo(powershelf))
		}
	}

	return &pb.GetPowershelvesResponse{
		Powershelves: responses,
	}, nil
}

func (s *PowershelfManagerServerImpl) ListAvailableFirmware(ctx context.Context, req *pb.PowershelfRequest) (*pb.ListAvailableFirmwareResponse, error) {
	responses := make([]*pb.AvailableFirmware, 0, len(req.PmcMacs))
	for _, mac := range req.PmcMacs {
		response, err := s.listAvailableFirmware(ctx, mac)
		if err != nil {
			return nil, err
		}
		responses = append(responses, response)
	}

	return &pb.ListAvailableFirmwareResponse{
		Upgrades: responses,
	}, nil
}

// CanUpdateFirmware returns whether a firmware upgrade is supported for the PMC’s current version given vendor rules and available artifacts.
func (s *PowershelfManagerServerImpl) listAvailableFirmware(ctx context.Context, pmc_mac string) (*pb.AvailableFirmware, error) {
	mac, err := net.ParseMAC(pmc_mac)
	if err != nil {
		return nil, err
	}

	upgrades, err := s.psm.ListAvailableFirmware(ctx, mac)
	if err != nil {
		return nil, err
	}

	protoUpgrades := make([]*pb.FirmwareVersion, 0, len(upgrades))
	for _, upgrade := range upgrades {
		protoUpgrades = append(protoUpgrades, &pb.FirmwareVersion{
			Version: upgrade.UpgradeTo().String(),
		})
	}

	pmcComponent := &pb.ComponentFirmwareUpgrades{
		Component: pb.PowershelfComponent_PMC,
		Upgrades:  protoUpgrades,
	}

	componentUpgrades := []*pb.ComponentFirmwareUpgrades{pmcComponent}
	return &pb.AvailableFirmware{
		PmcMacAddress: pmc_mac,
		Upgrades:      componentUpgrades,
	}, nil
}

func (s *PowershelfManagerServerImpl) UpdateFirmware(ctx context.Context, req *pb.UpdateFirmwareRequest) (*pb.UpdateFirmwareResponse, error) {
	if s.psm == nil {
		return nil, status.Error(codes.Unavailable, "powershelf manager not initialized")
	}

	// upgrades and targets are mutually exclusive: a request must populate
	// exactly one. Allowing both would risk queuing duplicate updates for any
	// device whose PMC MAC is in upgrades and whose FirmwareTarget is in
	// targets, since the handler does not deduplicate across the two lists.
	// Empty requests are rejected as well — almost certainly a client bug.
	hasUpgrades := len(req.Upgrades) > 0
	hasTargets := len(req.Targets) > 0
	switch {
	case !hasUpgrades && !hasTargets:
		return nil, status.Error(codes.InvalidArgument,
			"request must populate either upgrades or targets")
	case hasUpgrades && hasTargets:
		return nil, status.Error(codes.InvalidArgument,
			"request must populate only one of upgrades or targets, not both")
	}

	responses := make([]*pb.UpdatePowershelfFirmwareResponse, 0, len(req.Upgrades)+len(req.Targets))

	// Registered path: upgrade by MAC
	for _, powershelf := range req.Upgrades {
		responses = append(responses, &pb.UpdatePowershelfFirmwareResponse{
			PmcMacAddress: powershelf.PmcMacAddress,
			Components:    s.upgradeComponents(ctx, powershelf.PmcMacAddress, powershelf.Components),
		})
	}

	// Direct path: auto-register unregistered targets, then upgrade
	for _, targetReq := range req.Targets {
		if targetReq == nil {
			continue
		}

		target := targetReq.Target
		if target == nil {
			responses = append(responses, &pb.UpdatePowershelfFirmwareResponse{
				Components: fanOutComponentError(targetReq.Components, pb.StatusCode_INVALID_ARGUMENT, "target is required"),
			})
			continue
		}

		resp := &pb.UpdatePowershelfFirmwareResponse{
			PmcIp: target.GetPmcIpAddress(),
		}

		if err := validateFirmwareTarget(target); err != nil {
			resp.Components = fanOutComponentError(targetReq.Components, pb.StatusCode_INVALID_ARGUMENT, err.Error())
			responses = append(responses, resp)
			continue
		}

		regResp := s.registerPowershelf(ctx, firmwareTargetToRegisterRequest(target))
		if regResp.Status != pb.StatusCode_SUCCESS {
			resp.Components = fanOutComponentError(targetReq.Components, regResp.Status,
				fmt.Sprintf("failed to register target: %s", regResp.Error))
			responses = append(responses, resp)
			continue
		}
		resp.PmcMacAddress = regResp.PmcMacAddress
		resp.Components = s.upgradeComponents(ctx, target.PmcMacAddress, targetReq.Components)
		responses = append(responses, resp)
	}

	return &pb.UpdateFirmwareResponse{
		Responses: responses,
	}, nil
}

// fanOutComponentError emits one UpdateComponentFirmwareResponse per requested
// component, all sharing the given status and error. Used when a target-level
// failure (validation, registration) prevents any per-component work from
// running, so the response shape still mirrors the success path and callers
// can correlate by index/component.
func fanOutComponentError(components []*pb.UpdateComponentFirmwareRequest, status pb.StatusCode, errMsg string) []*pb.UpdateComponentFirmwareResponse {
	results := make([]*pb.UpdateComponentFirmwareResponse, 0, len(components))
	for _, c := range components {
		results = append(results, &pb.UpdateComponentFirmwareResponse{
			Component: c.Component,
			Status:    status,
			Error:     errMsg,
		})
	}
	return results
}

// upgradeComponents upgrades each requested component for a given PMC MAC and returns per-component results.
func (s *PowershelfManagerServerImpl) upgradeComponents(ctx context.Context, pmcMac string, components []*pb.UpdateComponentFirmwareRequest) []*pb.UpdateComponentFirmwareResponse {
	results := make([]*pb.UpdateComponentFirmwareResponse, 0, len(components))
	for _, component := range components {
		if component.UpgradeTo == nil {
			results = append(results, &pb.UpdateComponentFirmwareResponse{
				Component: component.Component,
				Status:    pb.StatusCode_INVALID_ARGUMENT,
				Error:     "upgrade_to firmware version is required",
			})
			continue
		}
		results = append(results, s.updateFirmware(ctx, pmcMac, component.Component, component.UpgradeTo.Version))
	}
	return results
}

// UpdateFirmware triggers a firmware upgrade for the PMC. If dry_run is true, it resolves artifacts and simulates the update without uploading.
func (s *PowershelfManagerServerImpl) updateFirmware(ctx context.Context, pmc_mac string, pbComponent pb.PowershelfComponent, targetFwVersion string) *pb.UpdateComponentFirmwareResponse {
	resp := &pb.UpdateComponentFirmwareResponse{Component: pbComponent}

	// TODO: support upgrading components other than the PMC
	if pbComponent != pb.PowershelfComponent_PMC {
		resp.Status = pb.StatusCode_INVALID_ARGUMENT
		resp.Error = fmt.Sprintf("PSM does not support upgrading %s component", pbComponent.String())
		return resp
	}

	mac, err := net.ParseMAC(pmc_mac)
	if err != nil {
		resp.Status = pb.StatusCode_INVALID_ARGUMENT
		resp.Error = err.Error()
		return resp
	}

	component, err := protobuf.ComponentTypeFromMap(pbComponent)
	if err != nil {
		resp.Status = pb.StatusCode_INVALID_ARGUMENT
		resp.Error = err.Error()
		return resp
	}

	if err := s.psm.UpgradeFirmware(ctx, mac, component, targetFwVersion); err != nil {
		resp.Status = pb.StatusCode_INTERNAL_ERROR
		resp.Error = err.Error()
		return resp
	}

	resp.Status = pb.StatusCode_SUCCESS
	return resp
}

// validateFirmwareTarget validates all required fields on a FirmwareTarget.
func validateFirmwareTarget(target *pb.FirmwareTarget) error {
	if target == nil {
		return fmt.Errorf("firmware target is required")
	}
	if target.PmcMacAddress == "" {
		return fmt.Errorf("pmc_mac_address is required")
	}
	if _, err := net.ParseMAC(target.PmcMacAddress); err != nil {
		return fmt.Errorf("invalid pmc_mac_address: %s", target.PmcMacAddress)
	}
	if net.ParseIP(target.PmcIpAddress) == nil {
		return fmt.Errorf("invalid pmc_ip_address: %s", target.PmcIpAddress)
	}
	if target.PmcCredentials == nil || target.PmcCredentials.Username == "" || target.PmcCredentials.Password == "" {
		return fmt.Errorf("pmc_credentials with username and password are required")
	}
	return nil
}

// firmwareTargetToRegisterRequest converts a FirmwareTarget to a RegisterPowershelfRequest.
func firmwareTargetToRegisterRequest(target *pb.FirmwareTarget) *pb.RegisterPowershelfRequest {
	return &pb.RegisterPowershelfRequest{
		PmcMacAddress:  target.PmcMacAddress,
		PmcIpAddress:   target.PmcIpAddress,
		PmcVendor:      target.PmcVendor,
		PmcCredentials: target.PmcCredentials,
	}
}

// GetFirmwareUpdateStatus returns the status of firmware updates for the specified PMC(s) and component(s).
func (s *PowershelfManagerServerImpl) GetFirmwareUpdateStatus(ctx context.Context, req *pb.GetFirmwareUpdateStatusRequest) (*pb.GetFirmwareUpdateStatusResponse, error) {
	statuses := make([]*pb.FirmwareUpdateStatus, 0, len(req.Queries))

	for _, query := range req.Queries {
		status := s.getFirmwareUpdateStatus(ctx, query.PmcMacAddress, query.Component)
		statuses = append(statuses, status)
	}

	return &pb.GetFirmwareUpdateStatusResponse{
		Statuses: statuses,
	}, nil
}

// getFirmwareUpdateStatus returns the status of a single firmware update.
func (s *PowershelfManagerServerImpl) getFirmwareUpdateStatus(ctx context.Context, pmcMac string, pbComponent pb.PowershelfComponent) *pb.FirmwareUpdateStatus {
	mac, err := net.ParseMAC(pmcMac)
	if err != nil {
		return &pb.FirmwareUpdateStatus{
			PmcMacAddress: pmcMac,
			Component:     pbComponent,
			Status:        pb.StatusCode_INVALID_ARGUMENT,
			Error:         fmt.Sprintf("invalid MAC address: %v", err),
		}
	}

	component, err := protobuf.ComponentTypeFromMap(pbComponent)
	if err != nil {
		return &pb.FirmwareUpdateStatus{
			PmcMacAddress: pmcMac,
			Component:     pbComponent,
			Status:        pb.StatusCode_INVALID_ARGUMENT,
			Error:         err.Error(),
		}
	}

	update, err := s.psm.GetFirmwareUpdateStatus(ctx, mac, component)
	if err != nil {
		return &pb.FirmwareUpdateStatus{
			PmcMacAddress: pmcMac,
			Component:     pbComponent,
			Status:        pb.StatusCode_INTERNAL_ERROR,
			Error:         err.Error(),
		}
	}

	return protobuf.FirmwareUpdateStatusTo(update, pbComponent)
}

// PowerOff issues a Redfish chassis off action for the PMC's powershelf.
func (s *PowershelfManagerServerImpl) powerOff(ctx context.Context, pmc_mac string) *pb.PowershelfResponse {
	mac, err := net.ParseMAC(pmc_mac)
	if err != nil {
		return &pb.PowershelfResponse{
			PmcMacAddress: pmc_mac,
			Status:        pb.StatusCode_INVALID_ARGUMENT,
			Error:         err.Error(),
		}
	}

	if err := s.psm.PowerOff(ctx, mac); err != nil {
		return &pb.PowershelfResponse{
			PmcMacAddress: pmc_mac,
			Status:        pb.StatusCode_INTERNAL_ERROR,
			Error:         err.Error(),
		}
	}

	return &pb.PowershelfResponse{
		PmcMacAddress: pmc_mac,
		Status:        pb.StatusCode_SUCCESS,
		Error:         "",
	}
}

// PowerOn issues a Redfish chassis on action for the PMC’s powershelf.
func (s *PowershelfManagerServerImpl) powerOn(ctx context.Context, pmc_mac string) *pb.PowershelfResponse {
	mac, err := net.ParseMAC(pmc_mac)
	if err != nil {
		return &pb.PowershelfResponse{
			PmcMacAddress: pmc_mac,
			Status:        pb.StatusCode_INVALID_ARGUMENT,
			Error:         err.Error(),
		}
	}

	if err := s.psm.PowerOn(ctx, mac); err != nil {
		return &pb.PowershelfResponse{
			PmcMacAddress: pmc_mac,
			Status:        pb.StatusCode_INTERNAL_ERROR,
			Error:         err.Error(),
		}
	}

	return &pb.PowershelfResponse{
		PmcMacAddress: pmc_mac,
		Status:        pb.StatusCode_SUCCESS,
		Error:         "",
	}
}

func (s *PowershelfManagerServerImpl) PowerOff(ctx context.Context, req *pb.PowerRequest) (*pb.PowerControlResponse, error) {
	responses := make([]*pb.PowershelfResponse, 0, len(req.PmcMacs)+len(req.Targets))
	for _, mac := range req.PmcMacs {
		responses = append(responses, s.powerOff(ctx, mac))
	}
	for _, target := range req.Targets {
		responses = append(responses, s.powerTarget(ctx, target, false))
	}

	return &pb.PowerControlResponse{
		Responses: responses,
	}, nil
}

func (s *PowershelfManagerServerImpl) PowerOn(ctx context.Context, req *pb.PowerRequest) (*pb.PowerControlResponse, error) {
	responses := make([]*pb.PowershelfResponse, 0, len(req.PmcMacs)+len(req.Targets))
	for _, mac := range req.PmcMacs {
		responses = append(responses, s.powerOn(ctx, mac))
	}
	for _, target := range req.Targets {
		responses = append(responses, s.powerTarget(ctx, target, true))
	}

	return &pb.PowerControlResponse{
		Responses: responses,
	}, nil
}

// powerTarget performs a power action against an unregistered device using inline connection details.
func (s *PowershelfManagerServerImpl) powerTarget(ctx context.Context, target *pb.PowerTarget, on bool) *pb.PowershelfResponse {
	ip := net.ParseIP(target.PmcIp)
	if ip == nil {
		return &pb.PowershelfResponse{
			PmcIp:  target.PmcIp,
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  fmt.Sprintf("invalid PMC IP: %s", target.PmcIp),
		}
	}

	if target.PmcCredentials == nil {
		return &pb.PowershelfResponse{
			PmcIp:  target.PmcIp,
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "credentials are required for power targets",
		}
	}

	if target.PmcCredentials.Username == "" || target.PmcCredentials.Password == "" {
		return &pb.PowershelfResponse{
			PmcIp:  target.PmcIp,
			Status: pb.StatusCode_INVALID_ARGUMENT,
			Error:  "credentials username and password must not be empty",
		}
	}

	cred := credential.New(target.PmcCredentials.Username, target.PmcCredentials.Password)
	p := &pmc.PMC{
		IP:         ip,
		Vendor:     vendor.CodeToVendor(protobuf.PMCVendorFrom(target.PmcVendor)),
		Credential: &cred,
	}

	if err := s.psm.PowerControlDirect(ctx, p, on); err != nil {
		return &pb.PowershelfResponse{
			PmcIp:  target.PmcIp,
			Status: pb.StatusCode_INTERNAL_ERROR,
			Error:  err.Error(),
		}
	}

	return &pb.PowershelfResponse{
		PmcIp:  target.PmcIp,
		Status: pb.StatusCode_SUCCESS,
	}
}

func (s *PowershelfManagerServerImpl) SetDryRun(
	ctx context.Context,
	req *pb.SetDryRunRequest,
) (*emptypb.Empty, error) {
	to := req.DryRun
	log.Printf("SetDryRun to %v", to)
	s.psm.FirmwareManager.SetDryRun(to)
	return &emptypb.Empty{}, nil
}
