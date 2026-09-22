// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"net"
	"net/url"
	"sort"
	"strings"
	"time"

	"github.com/gogo/status"
	"github.com/google/uuid"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/reflection"

	emptypb "google.golang.org/protobuf/types/known/emptypb"
	timestamppb "google.golang.org/protobuf/types/known/timestamppb"

	"github.com/rs/zerolog/log"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var (
	// DefaultPort is the default port that the server listens at
	DefaultPort = ":11079"
	// DefaultVpcId is the default VPC ID for testing
	DefaultVpcId = "00000000-0000-4000-8000-000000000000"
	// DefaultNetworkSegmentId is the default NetworkSegment ID for testing
	DefaultNetworkSegmentId = "00000000-0000-4000-9000-000000000000"
	// DefaultTenantKeysetId is the default TenantKeyset ID for testing
	DefaultTenantKeysetId = "00000000-0000-4000-a000-000000000000"
	// DefaultIBParitionId is the default IBPartition ID for testing
	DefaultIBParitionId = "00000000-0000-4000-b000-000000000000"
)

// NICoServerImpl implements interface NICoServer
type NICoServerImpl struct {
	corev1.UnimplementedForgeServer
	v   map[string]*corev1.Vpc
	ns  map[string]*corev1.NetworkSegment
	ins map[string]*corev1.Instance
	m   map[string]*corev1.Machine
	tk  map[string]*corev1.TenantKeyset
	ibp map[string]*corev1.IBPartition
	em  map[string]*corev1.ExpectedMachine
	eps map[string]*corev1.ExpectedPowerShelf
	es  map[string]*corev1.ExpectedSwitch
	er  map[string]*corev1.ExpectedRack

	// Per-org machine identity state.
	identityState    map[string]*identityOrgState
	tokenDelegations map[string]*corev1.TokenDelegationResponse

	// KEK-rotation simulation state. currentEncryptionKeyID is the key id the mock treats as
	// "current"; reencryptedOrgs records the key id each org's secrets were last re-wrapped to,
	// so re-runs are idempotent (already-on-target rows are skipped).
	currentEncryptionKeyID string
	reencryptedOrgs        map[string]string
}

// identityKeyMaterial is a per-org ES256 keypair plus its derived kid.
type identityKeyMaterial struct {
	privateKey *ecdsa.PrivateKey
	publicPEM  string
	kid        string
}

type identityOrgState struct {
	cfg                  *corev1.TenantIdentityConfigResponse
	slot1                *identityKeyMaterial
	slot2                *identityKeyMaterial
	currentSlot          int
	nonActiveSlotExpires *time.Time
}

func (s *identityOrgState) inactiveSlot() int {
	if s.currentSlot == 1 {
		return 2
	}
	return 1
}

func (s *identityOrgState) activeKey() *identityKeyMaterial {
	if s.currentSlot == 1 {
		return s.slot1
	}
	return s.slot2
}

func (s *identityOrgState) inactiveKey() *identityKeyMaterial {
	if s.currentSlot == 1 {
		return s.slot2
	}
	return s.slot1
}

func (s *identityOrgState) setSlot(n int, km *identityKeyMaterial) {
	if n == 1 {
		s.slot1 = km
	} else {
		s.slot2 = km
	}
}

func (s *identityOrgState) clearInactiveSlot() {
	if s.currentSlot == 1 {
		s.slot2 = nil
	} else {
		s.slot1 = nil
	}
	s.nonActiveSlotExpires = nil
}

func (f *NICoServerImpl) gcExpiredNonActiveSigningKey(orgID string) {
	st, ok := f.identityState[orgID]
	if !ok || st.nonActiveSlotExpires == nil {
		return
	}
	if !time.Now().Before(*st.nonActiveSlotExpires) {
		st.clearInactiveSlot()
	}
}

func tenantIdentitySigningKeysResponse(st *identityOrgState) []*corev1.TenantIdentitySigningKey {
	if st == nil {
		return nil
	}
	var entries []*corev1.TenantIdentitySigningKey
	if active := st.activeKey(); active != nil {
		entries = append(entries, &corev1.TenantIdentitySigningKey{
			Kid:           active.kid,
			Alg:           "ES256",
			CurrentSigner: true,
		})
	}
	if inactive := st.inactiveKey(); inactive != nil {
		entry := &corev1.TenantIdentitySigningKey{
			Kid:           inactive.kid,
			Alg:           "ES256",
			CurrentSigner: false,
		}
		if st.nonActiveSlotExpires != nil {
			entry.ExpireAt = timestamppb.New(*st.nonActiveSlotExpires)
		}
		entries = append(entries, entry)
	}
	sort.SliceStable(entries, func(i, j int) bool {
		return entries[i].GetKid() < entries[j].GetKid()
	})
	return entries
}

var logger = log.With().Str("Component", "Mock NICo gRPC Server").Logger()

// Version implements interface NICoServer
func (f *NICoServerImpl) Version(ctx context.Context, req *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	return &corev1.BuildInfo{
		BuildVersion: "1.0.0",
	}, nil
}

// CreateVpc implements interface NICoServer
func (f *NICoServerImpl) CreateVpc(c context.Context, req *corev1.VpcCreationRequest) (*corev1.Vpc, error) {
	if req == nil || req.Name == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	nid := DefaultVpcId
	_, ok := f.v[DefaultVpcId]
	if ok {
		// Default VPC already exists, create a new one with a different ID
		nid = uuid.NewString()
	}

	nv := &corev1.Vpc{
		Id:   &corev1.VpcId{Value: nid},
		Name: req.Name,
		Config: &corev1.VpcConfig{
			TenantOrganizationId: req.TenantOrganizationId,
		},
	}
	f.v[nid] = nv

	return nv, nil
}

// UpdateVpc implements interface NICoServer
func (f *NICoServerImpl) UpdateVpc(c context.Context, req *corev1.VpcUpdateRequest) (*corev1.VpcUpdateResult, error) {
	if req == nil || req.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	nv, ok := f.v[req.Id.Value]
	if ok {
		if req.Name != "" {
			nv.Name = req.Name
		}
		return &corev1.VpcUpdateResult{}, nil
	}

	return nil, status.Errorf(codes.NotFound, "VPC with ID %q not found", req.Id.Value)
}

// DeleteVpc implements interface NICoServer
func (f *NICoServerImpl) DeleteVpc(c context.Context, req *corev1.VpcDeletionRequest) (*corev1.VpcDeletionResult, error) {
	if req == nil || req.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	_, ok := f.v[req.Id.Value]
	if ok {
		delete(f.v, req.Id.Value)
		return &corev1.VpcDeletionResult{}, nil
	}

	return nil, status.Errorf(codes.NotFound, "VPC with ID %q not found", req.Id.Value)
}

// FindVpcIds implements interface NICoServer
func (f *NICoServerImpl) FindVpcIds(ctx context.Context, req *corev1.VpcSearchFilter) (*corev1.VpcIdList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.VpcIdList{}
	for id := range f.v {
		response.VpcIds = append(response.VpcIds, &corev1.VpcId{Value: id})
	}
	return &response, nil
}

// FindVpcsByIds implements interface NICoServer
func (f *NICoServerImpl) FindVpcsByIds(ctx context.Context, req *corev1.VpcsByIdsRequest) (*corev1.VpcList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.VpcList{}
	for _, id := range req.VpcIds {
		if obj, ok := f.v[id.GetValue()]; ok {
			response.Vpcs = append(response.Vpcs, obj)
		}
	}
	return &response, nil
}

// FindVpcs implements interface NICoServer
func (f *NICoServerImpl) FindVpcs(c context.Context, req *corev1.VpcSearchQuery) (*corev1.VpcList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	res := []*corev1.Vpc{}

	for _, v := range f.v {
		res = append(res, v)
	}

	if req.Id != nil && req.Id.Value != "" {
		v, ok := f.v[req.Id.Value]
		if ok {
			res = []*corev1.Vpc{v}
		} else {
			res = []*corev1.Vpc{}
		}
	}

	if req.Name != nil {
		filtered := []*corev1.Vpc{}
		for _, nv := range f.v {
			if nv.Name == *req.Name {
				filtered = append(filtered, nv)
			}
		}
		res = filtered
	}

	return &corev1.VpcList{Vpcs: res}, nil
}

