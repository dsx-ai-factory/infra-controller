// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import (
	"context"
	"errors"
	"net/http"
	"net/url"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRepeatableGetQuerySerialization(t *testing.T) {
	ctx := context.Background()
	tests := []struct {
		name    string
		path    string
		execute func(*APIClient) error
		query   url.Values
	}{
		{
			name: "get-all-allocation",
			path: "/v2/org/test-org/nico/allocation",
			execute: func(client *APIClient) error {
				_, _, err := client.AllocationAPI.GetAllAllocation(ctx, "test-org").
					ConstraintType([]string{"Reserved", "OnDemand"}).
					ConstraintValue([]int32{10, 20}).
					Id([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					IncludeRelation([]string{"InfrastructureProvider", "Tenant"}).
					ResourceType([]string{"InstanceType", "IPBlock"}).
					ResourceTypeId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready", "Error"}).
					TenantId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Execute()
				return err
			},
			query: url.Values{
				"constraintType":  {"Reserved", "OnDemand"},
				"constraintValue": {"10", "20"},
				"id":              {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"includeRelation": {"InfrastructureProvider", "Tenant"},
				"pageNumber":      {"1"},
				"resourceType":    {"InstanceType", "IPBlock"},
				"resourceTypeId":  {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"siteId":          {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":          {"Ready", "Error"},
				"tenantId":        {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
			},
		},
		{
			name: "get-dpu-extension-service",
			path: "/v2/org/test-org/nico/dpu-extension-service/resource-id",
			execute: func(client *APIClient) error {
				_, _, err := client.DPUExtensionServiceAPI.GetDpuExtensionService(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"Site", "Tenant"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Site", "Tenant"},
			},
		},
		{
			name: "get-all-infiniband-interface",
			path: "/v2/org/test-org/nico/infiniband-interface",
			execute: func(client *APIClient) error {
				_, _, err := client.InfiniBandPartitionAPI.GetAllInfinibandInterface(ctx, "test-org").
					IncludeRelation([]string{"Instance", "InfiniBandPartition"}).
					InfinibandPartitionId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					InstanceId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready", "Error"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation":       {"Instance", "InfiniBandPartition"},
				"infinibandPartitionId": {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"instanceId":            {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"pageNumber":            {"1"},
				"siteId":                {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":                {"Ready", "Error"},
			},
		},
		{
			name: "get-all-instance",
			path: "/v2/org/test-org/nico/instance",
			execute: func(client *APIClient) error {
				_, _, err := client.InstanceAPI.GetAllInstance(ctx, "test-org").
					IncludeRelation([]string{"InfrastructureProvider", "Tenant"}).
					InstanceTypeId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					IpAddress([]string{"192.0.2.10", "2001:db8::1"}).
					MachineId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					NetworkSecurityGroupId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					OperatingSystemId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready", "Error"}).
					VpcId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation":        {"InfrastructureProvider", "Tenant"},
				"instanceTypeId":         {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"ipAddress":              {"192.0.2.10", "2001:db8::1"},
				"machineId":              {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"networkSecurityGroupId": {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"operatingSystemId":      {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"pageNumber":             {"1"},
				"siteId":                 {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":                 {"Ready", "Error"},
				"vpcId":                  {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
			},
		},
		{
			name: "get-all-instance-infiniband-interface",
			path: "/v2/org/test-org/nico/instance/resource-id/infiniband-interface",
			execute: func(client *APIClient) error {
				_, _, err := client.InstanceAPI.GetAllInstanceInfinibandInterface(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"Instance", "InfiniBandPartition"}).
					Status([]string{"Ready", "Error"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Instance", "InfiniBandPartition"},
				"pageNumber":      {"1"},
				"status":          {"Ready", "Error"},
			},
		},
		{
			name: "get-all-interface",
			path: "/v2/org/test-org/nico/instance/resource-id/interface",
			execute: func(client *APIClient) error {
				_, _, err := client.InstanceAPI.GetAllInterface(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"Instance", "Subnet"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Instance", "Subnet"},
				"pageNumber":      {"1"},
			},
		},
		{
			name: "get-all-instance-nvlink-interface",
			path: "/v2/org/test-org/nico/instance/resource-id/nvlink-interface",
			execute: func(client *APIClient) error {
				_, _, err := client.InstanceAPI.GetAllInstanceNvlinkInterface(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"Instance", "NVLinkLogicalPartition"}).
					Status([]string{"Ready", "Error"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Instance", "NVLinkLogicalPartition"},
				"pageNumber":      {"1"},
				"status":          {"Ready", "Error"},
			},
		},
		{
			name: "get-instance-status-history",
			path: "/v2/org/test-org/nico/instance/resource-id/status-history",
			execute: func(client *APIClient) error {
				_, _, err := client.InstanceAPI.GetInstanceStatusHistory(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"InfrastructureProvider", "Tenant"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"InfrastructureProvider", "Tenant"},
			},
		},
		{
			name: "get-all-machine",
			path: "/v2/org/test-org/nico/machine",
			execute: func(client *APIClient) error {
				_, _, err := client.MachineAPI.GetAllMachine(ctx, "test-org").
					CapabilityName([]string{"value A", "value,B&C"}).
					HwSkuDeviceType([]string{"value A", "value,B&C"}).
					Id([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					IncludeRelation([]string{"InfrastructureProvider", "Site"}).
					InstanceTypeId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready", "Error"}).
					TenantId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Execute()
				return err
			},
			query: url.Values{
				"capabilityName":  {"value A", "value,B&C"},
				"hwSkuDeviceType": {"value A", "value,B&C"},
				"id":              {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"includeRelation": {"InfrastructureProvider", "Site"},
				"instanceTypeId":  {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"pageNumber":      {"1"},
				"status":          {"Ready", "Error"},
				"tenantId":        {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
			},
		},
		{
			name: "get-all-machine-capabilities",
			path: "/v2/org/test-org/nico/machine-capability",
			execute: func(client *APIClient) error {
				_, _, err := client.MachineAPI.GetAllMachineCapabilities(ctx, "test-org").
					SiteId("f97df110-f4de-492e-8849-4a6af68026b0").
					InactiveDevices([]int32{3, 1}).
					Execute()
				return err
			},
			query: url.Values{
				"inactiveDevices": {"3", "1"},
				"orderBy":         {"TYPE_ASC"},
				"pageNumber":      {"1"},
				"siteId":          {"f97df110-f4de-492e-8849-4a6af68026b0"},
			},
		},
		{
			name: "get-all-nvlink-interface",
			path: "/v2/org/test-org/nico/nvlink-interface",
			execute: func(client *APIClient) error {
				_, _, err := client.NVLinkLogicalPartitionAPI.GetAllNvlinkInterface(ctx, "test-org").
					IncludeRelation([]string{"Instance", "NVLinkLogicalPartition"}).
					InstanceId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					NvLinkDomainId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					NvLinkLogicalPartitionId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready", "Error"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation":          {"Instance", "NVLinkLogicalPartition"},
				"instanceId":               {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"nvLinkDomainId":           {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"nvLinkLogicalPartitionId": {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"pageNumber":               {"1"},
				"siteId":                   {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":                   {"Ready", "Error"},
			},
		},
		{
			name: "get-all-operating-system",
			path: "/v2/org/test-org/nico/operating-system",
			execute: func(client *APIClient) error {
				_, _, err := client.OperatingSystemAPI.GetAllOperatingSystem(ctx, "test-org").
					IncludeRelation([]string{"InfrastructureProvider", "Tenant"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready", "Error"}).
					Type_([]string{"Image", "iPXE"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"InfrastructureProvider", "Tenant"},
				"pageNumber":      {"1"},
				"siteId":          {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":          {"Ready", "Error"},
				"type":            {"Image", "iPXE"},
			},
		},
		{
			name: "get-all-rack",
			path: "/v2/org/test-org/nico/rack",
			execute: func(client *APIClient) error {
				_, _, err := client.RackAPI.GetAllRack(ctx, "test-org").
					SiteId("f97df110-f4de-492e-8849-4a6af68026b0").
					Manufacturer([]string{"value A", "value,B&C"}).
					Name([]string{"value A", "value,B&C"}).
					Execute()
				return err
			},
			query: url.Values{
				"manufacturer": {"value A", "value,B&C"},
				"name":         {"value A", "value,B&C"},
				"pageNumber":   {"1"},
				"siteId":       {"f97df110-f4de-492e-8849-4a6af68026b0"},
			},
		},
		{
			name: "validate-racks",
			path: "/v2/org/test-org/nico/rack/validation",
			execute: func(client *APIClient) error {
				_, _, err := client.RackAPI.ValidateRacks(ctx, "test-org").
					SiteId("f97df110-f4de-492e-8849-4a6af68026b0").
					Manufacturer([]string{"value A", "value,B&C"}).
					Name([]string{"value A", "value,B&C"}).
					Execute()
				return err
			},
			query: url.Values{
				"manufacturer": {"value A", "value,B&C"},
				"name":         {"value A", "value,B&C"},
				"siteId":       {"f97df110-f4de-492e-8849-4a6af68026b0"},
			},
		},
		{
			name: "get-all-site",
			path: "/v2/org/test-org/nico/site",
			execute: func(client *APIClient) error {
				_, _, err := client.SiteAPI.GetAllSite(ctx, "test-org").
					IncludeRelation([]string{"InfrastructureProvider"}).
					Status([]string{"Ready", "Error"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"InfrastructureProvider"},
				"pageNumber":      {"1"},
				"status":          {"Ready", "Error"},
			},
		},
		{
			name: "get-site",
			path: "/v2/org/test-org/nico/site/resource-id",
			execute: func(client *APIClient) error {
				_, _, err := client.SiteAPI.GetSite(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"InfrastructureProvider"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"InfrastructureProvider"},
			},
		},
		{
			name: "get-all-spectrumx-partition",
			path: "/v2/org/test-org/nico/spectrumx-partition",
			execute: func(client *APIClient) error {
				_, _, err := client.SpectrumXPartitionAPI.GetAllSpectrumxPartition(ctx, "test-org").
					IncludeRelation([]string{"Site", "Tenant"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Pending", "Provisioning"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Site", "Tenant"},
				"orderBy":         {"CREATED_ASC"},
				"pageNumber":      {"1"},
				"siteId":          {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":          {"Pending", "Provisioning"},
			},
		},
		{
			name: "get-ssh-key",
			path: "/v2/org/test-org/nico/sshkey/resource-id",
			execute: func(client *APIClient) error {
				_, _, err := client.SSHKeyAPI.GetSshKey(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"Tenant"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Tenant"},
			},
		},
		{
			name: "get-ssh-key-group",
			path: "/v2/org/test-org/nico/sshkeygroup/resource-id",
			execute: func(client *APIClient) error {
				_, _, err := client.SSHKeyGroupAPI.GetSshKeyGroup(ctx, "test-org", "resource-id").
					IncludeRelation([]string{"Tenant"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Tenant"},
			},
		},
		{
			name: "get-all-tray",
			path: "/v2/org/test-org/nico/tray",
			execute: func(client *APIClient) error {
				_, _, err := client.TrayAPI.GetAllTray(ctx, "test-org").
					SiteId("f97df110-f4de-492e-8849-4a6af68026b0").
					Id([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Execute()
				return err
			},
			query: url.Values{
				"id":         {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"pageNumber": {"1"},
				"siteId":     {"f97df110-f4de-492e-8849-4a6af68026b0"},
			},
		},
		{
			name: "validate-trays",
			path: "/v2/org/test-org/nico/tray/validation",
			execute: func(client *APIClient) error {
				_, _, err := client.TrayAPI.ValidateTrays(ctx, "test-org").
					SiteId("f97df110-f4de-492e-8849-4a6af68026b0").
					Id([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Manufacturer([]string{"value A", "value,B&C"}).
					Name([]string{"value A", "value,B&C"}).
					Execute()
				return err
			},
			query: url.Values{
				"id":           {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"manufacturer": {"value A", "value,B&C"},
				"name":         {"value A", "value,B&C"},
				"siteId":       {"f97df110-f4de-492e-8849-4a6af68026b0"},
			},
		},
		{
			name: "get-all-vpc",
			path: "/v2/org/test-org/nico/vpc",
			execute: func(client *APIClient) error {
				_, _, err := client.VPCAPI.GetAllVpc(ctx, "test-org").
					IncludeRelation([]string{"InfrastructureProvider", "Tenant"}).
					NetworkSecurityGroupId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					NvLinkLogicalPartitionId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					SiteId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Ready", "Error"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation":          {"InfrastructureProvider", "Tenant"},
				"networkSecurityGroupId":   {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"nvLinkLogicalPartitionId": {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"pageNumber":               {"1"},
				"siteId":                   {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":                   {"Ready", "Error"},
			},
		},
		{
			name: "get-all-vpc-peering",
			path: "/v2/org/test-org/nico/vpc-peering",
			execute: func(client *APIClient) error {
				_, _, err := client.VPCPeeringAPI.GetAllVpcPeering(ctx, "test-org").
					IncludeRelation([]string{"Vpc1", "Vpc2"}).
					PeerTenantId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Status([]string{"Pending", "Configuring"}).
					VpcId([]string{"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"}).
					Execute()
				return err
			},
			query: url.Values{
				"includeRelation": {"Vpc1", "Vpc2"},
				"pageNumber":      {"1"},
				"peerTenantId":    {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
				"status":          {"Pending", "Configuring"},
				"vpcId":           {"f97df110-f4de-492e-8849-4a6af68026b0", "60189e9c-7d12-438c-b9ca-6998d9c364b1"},
			},
		},
		{
			name: "omitted filters",
			path: "/v2/org/test-org/nico/machine",
			execute: func(client *APIClient) error {
				_, _, err := client.MachineAPI.GetAllMachine(ctx, "test-org").
					Execute()
				return err
			},
			query: url.Values{
				"pageNumber": {"1"},
			},
		},
		{
			name: "nil filters",
			path: "/v2/org/test-org/nico/machine",
			execute: func(client *APIClient) error {
				_, _, err := client.MachineAPI.GetAllMachine(ctx, "test-org").
					Id(nil).
					Execute()
				return err
			},
			query: url.Values{
				"pageNumber": {"1"},
			},
		},
		{
			name: "empty filters",
			path: "/v2/org/test-org/nico/machine",
			execute: func(client *APIClient) error {
				_, _, err := client.MachineAPI.GetAllMachine(ctx, "test-org").
					Id([]string{}).
					Execute()
				return err
			},
			query: url.Values{
				"pageNumber": {"1"},
			},
		},
		{
			name: "single filter",
			path: "/v2/org/test-org/nico/machine",
			execute: func(client *APIClient) error {
				_, _, err := client.MachineAPI.GetAllMachine(ctx, "test-org").
					Id([]string{"machine-id"}).
					Execute()
				return err
			},
			query: url.Values{
				"id":         {"machine-id"},
				"pageNumber": {"1"},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			captureErr := errors.New("request captured")
			transport := &queryCaptureTransport{
				err: captureErr,
			}
			cfg := NewConfiguration()
			cfg.HTTPClient = &http.Client{
				Transport: transport,
			}

			err := tt.execute(NewAPIClient(cfg))

			require.ErrorIs(t, err, captureErr)
			require.NotNil(t, transport.request)
			assert.Equal(t, http.MethodGet, transport.request.Method)
			assert.Equal(t, tt.path, transport.request.URL.Path)
			assert.Equal(t, tt.query, transport.request.URL.Query())
		})
	}
}

type queryCaptureTransport struct {
	request *http.Request
	err     error
}

func (t *queryCaptureTransport) RoundTrip(request *http.Request) (*http.Response, error) {
	t.request = request.Clone(request.Context())
	return nil, t.err
}
