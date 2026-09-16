// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIDpuMachine_FromProto(t *testing.T) {
	const maxUint32 = uint32(1<<32 - 1)

	site := &cdbm.Site{
		ID:                       uuid.New(),
		InfrastructureProviderID: uuid.New(),
		Name:                     "test-site",
		Status:                   "REGISTERED",
	}

	protoDpuMachine := &corev1.DpuMachine{
		Machine: &corev1.Machine{
			Id: &corev1.MachineId{
				Id: "test-machine-id",
			},
			Status: &corev1.MachineStatus{
				DpuAgentVersion: cutil.GetPtr("1.0.0"),
				DiscoveryInfo: &corev1.DiscoveryInfo{
					DmiData: &corev1.DmiData{
						BoardName:     "test-board-name",
						BoardSerial:   "test-board-serial",
						BoardVersion:  "test-board-version",
						BiosDate:      "test-bios-date",
						BiosVersion:   "test-bios-version",
						ProductSerial: "test-product-serial",
						ChassisSerial: "test-chassis-serial",
						ProductName:   "test-product-name",
						SysVendor:     "test-sys-vendor",
					},
				},
				Interfaces: []*corev1.MachineInterface{
					{
						Id: &corev1.MachineInterfaceId{
							Value: "test-interface-id",
						},
					},
				},
				Health: &corev1.HealthReport{
					Source:     "test-health-source",
					ObservedAt: timestamppb.New(time.Now()),
					Successes: []*corev1.HealthProbeSuccess{
						{
							Id:     "test-success-id",
							Target: cutil.GetPtr("test-success-target"),
						},
					},
					Alerts: []*corev1.HealthProbeAlert{
						{
							Id:           "test-alert-id",
							Target:       cutil.GetPtr("test-alert-target"),
							InAlertSince: nil,
							Classifications: []string{
								"test-alert-classification",
							},
							Message:       "test-alert-message",
							TenantMessage: nil,
						},
					},
				},
			},
			BmcInfo: &corev1.BmcInfo{
				Ip: cutil.GetPtr("10.0.0.1"),
			},
			Inventory: &corev1.MachineComponentInventory{
				Components: []*corev1.MachineInventorySoftwareComponent{
					{
						Name:    "test-software-component",
						Version: "test-software-component-version",
						Url:     "test-software-component-url",
					},
				},
			},
			Metadata: &corev1.Metadata{
				Labels: []*corev1.Label{
					{
						Key:   "env",
						Value: cutil.GetPtr("test"),
					},
				},
			},
		},
		DpuNetworkConfig: &corev1.ManagedHostNetworkConfigResponse{
			Asn:                    maxUint32,
			VpcVni:                 cutil.GetPtr(maxUint32),
			MinDpuFunctioningLinks: cutil.GetPtr(maxUint32),
			InternetL3Vni:          cutil.GetPtr(maxUint32),
			DatacenterAsn:          maxUint32,
			TenantHostAsn:          cutil.GetPtr(maxUint32),
			SiteGlobalVpcVni:       cutil.GetPtr(maxUint32),
			AdminInterface: &corev1.FlatInterfaceConfig{
				VlanId:            maxUint32,
				Vni:               maxUint32,
				VirtualFunctionId: cutil.GetPtr(maxUint32),
				VpcVni:            maxUint32,
				VpcPeerVnis:       []uint32{maxUint32},
				Mtu:               cutil.GetPtr(maxUint32),
			},
		},
	}

	hostMachineID := "test-host-machine-id"
	protoContext := APIDpuMachineProtoContext{
		HostMachineID:            hostMachineID,
		SiteID:                   site.ID,
		InfrastructureProviderID: site.InfrastructureProviderID,
	}
	dpuMachine := APIDpuMachine{}
	dpuMachine.FromProto(protoDpuMachine, protoContext)

	assert.Equal(t, "test-machine-id", dpuMachine.ID)
	// HostMachineID must be the host Machine ID from the context, not the DPU's own ID.
	assert.Equal(t, hostMachineID, dpuMachine.HostMachineID)
	assert.NotEqual(t, dpuMachine.ID, dpuMachine.HostMachineID)
	assert.Equal(t, "1.0.0", dpuMachine.DpuAgentVersion)
	assert.Equal(t, "10.0.0.1", *dpuMachine.BMCInfo.IP)
	assert.Equal(t, "test-board-name", *dpuMachine.DMIData.BoardName)
	assert.Equal(t, "test-board-serial", *dpuMachine.DMIData.BoardSerial)
	assert.Equal(t, "test-board-version", *dpuMachine.DMIData.BoardVersion)
	assert.Equal(t, "test-product-name", *dpuMachine.DMIData.ProductName)
	assert.Equal(t, "test-sys-vendor", *dpuMachine.DMIData.SysVendor)
	require.Len(t, dpuMachine.Interfaces, 1)
	assert.Equal(t, "test-interface-id", dpuMachine.Interfaces[0].ID)
	require.NotNil(t, dpuMachine.Health)
	assert.Equal(t, "test-health-source", dpuMachine.Health.Source)
	assert.Equal(t, maxUint32, dpuMachine.DpuNetworkConfig.Asn)
	assert.Equal(t, maxUint32, *dpuMachine.DpuNetworkConfig.VpcVni)
	assert.Equal(t, maxUint32, *dpuMachine.DpuNetworkConfig.MinDpuFunctioningLinks)
	assert.Equal(t, maxUint32, *dpuMachine.DpuNetworkConfig.InternetL3Vni)
	assert.Equal(t, maxUint32, dpuMachine.DpuNetworkConfig.DatacenterAsn)
	assert.Equal(t, maxUint32, *dpuMachine.DpuNetworkConfig.TenantHostAsn)
	assert.Equal(t, maxUint32, *dpuMachine.DpuNetworkConfig.SiteGlobalVpcVni)
	require.NotNil(t, dpuMachine.DpuNetworkConfig.AdminInterface)
	assert.Equal(t, maxUint32, dpuMachine.DpuNetworkConfig.AdminInterface.VlanID)
	assert.Equal(t, maxUint32, dpuMachine.DpuNetworkConfig.AdminInterface.Vni)
	assert.Equal(t, maxUint32, *dpuMachine.DpuNetworkConfig.AdminInterface.VirtualFunctionID)
	assert.Equal(t, maxUint32, dpuMachine.DpuNetworkConfig.AdminInterface.VpcVni)
	assert.Equal(t, []uint32{maxUint32}, dpuMachine.DpuNetworkConfig.AdminInterface.VpcPeerVnis)
	assert.Equal(t, maxUint32, *dpuMachine.DpuNetworkConfig.AdminInterface.Mtu)

	populatedNetworkConfig := proto.Clone(protoDpuMachine.DpuNetworkConfig).(*corev1.ManagedHostNetworkConfigResponse)
	populatedNetworkConfig.DhcpServers = []string{"10.0.0.2"}
	populatedNetworkConfig.TenantInterfaces = []*corev1.FlatInterfaceConfig{
		{
			VlanId: 100,
		},
	}
	populatedNetworkConfig.RouteServers = []string{"10.0.0.3"}
	populatedNetworkConfig.DeprecatedDenyPrefixes = []string{"10.0.1.0/24"}
	populatedNetworkConfig.DenyPrefixes = []string{"10.0.2.0/24"}
	populatedNetworkConfig.SiteFabricPrefixes = []string{"10.0.3.0/24"}
	populatedNetworkConfig.AnycastSitePrefixes = []string{"10.0.4.0/24"}

	tests := []struct {
		name        string
		protoConfig *corev1.ManagedHostNetworkConfigResponse
		check       func(*testing.T, *APIDpuNetworkConfig)
	}{
		{
			name:        "empty collections serialize as arrays",
			protoConfig: protoDpuMachine.DpuNetworkConfig,
			check: func(t *testing.T, networkConfig *APIDpuNetworkConfig) {
				encodedNetworkConfig, err := json.Marshal(networkConfig)
				require.NoError(t, err)

				var networkConfigResponse map[string]interface{}
				err = json.Unmarshal(encodedNetworkConfig, &networkConfigResponse)
				require.NoError(t, err)

				for _, field := range []string{
					"dhcpServers",
					"tenantInterfaces",
					"routeServers",
					"deprecatedDenyPrefixes",
					"denyPrefixes",
					"siteFabricPrefixes",
					"anycastSitePrefixes",
				} {
					assert.Equal(t, []interface{}{}, networkConfigResponse[field])
				}
			},
		},
		{
			name:        "populated collections are preserved",
			protoConfig: populatedNetworkConfig,
			check: func(t *testing.T, networkConfig *APIDpuNetworkConfig) {
				assert.Equal(t, []string{"10.0.0.2"}, networkConfig.DhcpServers)
				require.Len(t, networkConfig.TenantInterfaces, 1)
				assert.Equal(t, uint32(100), networkConfig.TenantInterfaces[0].VlanID)
				assert.Equal(t, []string{"10.0.0.3"}, networkConfig.RouteServers)
				assert.Equal(t, []string{"10.0.1.0/24"}, networkConfig.DeprecatedDenyPrefixes)
				assert.Equal(t, []string{"10.0.2.0/24"}, networkConfig.DenyPrefixes)
				assert.Equal(t, []string{"10.0.3.0/24"}, networkConfig.SiteFabricPrefixes)
				assert.Equal(t, []string{"10.0.4.0/24"}, networkConfig.AnycastSitePrefixes)
			},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			testProtoDpuMachine := proto.Clone(protoDpuMachine).(*corev1.DpuMachine)
			testProtoDpuMachine.DpuNetworkConfig = test.protoConfig
			dpuMachine := APIDpuMachine{}
			dpuMachine.FromProto(testProtoDpuMachine, protoContext)
			test.check(t, dpuMachine.DpuNetworkConfig)
		})
	}
}