// CreateNetworkSegment implements interface NICoServer
func (f *NICoServerImpl) CreateNetworkSegment(c context.Context, req *corev1.NetworkSegmentCreationRequest) (*corev1.NetworkSegment, error) {
	if req == nil || req.Name == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	nid := DefaultNetworkSegmentId
	_, ok := f.ns[DefaultNetworkSegmentId]
	if ok {
		// Default Network Segment already exists, create a new one with a different ID
		nid = uuid.NewString()
	}

	nns := &corev1.NetworkSegment{
		Id:          &corev1.NetworkSegmentId{Value: nid},
		Name:        req.Name,
		VpcId:       req.VpcId,
		SubdomainId: req.SubdomainId,
		Mtu:         req.Mtu,
		Prefixes:    req.Prefixes,
	}
	f.ns[nid] = nns

	return nns, nil
}

// DeleteNetworkSegment implements interface NICoServer
func (f *NICoServerImpl) DeleteNetworkSegment(c context.Context, req *corev1.NetworkSegmentDeletionRequest) (*corev1.NetworkSegmentDeletionResult, error) {
	if req == nil || req.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	_, ok := f.ns[req.Id.Value]

	if ok {
		delete(f.ns, req.Id.Value)
		return &corev1.NetworkSegmentDeletionResult{}, nil
	}

	return nil, status.Errorf(codes.NotFound, "NetworkSegment with ID %q not found", req.Id.Value)
}

// FindNetworkSegmentIds implements interface NICoServer
func (f *NICoServerImpl) FindNetworkSegmentIds(ctx context.Context, req *corev1.NetworkSegmentSearchFilter) (*corev1.NetworkSegmentIdList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.NetworkSegmentIdList{}
	for id := range f.ns {
		response.NetworkSegmentsIds = append(response.NetworkSegmentsIds, &corev1.NetworkSegmentId{Value: id})
	}
	return &response, nil
}

// FindNetworkSegmentsByIds implements interface NICoServer
func (f *NICoServerImpl) FindNetworkSegmentsByIds(ctx context.Context, req *corev1.NetworkSegmentsByIdsRequest) (*corev1.NetworkSegmentList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.NetworkSegmentList{}
	for _, id := range req.NetworkSegmentsIds {
		if obj, ok := f.ns[id.GetValue()]; ok {
			response.NetworkSegments = append(response.NetworkSegments, obj)
		}
	}
	return &response, nil
}

// CreateInstance implements interface NICoServer
func (f *NICoServerImpl) AllocateInstance(ctx context.Context, req *corev1.InstanceAllocationRequest) (*corev1.Instance, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	nid := uuid.NewString()
	if req.InstanceId != nil {
		nid = req.InstanceId.Value
	}

	_, ok := f.ins[nid]
	if !ok {
		ifcsts := []*corev1.InstanceInterfaceStatus{}
		for _, ifcreq := range req.Config.Network.Interfaces {
			ifcst := &corev1.InstanceInterfaceStatus{
				MacAddress: getStrPtr(generateMacAddress()),
				Addresses: []string{
					generateIPAddress(),
				},
			}
			if ifcreq.FunctionType == corev1.InterfaceFunctionType_VIRTUAL_FUNCTION {
				vfid := uint32(generateInteger(16))
				ifcst.VirtualFunctionId = &vfid
			}
			ifcsts = append(ifcsts, ifcst)
		}

		nins := corev1.Instance{
			Id:        &corev1.InstanceId{Value: nid},
			MachineId: req.MachineId,
			Config:    req.Config,
			Status: &corev1.InstanceStatus{
				Tenant: &corev1.InstanceTenantStatus{
					State: corev1.TenantState_PROVISIONING,
				},
				Network: &corev1.InstanceNetworkStatus{
					Interfaces: ifcsts,
				},
			},
		}

		f.ins[nid] = &nins

		m := corev1.Machine{
			Id:    req.MachineId,
			State: "Ready",
		}

		_, ok := f.m[req.MachineId.Id]
		if !ok {
			f.m[req.MachineId.Id] = &m
		}

		return &nins, nil
	}

	return nil, status.Errorf(codes.Internal, "Failed to create Instance")
}

// DeleteInstance implements interface NICoServer
func (f *NICoServerImpl) ReleaseInstance(c context.Context, req *corev1.InstanceReleaseRequest) (*corev1.InstanceReleaseResult, error) {
	if req == nil || req.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	_, ok := f.ins[req.Id.Value]
	if ok {
		delete(f.ins, req.Id.Value)
		return &corev1.InstanceReleaseResult{}, nil
	}

	return nil, status.Errorf(codes.NotFound, "Instance with ID %q not found", req.Id.Value)
}

// FindInstances implements interface NICoServer
func (f *NICoServerImpl) FindInstanceIds(ctx context.Context, req *corev1.InstanceSearchFilter) (*corev1.InstanceIdList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.InstanceIdList{}
	for id := range f.ins {
		response.InstanceIds = append(response.InstanceIds, &corev1.InstanceId{Value: id})
	}
	return &response, nil
}

// FindInstances implements interface NICoServer
func (f *NICoServerImpl) FindInstancesByIds(ctx context.Context, req *corev1.InstancesByIdsRequest) (*corev1.InstanceList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.InstanceList{}
	for _, id := range req.InstanceIds {
		if obj, ok := f.ins[id.GetValue()]; ok {
			response.Instances = append(response.Instances, obj)
		}
	}
	return &response, nil
}

// InvokeInstancePower implements interface NICoServer
func (f *NICoServerImpl) InvokeInstancePower(c context.Context, req *corev1.InstancePowerRequest) (*corev1.InstancePowerResult, error) {
	if req == nil || req.GetInstanceId().GetValue() == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	instanceID := req.GetInstanceId().GetValue()
	_, ok := f.ins[instanceID]
	if ok {
		if req.Operation == corev1.InstancePowerRequest_POWER_RESET {
			return &corev1.InstancePowerResult{}, nil
		}

		return &corev1.InstancePowerResult{}, status.Errorf(codes.InvalidArgument, "Invalid operation in request")
	}

	return nil, status.Errorf(codes.NotFound, "Instance with ID %q not found", instanceID)
}

func (f *NICoServerImpl) FindMachineIds(ctx context.Context, req *corev1.MachineSearchConfig) (*corev1.MachineIdList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.MachineIdList{}
	for id := range f.m {
		response.MachineIds = append(response.MachineIds, &corev1.MachineId{Id: id})
	}

	return &response, nil
}

func (f *NICoServerImpl) SetMaintenance(context.Context, *corev1.MaintenanceRequest) (*emptypb.Empty, error) {
	return &emptypb.Empty{}, nil
}

// CreateTenantKeyset implements interface NICoServer
func (f *NICoServerImpl) CreateTenantKeyset(c context.Context, req *corev1.CreateTenantKeysetRequest) (*corev1.CreateTenantKeysetResponse, error) {
	if req == nil || req.KeysetIdentifier == nil || req.KeysetIdentifier.KeysetId == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	nid := DefaultTenantKeysetId
	_, ok := f.tk[DefaultTenantKeysetId]
	if ok {
		// Default TenantKeyset already exists, create a new one with a different ID
		nid = uuid.NewString()
	}

	ntk := &corev1.TenantKeyset{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			KeysetId: nid,
		},
		KeysetContent: req.KeysetContent,
		Version:       req.Version,
	}

	f.tk[nid] = ntk

	result := &corev1.CreateTenantKeysetResponse{
		Keyset: ntk,
	}

	return result, nil
}

// UpdateTenantKeyset implements interface NICoServer
func (f *NICoServerImpl) UpdateTenantKeyset(c context.Context, req *corev1.UpdateTenantKeysetRequest) (*corev1.UpdateTenantKeysetResponse, error) {
	if req == nil || req.KeysetIdentifier == nil || req.KeysetIdentifier.KeysetId == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	eid := req.KeysetIdentifier.KeysetId

	_, ok := f.tk[eid]
	if ok {
		f.tk[eid].KeysetContent = req.KeysetContent
		f.tk[eid].Version = req.Version

		return &corev1.UpdateTenantKeysetResponse{}, nil
	}

	return nil, status.Errorf(codes.Internal, "TenantKeyset with ID not found")
}

// DeleteTenantKeyset implements interface NICoServer
func (f *NICoServerImpl) DeleteTenantKeyset(c context.Context, req *corev1.DeleteTenantKeysetRequest) (*corev1.DeleteTenantKeysetResponse, error) {
	if req == nil || req.KeysetIdentifier == nil || req.KeysetIdentifier.KeysetId == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	eid := req.KeysetIdentifier.KeysetId

	_, ok := f.tk[eid]
	if ok {
		delete(f.tk, eid)
		return &corev1.DeleteTenantKeysetResponse{}, nil
	}

	return nil, status.Errorf(codes.NotFound, "TenantKeyset with ID %q not found", eid)
}

