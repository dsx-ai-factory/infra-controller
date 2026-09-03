// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"fmt"
	"math/rand"
	"net"
	"slices"
	"strings"
	"time"

	"github.com/gogo/status"
	"github.com/google/uuid"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

var runes = []rune("abcdefghijklmnopqrstuvwxyz0123456789")

// Add utlity methods here
// randSeq generates a random sequence of runes
func randSeq(n int) string {
	b := make([]rune, n)
	for i := range b {
		b[i] = runes[rand.Intn(len(runes))]
	}
	return string(b)
}

// generateSiteVersion generates a version in the format of "V1-T<timestamp>"
func generateSiteVersion() string {
	// Get the current time
	now := time.Now()
	// Get microseconds since epoch
	microseconds := now.UnixMicro()
	return fmt.Sprintf("V1-T%d", microseconds)
}

// incrementMAC takes a hardware address (MAC address) and increments it by one.
// It handles carrying over to the next byte when a byte overflows (reaches 255).
func incrementMAC(mac net.HardwareAddr) {
	// Iterate from the last byte to the first.
	for i := range slices.Backward(mac) {
		// Increment the current byte.
		mac[i]++
		// If the byte is not 0, it means there was no overflow, so we can stop.
		if mac[i] != 0 {
			break
		}
		// If the byte is 0, it means it overflowed from 255, so we continue to the next
		// byte to handle the "carry-over".
	}
}

// MockCoreGrpcService is a mock implementation of Core gRPC protobuf Service
type MockCoreGrpcServiceClient struct {
	corev1.ForgeClient
	BuildCapabilities []corev1.BuildCapability
}

/* Version mock methods */
func (mcgsc *MockCoreGrpcServiceClient) Version(ctx context.Context, in *corev1.VersionRequest, opts ...grpc.CallOption) (*corev1.BuildInfo, error) {
	out := new(corev1.BuildInfo)
	out.BuildVersion = "1.0.0"
	out.Capabilities = mcgsc.BuildCapabilities
	// Core reports the runtime config only when the request asks for it, so a
	// caller that omits DisplayConfig gets BuildInfo with no RuntimeConfig.
	if in.GetDisplayConfig() {
		siteFabricPrefixes, ok := ctx.Value("siteFabricPrefixes").([]string)
		if ok {
			out.RuntimeConfig = &corev1.RuntimeConfig{
				SiteFabricPrefixes: siteFabricPrefixes,
			}
		}
	}
	return out, nil
}

/* VPC mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateVpc(ctx context.Context, in *corev1.VpcCreationRequest, opts ...grpc.CallOption) (*corev1.Vpc, error) {
	out := new(corev1.Vpc)
	out.Id = &corev1.VpcId{Value: uuid.NewString()}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateVpc(ctx context.Context, in *corev1.VpcUpdateRequest, opts ...grpc.CallOption) (*corev1.VpcUpdateResult, error) {
	out := new(corev1.VpcUpdateResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateVpcVirtualization(ctx context.Context, in *corev1.VpcUpdateVirtualizationRequest, opts ...grpc.CallOption) (*corev1.VpcUpdateVirtualizationResult, error) {
	out := new(corev1.VpcUpdateVirtualizationResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteVpc(ctx context.Context, in *corev1.VpcDeletionRequest, opts ...grpc.CallOption) (*corev1.VpcDeletionResult, error) {
	out := new(corev1.VpcDeletionResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindVpcIds(ctx context.Context, in *corev1.VpcSearchFilter, opts ...grpc.CallOption) (*corev1.VpcIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve vpc ids")
	}

	out := &corev1.VpcIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.VpcIds = append(out.VpcIds, &corev1.VpcId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindVpcsByIds(ctx context.Context, in *corev1.VpcsByIdsRequest, opts ...grpc.CallOption) (*corev1.VpcList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve vpcs")
	}

	out := &corev1.VpcList{}
	if in != nil {
		for _, id := range in.VpcIds {
			out.Vpcs = append(out.Vpcs, &corev1.Vpc{
				Id: id,
			})
		}
	}

	return out, nil
}

/* Network Segment mock methods */