// TestAPIDpuMachine_FromProto_NilMachine guards against a panic when a
// DpuMachine proto carries no inner Machine (or interfaces with nil IDs):
// the Site worker / workflow could legitimately return such a shape, and the
// handler must surface a clean response rather than crash the process.
func TestAPIDpuMachine_FromProto_NilMachine(t *testing.T) {
	apdCtx := APIDpuMachineProtoContext{
		SiteID:                   uuid.New(),
		InfrastructureProviderID: uuid.New(),
	}

	assert.NotPanics(t, func() {
		apd := APIDpuMachine{}
		apd.FromProto(&corev1.DpuMachine{Machine: nil}, apdCtx)
	})

	assert.NotPanics(t, func() {
		apdi := APIDpuMachineInterface{}
		apdi.FromProto(&corev1.MachineInterface{})
	})
}

func TestAPIDpuMachineInterface_FromProto_InterfaceType(t *testing.T) {
	tests := []struct {
		name          string
		interfaceType *corev1.InterfaceType
		legacyIsBmc   *bool
		want          bool
	}{
		{
			name:          "uses BMC interface type",
			interfaceType: cutil.GetPtr(corev1.InterfaceType_INTERFACE_TYPE_BMC),
			legacyIsBmc:   cutil.GetPtr(false),
			want:          true,
		},
		{
			name:          "uses data interface type",
			interfaceType: cutil.GetPtr(corev1.InterfaceType_INTERFACE_TYPE_DATA),
			legacyIsBmc:   cutil.GetPtr(true),
			want:          false,
		},
		{
			name:        "falls back to legacy BMC field",
			legacyIsBmc: cutil.GetPtr(true),
			want:        true,
		},
		{
			name: "defaults to data interface",
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			protoInterface := &corev1.MachineInterface{
				InterfaceType: tt.interfaceType,
				IsBmc:         tt.legacyIsBmc, //nolint:staticcheck // Exercise compatibility with Core responses that predate interface_type.
			}
			apiInterface := APIDpuMachineInterface{}
			apiInterface.FromProto(protoInterface)
			assert.Equal(t, tt.want, apiInterface.IsBmc)
		})
	}
}