// FindTenantKeysetIds implements interface NICoServer
func (f *NICoServerImpl) FindTenantKeysetIds(ctx context.Context, req *corev1.TenantKeysetSearchFilter) (*corev1.TenantKeysetIdList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.TenantKeysetIdList{}
	for id := range f.tk {
		response.KeysetIds = append(response.KeysetIds, &corev1.TenantKeysetIdentifier{KeysetId: id})
	}
	return &response, nil
}

// FindTenantKeysetsByIds implements interface NICoServer
func (f *NICoServerImpl) FindTenantKeysetsByIds(ctx context.Context, req *corev1.TenantKeysetsByIdsRequest) (*corev1.TenantKeySetList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.TenantKeySetList{}
	for _, id := range req.KeysetIds {
		if obj, ok := f.tk[id.KeysetId]; ok {
			response.Keyset = append(response.Keyset, obj)
		}
	}
	return &response, nil
}

// CreateIBPartition implements interface NICoServer
func (f *NICoServerImpl) CreateIBPartition(c context.Context, req *corev1.IBPartitionCreationRequest) (*corev1.IBPartition, error) {
	if req == nil || req.Config == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	nid := DefaultIBParitionId
	_, ok := f.ibp[DefaultNetworkSegmentId]
	if ok {
		// Default IBPartition already exists, create a new one with a different ID
		nid = uuid.NewString()
	}

	nibp := &corev1.IBPartition{
		Id: &corev1.IBPartitionId{Value: nid},
		Config: &corev1.IBPartitionConfig{
			Name:                 req.Config.Name,
			TenantOrganizationId: req.Config.TenantOrganizationId,
		},
	}

	f.ibp[nid] = nibp
	return nibp, nil
}

// UpdateIBPartition implements interface NICoServer
func (f *NICoServerImpl) UpdateIBPartition(c context.Context, req *corev1.IBPartitionUpdateRequest) (*corev1.IBPartition, error) {
	if req == nil || req.Id == nil || req.Id.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	ibp, ok := f.ibp[req.Id.Value]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "IB Partition with ID %q not found", req.Id.Value)
	}
	if req.Config != nil {
		if ibp.Config == nil {
			ibp.Config = &corev1.IBPartitionConfig{}
		}
		if req.Config.Name != "" {
			ibp.Config.Name = req.Config.Name
		}
		if req.Config.TenantOrganizationId != "" {
			ibp.Config.TenantOrganizationId = req.Config.TenantOrganizationId
		}
		if req.Config.Pkey != nil {
			ibp.Config.Pkey = req.Config.Pkey
		}
	}
	if req.Metadata != nil {
		ibp.Metadata = req.Metadata
	}
	return ibp, nil
}

// DeleteIBPartition implements interface NICoServer
func (f *NICoServerImpl) DeleteIBPartition(c context.Context, req *corev1.IBPartitionDeletionRequest) (*corev1.IBPartitionDeletionResult, error) {
	if req == nil || req.Id == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}

	_, ok := f.ibp[req.Id.Value]

	if ok {
		delete(f.ibp, req.Id.Value)
		return &corev1.IBPartitionDeletionResult{}, nil
	}

	return nil, status.Errorf(codes.NotFound, "IB Partition with ID %q not found", req.Id.Value)
}

// FindIBPartitionIds implements interface NICoServer
func (f *NICoServerImpl) FindIBPartitionIds(ctx context.Context, req *corev1.IBPartitionSearchFilter) (*corev1.IBPartitionIdList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.IBPartitionIdList{}
	for id := range f.ibp {
		response.IbPartitionIds = append(response.IbPartitionIds, &corev1.IBPartitionId{Value: id})
	}
	return &response, nil
}

// FindIBPartitionsByIds implements interface NICoServer
func (f *NICoServerImpl) FindIBPartitionsByIds(ctx context.Context, req *corev1.IBPartitionsByIdsRequest) (*corev1.IBPartitionList, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	response := corev1.IBPartitionList{}
	for _, id := range req.IbPartitionIds {
		if obj, ok := f.ibp[id.GetValue()]; ok {
			response.IbPartitions = append(response.IbPartitions, obj)
		}
	}
	return &response, nil
}

// AddExpectedMachine implements interface NICoServer
func (f *NICoServerImpl) AddExpectedMachine(ctx context.Context, req *corev1.ExpectedMachine) (*emptypb.Empty, error) {
	if req == nil || req.Id == nil || req.Id.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for AddExpectedMachine")
	}
	if req.BmcMacAddress == "" {
		return nil, status.Errorf(codes.InvalidArgument, "MAC address not provided for AddExpectedMachine")
	}
	if req.ChassisSerialNumber == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Chassis Serial Number not provided for AddExpectedMachine")
	}
	f.em[req.Id.Value] = req
	return &emptypb.Empty{}, nil
}

// UpdateExpectedMachine implements interface NICoServer
func (f *NICoServerImpl) UpdateExpectedMachine(ctx context.Context, req *corev1.ExpectedMachine) (*emptypb.Empty, error) {
	if req == nil || req.Id == nil || req.Id.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for UpdateExpectedMachine")
	}
	if req.BmcMacAddress == "" {
		return nil, status.Errorf(codes.InvalidArgument, "MAC address not provided for UpdateExpectedMachine")
	}
	if req.ChassisSerialNumber == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Chassis Serial Number not provided for UpdateExpectedMachine")
	}
	if _, ok := f.em[req.Id.Value]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedMachine with ID %q not found", req.Id.Value)
	}
	f.em[req.Id.Value] = req
	return &emptypb.Empty{}, nil
}

// DeleteExpectedMachine implements interface NICoServer
func (f *NICoServerImpl) DeleteExpectedMachine(ctx context.Context, req *corev1.ExpectedMachineRequest) (*emptypb.Empty, error) {
	if req == nil || req.Id == nil || req.Id.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for DeleteExpectedMachine")
	}
	if _, ok := f.em[req.Id.Value]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedMachine with ID %q not found", req.Id.Value)
	}
	delete(f.em, req.Id.Value)
	return &emptypb.Empty{}, nil
}

// CreateExpectedMachines implements interface NICoServer
func (f *NICoServerImpl) CreateExpectedMachines(ctx context.Context, req *corev1.BatchExpectedMachineOperationRequest) (*corev1.BatchExpectedMachineOperationResponse, error) {
	if req == nil || req.GetExpectedMachines() == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request for CreateExpectedMachines")
	}
	emList := req.GetExpectedMachines().GetExpectedMachines()
	out := &corev1.BatchExpectedMachineOperationResponse{
		Results: make([]*corev1.ExpectedMachineOperationResult, 0, len(emList)),
	}
	for _, em := range emList {
		if em == nil {
			msg := "nil expected machine entry"
			out.Results = append(out.Results, &corev1.ExpectedMachineOperationResult{
				Success:         false,
				ErrorMessage:    &msg,
				ExpectedMachine: nil,
			})
			continue
		}
		result := &corev1.ExpectedMachineOperationResult{
			Id:              em.Id,
			Success:         true,
			ExpectedMachine: em,
		}
		if em.GetId() == nil || em.GetId().GetValue() == "" {
			result.Success = false
			msg := "ID not provided"
			result.ErrorMessage = &msg
			result.ExpectedMachine = nil
		} else if em.GetBmcMacAddress() == "" {
			result.Success = false
			msg := "MAC address not provided"
			result.ErrorMessage = &msg
			result.ExpectedMachine = nil
		} else if em.GetChassisSerialNumber() == "" {
			result.Success = false
			msg := "Chassis Serial Number not provided"
			result.ErrorMessage = &msg
			result.ExpectedMachine = nil
		} else {
			f.em[em.Id.Value] = em
		}
		out.Results = append(out.Results, result)
	}
	return out, nil
}