func (mcgsc *MockCoreGrpcServiceClient) CreateNetworkSegment(ctx context.Context, in *corev1.NetworkSegmentCreationRequest, opts ...grpc.CallOption) (*corev1.NetworkSegment, error) {
	out := new(corev1.NetworkSegment)
	out.Id = &corev1.NetworkSegmentId{Value: uuid.NewString()}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteNetworkSegment(ctx context.Context, in *corev1.NetworkSegmentDeletionRequest, opts ...grpc.CallOption) (*corev1.NetworkSegmentDeletionResult, error) {
	out := new(corev1.NetworkSegmentDeletionResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindNetworkSegmentIds(ctx context.Context, in *corev1.NetworkSegmentSearchFilter, opts ...grpc.CallOption) (*corev1.NetworkSegmentIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve network segment ids")
	}

	out := &corev1.NetworkSegmentIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.NetworkSegmentsIds = append(out.NetworkSegmentsIds, &corev1.NetworkSegmentId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindNetworkSegmentsByIds(ctx context.Context, in *corev1.NetworkSegmentsByIdsRequest, opts ...grpc.CallOption) (*corev1.NetworkSegmentList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve network segments")
	}

	out := &corev1.NetworkSegmentList{}
	if in != nil {
		for _, id := range in.NetworkSegmentsIds {
			out.NetworkSegments = append(out.NetworkSegments, &corev1.NetworkSegment{
				Id: id,
			})
		}
	}

	return out, nil
}

/* InfiniBand Partition mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateIBPartition(ctx context.Context, in *corev1.IBPartitionCreationRequest, opts ...grpc.CallOption) (*corev1.IBPartition, error) {
	out := new(corev1.IBPartition)
	out.Id = &corev1.IBPartitionId{Value: uuid.NewString()}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateIBPartition(ctx context.Context, in *corev1.IBPartitionUpdateRequest, opts ...grpc.CallOption) (*corev1.IBPartition, error) {
	out := new(corev1.IBPartition)
	if in != nil && in.Id != nil {
		out.Id = in.Id
	} else {
		out.Id = &corev1.IBPartitionId{Value: uuid.NewString()}
	}
	if in != nil {
		out.Config = in.GetConfig()
		out.Metadata = in.GetMetadata()
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteIBPartition(ctx context.Context, in *corev1.IBPartitionDeletionRequest, opts ...grpc.CallOption) (*corev1.IBPartitionDeletionResult, error) {
	out := new(corev1.IBPartitionDeletionResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindIBPartitionIds(ctx context.Context, in *corev1.IBPartitionSearchFilter, opts ...grpc.CallOption) (*corev1.IBPartitionIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve ib partition ids")
	}

	out := &corev1.IBPartitionIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.IbPartitionIds = append(out.IbPartitionIds, &corev1.IBPartitionId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindIBPartitionsByIds(ctx context.Context, in *corev1.IBPartitionsByIdsRequest, opts ...grpc.CallOption) (*corev1.IBPartitionList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve ib partitions")
	}

	out := &corev1.IBPartitionList{}
	if in != nil {
		for _, id := range in.IbPartitionIds {
			out.IbPartitions = append(out.IbPartitions, &corev1.IBPartition{
				Id: id,
			})
		}
	}

	return out, nil
}

/* SpectrumX Partition mock methods */
func (mcgsc *MockCoreGrpcServiceClient) FindSpxPartitionIds(ctx context.Context, in *corev1.SpxPartitionSearchFilter, opts ...grpc.CallOption) (*corev1.SpxPartitionIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve spx partition ids")
	}

	out := &corev1.SpxPartitionIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.SpxPartitionIds = append(out.SpxPartitionIds, &corev1.SpxPartitionId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindSpxPartitionsByIds(ctx context.Context, in *corev1.SpxPartitionsByIdsRequest, opts ...grpc.CallOption) (*corev1.SpxPartitionList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve spx partitions")
	}

	out := &corev1.SpxPartitionList{}
	if in != nil {
		for _, id := range in.SpxPartitionIds {
			out.SpxPartitions = append(out.SpxPartitions, &corev1.SpxPartition{
				Id: id,
			})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) CreateSpxPartition(ctx context.Context, in *corev1.SpxPartitionCreationRequest, opts ...grpc.CallOption) (*corev1.SpxPartition, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to create spx partition")
	}

	out := &corev1.SpxPartition{}
	if in != nil {
		out.Id = in.GetId()
		out.Metadata = in.GetMetadata()
		out.TenantOrganizationId = in.GetTenantOrganizationId()
		out.Vni = in.GetVni()
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteSpxPartition(ctx context.Context, in *corev1.SpxPartitionDeletionRequest, opts ...grpc.CallOption) (*corev1.SpxPartitionDeletionResult, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to delete spx partition")
	}

	return &corev1.SpxPartitionDeletionResult{}, nil
}

/* Instance mock methods */
func (mcgsc *MockCoreGrpcServiceClient) AllocateInstance(ctx context.Context, in *corev1.InstanceAllocationRequest, opts ...grpc.CallOption) (*corev1.Instance, error) {
	out := new(corev1.Instance)
	out.Id = &corev1.InstanceId{Value: uuid.NewString()}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) AllocateInstances(ctx context.Context, in *corev1.BatchInstanceAllocationRequest, opts ...grpc.CallOption) (*corev1.BatchInstanceAllocationResponse, error) {
	out := &corev1.BatchInstanceAllocationResponse{
		Instances: make([]*corev1.Instance, len(in.InstanceRequests)),
	}
	for i := range in.InstanceRequests {
		out.Instances[i] = &corev1.Instance{
			Id: &corev1.InstanceId{Value: uuid.NewString()},
		}
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateInstanceConfig(ctx context.Context, in *corev1.InstanceConfigUpdateRequest, opts ...grpc.CallOption) (*corev1.Instance, error) {
	out := new(corev1.Instance)
	out.Id = in.InstanceId
	out.Metadata = in.Metadata
	out.Config = in.Config
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) ReleaseInstance(ctx context.Context, in *corev1.InstanceReleaseRequest, opts ...grpc.CallOption) (*corev1.InstanceReleaseResult, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.NotFound {
			return nil, status.Error(codes.NotFound, "instance not found: ")
		}
	}
	out := new(corev1.InstanceReleaseResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindInstanceIds(ctx context.Context, in *corev1.InstanceSearchFilter, opts ...grpc.CallOption) (*corev1.InstanceIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve instance ids")
	}

	out := &corev1.InstanceIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.InstanceIds = append(out.InstanceIds, &corev1.InstanceId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindInstancesByIds(ctx context.Context, in *corev1.InstancesByIdsRequest, opts ...grpc.CallOption) (*corev1.InstanceList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve instances")
	}

	// Stands in for a response that outgrows the gRPC client receive limit, which is what makes
	// the inventory pager step its site page size down and ask for fewer Instances.
	maxRequestIDs, ok := ctx.Value("maxRequestIDs").(int)
	if ok && in != nil && len(in.InstanceIds) > maxRequestIDs {
		return nil, status.Errorf(codes.ResourceExhausted,
			"grpc: received message larger than max (%v IDs requested, mock accepts %v)", len(in.InstanceIds), maxRequestIDs)
	}

	// Inflates every Instance so a published inventory page can outgrow the Temporal blob budget.
	var padding *string
	padBytes, _ := ctx.Value("instancePadBytes").(int)
	if padBytes > 0 {
		pad := strings.Repeat("a", padBytes)
		padding = &pad
	}

	// Stands in for Instances deleted between FindInstanceIds and FindInstancesByIds, which is
	// how Core comes to return fewer Instances than the IDs asked for.
	dropPerPage, _ := ctx.Value("dropInstancesPerPage").(int)

	out := &corev1.InstanceList{}
	if in != nil {
		ids := in.InstanceIds
		if dropPerPage > 0 && len(ids) >= dropPerPage {
			ids = ids[:len(ids)-dropPerPage]
		}
		for _, id := range ids {
			out.Instances = append(out.Instances, &corev1.Instance{
				Id:               id,
				TpmEkCertificate: padding,
			})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) InvokeInstancePower(ctx context.Context, in *corev1.InstancePowerRequest, opts ...grpc.CallOption) (*corev1.InstancePowerResult, error) {
	out := new(corev1.InstancePowerResult)
	return out, nil
}

/* Machine mock methods */
func (mcgsc *MockCoreGrpcServiceClient) SetMaintenance(ctx context.Context, in *corev1.MaintenanceRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateMachineMetadata(ctx context.Context, in *corev1.MachineMetadataUpdateRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to update machine metadata")
	}

	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) InsertMachineHealthReport(ctx context.Context, in *corev1.InsertMachineHealthReportRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to insert machine health report")
	}

	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) RemoveMachineHealthReport(ctx context.Context, in *corev1.RemoveMachineHealthReportRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to remove machine health report")
	}

	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindMachineIds(ctx context.Context, in *corev1.MachineSearchConfig, opts ...grpc.CallOption) (*corev1.MachineIdList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve machine ids")
		}
	}

	out := &corev1.MachineIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.MachineIds = append(out.MachineIds, &corev1.MachineId{Id: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindMachinesByIds(ctx context.Context, in *corev1.MachinesByIdsRequest, opts ...grpc.CallOption) (*corev1.MachineList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve machines by ids")
		}
	}

	out := &corev1.MachineList{}
	if in != nil {
		for _, id := range in.MachineIds {
			out.Machines = append(out.Machines, &corev1.Machine{
				Id:    id,
				State: "Ready",
			})
		}
	}

	return out, nil
}

/* Tenant Keyset mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateTenantKeyset(ctx context.Context, in *corev1.CreateTenantKeysetRequest, opts ...grpc.CallOption) (*corev1.CreateTenantKeysetResponse, error) {
	out := new(corev1.CreateTenantKeysetResponse)
	out.Keyset = &corev1.TenantKeyset{
		KeysetIdentifier: &corev1.TenantKeysetIdentifier{
			OrganizationId: in.KeysetIdentifier.OrganizationId,
			KeysetId:       uuid.NewString(),
		},
	}
	out.Keyset.KeysetContent = in.KeysetContent
	out.Keyset.Version = in.Version
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateTenantKeyset(ctx context.Context, in *corev1.UpdateTenantKeysetRequest, opts ...grpc.CallOption) (*corev1.UpdateTenantKeysetResponse, error) {
	out := new(corev1.UpdateTenantKeysetResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteTenantKeyset(ctx context.Context, in *corev1.DeleteTenantKeysetRequest, opts ...grpc.CallOption) (*corev1.DeleteTenantKeysetResponse, error) {
	out := new(corev1.DeleteTenantKeysetResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindTenantKeysetIds(ctx context.Context, in *corev1.TenantKeysetSearchFilter, opts ...grpc.CallOption) (*corev1.TenantKeysetIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve tenant keyset ids")
	}

	out := &corev1.TenantKeysetIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		orgID := uuid.NewString()
		for range count {
			out.KeysetIds = append(out.KeysetIds, &corev1.TenantKeysetIdentifier{OrganizationId: orgID, KeysetId: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindTenantKeysetsByIds(ctx context.Context, in *corev1.TenantKeysetsByIdsRequest, opts ...grpc.CallOption) (*corev1.TenantKeySetList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve tenant keysets")
	}

	out := &corev1.TenantKeySetList{}
	if in != nil {
		for _, id := range in.KeysetIds {
			out.Keyset = append(out.Keyset, &corev1.TenantKeyset{
				KeysetIdentifier: &corev1.TenantKeysetIdentifier{
					OrganizationId: id.OrganizationId,
					KeysetId:       id.KeysetId,
				},
			})
		}
	}

	return out, nil
}

/* OS Image mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateOsImage(ctx context.Context, in *corev1.OsImageAttributes, opts ...grpc.CallOption) (*corev1.OsImage, error) {
	out := new(corev1.OsImage)
	out.Attributes = &corev1.OsImageAttributes{Id: &corev1.UUID{Value: uuid.NewString()}}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateOsImage(ctx context.Context, in *corev1.OsImageAttributes, opts ...grpc.CallOption) (*corev1.OsImage, error) {
	out := new(corev1.OsImage)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteOsImage(ctx context.Context, in *corev1.DeleteOsImageRequest, opts ...grpc.CallOption) (*corev1.DeleteOsImageResponse, error) {
	out := new(corev1.DeleteOsImageResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) ListOsImage(ctx context.Context, in *corev1.ListOsImageRequest, opts ...grpc.CallOption) (*corev1.ListOsImageResponse, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve os image list")
	}

	out := &corev1.ListOsImageResponse{}
	count, ok := ctx.Value("wantCount").(int)
	if ok {
		id := uuid.NewString()
		for range count {
			out.Images = append(out.Images, &corev1.OsImage{Attributes: &corev1.OsImageAttributes{Id: &corev1.UUID{Value: id}}})
		}
	}
	return out, nil
}

/* Tenant mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateTenant(ctx context.Context, in *corev1.CreateTenantRequest, opts ...grpc.CallOption) (*corev1.CreateTenantResponse, error) {
	out := new(corev1.CreateTenantResponse)
	out.Tenant = &corev1.Tenant{
		OrganizationId: in.OrganizationId,
	}
	if in.Metadata != nil {
		out.Tenant.Metadata = &corev1.Metadata{
			Name: in.Metadata.Name,
		}
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindTenant(ctx context.Context, in *corev1.FindTenantRequest, opts ...grpc.CallOption) (*corev1.FindTenantResponse, error) {
	out := new(corev1.FindTenantResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateTenant(ctx context.Context, in *corev1.UpdateTenantRequest, opts ...grpc.CallOption) (*corev1.UpdateTenantResponse, error) {
	out := new(corev1.UpdateTenantResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindTenantOrganizationIds(ctx context.Context, in *corev1.TenantSearchFilter, opts ...grpc.CallOption) (*corev1.TenantOrganizationIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve Tenant organization ids")
	}

	out := &corev1.TenantOrganizationIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.TenantOrganizationIds = append(out.TenantOrganizationIds, randSeq(10))
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindTenantsByOrganizationIds(ctx context.Context, in *corev1.TenantByOrganizationIdsRequest, opts ...grpc.CallOption) (*corev1.TenantList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve Tenants")
	}

	out := &corev1.TenantList{}
	if in != nil {
		for _, id := range in.OrganizationIds {
			out.Tenants = append(out.Tenants, &corev1.Tenant{
				OrganizationId: id,
			})
		}
	}

	return out, nil
}

/* Instance Type mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateInstanceType(ctx context.Context, in *corev1.CreateInstanceTypeRequest, opts ...grpc.CallOption) (*corev1.CreateInstanceTypeResponse, error) {
	out := &corev1.CreateInstanceTypeResponse{InstanceType: &corev1.InstanceType{}}
	out.InstanceType.Id = uuid.NewString()
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateInstanceType(ctx context.Context, in *corev1.UpdateInstanceTypeRequest, opts ...grpc.CallOption) (*corev1.UpdateInstanceTypeResponse, error) {
	out := &corev1.UpdateInstanceTypeResponse{InstanceType: &corev1.InstanceType{}}
	out.InstanceType.Id = in.Id
	out.InstanceType.Metadata = in.Metadata
	out.InstanceType.Attributes = in.InstanceTypeAttributes
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteInstanceType(ctx context.Context, in *corev1.DeleteInstanceTypeRequest, opts ...grpc.CallOption) (*corev1.DeleteInstanceTypeResponse, error) {
	out := &corev1.DeleteInstanceTypeResponse{}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) AssociateMachinesWithInstanceType(ctx context.Context, in *corev1.AssociateMachinesWithInstanceTypeRequest, opts ...grpc.CallOption) (*corev1.AssociateMachinesWithInstanceTypeResponse, error) {
	out := &corev1.AssociateMachinesWithInstanceTypeResponse{}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) RemoveMachineInstanceTypeAssociation(ctx context.Context, in *corev1.RemoveMachineInstanceTypeAssociationRequest, opts ...grpc.CallOption) (*corev1.RemoveMachineInstanceTypeAssociationResponse, error) {
	out := &corev1.RemoveMachineInstanceTypeAssociationResponse{}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindInstanceTypeIds(ctx context.Context, in *corev1.FindInstanceTypeIdsRequest, opts ...grpc.CallOption) (*corev1.FindInstanceTypeIdsResponse, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve InstanceType ids")
	}

	out := &corev1.FindInstanceTypeIdsResponse{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.InstanceTypeIds = append(out.InstanceTypeIds, randSeq(10))
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindInstanceTypesByIds(ctx context.Context, in *corev1.FindInstanceTypesByIdsRequest, opts ...grpc.CallOption) (*corev1.FindInstanceTypesByIdsResponse, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve InstanceTypes")
	}

	out := &corev1.FindInstanceTypesByIdsResponse{}
	if in != nil {
		for _, id := range in.InstanceTypeIds {
			out.InstanceTypes = append(out.InstanceTypes, &corev1.InstanceType{
				Id: id,
			})
		}
	}
	return out, nil
}

/* VPC Prefix mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateVpcPrefix(ctx context.Context, in *corev1.VpcPrefixCreationRequest, opts ...grpc.CallOption) (*corev1.VpcPrefix, error) {
	out := new(corev1.VpcPrefix)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateVpcPrefix(ctx context.Context, in *corev1.VpcPrefixUpdateRequest, opts ...grpc.CallOption) (*corev1.VpcPrefix, error) {
	out := new(corev1.VpcPrefix)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteVpcPrefix(ctx context.Context, in *corev1.VpcPrefixDeletionRequest, opts ...grpc.CallOption) (*corev1.VpcPrefixDeletionResult, error) {
	out := new(corev1.VpcPrefixDeletionResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) SearchVpcPrefixes(ctx context.Context, in *corev1.VpcPrefixSearchQuery, opts ...grpc.CallOption) (*corev1.VpcPrefixIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve vpcprefix ids")
	}
	if wantDeletedFilter, ok := ctx.Value("wantDeletedFilter").(corev1.DeletedFilter); ok && in.GetDeleted() != wantDeletedFilter {
		return nil, status.Errorf(codes.InvalidArgument, "expected deleted filter %s, got %s", wantDeletedFilter, in.GetDeleted())
	}

	out := &corev1.VpcPrefixIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.VpcPrefixIds = append(out.VpcPrefixIds, &corev1.VpcPrefixId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetVpcPrefixes(ctx context.Context, in *corev1.VpcPrefixGetRequest, opts ...grpc.CallOption) (*corev1.VpcPrefixList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve vpcprefixes")
	}
	if wantDeletedFilter, ok := ctx.Value("wantDeletedFilter").(corev1.DeletedFilter); ok && in.GetDeleted() != wantDeletedFilter {
		return nil, status.Errorf(codes.InvalidArgument, "expected deleted filter %s, got %s", wantDeletedFilter, in.GetDeleted())
	}

	out := &corev1.VpcPrefixList{}
	if in != nil {
		for _, id := range in.VpcPrefixIds {
			out.VpcPrefixes = append(out.VpcPrefixes, &corev1.VpcPrefix{
				Id: id,
			})
		}
	}

	return out, nil
}

/* VPC Peering mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateVpcPeering(ctx context.Context, in *corev1.VpcPeeringCreationRequest, opts ...grpc.CallOption) (*corev1.VpcPeering, error) {
	out := new(corev1.VpcPeering)
	out.Id = &corev1.VpcPeeringId{Value: uuid.NewString()}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteVpcPeering(ctx context.Context, in *corev1.VpcPeeringDeletionRequest, opts ...grpc.CallOption) (*corev1.VpcPeeringDeletionResult, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to delete vpc peering")
	}

	return &corev1.VpcPeeringDeletionResult{}, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindVpcPeeringIds(ctx context.Context, in *corev1.VpcPeeringSearchFilter, opts ...grpc.CallOption) (*corev1.VpcPeeringIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve vpc peering ids")
	}

	out := &corev1.VpcPeeringIdList{}

	count, ok := ctx.Value("WantCount").(int)
	if ok {
		for range count {
			out.VpcPeeringIds = append(out.VpcPeeringIds, &corev1.VpcPeeringId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindVpcPeeringsByIds(ctx context.Context, in *corev1.VpcPeeringsByIdsRequest, opts ...grpc.CallOption) (*corev1.VpcPeeringList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve vpc peerings")
	}

	out := &corev1.VpcPeeringList{}
	for _, id := range in.VpcPeeringIds {
		out.VpcPeerings = append(out.VpcPeerings, &corev1.VpcPeering{
			Id:        id,
			VpcId:     &corev1.VpcId{Value: uuid.NewString()},
			PeerVpcId: &corev1.VpcId{Value: uuid.NewString()},
		})
	}

	return out, nil
}

/* Machine Validation Test mock methods */
func (mcgsc *MockCoreGrpcServiceClient) AddMachineValidationTest(ctx context.Context, in *corev1.MachineValidationTestAddRequest, opts ...grpc.CallOption) (*corev1.MachineValidationTestAddUpdateResponse, error) {
	out := new(corev1.MachineValidationTestAddUpdateResponse)
	id, ok := ctx.Value("wantID").(string)
	if ok {
		out.TestId = id
		out.Version = "version-1"
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateMachineValidationTest(ctx context.Context, in *corev1.MachineValidationTestUpdateRequest, opts ...grpc.CallOption) (*corev1.MachineValidationTestAddUpdateResponse, error) {
	out := new(corev1.MachineValidationTestAddUpdateResponse)
	out.TestId = in.TestId
	out.Version = in.Version
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetMachineValidationTests(ctx context.Context, in *corev1.MachineValidationTestsGetRequest, opts ...grpc.CallOption) (*corev1.MachineValidationTestsGetResponse, error) {
	out := new(corev1.MachineValidationTestsGetResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) MachineValidationTestEnableDisableTest(ctx context.Context, in *corev1.MachineValidationTestEnableDisableTestRequest, opts ...grpc.CallOption) (*corev1.MachineValidationTestEnableDisableTestResponse, error) {
	out := new(corev1.MachineValidationTestEnableDisableTestResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) AddUpdateMachineValidationExternalConfig(ctx context.Context, in *corev1.AddUpdateMachineValidationExternalConfigRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) RemoveMachineValidationExternalConfig(ctx context.Context, in *corev1.RemoveMachineValidationExternalConfigRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetMachineValidationExternalConfigs(ctx context.Context, in *corev1.GetMachineValidationExternalConfigsRequest, opts ...grpc.CallOption) (*corev1.GetMachineValidationExternalConfigsResponse, error) {
	out := new(corev1.GetMachineValidationExternalConfigsResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetMachineValidationRuns(ctx context.Context, in *corev1.MachineValidationRunListGetRequest, opts ...grpc.CallOption) (*corev1.MachineValidationRunList, error) {
	out := new(corev1.MachineValidationRunList)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetMachineValidationResults(ctx context.Context, in *corev1.MachineValidationGetRequest, opts ...grpc.CallOption) (*corev1.MachineValidationResultList, error) {
	out := new(corev1.MachineValidationResultList)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) PersistValidationResult(ctx context.Context, in *corev1.MachineValidationResultPostRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

/* Network Security Group mock methods */
func (mcgsc *MockCoreGrpcServiceClient) UpdateMachineValidationRun(ctx context.Context, in *corev1.MachineValidationRunRequest, opts ...grpc.CallOption) (*corev1.MachineValidationRunResponse, error) {
	out := new(corev1.MachineValidationRunResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) CreateNetworkSecurityGroup(ctx context.Context, in *corev1.CreateNetworkSecurityGroupRequest, opts ...grpc.CallOption) (*corev1.CreateNetworkSecurityGroupResponse, error) {
	out := &corev1.CreateNetworkSecurityGroupResponse{NetworkSecurityGroup: &corev1.NetworkSecurityGroup{}}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateNetworkSecurityGroup(ctx context.Context, in *corev1.UpdateNetworkSecurityGroupRequest, opts ...grpc.CallOption) (*corev1.UpdateNetworkSecurityGroupResponse, error) {
	out := &corev1.UpdateNetworkSecurityGroupResponse{NetworkSecurityGroup: &corev1.NetworkSecurityGroup{}}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteNetworkSecurityGroup(ctx context.Context, in *corev1.DeleteNetworkSecurityGroupRequest, opts ...grpc.CallOption) (*corev1.DeleteNetworkSecurityGroupResponse, error) {
	out := &corev1.DeleteNetworkSecurityGroupResponse{}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetNetworkSecurityGroupAttachments(ctx context.Context, in *corev1.GetNetworkSecurityGroupAttachmentsRequest, opts ...grpc.CallOption) (*corev1.GetNetworkSecurityGroupAttachmentsResponse, error) {
	out := &corev1.GetNetworkSecurityGroupAttachmentsResponse{}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetNetworkSecurityGroupPropagationStatus(ctx context.Context, in *corev1.GetNetworkSecurityGroupPropagationStatusRequest, opts ...grpc.CallOption) (*corev1.GetNetworkSecurityGroupPropagationStatusResponse, error) {
	out := &corev1.GetNetworkSecurityGroupPropagationStatusResponse{}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindNetworkSecurityGroupIds(ctx context.Context, in *corev1.FindNetworkSecurityGroupIdsRequest, opts ...grpc.CallOption) (*corev1.FindNetworkSecurityGroupIdsResponse, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve NetworkSecurityGroup ids")
	}

	out := &corev1.FindNetworkSecurityGroupIdsResponse{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.NetworkSecurityGroupIds = append(out.NetworkSecurityGroupIds, randSeq(10))
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindNetworkSecurityGroupsByIds(ctx context.Context, in *corev1.FindNetworkSecurityGroupsByIdsRequest, opts ...grpc.CallOption) (*corev1.FindNetworkSecurityGroupsByIdsResponse, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve NetworkSecurityGroups")
	}

	out := &corev1.FindNetworkSecurityGroupsByIdsResponse{}
	if in != nil {
		for _, id := range in.NetworkSecurityGroupIds {
			out.NetworkSecurityGroups = append(out.NetworkSecurityGroups, &corev1.NetworkSecurityGroup{
				Id: id,
			})
		}
	}
	return out, nil
}

/* Expected Machine mock methods */
func (mcgsc *MockCoreGrpcServiceClient) AddExpectedMachine(ctx context.Context, in *corev1.ExpectedMachine, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.Id == nil || in.Id.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for AddExpectedMachine")
	}
	if in.BmcMacAddress == "" {
		return nil, status.Error(codes.Internal, "MAC address not provided for AddExpectedMachine")
	}
	if in.ChassisSerialNumber == "" {
		return nil, status.Error(codes.Internal, "Chassis Serial Number not provided for AddExpectedMachine")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteExpectedMachine(ctx context.Context, in *corev1.ExpectedMachineRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.Id == nil || in.Id.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for DeleteExpectedMachine")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateExpectedMachine(ctx context.Context, in *corev1.ExpectedMachine, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.Id == nil || in.Id.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for UpdateExpectedMachine")
	}
	if in.BmcMacAddress == "" {
		return nil, status.Error(codes.Internal, "MAC address not provided for UpdateExpectedMachine")
	}
	if in.ChassisSerialNumber == "" {
		return nil, status.Error(codes.Internal, "Chassis Serial Number not provided for UpdateExpectedMachine")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) CreateExpectedMachines(ctx context.Context, in *corev1.BatchExpectedMachineOperationRequest, opts ...grpc.CallOption) (*corev1.BatchExpectedMachineOperationResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	out := &corev1.BatchExpectedMachineOperationResponse{
		Results: make([]*corev1.ExpectedMachineOperationResult, 0, len(in.GetExpectedMachines().GetExpectedMachines())),
	}

	// Simulate individual processing of each ExpectedMachine
	for _, em := range in.GetExpectedMachines().GetExpectedMachines() {
		result := &corev1.ExpectedMachineOperationResult{
			Id:              em.GetId(),
			Success:         true,
			ExpectedMachine: em,
		}

		// Validate required fields
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
		}

		out.Results = append(out.Results, result)
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateExpectedMachines(ctx context.Context, in *corev1.BatchExpectedMachineOperationRequest, opts ...grpc.CallOption) (*corev1.BatchExpectedMachineOperationResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	out := &corev1.BatchExpectedMachineOperationResponse{
		Results: make([]*corev1.ExpectedMachineOperationResult, 0, len(in.GetExpectedMachines().GetExpectedMachines())),
	}

	// Simulate individual processing of each ExpectedMachine
	for _, em := range in.GetExpectedMachines().GetExpectedMachines() {
		result := &corev1.ExpectedMachineOperationResult{
			Id:              em.GetId(),
			Success:         true,
			ExpectedMachine: em,
		}

		// Validate required fields
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
		}

		out.Results = append(out.Results, result)
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllExpectedMachines(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.ExpectedMachineList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve machine ids")
		}
	}

	out := &corev1.ExpectedMachineList{}

	// we generate predictable unique IDs and values
	count, ok := ctx.Value("wantCount").(int)
	if ok {
		mac, _ := net.ParseMAC("02:00:00:00:00:00")
		for range count {
			// Create a 16-byte array for UUID from MAC address (6 bytes) + padding
			var uuidBytes [16]byte
			copy(uuidBytes[:6], mac)
			emID, _ := uuid.FromBytes(uuidBytes[:])
			out.ExpectedMachines = append(out.ExpectedMachines, &corev1.ExpectedMachine{
				Id:                  &corev1.UUID{Value: emID.String()},
				BmcMacAddress:       mac.String(),
				ChassisSerialNumber: "serial-" + mac.String()})
			incrementMAC(mac)
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetExpectedMachine(ctx context.Context, in *corev1.ExpectedMachineRequest, opts ...grpc.CallOption) (*corev1.ExpectedMachine, error) {
	if in.Id == nil || in.Id.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for GetExpectedMachine")
	}
	out := new(corev1.ExpectedMachine)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllExpectedMachinesLinked(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.LinkedExpectedMachineList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve linked expected machines")
		}
	}

	out := &corev1.LinkedExpectedMachineList{}

	// Generate linked machines based on the count in context
	count, ok := ctx.Value("wantCount").(int)
	if ok {
		mac, _ := net.ParseMAC("02:00:00:00:00:00")
		for range count {
			// Create a 16-byte array for UUID from MAC address (6 bytes) + padding
			var uuidBytes [16]byte
			copy(uuidBytes[:6], mac)
			machineID, _ := uuid.FromBytes(uuidBytes[:])

			out.ExpectedMachines = append(out.ExpectedMachines, &corev1.LinkedExpectedMachine{
				ChassisSerialNumber: "serial-" + mac.String(),
				BmcMacAddress:       mac.String(),
				MachineId:           &corev1.MachineId{Id: machineID.String()},
			})
			incrementMAC(mac)
		}
	}

	return out, nil
}

/* Expected Power Shelf mock methods */
func (mcgsc *MockCoreGrpcServiceClient) AddExpectedPowerShelf(ctx context.Context, in *corev1.ExpectedPowerShelf, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.ExpectedPowerShelfId == nil || in.ExpectedPowerShelfId.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for AddExpectedPowerShelf")
	}
	if in.BmcMacAddress == "" {
		return nil, status.Error(codes.Internal, "MAC address not provided for AddExpectedPowerShelf")
	}
	if in.ShelfSerialNumber == "" {
		return nil, status.Error(codes.Internal, "Shelf Serial Number not provided for AddExpectedPowerShelf")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteExpectedPowerShelf(ctx context.Context, in *corev1.ExpectedPowerShelfRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.ExpectedPowerShelfId == nil || in.ExpectedPowerShelfId.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for DeleteExpectedPowerShelf")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateExpectedPowerShelf(ctx context.Context, in *corev1.ExpectedPowerShelf, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.ExpectedPowerShelfId == nil || in.ExpectedPowerShelfId.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for UpdateExpectedPowerShelf")
	}
	if in.BmcMacAddress == "" {
		return nil, status.Error(codes.Internal, "MAC address not provided for UpdateExpectedPowerShelf")
	}
	if in.ShelfSerialNumber == "" {
		return nil, status.Error(codes.Internal, "Shelf Serial Number not provided for UpdateExpectedPowerShelf")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllExpectedPowerShelves(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.ExpectedPowerShelfList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve expected power shelves")
		}
	}

	out := &corev1.ExpectedPowerShelfList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		mac, _ := net.ParseMAC("02:00:00:00:00:00")
		for range count {
			var uuidBytes [16]byte
			copy(uuidBytes[:6], mac)
			epsID, _ := uuid.FromBytes(uuidBytes[:])
			out.ExpectedPowerShelves = append(out.ExpectedPowerShelves, &corev1.ExpectedPowerShelf{
				ExpectedPowerShelfId: &corev1.UUID{Value: epsID.String()},
				BmcMacAddress:        mac.String(),
				ShelfSerialNumber:    "shelf-serial-" + mac.String()})
			incrementMAC(mac)
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllExpectedPowerShelvesLinked(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.LinkedExpectedPowerShelfList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve linked expected power shelves")
		}
	}

	out := &corev1.LinkedExpectedPowerShelfList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		mac, _ := net.ParseMAC("02:00:00:00:00:00")
		for range count {
			var uuidBytes [16]byte
			copy(uuidBytes[:6], mac)
			powerShelfID, _ := uuid.FromBytes(uuidBytes[:])

			out.ExpectedPowerShelves = append(out.ExpectedPowerShelves, &corev1.LinkedExpectedPowerShelf{
				ShelfSerialNumber: "shelf-serial-" + mac.String(),
				BmcMacAddress:     mac.String(),
				PowerShelfId:      &corev1.PowerShelfId{Id: powerShelfID.String()},
			})
			incrementMAC(mac)
		}
	}

	return out, nil
}

/* Expected Switch mock methods */
func (mcgsc *MockCoreGrpcServiceClient) AddExpectedSwitch(ctx context.Context, in *corev1.ExpectedSwitch, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.ExpectedSwitchId == nil || in.ExpectedSwitchId.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for AddExpectedSwitch")
	}
	if in.BmcMacAddress == "" {
		return nil, status.Error(codes.Internal, "MAC address not provided for AddExpectedSwitch")
	}
	if in.SwitchSerialNumber == "" {
		return nil, status.Error(codes.Internal, "Switch Serial Number not provided for AddExpectedSwitch")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteExpectedSwitch(ctx context.Context, in *corev1.ExpectedSwitchRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.ExpectedSwitchId == nil || in.ExpectedSwitchId.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for DeleteExpectedSwitch")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateExpectedSwitch(ctx context.Context, in *corev1.ExpectedSwitch, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.ExpectedSwitchId == nil || in.ExpectedSwitchId.Value == "" {
		return nil, status.Error(codes.Internal, "ID not provided for UpdateExpectedSwitch")
	}
	if in.BmcMacAddress == "" {
		return nil, status.Error(codes.Internal, "MAC address not provided for UpdateExpectedSwitch")
	}
	if in.SwitchSerialNumber == "" {
		return nil, status.Error(codes.Internal, "Switch Serial Number not provided for UpdateExpectedSwitch")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllExpectedSwitches(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.ExpectedSwitchList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve expected switches")
		}
	}

	out := &corev1.ExpectedSwitchList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		mac, _ := net.ParseMAC("02:00:00:00:00:00")
		for range count {
			var uuidBytes [16]byte
			copy(uuidBytes[:6], mac)
			esID, _ := uuid.FromBytes(uuidBytes[:])
			out.ExpectedSwitches = append(out.ExpectedSwitches, &corev1.ExpectedSwitch{
				ExpectedSwitchId:   &corev1.UUID{Value: esID.String()},
				BmcMacAddress:      mac.String(),
				SwitchSerialNumber: "switch-serial-" + mac.String()})
			incrementMAC(mac)
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllExpectedSwitchesLinked(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.LinkedExpectedSwitchList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve linked expected switches")
		}
	}

	out := &corev1.LinkedExpectedSwitchList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		mac, _ := net.ParseMAC("02:00:00:00:00:00")
		for range count {
			var uuidBytes [16]byte
			copy(uuidBytes[:6], mac)
			switchID, _ := uuid.FromBytes(uuidBytes[:])

			out.ExpectedSwitches = append(out.ExpectedSwitches, &corev1.LinkedExpectedSwitch{
				SwitchSerialNumber: "switch-serial-" + mac.String(),
				BmcMacAddress:      mac.String(),
				SwitchId:           &corev1.SwitchId{Id: switchID.String()},
			})
			incrementMAC(mac)
		}
	}

	return out, nil
}

/* Expected Rack mock methods */
func (mcgsc *MockCoreGrpcServiceClient) AddExpectedRack(ctx context.Context, in *corev1.ExpectedRack, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.RackId == nil || in.RackId.Id == "" {
		return nil, status.Error(codes.Internal, "ID not provided for AddExpectedRack")
	}
	if in.RackProfileId == nil || in.RackProfileId.Id == "" {
		return nil, status.Error(codes.Internal, "Rack Profile ID not provided for AddExpectedRack")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateExpectedRack(ctx context.Context, in *corev1.ExpectedRack, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.RackId == nil || in.RackId.Id == "" {
		return nil, status.Error(codes.Internal, "ID not provided for UpdateExpectedRack")
	}
	if in.RackProfileId == nil || in.RackProfileId.Id == "" {
		return nil, status.Error(codes.Internal, "Rack Profile ID not provided for UpdateExpectedRack")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteExpectedRack(ctx context.Context, in *corev1.ExpectedRackRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in.RackId == "" {
		return nil, status.Error(codes.Internal, "ID not provided for DeleteExpectedRack")
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetExpectedRack(ctx context.Context, in *corev1.ExpectedRackRequest, opts ...grpc.CallOption) (*corev1.ExpectedRack, error) {
	if in.RackId == "" {
		return nil, status.Error(codes.Internal, "ID not provided for GetExpectedRack")
	}
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve expected rack")
		}
	}
	out := &corev1.ExpectedRack{
		RackId:        &corev1.RackId{Id: in.RackId},
		RackProfileId: &corev1.RackProfileId{Id: uuid.NewString()},
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllExpectedRacks(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.ExpectedRackList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		if status.Code(err) == codes.Internal {
			return nil, status.Error(codes.Internal, "failed to retrieve expected racks")
		}
	}

	out := &corev1.ExpectedRackList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.ExpectedRacks = append(out.ExpectedRacks, &corev1.ExpectedRack{
				RackId:        &corev1.RackId{Id: uuid.NewString()},
				RackProfileId: &corev1.RackProfileId{Id: uuid.NewString()},
			})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) ReplaceAllExpectedRacks(ctx context.Context, in *corev1.ExpectedRackList, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	if in == nil {
		return nil, status.Error(codes.Internal, "Invalid request argument")
	}
	for _, er := range in.ExpectedRacks {
		if er == nil || er.RackId == nil || er.RackId.Id == "" {
			return nil, status.Error(codes.Internal, "ID not provided for ReplaceAllExpectedRacks")
		}
		if er.RackProfileId == nil || er.RackProfileId.Id == "" {
			return nil, status.Error(codes.Internal, "Rack Profile ID not provided for ReplaceAllExpectedRacks")
		}
	}
	out := new(emptypb.Empty)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteAllExpectedRacks(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

/* SKU mock methods */
func (mcgsc *MockCoreGrpcServiceClient) FindSkusByIds(ctx context.Context, in *corev1.SkusByIdsRequest, opts ...grpc.CallOption) (*corev1.SkuList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve skus")
	}

	out := &corev1.SkuList{}
	if in != nil {
		for _, id := range in.Ids {
			out.Skus = append(out.Skus, &corev1.Sku{
				Id: id,
			})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetAllSkuIds(ctx context.Context, in *emptypb.Empty, opts ...grpc.CallOption) (*corev1.SkuIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve sku ids")
	}

	out := &corev1.SkuIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.Ids = append(out.Ids, uuid.NewString())
		}
	}

	return out, nil
}

/* DPU Extension Service mock methods */
func (mcgsc *MockCoreGrpcServiceClient) CreateDpuExtensionService(ctx context.Context, in *corev1.CreateDpuExtensionServiceRequest, opts ...grpc.CallOption) (*corev1.DpuExtensionService, error) {
	versionInfo := &corev1.DpuExtensionServiceVersionInfo{
		Version:       generateSiteVersion(),
		Data:          "test data",
		HasCredential: false,
		Observability: in.Observability,
	}

	serviceID := uuid.NewString()
	if in.ServiceId != nil {
		serviceID = *in.ServiceId
	}

	out := &corev1.DpuExtensionService{
		ServiceId:            serviceID,
		ServiceName:          in.ServiceName,
		ServiceType:          in.ServiceType,
		TenantOrganizationId: in.TenantOrganizationId,
		LatestVersionInfo:    versionInfo,
		ActiveVersions:       []string{versionInfo.Version},
	}

	if in.Description != nil {
		out.Description = *in.Description
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateDpuExtensionService(ctx context.Context, in *corev1.UpdateDpuExtensionServiceRequest, opts ...grpc.CallOption) (*corev1.DpuExtensionService, error) {
	versionInfo := &corev1.DpuExtensionServiceVersionInfo{
		Version:       generateSiteVersion(),
		Data:          "test data",
		HasCredential: false,
		Observability: in.Observability,
	}

	out := &corev1.DpuExtensionService{
		ServiceId:         in.ServiceId,
		LatestVersionInfo: versionInfo,
		ActiveVersions:    []string{versionInfo.Version},
	}

	if in.ServiceName != nil {
		out.ServiceName = *in.ServiceName
	}

	if in.Description != nil {
		out.Description = *in.Description
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteDpuExtensionService(ctx context.Context, in *corev1.DeleteDpuExtensionServiceRequest, opts ...grpc.CallOption) (*corev1.DeleteDpuExtensionServiceResponse, error) {
	out := new(corev1.DeleteDpuExtensionServiceResponse)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindDpuExtensionServiceIds(ctx context.Context, in *corev1.DpuExtensionServiceSearchFilter, opts ...grpc.CallOption) (*corev1.DpuExtensionServiceIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve dpu extension service ids")
	}

	out := &corev1.DpuExtensionServiceIdList{}
	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.ServiceIds = append(out.ServiceIds, uuid.NewString())
		}
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindDpuExtensionServicesByIds(ctx context.Context, in *corev1.DpuExtensionServicesByIdsRequest, opts ...grpc.CallOption) (*corev1.DpuExtensionServiceList, error) {
	out := &corev1.DpuExtensionServiceList{}
	if in != nil {
		for _, id := range in.ServiceIds {
			out.Services = append(out.Services, &corev1.DpuExtensionService{
				ServiceId: id,
			})
		}
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetDpuExtensionServiceVersionsInfo(ctx context.Context, in *corev1.GetDpuExtensionServiceVersionsInfoRequest, opts ...grpc.CallOption) (*corev1.DpuExtensionServiceVersionInfoList, error) {
	out := &corev1.DpuExtensionServiceVersionInfoList{
		VersionInfos: []*corev1.DpuExtensionServiceVersionInfo{},
	}
	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.VersionInfos = append(out.VersionInfos, &corev1.DpuExtensionServiceVersionInfo{
				Version:       generateSiteVersion(),
				Data:          "test data",
				HasCredential: false,
			})
		}
	}
	return out, nil
}

// NVLink Logical Partition Mocks
func (mcgsc *MockCoreGrpcServiceClient) CreateNVLinkLogicalPartition(ctx context.Context, in *corev1.NVLinkLogicalPartitionCreationRequest, opts ...grpc.CallOption) (*corev1.NVLinkLogicalPartition, error) {
	out := new(corev1.NVLinkLogicalPartition)
	if in != nil {
		out.Id = in.Id
		out.Config = in.Config
		out.Config.Metadata = in.Config.Metadata
		out.Config.TenantOrganizationId = in.Config.TenantOrganizationId
		out.Status = &corev1.NVLinkLogicalPartitionStatus{
			State: corev1.TenantState_READY,
		}
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) UpdateNVLinkLogicalPartition(ctx context.Context, in *corev1.NVLinkLogicalPartitionUpdateRequest, opts ...grpc.CallOption) (*corev1.NVLinkLogicalPartitionUpdateResult, error) {
	out := new(corev1.NVLinkLogicalPartitionUpdateResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteNVLinkLogicalPartition(ctx context.Context, in *corev1.NVLinkLogicalPartitionDeletionRequest, opts ...grpc.CallOption) (*corev1.NVLinkLogicalPartitionDeletionResult, error) {
	out := new(corev1.NVLinkLogicalPartitionDeletionResult)
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindNVLinkLogicalPartitionIds(ctx context.Context, in *corev1.NVLinkLogicalPartitionSearchFilter, opts ...grpc.CallOption) (*corev1.NVLinkLogicalPartitionIdList, error) {
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, status.Error(status.Code(err), "failed to retrieve nvlink logical partition ids")
	}

	out := &corev1.NVLinkLogicalPartitionIdList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.PartitionIds = append(out.PartitionIds, &corev1.NVLinkLogicalPartitionId{Value: uuid.NewString()})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) FindNVLinkLogicalPartitionsByIds(ctx context.Context, in *corev1.NVLinkLogicalPartitionsByIdsRequest, opts ...grpc.CallOption) (*corev1.NVLinkLogicalPartitionList, error) {
	err, ok := ctx.Value("wantError").(error)
	if ok {
		return nil, status.Error(status.Code(err), "failed to retrieve nvlink logical partitions")
	}

	out := &corev1.NVLinkLogicalPartitionList{}
	if in != nil {
		for _, id := range in.PartitionIds {
			out.Partitions = append(out.Partitions, &corev1.NVLinkLogicalPartition{
				Id: id,
			})
		}
	}

	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) NVLinkLogicalPartitionsForTenant(ctx context.Context, in *corev1.TenantSearchQuery, opts ...grpc.CallOption) (*corev1.NVLinkLogicalPartitionList, error) {
	out := &corev1.NVLinkLogicalPartitionList{}

	count, ok := ctx.Value("wantCount").(int)
	if ok {
		for range count {
			out.Partitions = append(out.Partitions, &corev1.NVLinkLogicalPartition{
				Id: &corev1.NVLinkLogicalPartitionId{Value: uuid.NewString()},
			})
		}
	}

	return out, nil
}

/* Machine Identity (JWT-SVID) mock methods */

// SetTenantIdentityConfiguration returns a minimally-populated response echoing the
// incoming config. On simulated first-create the two timestamps are equal.
func (mcgsc *MockCoreGrpcServiceClient) SetTenantIdentityConfiguration(ctx context.Context, in *corev1.SetTenantIdentityConfigRequest, opts ...grpc.CallOption) (*corev1.TenantIdentityConfigResponse, error) {
	now := timestamppb.Now()
	return &corev1.TenantIdentityConfigResponse{
		OrganizationId: in.GetOrganizationId(),
		Config:         in.GetConfig(),
		SigningKeys: []*corev1.TenantIdentitySigningKey{
			{Kid: uuid.NewString(), Alg: "ES256", CurrentSigner: true},
		},
		CreatedAt: now,
		UpdatedAt: now,
	}, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetTenantIdentityConfiguration(ctx context.Context, in *corev1.GetTenantIdentityConfigRequest, opts ...grpc.CallOption) (*corev1.TenantIdentityConfigResponse, error) {
	now := timestamppb.Now()
	return &corev1.TenantIdentityConfigResponse{
		OrganizationId: in.GetOrganizationId(),
		Config: &corev1.TenantIdentityConfig{
			Enabled:         true,
			Issuer:          "https://carbide.example.com/iss",
			DefaultAudience: "openbao",
			TokenTtlSec:     600,
		},
		SigningKeys: []*corev1.TenantIdentitySigningKey{
			{Kid: "mock-key-id", Alg: "ES256", CurrentSigner: true},
		},
		CreatedAt: now,
		UpdatedAt: now,
	}, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteTenantIdentityConfiguration(ctx context.Context, in *corev1.GetTenantIdentityConfigRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	return &emptypb.Empty{}, nil
}

func (mcgsc *MockCoreGrpcServiceClient) SetTokenDelegation(ctx context.Context, in *corev1.TokenDelegationRequest, opts ...grpc.CallOption) (*corev1.TokenDelegationResponse, error) {
	now := timestamppb.Now()
	out := &corev1.TokenDelegationResponse{
		OrganizationId:       in.GetOrganizationId(),
		TokenEndpoint:        in.GetConfig().GetTokenEndpoint(),
		SubjectTokenAudience: in.GetConfig().GetSubjectTokenAudience(),
		CreatedAt:            now,
		UpdatedAt:            now,
	}
	if basic := in.GetConfig().GetClientSecretBasic(); basic != nil {
		out.AuthMethodConfig = &corev1.TokenDelegationResponse_ClientSecretBasic{
			ClientSecretBasic: &corev1.ClientSecretBasicResponse{
				ClientId:         basic.GetClientId(),
				ClientSecretHash: "sha256:mock-hash",
			},
		}
	}
	return out, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetTokenDelegation(ctx context.Context, in *corev1.GetTokenDelegationRequest, opts ...grpc.CallOption) (*corev1.TokenDelegationResponse, error) {
	now := timestamppb.Now()
	return &corev1.TokenDelegationResponse{
		OrganizationId:       in.GetOrganizationId(),
		TokenEndpoint:        "https://auth.example.com/oauth2/token",
		SubjectTokenAudience: "mock-exchange-audience",
		AuthMethodConfig: &corev1.TokenDelegationResponse_ClientSecretBasic{
			ClientSecretBasic: &corev1.ClientSecretBasicResponse{
				ClientId:         "mock-client-id",
				ClientSecretHash: "sha256:mock-hash",
			},
		},
		CreatedAt: now,
		UpdatedAt: now,
	}, nil
}

func (mcgsc *MockCoreGrpcServiceClient) DeleteTokenDelegation(ctx context.Context, in *corev1.GetTokenDelegationRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	return &emptypb.Empty{}, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetJWKS(ctx context.Context, in *corev1.JwksRequest, opts ...grpc.CallOption) (*corev1.Jwks, error) {
	use := "sig"
	if in.GetKind() == corev1.JwksKind_Spiffe {
		use = "jwt-svid"
	}
	jwks := `{"keys":[{"kty":"EC","use":"` + use + `","crv":"P-256","kid":"mock-key-id",` +
		`"x":"mock-x","y":"mock-y","alg":"ES256"}]}`
	return &corev1.Jwks{Jwks: jwks}, nil
}

func (mcgsc *MockCoreGrpcServiceClient) GetOpenIDConfiguration(ctx context.Context, in *corev1.OpenIdConfigRequest, opts ...grpc.CallOption) (*corev1.OpenIdConfiguration, error) {
	iss := "https://carbide.example.com/iss"
	return &corev1.OpenIdConfiguration{
		Issuer:                           iss,
		JwksUri:                          iss + "/.well-known/jwks.json",
		ResponseTypesSupported:           []string{"token"},
		SubjectTypesSupported:            []string{"public"},
		IdTokenSigningAlgValuesSupported: []string{},
		SpiffeJwksUri:                    iss + "/.well-known/spiffe/jwks.json",
	}, nil
}

// NewMockCoreGrpcClient creates a new mock CoreGrpcClient
func NewMockCoreGrpcClient() *CoreGrpcClient {
	return &CoreGrpcClient{
		grpcServiceClient: &MockCoreGrpcServiceClient{},
	}
}

// MockFlowGrpcService is a mock implementation of Flow gRPC protobuf Service
type MockFlowGrpcServiceClient struct {
	flowv1.FlowClient
}

/* Version mock methods */
func (mfgsc *MockFlowGrpcServiceClient) Version(ctx context.Context, in *flowv1.VersionRequest, opts ...grpc.CallOption) (*flowv1.BuildInfo, error) {
	out := &flowv1.BuildInfo{
		Version:   "1.0.0",
		BuildTime: time.Now().Format(time.RFC3339),
		GitCommit: "test-commit",
	}
	return out, nil
}

/* Rack mock methods */
func (mfgsc *MockFlowGrpcServiceClient) CreateExpectedRack(ctx context.Context, in *flowv1.CreateExpectedRackRequest, opts ...grpc.CallOption) (*flowv1.CreateExpectedRackResponse, error) {
	out := &flowv1.CreateExpectedRackResponse{
		Id: &flowv1.UUID{Id: uuid.NewString()},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) PatchRack(ctx context.Context, in *flowv1.PatchRackRequest, opts ...grpc.CallOption) (*flowv1.PatchRackResponse, error) {
	out := new(flowv1.PatchRackResponse)
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetRackInfoByID(ctx context.Context, in *flowv1.GetRackInfoByIDRequest, opts ...grpc.CallOption) (*flowv1.GetRackInfoResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	// Check for custom response via context
	if resp, ok := ctx.Value("wantResponse").(*flowv1.GetRackInfoResponse); ok {
		return resp, nil
	}

	out := &flowv1.GetRackInfoResponse{
		Rack: &flowv1.Rack{
			Info: &flowv1.DeviceInfo{
				Id: in.GetId(),
			},
		},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetRackInfoBySerial(ctx context.Context, in *flowv1.GetRackInfoBySerialRequest, opts ...grpc.CallOption) (*flowv1.GetRackInfoResponse, error) {
	out := &flowv1.GetRackInfoResponse{
		Rack: &flowv1.Rack{
			Info: &flowv1.DeviceInfo{
				SerialNumber: in.GetSerialInfo().GetSerialNumber(),
			},
		},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetListOfRacks(ctx context.Context, in *flowv1.GetListOfRacksRequest, opts ...grpc.CallOption) (*flowv1.GetListOfRacksResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	// Check for custom response via context
	if resp, ok := ctx.Value("wantResponse").(*flowv1.GetListOfRacksResponse); ok {
		return resp, nil
	}

	out := &flowv1.GetListOfRacksResponse{
		Racks: []*flowv1.Rack{},
	}
	return out, nil
}

/* Component mock methods */
func (mfgsc *MockFlowGrpcServiceClient) GetComponentInfoByID(ctx context.Context, in *flowv1.GetComponentInfoByIDRequest, opts ...grpc.CallOption) (*flowv1.GetComponentInfoResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	// Check for custom response via context
	if resp, ok := ctx.Value("wantResponse").(*flowv1.GetComponentInfoResponse); ok {
		return resp, nil
	}

	out := &flowv1.GetComponentInfoResponse{
		Component: &flowv1.Component{
			ComponentId: in.GetId().GetId(),
		},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetComponentInfoBySerial(ctx context.Context, in *flowv1.GetComponentInfoBySerialRequest, opts ...grpc.CallOption) (*flowv1.GetComponentInfoResponse, error) {
	out := &flowv1.GetComponentInfoResponse{
		Component: &flowv1.Component{
			Info: &flowv1.DeviceInfo{
				SerialNumber: in.GetSerialInfo().GetSerialNumber(),
			},
		},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetComponents(ctx context.Context, in *flowv1.GetComponentsRequest, opts ...grpc.CallOption) (*flowv1.GetComponentsResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	// Check for custom response via context
	if resp, ok := ctx.Value("wantResponse").(*flowv1.GetComponentsResponse); ok {
		return resp, nil
	}

	out := &flowv1.GetComponentsResponse{
		Components: []*flowv1.Component{},
		Total:      0,
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) ValidateComponents(ctx context.Context, in *flowv1.ValidateComponentsRequest, opts ...grpc.CallOption) (*flowv1.ValidateComponentsResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	// Check for custom response via context
	if resp, ok := ctx.Value("wantResponse").(*flowv1.ValidateComponentsResponse); ok {
		return resp, nil
	}

	out := &flowv1.ValidateComponentsResponse{
		Diffs:           []*flowv1.ComponentDiff{},
		TotalDiffs:      0,
		MissingCount:    0,
		UnexpectedCount: 0,
		MismatchCount:   0,
		MatchCount:      0,
	}
	return out, nil
}

/* Component mutation mock methods */
func (mfgsc *MockFlowGrpcServiceClient) AddComponent(ctx context.Context, in *flowv1.AddComponentRequest, opts ...grpc.CallOption) (*flowv1.AddComponentResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	// Check for custom response via context
	if resp, ok := ctx.Value("wantResponse").(*flowv1.AddComponentResponse); ok {
		return resp, nil
	}

	out := &flowv1.AddComponentResponse{
		Component: &flowv1.Component{},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) PatchComponent(ctx context.Context, in *flowv1.PatchComponentRequest, opts ...grpc.CallOption) (*flowv1.PatchComponentResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	// Check for custom response via context
	if resp, ok := ctx.Value("wantResponse").(*flowv1.PatchComponentResponse); ok {
		return resp, nil
	}

	out := &flowv1.PatchComponentResponse{
		Component: &flowv1.Component{},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) DeleteComponent(ctx context.Context, in *flowv1.DeleteComponentRequest, opts ...grpc.CallOption) (*flowv1.DeleteComponentResponse, error) {
	// Check for error injection via context
	if err, ok := ctx.Value("wantError").(error); ok {
		return nil, err
	}

	out := &flowv1.DeleteComponentResponse{}
	return out, nil
}

/* NVL Domain mock methods */
func (mfgsc *MockFlowGrpcServiceClient) CreateNVLDomain(ctx context.Context, in *flowv1.CreateNVLDomainRequest, opts ...grpc.CallOption) (*flowv1.CreateNVLDomainResponse, error) {
	out := &flowv1.CreateNVLDomainResponse{
		Id: &flowv1.UUID{Id: uuid.NewString()},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) AttachRacksToNVLDomain(ctx context.Context, in *flowv1.AttachRacksToNVLDomainRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) DetachRacksFromNVLDomain(ctx context.Context, in *flowv1.DetachRacksFromNVLDomainRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetListOfNVLDomains(ctx context.Context, in *flowv1.GetListOfNVLDomainsRequest, opts ...grpc.CallOption) (*flowv1.GetListOfNVLDomainsResponse, error) {
	out := &flowv1.GetListOfNVLDomainsResponse{
		NvlDomains: []*flowv1.NVLDomain{},
		Total:      0,
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetRacksForNVLDomain(ctx context.Context, in *flowv1.GetRacksForNVLDomainRequest, opts ...grpc.CallOption) (*flowv1.GetRacksForNVLDomainResponse, error) {
	out := &flowv1.GetRacksForNVLDomainResponse{
		Racks: []*flowv1.Rack{},
	}
	return out, nil
}

/* Task mock methods */
func (mfgsc *MockFlowGrpcServiceClient) UpgradeFirmware(ctx context.Context, in *flowv1.UpgradeFirmwareRequest, opts ...grpc.CallOption) (*flowv1.SubmitTaskResponse, error) {
	out := &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) PowerOnRack(ctx context.Context, in *flowv1.PowerOnRackRequest, opts ...grpc.CallOption) (*flowv1.SubmitTaskResponse, error) {
	out := &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) PowerOffRack(ctx context.Context, in *flowv1.PowerOffRackRequest, opts ...grpc.CallOption) (*flowv1.SubmitTaskResponse, error) {
	out := &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) PowerResetRack(ctx context.Context, in *flowv1.PowerResetRackRequest, opts ...grpc.CallOption) (*flowv1.SubmitTaskResponse, error) {
	out := &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) BringUpRack(ctx context.Context, in *flowv1.BringUpRackRequest, opts ...grpc.CallOption) (*flowv1.SubmitTaskResponse, error) {
	out := &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) IngestRack(ctx context.Context, in *flowv1.IngestRackRequest, opts ...grpc.CallOption) (*flowv1.SubmitTaskResponse, error) {
	out := &flowv1.SubmitTaskResponse{
		TaskIds: []*flowv1.UUID{{Id: uuid.NewString()}},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) ListTasks(ctx context.Context, in *flowv1.ListTasksRequest, opts ...grpc.CallOption) (*flowv1.ListTasksResponse, error) {
	out := &flowv1.ListTasksResponse{
		Tasks: []*flowv1.Task{},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetTasksByIDs(ctx context.Context, in *flowv1.GetTasksByIDsRequest, opts ...grpc.CallOption) (*flowv1.GetTasksByIDsResponse, error) {
	out := &flowv1.GetTasksByIDsResponse{
		Tasks: []*flowv1.Task{},
	}
	if in != nil {
		for _, taskID := range in.GetTaskIds() {
			out.Tasks = append(out.Tasks, &flowv1.Task{
				Id: taskID,
			})
		}
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) CancelTask(ctx context.Context, in *flowv1.CancelTaskRequest, opts ...grpc.CallOption) (*flowv1.CancelTaskResponse, error) {
	out := &flowv1.CancelTaskResponse{}
	if in != nil && in.GetTaskId() != nil {
		out.Task = &flowv1.Task{
			Id:     in.GetTaskId(),
			Status: flowv1.TaskStatus_TASK_STATUS_TERMINATED,
		}
	}
	return out, nil
}

/* Operation rule mock methods */
func (mfgsc *MockFlowGrpcServiceClient) CreateOperationRule(ctx context.Context, in *flowv1.CreateOperationRuleRequest, opts ...grpc.CallOption) (*flowv1.CreateOperationRuleResponse, error) {
	out := &flowv1.CreateOperationRuleResponse{
		Id: &flowv1.UUID{Id: uuid.NewString()},
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) UpdateOperationRule(ctx context.Context, in *flowv1.UpdateOperationRuleRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) DeleteOperationRule(ctx context.Context, in *flowv1.DeleteOperationRuleRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetOperationRule(ctx context.Context, in *flowv1.GetOperationRuleRequest, opts ...grpc.CallOption) (*flowv1.OperationRule, error) {
	out := &flowv1.OperationRule{}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) ListOperationRules(ctx context.Context, in *flowv1.ListOperationRulesRequest, opts ...grpc.CallOption) (*flowv1.ListOperationRulesResponse, error) {
	out := &flowv1.ListOperationRulesResponse{
		Rules:      []*flowv1.OperationRule{},
		TotalCount: 0,
	}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) SetRuleAsDefault(ctx context.Context, in *flowv1.SetRuleAsDefaultRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

/* Rack-rule association mock methods */
func (mfgsc *MockFlowGrpcServiceClient) AssociateRuleWithRack(ctx context.Context, in *flowv1.AssociateRuleWithRackRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) DisassociateRuleFromRack(ctx context.Context, in *flowv1.DisassociateRuleFromRackRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	out := new(emptypb.Empty)
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetRackRuleAssociation(ctx context.Context, in *flowv1.GetRackRuleAssociationRequest, opts ...grpc.CallOption) (*flowv1.GetRackRuleAssociationResponse, error) {
	out := &flowv1.GetRackRuleAssociationResponse{}
	return out, nil
}

func (mfgsc *MockFlowGrpcServiceClient) ListRackRuleAssociations(ctx context.Context, in *flowv1.ListRackRuleAssociationsRequest, opts ...grpc.CallOption) (*flowv1.ListRackRuleAssociationsResponse, error) {
	out := &flowv1.ListRackRuleAssociationsResponse{
		Associations: []*flowv1.RackRuleAssociation{},
	}
	return out, nil
}

/* Operation Run mock methods */
func (mfgsc *MockFlowGrpcServiceClient) CreateOperationRun(ctx context.Context, in *flowv1.CreateOperationRunRequest, opts ...grpc.CallOption) (*flowv1.CreateOperationRunResponse, error) {
	return &flowv1.CreateOperationRunResponse{Id: &flowv1.UUID{Id: uuid.NewString()}}, nil
}

func (mfgsc *MockFlowGrpcServiceClient) GetOperationRun(ctx context.Context, in *flowv1.GetOperationRunRequest, opts ...grpc.CallOption) (*flowv1.GetOperationRunResponse, error) {
	return &flowv1.GetOperationRunResponse{
		OperationRun: &flowv1.OperationRun{Summary: &flowv1.OperationRunSummary{Id: in.GetId()}},
	}, nil
}

func (mfgsc *MockFlowGrpcServiceClient) ListOperationRuns(ctx context.Context, in *flowv1.ListOperationRunsRequest, opts ...grpc.CallOption) (*flowv1.ListOperationRunsResponse, error) {
	return &flowv1.ListOperationRunsResponse{OperationRuns: []*flowv1.OperationRunSummary{}, Total: 0}, nil
}

func (mfgsc *MockFlowGrpcServiceClient) ListOperationRunTargets(ctx context.Context, in *flowv1.ListOperationRunTargetsRequest, opts ...grpc.CallOption) (*flowv1.ListOperationRunTargetsResponse, error) {
	return &flowv1.ListOperationRunTargetsResponse{Targets: []*flowv1.OperationRunTarget{}, Total: 0}, nil
}

func (mfgsc *MockFlowGrpcServiceClient) PauseOperationRun(ctx context.Context, in *flowv1.PauseOperationRunRequest, opts ...grpc.CallOption) (*flowv1.OperationRun, error) {
	return &flowv1.OperationRun{Summary: &flowv1.OperationRunSummary{Id: in.GetId()}}, nil
}

func (mfgsc *MockFlowGrpcServiceClient) ResumeOperationRun(ctx context.Context, in *flowv1.ResumeOperationRunRequest, opts ...grpc.CallOption) (*flowv1.OperationRun, error) {
	return &flowv1.OperationRun{Summary: &flowv1.OperationRunSummary{Id: in.GetId()}}, nil
}

func (mfgsc *MockFlowGrpcServiceClient) AdvanceOperationRunPhase(ctx context.Context, in *flowv1.AdvanceOperationRunPhaseRequest, opts ...grpc.CallOption) (*flowv1.OperationRun, error) {
	return &flowv1.OperationRun{Summary: &flowv1.OperationRunSummary{Id: in.GetId()}}, nil
}

func (mfgsc *MockFlowGrpcServiceClient) CancelOperationRun(ctx context.Context, in *flowv1.CancelOperationRunRequest, opts ...grpc.CallOption) (*flowv1.OperationRun, error) {
	return &flowv1.OperationRun{Summary: &flowv1.OperationRunSummary{Id: in.GetId()}}, nil
}

// NewMockFlowClient creates a new mock FlowClient that can be used with FlowAtomicClient.SwapClient
func NewMockFlowGrpcClient() *FlowGrpcClient {
	return &FlowGrpcClient{
		grpcServiceClient: &MockFlowGrpcServiceClient{},
	}
}