func TestNewAPIDpuMachines(t *testing.T) {
	ctx := APIDpuMachineProtoContext{
		HostMachineID:            "test-host-machine-id",
		SiteID:                   uuid.New(),
		InfrastructureProviderID: uuid.New(),
	}
	protoDpuMachines := []*corev1.DpuMachine{
		nil,
		{
			Machine: &corev1.Machine{
				Id:    &corev1.MachineId{Id: "test-dpu-machine-id-1"},
				State: "READY",
			},
		},
		{
			Machine: &corev1.Machine{
				Id:    &corev1.MachineId{Id: "test-dpu-machine-id-2"},
				State: "READY",
			},
		},
	}

	apiDpuMachines := NewAPIDpuMachines(protoDpuMachines, ctx)

	assert.Len(t, apiDpuMachines, 2)
	assert.Equal(t, "test-dpu-machine-id-1", apiDpuMachines[0].ID)
	assert.Equal(t, "test-dpu-machine-id-2", apiDpuMachines[1].ID)
	assert.Equal(t, ctx.HostMachineID, apiDpuMachines[0].HostMachineID)
	assert.Equal(t, ctx.SiteID.String(), apiDpuMachines[0].SiteID)
	assert.Equal(t, ctx.InfrastructureProviderID.String(), apiDpuMachines[0].InfrastructureProviderID)
	assert.NotNil(t, apiDpuMachines[0].DpuNetworkConfig)
	assert.NotNil(t, apiDpuMachines[1].DpuNetworkConfig)
}

func TestAPIDpuMachine_DpuNetworkConfigJSON(t *testing.T) {
	tests := []struct {
		name   string
		config *APIDpuNetworkConfig
		want   string
	}{
		{name: "unavailable configuration is null", want: `"dpuNetworkConfig":null`},
		{name: "available configuration is an object", config: &APIDpuNetworkConfig{Asn: 65001}, want: `"dpuNetworkConfig":{"asn":65001`},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			encoded, err := json.Marshal(APIDpuMachine{DpuNetworkConfig: tt.config})
			require.NoError(t, err)
			assert.Contains(t, string(encoded), tt.want)
		})
	}
}

func TestAPIDpuMachine_ZeroValueJSON(t *testing.T) {
	encoded, err := json.Marshal(APIDpuMachine{})
	require.NoError(t, err)

	var response map[string]interface{}
	require.NoError(t, json.Unmarshal(encoded, &response))
	assert.Equal(t, map[string]interface{}{
		"id":                       "",
		"infrastructureProviderId": "",
		"siteId":                   "",
		"hostMachineId":            "",
		"dpuAgentVersion":          "",
		"bmcInfo":                  nil,
		"dmiData":                  nil,
		"interfaces":               nil,
		"softwareComponents":       nil,
		"health":                   nil,
		"labels":                   map[string]interface{}{},
		"state":                    "",
		"dpuNetworkConfig":         nil,
		"lastRebooted":             nil,
		"placementInRack":          nil,
	}, response)
}