// UpdateExpectedMachines implements interface NICoServer
func (f *NICoServerImpl) UpdateExpectedMachines(ctx context.Context, req *corev1.BatchExpectedMachineOperationRequest) (*corev1.BatchExpectedMachineOperationResponse, error) {
	if req == nil || req.GetExpectedMachines() == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request for UpdateExpectedMachines")
	}
	emList := req.GetExpectedMachines().GetExpectedMachines()
	out := &corev1.BatchExpectedMachineOperationResponse{
		Results: make([]*corev1.ExpectedMachineOperationResult, 0, len(emList)),
	}
	for _, em := range emList {
		if em == nil {
			msg := "nil expected machine entry"
			out.Results = append(out.Results, &corev1.ExpectedMachineOperationResult{
				Success:         false,
				ErrorMessage:    &msg,
				ExpectedMachine: nil,
			})
			continue
		}
		result := &corev1.ExpectedMachineOperationResult{
			Id:              em.Id,
			Success:         true,
			ExpectedMachine: em,
		}
		if em.GetId() == nil || em.GetId().GetValue() == "" {
			result.Success = false
			msg := "ID not provided"
			result.ErrorMessage = &msg
			result.ExpectedMachine = nil
		} else if em.GetBmcMacAddress() == "" {
			result.Success = false
			msg := "MAC address not provided"
			result.ErrorMessage = &msg
			result.ExpectedMachine = nil
		} else if em.GetChassisSerialNumber() == "" {
			result.Success = false
			msg := "Chassis Serial Number not provided"
			result.ErrorMessage = &msg
			result.ExpectedMachine = nil
		} else if _, ok := f.em[em.Id.Value]; !ok {
			result.Success = false
			msg := fmt.Sprintf("ExpectedMachine with ID %q not found", em.Id.Value)
			result.ErrorMessage = &msg
			result.ExpectedMachine = nil
		} else {
			f.em[em.Id.Value] = em
		}
		out.Results = append(out.Results, result)
	}
	return out, nil
}

// AddExpectedPowerShelf implements interface NICoServer
func (f *NICoServerImpl) AddExpectedPowerShelf(ctx context.Context, req *corev1.ExpectedPowerShelf) (*emptypb.Empty, error) {
	if req == nil || req.ExpectedPowerShelfId == nil || req.ExpectedPowerShelfId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for AddExpectedPowerShelf")
	}
	if req.BmcMacAddress == "" {
		return nil, status.Errorf(codes.InvalidArgument, "MAC address not provided for AddExpectedPowerShelf")
	}
	if req.ShelfSerialNumber == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Shelf Serial Number not provided for AddExpectedPowerShelf")
	}
	f.eps[req.ExpectedPowerShelfId.Value] = req
	return &emptypb.Empty{}, nil
}

// UpdateExpectedPowerShelf implements interface NICoServer
func (f *NICoServerImpl) UpdateExpectedPowerShelf(ctx context.Context, req *corev1.ExpectedPowerShelf) (*emptypb.Empty, error) {
	if req == nil || req.ExpectedPowerShelfId == nil || req.ExpectedPowerShelfId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for UpdateExpectedPowerShelf")
	}
	if req.BmcMacAddress == "" {
		return nil, status.Errorf(codes.InvalidArgument, "MAC address not provided for UpdateExpectedPowerShelf")
	}
	if req.ShelfSerialNumber == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Shelf Serial Number not provided for UpdateExpectedPowerShelf")
	}
	if _, ok := f.eps[req.ExpectedPowerShelfId.Value]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedPowerShelf with ID %q not found", req.ExpectedPowerShelfId.Value)
	}
	f.eps[req.ExpectedPowerShelfId.Value] = req
	return &emptypb.Empty{}, nil
}

// DeleteExpectedPowerShelf implements interface NICoServer
func (f *NICoServerImpl) DeleteExpectedPowerShelf(ctx context.Context, req *corev1.ExpectedPowerShelfRequest) (*emptypb.Empty, error) {
	if req == nil || req.ExpectedPowerShelfId == nil || req.ExpectedPowerShelfId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for DeleteExpectedPowerShelf")
	}
	if _, ok := f.eps[req.ExpectedPowerShelfId.Value]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedPowerShelf with ID %q not found", req.ExpectedPowerShelfId.Value)
	}
	delete(f.eps, req.ExpectedPowerShelfId.Value)
	return &emptypb.Empty{}, nil
}

// GetExpectedPowerShelf implements interface NICoServer
func (f *NICoServerImpl) GetExpectedPowerShelf(ctx context.Context, req *corev1.ExpectedPowerShelfRequest) (*corev1.ExpectedPowerShelf, error) {
	if req == nil || req.ExpectedPowerShelfId == nil || req.ExpectedPowerShelfId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for GetExpectedPowerShelf")
	}
	eps, ok := f.eps[req.ExpectedPowerShelfId.Value]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedPowerShelf with ID %q not found", req.ExpectedPowerShelfId.Value)
	}
	return eps, nil
}

// GetAllExpectedPowerShelves implements interface NICoServer
func (f *NICoServerImpl) GetAllExpectedPowerShelves(ctx context.Context, req *emptypb.Empty) (*corev1.ExpectedPowerShelfList, error) {
	res := make([]*corev1.ExpectedPowerShelf, 0, len(f.eps))
	for _, eps := range f.eps {
		res = append(res, eps)
	}
	return &corev1.ExpectedPowerShelfList{ExpectedPowerShelves: res}, nil
}

// AddExpectedSwitch implements interface NICoServer
func (f *NICoServerImpl) AddExpectedSwitch(ctx context.Context, req *corev1.ExpectedSwitch) (*emptypb.Empty, error) {
	if req == nil || req.ExpectedSwitchId == nil || req.ExpectedSwitchId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for AddExpectedSwitch")
	}
	if req.BmcMacAddress == "" {
		return nil, status.Errorf(codes.InvalidArgument, "MAC address not provided for AddExpectedSwitch")
	}
	if req.SwitchSerialNumber == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Switch Serial Number not provided for AddExpectedSwitch")
	}
	f.es[req.ExpectedSwitchId.Value] = req
	return &emptypb.Empty{}, nil
}

// UpdateExpectedSwitch implements interface NICoServer
func (f *NICoServerImpl) UpdateExpectedSwitch(ctx context.Context, req *corev1.ExpectedSwitch) (*emptypb.Empty, error) {
	if req == nil || req.ExpectedSwitchId == nil || req.ExpectedSwitchId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for UpdateExpectedSwitch")
	}
	if req.BmcMacAddress == "" {
		return nil, status.Errorf(codes.InvalidArgument, "MAC address not provided for UpdateExpectedSwitch")
	}
	if req.SwitchSerialNumber == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Switch Serial Number not provided for UpdateExpectedSwitch")
	}
	if _, ok := f.es[req.ExpectedSwitchId.Value]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedSwitch with ID %q not found", req.ExpectedSwitchId.Value)
	}
	f.es[req.ExpectedSwitchId.Value] = req
	return &emptypb.Empty{}, nil
}

// DeleteExpectedSwitch implements interface NICoServer
func (f *NICoServerImpl) DeleteExpectedSwitch(ctx context.Context, req *corev1.ExpectedSwitchRequest) (*emptypb.Empty, error) {
	if req == nil || req.ExpectedSwitchId == nil || req.ExpectedSwitchId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for DeleteExpectedSwitch")
	}
	if _, ok := f.es[req.ExpectedSwitchId.Value]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedSwitch with ID %q not found", req.ExpectedSwitchId.Value)
	}
	delete(f.es, req.ExpectedSwitchId.Value)
	return &emptypb.Empty{}, nil
}

// GetExpectedSwitch implements interface NICoServer
func (f *NICoServerImpl) GetExpectedSwitch(ctx context.Context, req *corev1.ExpectedSwitchRequest) (*corev1.ExpectedSwitch, error) {
	if req == nil || req.ExpectedSwitchId == nil || req.ExpectedSwitchId.Value == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for GetExpectedSwitch")
	}
	es, ok := f.es[req.ExpectedSwitchId.Value]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedSwitch with ID %q not found", req.ExpectedSwitchId.Value)
	}
	return es, nil
}

// GetAllExpectedSwitches implements interface NICoServer
func (f *NICoServerImpl) GetAllExpectedSwitches(ctx context.Context, req *emptypb.Empty) (*corev1.ExpectedSwitchList, error) {
	res := make([]*corev1.ExpectedSwitch, 0, len(f.es))
	for _, es := range f.es {
		res = append(res, es)
	}
	return &corev1.ExpectedSwitchList{ExpectedSwitches: res}, nil
}

// AddExpectedRack implements interface NICoServer
func (f *NICoServerImpl) AddExpectedRack(ctx context.Context, req *corev1.ExpectedRack) (*emptypb.Empty, error) {
	if req == nil || req.RackId == nil || req.RackId.Id == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for AddExpectedRack")
	}
	if req.RackProfileId == nil || req.RackProfileId.Id == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Rack Profile ID not provided for AddExpectedRack")
	}
	f.er[req.RackId.Id] = req
	return &emptypb.Empty{}, nil
}

// UpdateExpectedRack implements interface NICoServer
func (f *NICoServerImpl) UpdateExpectedRack(ctx context.Context, req *corev1.ExpectedRack) (*emptypb.Empty, error) {
	if req == nil || req.RackId == nil || req.RackId.Id == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for UpdateExpectedRack")
	}
	if req.RackProfileId == nil || req.RackProfileId.Id == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Rack Profile ID not provided for UpdateExpectedRack")
	}
	if _, ok := f.er[req.RackId.Id]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedRack with ID %q not found", req.RackId.Id)
	}
	f.er[req.RackId.Id] = req
	return &emptypb.Empty{}, nil
}

// DeleteExpectedRack implements interface NICoServer
func (f *NICoServerImpl) DeleteExpectedRack(ctx context.Context, req *corev1.ExpectedRackRequest) (*emptypb.Empty, error) {
	if req == nil || req.RackId == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for DeleteExpectedRack")
	}
	if _, ok := f.er[req.RackId]; !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedRack with ID %q not found", req.RackId)
	}
	delete(f.er, req.RackId)
	return &emptypb.Empty{}, nil
}

// GetExpectedRack implements interface NICoServer
func (f *NICoServerImpl) GetExpectedRack(ctx context.Context, req *corev1.ExpectedRackRequest) (*corev1.ExpectedRack, error) {
	if req == nil || req.RackId == "" {
		return nil, status.Errorf(codes.InvalidArgument, "ID not provided for GetExpectedRack")
	}
	er, ok := f.er[req.RackId]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "ExpectedRack with ID %q not found", req.RackId)
	}
	return er, nil
}

// GetAllExpectedRacks implements interface NICoServer
func (f *NICoServerImpl) GetAllExpectedRacks(ctx context.Context, req *emptypb.Empty) (*corev1.ExpectedRackList, error) {
	res := make([]*corev1.ExpectedRack, 0, len(f.er))
	for _, er := range f.er {
		res = append(res, er)
	}
	return &corev1.ExpectedRackList{ExpectedRacks: res}, nil
}

// ReplaceAllExpectedRacks implements interface NICoServer
func (f *NICoServerImpl) ReplaceAllExpectedRacks(ctx context.Context, req *corev1.ExpectedRackList) (*emptypb.Empty, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	for _, er := range req.ExpectedRacks {
		if er == nil || er.RackId == nil || er.RackId.Id == "" {
			return nil, status.Errorf(codes.InvalidArgument, "ID not provided for ReplaceAllExpectedRacks")
		}
		if er.RackProfileId == nil || er.RackProfileId.Id == "" {
			return nil, status.Errorf(codes.InvalidArgument, "Rack Profile ID not provided for ReplaceAllExpectedRacks")
		}
	}
	f.er = make(map[string]*corev1.ExpectedRack)
	for _, er := range req.ExpectedRacks {
		f.er[er.RackId.Id] = er
	}
	return &emptypb.Empty{}, nil
}

// DeleteAllExpectedRacks implements interface NICoServer
func (f *NICoServerImpl) DeleteAllExpectedRacks(ctx context.Context, req *emptypb.Empty) (*emptypb.Empty, error) {
	f.er = make(map[string]*corev1.ExpectedRack)
	return &emptypb.Empty{}, nil
}

// LoadTestMachines loads test machines into the server
func (f *NICoServerImpl) LoadTestMachines() {
	nid := uuid.NewString()

	var memSize uint32 = 16384

	f.m[nid] = &corev1.Machine{
		Id:    &corev1.MachineId{Id: nid},
		State: "Ready",
		Status: &corev1.MachineStatus{
			Interfaces: []*corev1.MachineInterface{
				{
					Id:                   &corev1.MachineInterfaceId{Value: uuid.NewString()},
					AttachedDpuMachineId: &corev1.MachineId{Id: uuid.NewString()},
					MachineId:            &corev1.MachineId{Id: nid},
					SegmentId:            &corev1.NetworkSegmentId{Value: uuid.NewString()},
					Hostname:             "nico.nvidia.com",
					PrimaryInterface:     true,
					MacAddress:           generateMacAddress(),
					Address:              []string{generateIPAddress()},
				},
			},
			DiscoveryInfo: &corev1.DiscoveryInfo{
				NetworkInterfaces: []*corev1.NetworkInterface{
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "0x14e4",
							Device:      "0x165f",
							Path:        "/devices/pci0000:00/0000:00:1c.5/0000:04:00.0/net/eno8303",
							Description: getStrPtr("NetXtreme BCM5720 2-port Gigabit Ethernet PCIe (PowerEdge Rx5xx LOM Board)"),
						},
					},
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "0x14e4",
							Device:      "0x165f",
							Path:        "/devices/pci0000:00/0000:00:1c.5/0000:04:00.1/net/eno8403",
							Description: getStrPtr("NetXtreme BCM5720 2-port Gigabit Ethernet PCIe (PowerEdge Rx5xx LOM Board)"),
						},
					},
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "0x14e4",
							Device:      "0x16d7",
							Path:        "/devices/pci0000:30/0000:30:04.0/0000:31:00.0/net/eno12399np0",
							Description: getStrPtr("BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller"),
						},
					},
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "0x14e4",
							Device:      "0x16d7",
							Path:        "/devices/pci0000:30/0000:30:04.0/0000:31:00.1/net/eno12409np1",
							Description: getStrPtr("BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller"),
						},
					},
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "0x15b3",
							Device:      "0xa2d6",
							Path:        "/devices/pci0000:b0/0000:b0:02.0/0000:b1:00.0/net/enp177s0f0np0",
							NumaNode:    1,
							Description: getStrPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
						},
					},
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "0x15b3",
							Device:      "0xa2d6",
							Path:        "/devices/pci0000:b0/0000:b0:02.0/0000:b1:00.1/net/enp177s0f1np1",
							NumaNode:    1,
							Description: getStrPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
						},
					},
				},
				BlockDevices: []*corev1.BlockDevice{
					{
						Model:    "NO_MODEL",
						Revision: "NO_REVISION",
					},
					{
						Model:    "LOGICAL_VOLUME",
						Revision: "3.53",
						Serial:   "600508b1001cb4d1a278bf3ee7a72228",
					},
					{
						Model:    "Dell Ent NVMe CM6 RI 1.92TB",
						Revision: "2.1.3",
					},
					{
						Model:    "SSDPF2KE016T9L",
						Revision: "2CV1L028",
					},
					{
						Model:    "DELLBOSS_VD",
						Revision: "MV.R00-0",
					},
				},
				DmiData: &corev1.DmiData{
					BoardName:     "7Z23CTOLWW",
					BoardVersion:  "06",
					BiosVersion:   "U8E122J-1.51",
					ProductSerial: "J1050ACR",
					BoardSerial:   ".C1KS2CS001G.",
					ChassisSerial: "J1050ACR",
					BiosDate:      "03/30/2023",
					ProductName:   "ThinkSystem SR670 V2",
					SysVendor:     "Lenovo",
				},
				NvmeDevices: []*corev1.NvmeDevice{
					{
						Model:       "Dell Ent NVMe CM6 RI 1.92TB",
						FirmwareRev: "2.1.3",
					},
					{
						Model:       "Dell Ent NVMe CM6 RI 1.92TB",
						FirmwareRev: "2.1.3",
					},
					{
						Model:       "Dell Ent NVMe CM6 RI 1.92TB",
						FirmwareRev: "2.1.3",
					},
				},
				Gpus: []*corev1.Gpu{
					{
						Name:           "NVIDIA H100 PCIe",
						Serial:         "1654422005434",
						DriverVersion:  "530.30.02",
						VbiosVersion:   "96.00.30.00.01",
						InforomVersion: "1010.0200.00.02",
						TotalMemory:    "81559 MiB",
						Frequency:      "1755 MHz",
						PciBusId:       "00000000:17:00.0",
					},
				},
				MemoryDevices: []*corev1.MemoryDevice{
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  &memSize,
						MemType: getStrPtr("DDR4"),
					},
					{
						SizeMb:  nil,
						MemType: getStrPtr("UNKNOWN"),
					},
				},
				InfinibandInterfaces: []*corev1.InfinibandInterface{
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "Mellanox Technologies",
							Device:      "MT28908 Family [ConnectX-6]",
							Path:        "/devices/pci0000:c9/0000:c9:02.0/0000:ca:00.0/infiniband/rocep202s0f0",
							NumaNode:    1,
							Description: getStrPtr("MT28908 Family [ConnectX-6]"),
							Slot:        getStrPtr("0000:ca:00.0"),
						},
						Guid: "1070fd0300bd43ac",
					},
					{
						PciProperties: &corev1.PciDeviceProperties{
							Vendor:      "Mellanox Technologies",
							Device:      "MT28908 Family [ConnectX-6]",
							Path:        "/devices/pci0000:c9/0000:c9:02.0/0000:ca:00.1/infiniband/rocep202s0f1",
							NumaNode:    1,
							Description: getStrPtr("MT28908 Family [ConnectX-6]"),
							Slot:        getStrPtr("0000:ca:00.1"),
						},
						Guid: "1070fd0300bd43ad",
					},
				},
			},
		},
	}
}

// ~~~~~ Machine Identity mock methods ~~~~~ //

const (
	jwtESAlg          = "ES256"
	p256CoordinateLen = 32
)

// generateES256KeyMaterial returns a fresh P-256 keypair with derived kid.
func generateES256KeyMaterial() (*identityKeyMaterial, error) {
	priv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, fmt.Errorf("generate P-256 key: %w", err)
	}
	spki, err := x509.MarshalPKIXPublicKey(&priv.PublicKey)
	if err != nil {
		return nil, fmt.Errorf("marshal SPKI: %w", err)
	}
	publicPEM := string(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: spki}))
	sum := sha256.Sum256([]byte(publicPEM))
	return &identityKeyMaterial{
		privateKey: priv,
		publicPEM:  publicPEM,
		kid:        hex.EncodeToString(sum[:]),
	}, nil
}

// jwksDocumentForKey returns a one-key JWKS JSON document.
func jwksDocumentForKey(km *identityKeyMaterial, use string) (string, error) {
	return jwksDocumentForKeys([]*identityKeyMaterial{km}, use)
}

// jwksDocumentForKeys returns a JWKS JSON document for the supplied keys
// (two during a rotation overlap window; one in steady state).
func jwksDocumentForKeys(kms []*identityKeyMaterial, use string) (string, error) {
	jwks := make([]map[string]string, 0, len(kms))
	for _, km := range kms {
		if km == nil || km.privateKey == nil {
			return "", fmt.Errorf("nil key material")
		}
		pub := km.privateKey.PublicKey
		xb := pub.X.FillBytes(make([]byte, p256CoordinateLen))
		yb := pub.Y.FillBytes(make([]byte, p256CoordinateLen))
		jwks = append(jwks, map[string]string{
			"kty": "EC",
			"crv": "P-256",
			"alg": jwtESAlg,
			"use": use,
			"kid": km.kid,
			"x":   base64.RawURLEncoding.EncodeToString(xb),
			"y":   base64.RawURLEncoding.EncodeToString(yb),
		})
	}
	out, err := json.Marshal(map[string]any{"keys": jwks})
	if err != nil {
		return "", err
	}
	return string(out), nil
}

// signES256JWT returns a compact-serialized ES256 JWS.
func signES256JWT(priv *ecdsa.PrivateKey, kid string, claims map[string]any) (string, error) {
	header := map[string]string{"alg": jwtESAlg, "kid": kid, "typ": "JWT"}
	hb, err := json.Marshal(header)
	if err != nil {
		return "", fmt.Errorf("marshal JWT header: %w", err)
	}
	cb, err := json.Marshal(claims)
	if err != nil {
		return "", fmt.Errorf("marshal JWT claims: %w", err)
	}
	signingInput := base64.RawURLEncoding.EncodeToString(hb) + "." +
		base64.RawURLEncoding.EncodeToString(cb)
	digest := sha256.Sum256([]byte(signingInput))
	r, s, err := ecdsa.Sign(rand.Reader, priv, digest[:])
	if err != nil {
		return "", fmt.Errorf("ecdsa sign: %w", err)
	}
	sig := make([]byte, 2*p256CoordinateLen)
	r.FillBytes(sig[:p256CoordinateLen])
	s.FillBytes(sig[p256CoordinateLen:])
	return signingInput + "." + base64.RawURLEncoding.EncodeToString(sig), nil
}

// clientSecretDisplayHash returns the truncated SHA-256 display form.
func clientSecretDisplayHash(secret string) string {
	sum := sha256.Sum256([]byte(secret))
	full := hex.EncodeToString(sum[:])
	if len(full) >= 8 {
		return "sha256:" + full[:8] + ".."
	}
	return "sha256:" + full
}

// resolveSubjectPrefix mirrors carbide-core's `resolve_subject_prefix`:
// derives `spiffe://<trust-domain-from-issuer>` from the issuer URL host.
func resolveSubjectPrefix(issuer string) string {
	issuer = strings.TrimSpace(issuer)
	if issuer == "" {
		return ""
	}
	u, err := url.Parse(issuer)
	if err != nil {
		return ""
	}
	host := strings.ToLower(u.Hostname())
	if host == "" {
		return ""
	}
	return "spiffe://" + host
}

// normalizeAllowedAudiences defaults to [defaultAud] when empty; otherwise defaultAud must appear in allowed.
func normalizeAllowedAudiences(defaultAud string, allowed []string) ([]string, error) {
	if len(allowed) == 0 {
		return []string{defaultAud}, nil
	}
	for _, a := range allowed {
		if a == defaultAud {
			out := make([]string, len(allowed))
			copy(out, allowed)
			return out, nil
		}
	}
	return nil, fmt.Errorf("default_audience %q must appear in allowed_audiences", defaultAud)
}

// SetTenantIdentityConfiguration implements interface NICoServer
func (f *NICoServerImpl) SetTenantIdentityConfiguration(ctx context.Context, req *corev1.SetTenantIdentityConfigRequest) (*corev1.TenantIdentityConfigResponse, error) {
	if req == nil || req.GetOrganizationId() == "" || req.GetConfig() == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	in := req.GetConfig()
	if strings.TrimSpace(in.GetIssuer()) == "" {
		return nil, status.Errorf(codes.InvalidArgument, "issuer is required")
	}
	if strings.TrimSpace(in.GetDefaultAudience()) == "" {
		return nil, status.Errorf(codes.InvalidArgument, "default_audience is required")
	}
	if in.GetTokenTtlSec() == 0 {
		return nil, status.Errorf(codes.InvalidArgument, "token_ttl_sec must be greater than zero")
	}
	allowed, err := normalizeAllowedAudiences(in.GetDefaultAudience(), in.GetAllowedAudiences())
	if err != nil {
		return nil, status.Errorf(codes.InvalidArgument, "%s", err.Error())
	}

	switch {
	case in.GetRotateKey() && in.SigningKeyOverlapSec == nil:
		return nil, status.Errorf(codes.InvalidArgument, "signing_key_overlap_sec is required when rotate_key is true")
	case !in.GetRotateKey() && in.SigningKeyOverlapSec != nil:
		return nil, status.Errorf(codes.InvalidArgument, "signing_key_overlap_sec must be omitted when rotate_key is false")
	case in.GetRotateKey() && in.SigningKeyOverlapSec != nil && *in.SigningKeyOverlapSec < in.GetTokenTtlSec():
		return nil, status.Errorf(codes.InvalidArgument, "signing_key_overlap_sec must be >= token_ttl_sec")
	}

	orgID := req.GetOrganizationId()
	f.gcExpiredNonActiveSigningKey(orgID)
	now := timestamppb.Now()
	st, isUpdate := f.identityState[orgID]
	if !isUpdate {
		st = &identityOrgState{}
		f.identityState[orgID] = st
	}

	switch {
	case !isUpdate:
		newKey, err := generateES256KeyMaterial()
		if err != nil {
			return nil, status.Errorf(codes.Internal, "failed to generate signing key: %v", err)
		}
		st.slot1 = newKey
		st.slot2 = nil
		st.currentSlot = 1
		st.nonActiveSlotExpires = nil
	case in.GetRotateKey():
		st.clearInactiveSlot()
		newKey, err := generateES256KeyMaterial()
		if err != nil {
			return nil, status.Errorf(codes.Internal, "failed to generate signing key: %v", err)
		}
		st.setSlot(st.inactiveSlot(), newKey)
		st.currentSlot = st.inactiveSlot()
		expire := now.AsTime().Add(time.Duration(*in.SigningKeyOverlapSec) * time.Second)
		st.nonActiveSlotExpires = &expire
	}

	resolvedSubjectPrefix := in.SubjectPrefix
	if resolvedSubjectPrefix == nil || strings.TrimSpace(*resolvedSubjectPrefix) == "" {
		if derived := resolveSubjectPrefix(in.GetIssuer()); derived != "" {
			resolvedSubjectPrefix = &derived
		}
	}

	resp := &corev1.TenantIdentityConfigResponse{
		OrganizationId: orgID,
		Config: &corev1.TenantIdentityConfig{
			Enabled:          in.GetEnabled(),
			Issuer:           in.GetIssuer(),
			DefaultAudience:  in.GetDefaultAudience(),
			AllowedAudiences: allowed,
			TokenTtlSec:      in.GetTokenTtlSec(),
			SubjectPrefix:    resolvedSubjectPrefix,
			RotateKey:        st.nonActiveSlotExpires != nil,
		},
		UpdatedAt: now,
	}
	if isUpdate {
		resp.CreatedAt = st.cfg.GetCreatedAt()
	} else {
		resp.CreatedAt = now
	}
	resp.SigningKeys = tenantIdentitySigningKeysResponse(st)
	st.cfg = resp
	return resp, nil
}

// GetTenantIdentityConfiguration implements interface NICoServer
func (f *NICoServerImpl) GetTenantIdentityConfiguration(ctx context.Context, req *corev1.GetTenantIdentityConfigRequest) (*corev1.TenantIdentityConfigResponse, error) {
	if req == nil || req.GetOrganizationId() == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	orgID := req.GetOrganizationId()
	f.gcExpiredNonActiveSigningKey(orgID)
	st, ok := f.identityState[orgID]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "Identity configuration not found for org %q", orgID)
	}
	resp := cloneTenantIdentityConfigResponse(st.cfg)
	resp.SigningKeys = tenantIdentitySigningKeysResponse(st)
	if cfg := resp.GetConfig(); cfg != nil {
		cfg.RotateKey = st.nonActiveSlotExpires != nil
	}
	return resp, nil
}

func cloneTenantIdentityConfigResponse(in *corev1.TenantIdentityConfigResponse) *corev1.TenantIdentityConfigResponse {
	if in == nil {
		return nil
	}
	out := &corev1.TenantIdentityConfigResponse{
		OrganizationId: in.GetOrganizationId(),
		CreatedAt:      in.GetCreatedAt(),
		UpdatedAt:      in.GetUpdatedAt(),
	}
	if cfg := in.GetConfig(); cfg != nil {
		out.Config = &corev1.TenantIdentityConfig{
			Enabled:              cfg.GetEnabled(),
			Issuer:               cfg.GetIssuer(),
			DefaultAudience:      cfg.GetDefaultAudience(),
			AllowedAudiences:     cfg.GetAllowedAudiences(),
			TokenTtlSec:          cfg.GetTokenTtlSec(),
			SubjectPrefix:        cfg.SubjectPrefix,
			RotateKey:            cfg.GetRotateKey(),
			SigningKeyOverlapSec: cfg.SigningKeyOverlapSec,
		}
	}
	return out
}

// DeleteTenantIdentityConfiguration implements interface NICoServer
func (f *NICoServerImpl) DeleteTenantIdentityConfiguration(ctx context.Context, req *corev1.GetTenantIdentityConfigRequest) (*emptypb.Empty, error) {
	if req == nil || req.GetOrganizationId() == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	orgID := req.GetOrganizationId()
	if _, ok := f.identityState[orgID]; !ok {
		return nil, status.Errorf(codes.NotFound, "Identity configuration not found for org %q", orgID)
	}
	delete(f.identityState, orgID)
	delete(f.tokenDelegations, orgID)
	return &emptypb.Empty{}, nil
}

// SetTokenDelegation implements interface NICoServer
func (f *NICoServerImpl) SetTokenDelegation(ctx context.Context, req *corev1.TokenDelegationRequest) (*corev1.TokenDelegationResponse, error) {
	if req == nil || req.GetOrganizationId() == "" || req.GetConfig() == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	orgID := req.GetOrganizationId()
	in := req.GetConfig()
	if strings.TrimSpace(in.GetTokenEndpoint()) == "" {
		return nil, status.Errorf(codes.InvalidArgument, "token_endpoint is required")
	}
	if strings.TrimSpace(in.GetSubjectTokenAudience()) == "" {
		return nil, status.Errorf(codes.InvalidArgument, "subject_token_audience is required")
	}

	if _, ok := f.identityState[orgID]; !ok {
		return nil, status.Errorf(codes.NotFound, "Identity configuration must exist before token delegation is set for org %q", orgID)
	}

	now := timestamppb.Now()
	existing, isUpdate := f.tokenDelegations[orgID]

	resp := &corev1.TokenDelegationResponse{
		OrganizationId:       orgID,
		TokenEndpoint:        in.GetTokenEndpoint(),
		SubjectTokenAudience: in.GetSubjectTokenAudience(),
		UpdatedAt:            now,
	}
	if basic := in.GetClientSecretBasic(); basic != nil {
		if strings.TrimSpace(basic.GetClientId()) == "" {
			return nil, status.Errorf(codes.InvalidArgument, "client_id is required for client_secret_basic")
		}
		if strings.TrimSpace(basic.GetClientSecret()) == "" {
			return nil, status.Errorf(codes.InvalidArgument, "client_secret is required for client_secret_basic")
		}
		resp.AuthMethodConfig = &corev1.TokenDelegationResponse_ClientSecretBasic{
			ClientSecretBasic: &corev1.ClientSecretBasicResponse{
				ClientId:         basic.GetClientId(),
				ClientSecretHash: clientSecretDisplayHash(basic.GetClientSecret()),
			},
		}
	}
	if isUpdate {
		resp.CreatedAt = existing.GetCreatedAt()
	} else {
		resp.CreatedAt = now
	}

	f.tokenDelegations[orgID] = resp
	return resp, nil
}

// GetTokenDelegation implements interface NICoServer
func (f *NICoServerImpl) GetTokenDelegation(ctx context.Context, req *corev1.GetTokenDelegationRequest) (*corev1.TokenDelegationResponse, error) {
	if req == nil || req.GetOrganizationId() == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	td, ok := f.tokenDelegations[req.GetOrganizationId()]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "Token delegation not found for org %q", req.GetOrganizationId())
	}
	return td, nil
}

// DeleteTokenDelegation implements interface NICoServer. Idempotent: a
// missing entry returns success (matches carbide-core's no-op delete).
func (f *NICoServerImpl) DeleteTokenDelegation(ctx context.Context, req *corev1.GetTokenDelegationRequest) (*emptypb.Empty, error) {
	if req == nil || req.GetOrganizationId() == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	delete(f.tokenDelegations, req.GetOrganizationId())
	return &emptypb.Empty{}, nil
}

// GetJWKS implements interface NICoServer
func (f *NICoServerImpl) GetJWKS(ctx context.Context, req *corev1.JwksRequest) (*corev1.Jwks, error) {
	if req == nil || req.GetOrganizationId() == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	orgID := req.GetOrganizationId()
	f.gcExpiredNonActiveSigningKey(orgID)
	st, hasCfg := f.identityState[orgID]
	if !hasCfg {
		return nil, status.Errorf(codes.NotFound, "Identity configuration not found for org %q", orgID)
	}
	use := "sig"
	if req.GetKind() == corev1.JwksKind_Spiffe {
		use = "jwt-svid"
	}
	keys := []*identityKeyMaterial{}
	if active := st.activeKey(); active != nil {
		keys = append(keys, active)
	}
	if inactive := st.inactiveKey(); inactive != nil {
		keys = append(keys, inactive)
	}
	if len(keys) == 0 {
		return nil, status.Errorf(codes.Internal, "Signing key missing for org %q (mock state inconsistent)", orgID)
	}
	sort.SliceStable(keys, func(i, j int) bool { return keys[i].kid < keys[j].kid })
	doc, err := jwksDocumentForKeys(keys, use)
	if err != nil {
		return nil, status.Errorf(codes.Internal, "failed to serialize JWKS: %v", err)
	}
	return &corev1.Jwks{Jwks: doc}, nil
}

// GetOpenIDConfiguration implements interface NICoServer
func (f *NICoServerImpl) GetOpenIDConfiguration(ctx context.Context, req *corev1.OpenIdConfigRequest) (*corev1.OpenIdConfiguration, error) {
	if req == nil || req.GetOrganizationId() == "" {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	orgID := req.GetOrganizationId()
	f.gcExpiredNonActiveSigningKey(orgID)
	st, ok := f.identityState[orgID]
	if !ok {
		return nil, status.Errorf(codes.NotFound, "Identity configuration not found for org %q", orgID)
	}
	iss := st.cfg.GetConfig().GetIssuer()
	if strings.TrimSpace(iss) == "" {
		return nil, status.Errorf(codes.NotFound, "Issuer not configured for org %q", orgID)
	}
	if st.activeKey() == nil {
		return nil, status.Errorf(codes.NotFound, "No active signing key for org %q", orgID)
	}
	base := strings.TrimRight(iss, "/")
	return &corev1.OpenIdConfiguration{
		Issuer:                           iss,
		JwksUri:                          base + "/.well-known/jwks.json",
		ResponseTypesSupported:           []string{"token"},
		SubjectTypesSupported:            []string{"public"},
		IdTokenSigningAlgValuesSupported: []string{},
		SpiffeJwksUri:                    base + "/.well-known/spiffe/jwks.json",
	}, nil
}

// mockCurrentEncryptionKeyID is the KEK id the mock reports as current for re-wrap. Seeded orgs
// start un-stamped, so the first apply re-wraps them and subsequent runs report them skipped.
const mockCurrentEncryptionKeyID = "mock-kek-1"

// tenantIdentityEncryptedFieldCount returns how many at-rest encrypted fields the mock would
// re-wrap for an org: one per populated signing-key slot plus optional token-delegation
// credentials. This mirrors the real re-wrap surface (encrypted_signing_key_1/2 and
// encrypted_auth_method_config).
func (f *NICoServerImpl) tenantIdentityEncryptedFieldCount(orgID string, st *identityOrgState) uint32 {
	var n uint32
	if st.slot1 != nil {
		n++
	}
	if st.slot2 != nil {
		n++
	}
	if _, ok := f.tokenDelegations[orgID]; ok {
		n++
	}
	return n
}

// ReencryptTenantIdentitySecrets implements interface NICoServer. It simulates a KEK-rotation
// re-wrap over the in-memory identity state so the endpoint can be exercised in local/dev
// environments where Core is not deployed. Rows already stamped with the current key id are
// reported as skipped (idempotent); dry_run reports would-change counts without mutating state.
func (f *NICoServerImpl) ReencryptTenantIdentitySecrets(ctx context.Context, req *corev1.ReencryptTenantIdentitySecretsRequest) (*corev1.ReencryptTenantIdentitySecretsResponse, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	if f.reencryptedOrgs == nil {
		f.reencryptedOrgs = make(map[string]string)
	}
	current := f.currentEncryptionKeyID
	if current == "" {
		current = mockCurrentEncryptionKeyID
	}

	resp := &corev1.ReencryptTenantIdentitySecretsResponse{
		CurrentEncryptionKeyId: current,
	}
	orgFilter := strings.TrimSpace(req.GetOrganizationId())
	if orgFilter != "" {
		state, ok := f.identityState[orgFilter]
		if !ok || state == nil {
			return nil, status.Errorf(codes.NotFound, "Identity configuration not found for org %q", orgFilter)
		}
	}
	for orgID, st := range f.identityState {
		if st == nil || (orgFilter != "" && orgID != orgFilter) {
			continue
		}
		resp.RowsExamined++
		fieldCount := f.tenantIdentityEncryptedFieldCount(orgID, st)
		if f.reencryptedOrgs[orgID] == current {
			resp.RowsSkippedAllOnTarget++
			resp.FieldsSkippedOnTarget += fieldCount
			continue
		}
		resp.RowsUpdated++
		resp.FieldsReencrypted += fieldCount
		if !req.GetDryRun() {
			f.reencryptedOrgs[orgID] = current
		}
	}
	return resp, nil
}

// resolveSigningOrg returns the seeded org when exactly one identity is configured,
// otherwise the empty string.
func (f *NICoServerImpl) resolveSigningOrg(_ context.Context) string {
	if len(f.identityState) == 1 {
		for k := range f.identityState {
			return k
		}
	}
	return ""
}

// SignMachineIdentity implements interface NICoServer
func (f *NICoServerImpl) SignMachineIdentity(ctx context.Context, req *corev1.MachineIdentityRequest) (*corev1.MachineIdentityResponse, error) {
	if req == nil {
		return nil, status.Errorf(codes.InvalidArgument, "Invalid request argument")
	}
	orgID := f.resolveSigningOrg(ctx)
	if orgID == "" {
		return nil, status.Errorf(codes.Unauthenticated, "Cannot resolve signing org; seed exactly one identity in the mock")
	}
	st, hasCfg := f.identityState[orgID]
	if !hasCfg {
		return nil, status.Errorf(codes.NotFound, "Identity configuration not found for org %q", orgID)
	}
	km := st.activeKey()
	if km == nil {
		return nil, status.Errorf(codes.Internal, "Signing key missing for org %q (mock state inconsistent)", orgID)
	}
	cfg := st.cfg

	audiences := req.GetAudience()
	if len(audiences) == 0 {
		audiences = []string{cfg.GetConfig().GetDefaultAudience()}
	} else {
		allowed := make(map[string]struct{}, len(cfg.GetConfig().GetAllowedAudiences()))
		for _, a := range cfg.GetConfig().GetAllowedAudiences() {
			allowed[a] = struct{}{}
		}
		for _, a := range audiences {
			if _, ok := allowed[a]; !ok {
				return nil, status.Errorf(codes.InvalidArgument, "audience %q is not in allowed_audiences", a)
			}
		}
	}

	ttl := int64(cfg.GetConfig().GetTokenTtlSec())
	if ttl <= 0 {
		ttl = 600
	}
	now := time.Now().Unix()

	subjectPrefix := cfg.GetConfig().GetSubjectPrefix()
	if subjectPrefix == "" {
		subjectPrefix = strings.TrimRight(cfg.GetConfig().GetIssuer(), "/") + "/machine"
	}
	sub := strings.TrimRight(subjectPrefix, "/") + "/" + uuid.NewString()

	var aud any = audiences[0]
	if len(audiences) > 1 {
		aud = audiences
	}
	claims := map[string]any{
		"iss": cfg.GetConfig().GetIssuer(),
		"sub": sub,
		"aud": aud,
		"iat": now,
		"nbf": now,
		"exp": now + ttl,
		"jti": uuid.NewString(),
	}

	token, err := signES256JWT(km.privateKey, km.kid, claims)
	if err != nil {
		return nil, status.Errorf(codes.Internal, "failed to sign token: %v", err)
	}
	return &corev1.MachineIdentityResponse{
		AccessToken:     token,
		IssuedTokenType: "urn:ietf:params:oauth:token-type:jwt",
		TokenType:       "Bearer",
		ExpiresInSec:    uint32(ttl),
	}, nil
}

// LoadTestIdentity seeds one example identity configuration.
func (f *NICoServerImpl) LoadTestIdentity() {
	const seedOrg = "test-org"
	if f.identityState == nil {
		f.identityState = make(map[string]*identityOrgState)
	}
	km, err := generateES256KeyMaterial()
	if err != nil {
		logger.Fatal().Err(err).Msg("LoadTestIdentity: failed to generate ES256 keypair")
		return
	}
	now := timestamppb.Now()
	st := &identityOrgState{
		slot1:       km,
		currentSlot: 1,
	}
	st.cfg = &corev1.TenantIdentityConfigResponse{
		OrganizationId: seedOrg,
		Config: &corev1.TenantIdentityConfig{
			Enabled:          true,
			Issuer:           "https://carbide-rest.mock/v2/org/test-org/nico/site/mock-site",
			DefaultAudience:  "openbao",
			AllowedAudiences: []string{"openbao", "vault"},
			TokenTtlSec:      600,
		},
		CreatedAt:   now,
		UpdatedAt:   now,
		SigningKeys: tenantIdentitySigningKeysResponse(st),
	}
	f.identityState[seedOrg] = st
}

// NICoTest tests the grpc server
func NICoTest(secs int) {
	listener, err := net.Listen("tcp", DefaultPort)
	if err != nil {
		panic(err)
	}

	s := grpc.NewServer()
	reflection.Register(s)

	nicoServer := &NICoServerImpl{
		v:                      make(map[string]*corev1.Vpc),
		ns:                     make(map[string]*corev1.NetworkSegment),
		ins:                    make(map[string]*corev1.Instance),
		m:                      make(map[string]*corev1.Machine),
		tk:                     make(map[string]*corev1.TenantKeyset),
		ibp:                    make(map[string]*corev1.IBPartition),
		em:                     make(map[string]*corev1.ExpectedMachine),
		eps:                    make(map[string]*corev1.ExpectedPowerShelf),
		es:                     make(map[string]*corev1.ExpectedSwitch),
		er:                     make(map[string]*corev1.ExpectedRack),
		identityState:          make(map[string]*identityOrgState),
		tokenDelegations:       make(map[string]*corev1.TokenDelegationResponse),
		currentEncryptionKeyID: mockCurrentEncryptionKeyID,
		reencryptedOrgs:        make(map[string]string),
	}
	nicoServer.LoadTestMachines()
	nicoServer.LoadTestIdentity()

	corev1.RegisterForgeServer(s, nicoServer)

	if secs != 0 {
		timer := time.AfterFunc(time.Second*time.Duration(secs), func() {
			s.GracefulStop()
			logger.Info().Msgf("Timer started for: %v seconds", secs)
		})
		defer timer.Stop()
	}

	logger.Info().Msg("Started API server")

	err = s.Serve(listener)
	if err != nil {
		logger.Fatal().Err(err).Msg("Failed to start API server")
	}

	logger.Info().Msg("Stopped API server")
}
