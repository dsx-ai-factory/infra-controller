// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import (
	"context"
	"database/sql"
	"fmt"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	"os"

	"go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
)

// testTemporalSiteClientPool Building site client pool
// TODO commonize this
func testTemporalSiteClientPool(t *testing.T) *sc.ClientPool {
	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := config.NewConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)

	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)

	tSiteClientPool := sc.NewClientPool(tcfg)
	return tSiteClientPool
}

func TestResolvedVpcPrefixIDs(t *testing.T) {
	ipv4 := &corev1.VpcPrefixId{Value: uuid.NewString()}
	ipv6 := &corev1.VpcPrefixId{Value: uuid.NewString()}

	tests := []struct {
		name          string
		prefixes      *corev1.InstanceInterfaceResolvedVpcPrefixes
		wantPrimary   *corev1.VpcPrefixId
		wantSecondary *corev1.VpcPrefixId
	}{
		{name: "unresolved", prefixes: nil},
		{
			name: "IPv4-only uses IPv4",
			prefixes: &corev1.InstanceInterfaceResolvedVpcPrefixes{
				Ipv4VpcPrefixId: ipv4,
			},
			wantPrimary: ipv4,
		},
		{
			name: "IPv6-only uses IPv6",
			prefixes: &corev1.InstanceInterfaceResolvedVpcPrefixes{
				Ipv6VpcPrefixId: ipv6,
			},
			wantPrimary: ipv6,
		},
		{
			name: "dual-stack keeps IPv4 primary",
			prefixes: &corev1.InstanceInterfaceResolvedVpcPrefixes{
				Ipv4VpcPrefixId: ipv4,
				Ipv6VpcPrefixId: ipv6,
			},
			wantPrimary:   ipv4,
			wantSecondary: ipv6,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			primary, secondary := resolvedVpcPrefixIDs(tt.prefixes)
			assert.Equal(t, tt.wantPrimary, primary)
			assert.Equal(t, tt.wantSecondary, secondary)
		})
	}
}

func TestGetDevicelessInterfaceKey(t *testing.T) {
	prefixID := uuid.NewString()
	tests := []struct {
		name              string
		isPhysical        bool
		virtualFunctionID *int
		want              string
	}{
		{
			name:       "physical function",
			isPhysical: true,
			want:       prefixID + "-physical",
		},
		{
			name: "legacy virtual function without ID",
			want: prefixID + "-virtual",
		},
		{
			name:              "virtual function zero",
			virtualFunctionID: cutil.GetPtr(0),
			want:              prefixID + "-virtual-0",
		},
		{
			name:              "virtual function fifteen",
			virtualFunctionID: cutil.GetPtr(15),
			want:              prefixID + "-virtual-15",
		},
	}

	keys := make(map[string]bool, len(tests))
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := getDevicelessInterfaceKey(prefixID, test.isPhysical, test.virtualFunctionID)

			assert.Equal(t, test.want, got)
			assert.False(t, keys[got], "reconciliation keys must be unique for a shared VPC prefix")
			keys[got] = true
		})
	}
}

// TestManageInstance_UpdateInstancesInDBVpcSelectionInventory verifies that
// inventory caches and authoritatively clears Core-resolved prefixes while
// preserving the VPC selection intent used for prefix accounting.
func TestManageInstance_UpdateInstancesInDBVpcSelectionInventory(t *testing.T) {
	ctx := context.Background()
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-vpc-selection-provider"
	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-vpc-selection-ip", ipOrg, ipu)
	tenantOrg := "test-vpc-selection-tenant"
	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tenantOrg}, []string{"FORGE_TENANT_ADMIN"})
	tenant := util.TestBuildTenant(t, dbSession, tenantOrg, "VPC Selection Tenant", &cdbm.TenantConfig{}, tnu)
	site := util.TestBuildSite(t, dbSession, ip, "test-vpc-selection-site", cdbm.SiteStatusPending, nil, ipu)
	vpc := util.TestBuildVpc(t, dbSession, ip, site, tenant, "test-vpc-selection-vpc")
	controllerVpcID := uuid.New()
	vpc.ControllerVpcID = &controllerVpcID
	_, err := dbSession.DB.NewUpdate().Model(vpc).Column("controller_vpc_id").WherePK().Exec(ctx)
	require.NoError(t, err)
	ipBlock := util.TestBuildBuildIPBlock(
		t,
		dbSession,
		"test-vpc-selection-ip-block",
		site,
		ip,
		&tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly,
		"192.0.2.0",
		24,
		cdbm.IPBlockProtocolVersionV4,
		false,
		cdbm.IPBlockStatusReady,
		tnu,
	)
	require.NotNil(t, ipBlock)
	ipv6IPBlock := util.TestBuildBuildIPBlock(
		t,
		dbSession,
		"test-vpc-selection-ipv6-block",
		site,
		ip,
		&tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly,
		"2001:db8::",
		64,
		cdbm.IPBlockProtocolVersionV6,
		false,
		cdbm.IPBlockStatusReady,
		tnu,
	)
	require.NotNil(t, ipv6IPBlock)
	buildPrefix := func(name, prefix string) *cdbm.VpcPrefix {
		vpcPrefix := util.TestBuildVPCPrefix(
			t,
			dbSession,
			name,
			site,
			tenant,
			vpc.ID,
			&ipBlock.ID,
			&prefix,
			cutil.GetPtr(28),
			cdbm.VpcPrefixStatusReady,
			tnu,
		)
		require.NotNil(t, vpcPrefix)
		return vpcPrefix
	}
	deviceLessPrefix := buildPrefix("test-vpc-selection-prefix-1", "192.0.2.0/28")
	devicePrefix := buildPrefix("test-vpc-selection-prefix-2", "192.0.2.16/28")
	ipv6PrefixValue := "2001:db8::/120"
	ipv6Prefix := util.TestBuildVPCPrefix(
		t,
		dbSession,
		"test-vpc-selection-ipv6-prefix",
		site,
		tenant,
		vpc.ID,
		&ipv6IPBlock.ID,
		&ipv6PrefixValue,
		cutil.GetPtr(120),
		cdbm.VpcPrefixStatusReady,
		tnu,
	)
	require.NotNil(t, ipv6Prefix)

	instanceDAO := cdbm.NewInstanceDAO(dbSession)
	buildInstance := func(name string) *cdbm.Instance {
		instance, createErr := instanceDAO.Create(ctx, nil, cdbm.InstanceCreateInput{
			Name:                     name,
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			VpcID:                    vpc.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		})
		require.NoError(t, createErr)
		_, createErr = dbSession.DB.Exec(
			"UPDATE instance SET updated = ? WHERE id = ?",
			time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
			instance.ID,
		)
		require.NoError(t, createErr)
		return instance
	}

	deviceLessInstance := buildInstance("device-less-vpc-selection")
	deviceInstance := buildInstance("device-vpc-selection")
	shortStatusInstance := buildInstance("short-vpc-selection-status")
	nilConfigInstance := buildInstance("nil-config-vpc-selection")
	reportedPowerProfile := "reported-power-profile"
	existingPowerProfile := "existing-power-profile"
	_, err = dbSession.DB.Exec(
		"UPDATE instance SET power_profile = ?, updated = ? WHERE id = ?",
		existingPowerProfile,
		time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
		deviceInstance.ID,
	)
	require.NoError(t, err)
	_, err = dbSession.DB.Exec(
		"UPDATE instance SET power_profile = ?, is_missing_on_site = true, updated = ? WHERE id = ?",
		existingPowerProfile,
		time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
		nilConfigInstance.ID,
	)
	require.NoError(t, err)
	interfaceDAO := cdbm.NewInterfaceDAO(dbSession)
	createInterface := func(instanceID uuid.UUID, familyMode cdbm.InterfaceVpcIPFamilyMode, isPhysical bool, device *string, deviceInstance, virtualFunctionID *int) *cdbm.Interface {
		ifc, createErr := interfaceDAO.Create(ctx, nil, cdbm.InterfaceCreateInput{
			InstanceID:        instanceID,
			VpcID:             &vpc.ID,
			VpcIPFamilyMode:   &familyMode,
			IsPhysical:        isPhysical,
			Device:            device,
			DeviceInstance:    deviceInstance,
			VirtualFunctionID: virtualFunctionID,
			Status:            cdbm.InterfaceStatusPending,
			CreatedBy:         tnu.ID,
		})
		require.NoError(t, createErr)
		return ifc
	}

	// Keep device-less and device-selected modes on separate Instances, as API
	// validation requires.
	deviceLessIfc := createInterface(deviceLessInstance.ID, cdbm.InterfaceVpcIPFamilyModeDualStack, true, nil, nil, nil)
	device := "BlueField"
	deviceInstanceID := 1
	virtualFunctionID := 2
	deviceIfc := createInterface(deviceInstance.ID, cdbm.InterfaceVpcIPFamilyModeIPv4Only, false, &device, &deviceInstanceID, &virtualFunctionID)
	shortStatusIfc := createInterface(shortStatusInstance.ID, cdbm.InterfaceVpcIPFamilyModeIPv4Only, true, nil, nil, nil)

	deviceLessMac := "02:00:00:00:00:01"
	deviceMac := "02:00:00:00:00:02"
	selection := func(familyMode corev1.InstanceInterfaceIpFamilyMode) *corev1.InstanceInterfaceConfig_Vpc {
		return &corev1.InstanceInterfaceConfig_Vpc{Vpc: &corev1.InstanceInterfaceVpcSelection{
			VpcId:      &corev1.VpcId{Value: controllerVpcID.String()},
			FamilyMode: familyMode,
		}}
	}
	status := func(prefixID uuid.UUID, macAddress, address string, virtualFunctionID *uint32) *corev1.InstanceInterfaceStatus {
		return &corev1.InstanceInterfaceStatus{
			VirtualFunctionId: virtualFunctionID,
			MacAddress:        &macAddress,
			Addresses:         []string{address},
			VpcId:             &corev1.VpcId{Value: controllerVpcID.String()},
			ResolvedVpcPrefixes: &corev1.InstanceInterfaceResolvedVpcPrefixes{
				Ipv4VpcPrefixId: &corev1.VpcPrefixId{Value: prefixID.String()},
			},
		}
	}

	// Core may report observed device data for an Interface whose request did not
	// select a device. Repeated inventory must continue matching it by VPC intent.
	observedDevice := "observed-device"
	deviceLessStatus := status(deviceLessPrefix.ID, deviceLessMac, "192.0.2.10", nil)
	deviceLessStatus.Addresses = append(deviceLessStatus.Addresses, "2001:db8::10")
	deviceLessStatus.ResolvedVpcPrefixes.Ipv6VpcPrefixId = &corev1.VpcPrefixId{Value: ipv6Prefix.ID.String()}
	deviceLessStatus.Device = &observedDevice
	deviceStatus := status(devicePrefix.ID, deviceMac, "192.0.2.20", cutil.GetPtr(uint32(virtualFunctionID)))
	deviceStatus.Device = &device
	deviceStatus.DeviceInstance = uint32(deviceInstanceID)

	inventory := &corev1.InstanceInventory{Instances: []*corev1.Instance{
		{
			Id: &corev1.InstanceId{Value: deviceLessInstance.ControllerInstanceID.String()},
			Config: &corev1.InstanceConfig{
				PowerProfile: &reportedPowerProfile,
				Network: &corev1.InstanceNetworkConfig{Interfaces: []*corev1.InstanceInterfaceConfig{
					{
						FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
						NetworkDetails: selection(
							corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_DUAL_STACK,
						),
					},
				}},
			},
			Status: &corev1.InstanceStatus{
				Tenant: &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
				Network: &corev1.InstanceNetworkStatus{
					ConfigsSynced: corev1.SyncState_SYNCED,
					Interfaces:    []*corev1.InstanceInterfaceStatus{deviceLessStatus},
				},
			},
		},
		{
			Id: &corev1.InstanceId{Value: deviceInstance.ControllerInstanceID.String()},
			Config: &corev1.InstanceConfig{Network: &corev1.InstanceNetworkConfig{Interfaces: []*corev1.InstanceInterfaceConfig{
				{
					FunctionType: corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
					NetworkDetails: selection(
						corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_IPV4_ONLY,
					),
					Device:            &device,
					DeviceInstance:    uint32(deviceInstanceID),
					VirtualFunctionId: cutil.GetPtr(uint32(virtualFunctionID)),
				},
			}}},
			Status: &corev1.InstanceStatus{
				Tenant: &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
				Network: &corev1.InstanceNetworkStatus{
					ConfigsSynced: corev1.SyncState_SYNCED,
					Interfaces:    []*corev1.InstanceInterfaceStatus{deviceStatus},
				},
			},
		},
		{
			Id: &corev1.InstanceId{Value: shortStatusInstance.ControllerInstanceID.String()},
			Config: &corev1.InstanceConfig{Network: &corev1.InstanceNetworkConfig{Interfaces: []*corev1.InstanceInterfaceConfig{
				{
					FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
					NetworkDetails: selection(
						corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_IPV4_ONLY,
					),
				},
			}}},
			Status: &corev1.InstanceStatus{
				Tenant: &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
				Network: &corev1.InstanceNetworkStatus{
					ConfigsSynced: corev1.SyncState_SYNCED,
				},
			},
		},
		{
			Id:     &corev1.InstanceId{Value: nilConfigInstance.ControllerInstanceID.String()},
			Config: nil,
			Status: &corev1.InstanceStatus{Tenant: &corev1.InstanceTenantStatus{State: corev1.TenantState_READY}},
		},
	}}

	tSiteClientPool := testTemporalSiteClientPool(t)
	tSiteClientPool.IDClientMap[site.ID.String()] = &tmocks.Client{}
	manager := NewManageInstance(dbSession, tSiteClientPool, &tmocks.Client{}, config.GetTestConfig())
	_, err = manager.UpdateInstancesInDB(ctx, site.ID, inventory)
	require.NoError(t, err)

	// Verify both resolved data and the original VPC selection intent persisted.
	assertResolvedInterface := func(
		ifc *cdbm.Interface,
		expectedPrefixID uuid.UUID,
		expectedSecondaryPrefixID *uuid.UUID,
		expectedFamilyMode cdbm.InterfaceVpcIPFamilyMode,
		expectedMac string,
		expectedAddresses []string,
	) *cdbm.Interface {
		updated, getErr := interfaceDAO.GetByID(ctx, nil, ifc.ID, nil)
		require.NoError(t, getErr)
		require.NotNil(t, updated.VpcPrefixID)
		require.NotNil(t, updated.VpcID)
		require.NotNil(t, updated.VpcIPFamilyMode)
		require.NotNil(t, updated.MacAddress)
		assert.Equal(t, expectedPrefixID, *updated.VpcPrefixID)
		assert.Equal(t, vpc.ID, *updated.VpcID)
		assert.Equal(t, expectedSecondaryPrefixID, updated.SecondaryVpcPrefixID)
		assert.Equal(t, expectedFamilyMode, *updated.VpcIPFamilyMode)
		assert.Equal(t, expectedMac, *updated.MacAddress)
		assert.Equal(t, expectedAddresses, updated.IPAddresses)
		assert.Equal(t, cdbm.InterfaceStatusReady, updated.Status)
		return updated
	}
	updatedDeviceLessIfc := assertResolvedInterface(
		deviceLessIfc,
		deviceLessPrefix.ID,
		&ipv6Prefix.ID,
		cdbm.InterfaceVpcIPFamilyModeDualStack,
		deviceLessMac,
		[]string{"192.0.2.10", "2001:db8::10"},
	)
	require.NotNil(t, updatedDeviceLessIfc.Device)
	assert.Equal(t, observedDevice, *updatedDeviceLessIfc.Device)
	updatedDeviceIfc := assertResolvedInterface(
		deviceIfc,
		devicePrefix.ID,
		nil,
		cdbm.InterfaceVpcIPFamilyModeIPv4Only,
		deviceMac,
		[]string{"192.0.2.20"},
	)
	require.NotNil(t, updatedDeviceIfc.Device)
	assert.Equal(t, device, *updatedDeviceIfc.Device)

	reportedProfilePersisted, err := instanceDAO.GetByID(ctx, nil, deviceLessInstance.ID, nil)
	require.NoError(t, err)
	require.NotNil(t, reportedProfilePersisted.PowerProfile)
	assert.Equal(t, reportedPowerProfile, *reportedProfilePersisted.PowerProfile)

	existingProfileCleared, err := instanceDAO.GetByID(ctx, nil, deviceInstance.ID, nil)
	require.NoError(t, err)
	assert.Nil(t, existingProfileCleared.PowerProfile)

	// A config without an aligned status entry must leave its Interface untouched.
	shortStatusUnchanged, err := interfaceDAO.GetByID(ctx, nil, shortStatusIfc.ID, nil)
	require.NoError(t, err)
	assert.Nil(t, shortStatusUnchanged.VpcPrefixID)
	assert.Nil(t, shortStatusUnchanged.SecondaryVpcPrefixID)
	assert.Equal(t, vpc.ID, *shortStatusUnchanged.VpcID)
	assert.Equal(t, cdbm.InterfaceVpcIPFamilyModeIPv4Only, *shortStatusUnchanged.VpcIPFamilyMode)
	assert.Nil(t, shortStatusUnchanged.MacAddress)
	assert.Equal(t, cdbm.InterfaceStatusPending, shortStatusUnchanged.Status)

	// An incomplete inventory entry must not panic or clear persisted state.
	nilConfigUnchanged, err := instanceDAO.GetByID(ctx, nil, nilConfigInstance.ID, nil)
	require.NoError(t, err)
	require.NotNil(t, nilConfigUnchanged.PowerProfile)
	assert.Equal(t, existingPowerProfile, *nilConfigUnchanged.PowerProfile)
	assert.True(t, nilConfigUnchanged.IsMissingOnSite)

	// Pending inventory is not authoritative enough to clear either resolution.
	inventory.Instances[0].Status.Network.Interfaces[0].ResolvedVpcPrefixes = nil
	inventory.Instances[0].Status.Network.ConfigsSynced = corev1.SyncState_PENDING
	_, err = dbSession.DB.Exec(
		"UPDATE instance SET updated = ? WHERE id = ?",
		time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
		deviceLessInstance.ID,
	)
	require.NoError(t, err)
	_, err = manager.UpdateInstancesInDB(ctx, site.ID, inventory)
	require.NoError(t, err)

	pendingEmptyResolution, err := interfaceDAO.GetByID(ctx, nil, deviceLessIfc.ID, nil)
	require.NoError(t, err)
	require.NotNil(t, pendingEmptyResolution.VpcPrefixID)
	assert.Equal(t, deviceLessPrefix.ID, *pendingEmptyResolution.VpcPrefixID)
	require.NotNil(t, pendingEmptyResolution.SecondaryVpcPrefixID)
	assert.Equal(t, ipv6Prefix.ID, *pendingEmptyResolution.SecondaryVpcPrefixID)

	// Pending partial inventory also preserves an omitted secondary resolution.
	inventory.Instances[0].Status.Network.Interfaces[0].ResolvedVpcPrefixes = &corev1.InstanceInterfaceResolvedVpcPrefixes{
		Ipv4VpcPrefixId: &corev1.VpcPrefixId{Value: deviceLessPrefix.ID.String()},
	}
	_, err = dbSession.DB.Exec(
		"UPDATE instance SET updated = ? WHERE id = ?",
		time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
		deviceLessInstance.ID,
	)
	require.NoError(t, err)
	_, err = manager.UpdateInstancesInDB(ctx, site.ID, inventory)
	require.NoError(t, err)

	pendingResolution, err := interfaceDAO.GetByID(ctx, nil, deviceLessIfc.ID, nil)
	require.NoError(t, err)
	require.NotNil(t, pendingResolution.VpcPrefixID)
	assert.Equal(t, deviceLessPrefix.ID, *pendingResolution.VpcPrefixID)
	require.NotNil(t, pendingResolution.SecondaryVpcPrefixID)
	assert.Equal(t, ipv6Prefix.ID, *pendingResolution.SecondaryVpcPrefixID)
	assert.Equal(t, cdbm.InterfaceStatusReady, pendingResolution.Status)
	require.NotNil(t, pendingResolution.VpcID)
	assert.Equal(t, vpc.ID, *pendingResolution.VpcID)
	require.NotNil(t, pendingResolution.VpcIPFamilyMode)
	assert.Equal(t, cdbm.InterfaceVpcIPFamilyModeDualStack, *pendingResolution.VpcIPFamilyMode)

	// Synchronized partial inventory authoritatively clears only the stale
	// secondary resolution while preserving the reported primary.
	inventory.Instances[0].Status.Network.ConfigsSynced = corev1.SyncState_SYNCED
	_, err = dbSession.DB.Exec(
		"UPDATE instance SET updated = ? WHERE id = ?",
		time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
		deviceLessInstance.ID,
	)
	require.NoError(t, err)
	_, err = manager.UpdateInstancesInDB(ctx, site.ID, inventory)
	require.NoError(t, err)

	primaryOnlyResolution, err := interfaceDAO.GetByID(ctx, nil, deviceLessIfc.ID, nil)
	require.NoError(t, err)
	require.NotNil(t, primaryOnlyResolution.VpcPrefixID)
	assert.Equal(t, deviceLessPrefix.ID, *primaryOnlyResolution.VpcPrefixID)
	assert.Nil(t, primaryOnlyResolution.SecondaryVpcPrefixID)

	// Synchronized inventory with no resolution clears the remaining primary.
	inventory.Instances[0].Status.Network.Interfaces[0].ResolvedVpcPrefixes = nil
	_, err = dbSession.DB.Exec(
		"UPDATE instance SET updated = ? WHERE id = ?",
		time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
		deviceLessInstance.ID,
	)
	require.NoError(t, err)
	_, err = manager.UpdateInstancesInDB(ctx, site.ID, inventory)
	require.NoError(t, err)

	clearedResolution, err := interfaceDAO.GetByID(ctx, nil, deviceLessIfc.ID, nil)
	require.NoError(t, err)
	assert.Nil(t, clearedResolution.VpcPrefixID)
	assert.Nil(t, clearedResolution.SecondaryVpcPrefixID)
	require.NotNil(t, clearedResolution.VpcID)
	assert.Equal(t, vpc.ID, *clearedResolution.VpcID)
	require.NotNil(t, clearedResolution.VpcIPFamilyMode)
	assert.Equal(t, cdbm.InterfaceVpcIPFamilyModeDualStack, *clearedResolution.VpcIPFamilyMode)
}

func TestManageInstance_deleteInstanceFromDB(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	tnOrg := "test-tenant-org-1"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg}, tnRoles)
	tenant := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)

	site := util.TestBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusPending, nil, ipu)
	vpc := util.TestBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	machine := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr("mcTypeTest"), cutil.GetPtr(true), cdbm.MachineStatusReady)
	allocation := util.TestBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	instanceType := util.TestBuildInstanceType(t, dbSession, ip, site, "testInstanceType")
	_ = util.TestBuildAllocationContraints(t, dbSession, allocation, cdbm.AllocationResourceTypeInstanceType, instanceType.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	operatingSystem := util.TestBuildOperatingSystem(t, dbSession, "testOS")

	isd := cdbm.NewInstanceDAO(dbSession)

	instance, err := isd.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusTerminating,
			PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusBootCompleted),
			CreatedBy:                tnu.ID,
		},
	)
	require.NoError(t, err)

	ibp := util.TestBuildInfiniBandPartition(t, dbSession, "ibpart", site, tenant, nil, cdbm.InfiniBandPartitionStatusReady, false)
	nvlp := util.TestBuildNVLinkLogicalPartition(t, dbSession, "nvlp", cutil.GetPtr("nvlp"), site, tenant, cdbm.NVLinkLogicalPartitionStatusReady, false)

	ibiDAO := cdbm.NewInfiniBandInterfaceDAO(dbSession)
	_, err = ibiDAO.Create(ctx, nil, cdbm.InfiniBandInterfaceCreateInput{
		InstanceID:            instance.ID,
		SiteID:                site.ID,
		InfiniBandPartitionID: ibp.ID,
		Device:                "ib0",
		DeviceInstance:        0,
		IsPhysical:            true,
		Status:                cdbm.InfiniBandInterfaceStatusReady,
		CreatedBy:             tnu.ID,
	})
	require.NoError(t, err)

	nvliDAO := cdbm.NewNVLinkInterfaceDAO(dbSession)
	_, err = nvliDAO.Create(ctx, nil, cdbm.NVLinkInterfaceCreateInput{
		InstanceID:               instance.ID,
		SiteID:                   site.ID,
		NVLinkLogicalPartitionID: nvlp.ID,
		DeviceInstance:           0,
		Status:                   cdbm.NVLinkInterfaceStatusReady,
		CreatedBy:                tnu.ID,
	})
	require.NoError(t, err)

	// Add SSH Key Group Instance Association
	skg := util.TestBuildSSHKeyGroup(t, dbSession, "test-ssh-key-group-1", tnOrg, nil, tenant.ID, cutil.GetPtr("fbc692b61ffef6fbfc38a3833f6b7e7ae508da75"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	_ = util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg.ID, site.ID, cutil.GetPtr("V1-1234567890"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	_ = util.TestBuildSSHKeyGroupInstanceAssociation(t, dbSession, skg.ID, site.ID, instance.ID, tnu.ID)

	// Add DPU Extension Service Deployment
	des := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-1", site, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, cutil.GetPtr("V1-1234567890"),
		&cdbm.DpuExtensionServiceVersionInfo{
			Version:        "V1-1234567890",
			Data:           "test-data",
			HasCredentials: true,
			Created:        time.Now(),
		}, []string{"V1-1234567890"}, cdbm.DpuExtensionServiceStatusReady, ipu)
	_ = util.TestBuildDpuExtensionServiceDeployment(t, dbSession, des.ID, site.ID, tenant.ID, instance.ID, "V1-1234567890", cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu)

	tx, err := cdb.BeginTx(ctx, dbSession, &sql.TxOptions{})
	require.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)
	wtc := &tmocks.Client{}
	cfg := config.GetTestConfig()
	ms := NewManageInstance(dbSession, tSiteClientPool, wtc, cfg)

	err = ms.deleteInstanceFromDB(ctx, tx, instance, zerolog.Nop())
	require.NoError(t, err)
	require.NoError(t, tx.Commit())

	ibis, _, err := ibiDAO.GetAll(ctx, nil, cdbm.InfiniBandInterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	require.NoError(t, err)
	require.Empty(t, ibis)

	nvlis, _, err := nvliDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	require.NoError(t, err)
	require.Empty(t, nvlis)

	skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(dbSession)
	skgias, _, err := skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
		InstanceIDs: []uuid.UUID{instance.ID},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	require.NoError(t, err)
	require.Empty(t, skgias)

	desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(dbSession)
	desds, _, err := desdDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceDeploymentFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	require.NoError(t, err)
	require.Empty(t, desds)
}

func TestManageInstance_UpdateInstancesInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	tnOrg := "test-tenant-org-1"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg}, tnRoles)
	tenant := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)

	site := util.TestBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusPending, nil, ipu)
	vpc := util.TestBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	subnet1 := util.TestBuildSubnet(t, dbSession, tenant, vpc, "testSubnet1", cdbm.SubnetStatusPending, cutil.GetPtr(uuid.New()))
	subnet2 := util.TestBuildSubnet(t, dbSession, tenant, vpc, "testSubnet2", cdbm.SubnetStatusPending, cutil.GetPtr(uuid.New()))
	subnet3 := util.TestBuildSubnet(t, dbSession, tenant, vpc, "testSubnet2", cdbm.SubnetStatusPending, nil)

	ipb1 := util.TestBuildBuildIPBlock(t, dbSession, "testipb", site, ip, &tenant.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu)
	assert.NotNil(t, ipb1)
	vpcPrefix1 := util.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-1", site, tenant, vpc.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)
	vpcPrefix2 := util.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-2", site, tenant, vpc.ID, &ipb1.ID, cutil.GetPtr("192.172.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	partition1 := util.TestBuildInfiniBandPartition(t, dbSession, "test-partition-1", site, tenant, cutil.GetPtr(uuid.New()), cdbm.InfiniBandPartitionStatusProvisioning, false)

	nvllPartition1 := util.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvlinklpartition-1", nil, site, tenant, cdbm.NVLinkLogicalPartitionStatusReady, false)
	assert.NotNil(t, nvllPartition1)

	machine1 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine2 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine3 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine4 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine5 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine6 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine7 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine8 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine9 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine10 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine11 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine13 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine15 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine19 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)

	allocation := util.TestBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	instanceType := util.TestBuildInstanceType(t, dbSession, ip, site, "testInstanceType")
	_ = util.TestBuildAllocationContraints(t, dbSession, allocation, cdbm.AllocationResourceTypeInstanceType, instanceType.ID, cdbm.AllocationConstraintTypeReserved, 7, ipu)
	operatingSystem := util.TestBuildOperatingSystem(t, dbSession, "testOS")

	instanceDAO := cdbm.NewInstanceDAO(dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)

	// Instance 1 receives updates from Site Controller, namely status update and Instance Subnet status, attribute updates
	instance1, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine1.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance1.ID.String())
	assert.NoError(t, err)

	interface1 := util.TestBuildInterface(t, dbSession, &instance1.ID, &subnet1.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, interface1)
	interface2 := util.TestBuildInterface(t, dbSession, &instance1.ID, &subnet2.ID, nil, false, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, interface2)
	interface3 := util.TestBuildInterface(t, dbSession, &instance1.ID, &subnet3.ID, nil, false, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, interface3)

	ibInterface1 := util.TestBuildInfiniBandInterface(t, dbSession, instance1.ID, site.ID, partition1.ID, "MT2910 Family [ConnectX-7]", 0, true, nil, cdbm.InfiniBandInterfaceStatusPending, false)
	assert.NotNil(t, ibInterface1)

	ibInterface2 := util.TestBuildInfiniBandInterface(t, dbSession, instance1.ID, site.ID, partition1.ID, "MT2910 Family [ConnectX-7]", 1, true, nil, cdbm.InfiniBandInterfaceStatusPending, false)
	assert.NotNil(t, ibInterface2)

	ibInterface3 := util.TestBuildInfiniBandInterface(t, dbSession, instance1.ID, site.ID, partition1.ID, "MT2910 Family [ConnectX-7]", 2, true, nil, cdbm.InfiniBandInterfaceStatusDeleting, false)
	assert.NotNil(t, ibInterface3)

	// Make Deleting InfiniBand row old enough to pass IsTimeWithinStaleInventoryThreshold deferral during inventory reconcile
	_, err = dbSession.DB.Exec("UPDATE infiniband_interface SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), ibInterface3.ID.String())
	assert.NoError(t, err)

	// NVLink Interfaces
	nvlinkInterface1 := util.TestBuildNVLinkInterface(t, dbSession, instance1.ID, site.ID, nvllPartition1.ID, cutil.GetPtr(""), 0, nil, nil, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, nvlinkInterface1)

	nvlinkInterface2 := util.TestBuildNVLinkInterface(t, dbSession, instance1.ID, site.ID, nvllPartition1.ID, cutil.GetPtr(""), 1, nil, nil, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, nvlinkInterface2)

	nvlinkInterface3 := util.TestBuildNVLinkInterface(t, dbSession, instance1.ID, site.ID, nvllPartition1.ID, cutil.GetPtr(""), 2, cutil.GetPtr("e1f2a30200d71e9f"), nil, cdbm.NVLinkInterfaceStatusDeleting)
	assert.NotNil(t, nvlinkInterface3)

	// Set updated earlier than the inventory receipt interval for nvlinkInterface3 so it can be deleted
	_, err = dbSession.DB.Exec("UPDATE nvlink_interface SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlinkInterface3.ID.String())
	assert.NoError(t, err)

	nvlinkInterface4 := util.TestBuildNVLinkInterface(t, dbSession, instance1.ID, site.ID, nvllPartition1.ID, cutil.GetPtr(""), 3, nil, nil, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, nvlinkInterface4)

	// Instance 2 is in Terminating state and gets deleted when missing from Site Controller inventory
	instance2, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-2",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine2.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusTerminating,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance2.ID.String())
	assert.NoError(t, err)

	instance2Subnet := util.TestBuildInterface(t, dbSession, &instance2.ID, &subnet1.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, instance2Subnet)

	skg1 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test1"), tenant.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSyncing, tnu.ID)
	assert.NotNil(t, skg1)
	skgsa1 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, site.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa1)
	skgia1 := util.TestBuildSSHKeyGroupInstanceAssociation(t, dbSession, skg1.ID, site.ID, instance2.ID, tnu.ID)
	assert.NotNil(t, skgia1)

	// Instance 3 is missing from Site Controller inventory, has controller ID, hence missing flag gets set and error raised
	instance3, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "Test Instance 3",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine3.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance3.ID.String())
	assert.NoError(t, err)

	// Set updated earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance3.ID.String())
	assert.NoError(t, err)

	instance3Subnet := util.TestBuildInterface(t, dbSession, &instance3.ID, &subnet1.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, instance3Subnet)

	// Instance 4 is missing from Site Controller inventory and does not have controller ID, hence missing flag does not get set
	instance4, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "Test Instance 4",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine4.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set updated earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance4.ID.String())
	assert.NoError(t, err)

	instance4Subnet := util.TestBuildInterface(t, dbSession, &instance4.ID, &subnet1.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusError)
	assert.NotNil(t, instance4Subnet)

	// Instance 5 is in Terminating state and does not get set back to Ready state from stale inventory
	instance5, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "Test Instance 5",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine5.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusTerminating,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance5.ID.String())
	assert.NoError(t, err)

	// Instance 6 is in Error state and gets restored to Ready state from inventory
	instance6, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "Test Instance 6",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine6.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusError,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	_, err = instanceDAO.Update(ctx, nil, cdbm.InstanceUpdateInput{InstanceID: instance6.ID, InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{IsMissingOnSite: cutil.GetPtr(true)}})
	assert.Nil(t, err)

	// Set updated earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance6.ID.String())
	assert.NoError(t, err)

	// Instance 7 does not have controller Instance ID set and is present in inventory, and gets controller Instance ID set
	instance7, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "Test Instance 7",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine7.ID,
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set updated earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance7.ID.String())
	assert.NoError(t, err)

	// Instance 8 is in Terminating state and has no controller ID, gets deleted on inventory update
	instance8, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "Test Instance 8",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine8.ID,
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusTerminating,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set updated earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance8.ID.String())
	assert.NoError(t, err)

	// Instance 9 is in Ready state and power status is Rebooting, gets set to BootCompleted
	instance9, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "Test Instance 9",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine9.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			PhoneHomeEnabled:         true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusReady,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set updated earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance9.ID.String())
	assert.NoError(t, err)

	var vfID uint32 = 1
	macAddress := "2F-FC-34-AE-9C-2A"
	ipAddresses := []string{"200.32.11.190", "51aa:f78b:ffb0:1c58:7bee:b9e7:bf35:0962"}
	requestedIpAddress := "10.0.0.15"
	routingProfilePrefixes := []string{"192.0.2.0/24", "2001:db8::/64"}

	// Instance 10 is already set to Error/marked as missing, no new status details is created for it when it's reported missing again
	instance10, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-10",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine10.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusError,
			CreatedBy:                tnu.ID,
		},
	)
	assert.NoError(t, err)
	// Update creation timestamp to be earlier than inventory processing interval
	_, err = dbSession.DB.Exec("UPDATE instance SET is_missing_on_site = true, created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance10.ID.String())
	assert.NoError(t, err)

	// Set updated earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance10.ID.String())
	assert.NoError(t, err)

	// Create status detail for instance 10
	_, err = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: instance10.ID.String(), Status: instance10.Status, Message: cutil.GetPtr("Instance is missing on Site")})
	assert.NoError(t, err)

	// Instance 11 receives updates from Site Controller, namely status update and Instance VPC Prefix status, attribute updates
	instance11, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-11",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine11.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			NetworkSecurityGroupPropagationDetails: &cdbm.NetworkSecurityGroupPropagationDetails{
				NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{},
			},
			CreatedBy: tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance11.ID.String())
	assert.NoError(t, err)

	// Replicate the bug fix test
	ifcvpc0 := util.TestBuildInterface(t, dbSession, &instance11.ID, nil, &vpcPrefix1.ID, true, cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"), nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, ifcvpc0)
	_, err = cdbm.NewInterfaceDAO(dbSession).Update(ctx, nil, cdbm.InterfaceUpdateInput{
		InterfaceID: ifcvpc0.ID,
		InlineRoutingProfile: &cdbm.InterfaceInlineRoutingProfile{
			AllowedAnycastPrefixes: []string{"198.51.100.0/24"},
		},
	})
	assert.Nil(t, err)

	ifcvpc0_1 := util.TestBuildInterface(t, dbSession, &instance11.ID, nil, &vpcPrefix1.ID, false, cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"), nil, cutil.GetPtr(1), &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, ifcvpc0_1)

	ifcvpc1 := util.TestBuildInterface(t, dbSession, &instance11.ID, nil, &vpcPrefix1.ID, true, cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"), cutil.GetPtr(1), nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, ifcvpc1)

	ifcvpc1_1 := util.TestBuildInterface(t, dbSession, &instance11.ID, nil, &vpcPrefix1.ID, false, cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"), cutil.GetPtr(1), cutil.GetPtr(1), &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, ifcvpc1_1)

	// Instance 12 is defined below

	// Instance 13 receives updates from Site Controller but same status as current
	instance13, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-11",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine13.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	util.TestBuildStatusDetail(t, dbSession, instance13.ID.String(), cdbm.InstanceStatusProvisioning, cutil.GetPtr("Instance is being provisioned on Site"))

	// Instance 14 receives updates from Site Controller, but cloud DB saw an update that was too recent.
	instance14, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-1",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine1.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	util.TestBuildStatusDetail(t, dbSession, instance14.ID.String(), cdbm.InstanceStatusProvisioning, cutil.GetPtr("Instance is being provisioned on Site"))

	// Instance 15 is in Configuring state and gets set to Ready
	// Make sure the interfaces are deleted and pending one is converted to ready

	// Instance 15 receives updates from Site Controller, namely status update and Instance VPC Prefix status, attribute updates
	instance15, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-15",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine15.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusConfiguring,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance15.ID.String())
	assert.NoError(t, err)

	ifcvpc_deleting := util.TestBuildInterface(t, dbSession, &instance15.ID, nil, &vpcPrefix1.ID, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusDeleting)
	assert.NotNil(t, ifcvpc_deleting)

	ifcvpc_pending := util.TestBuildInterface(t, dbSession, &instance15.ID, nil, &vpcPrefix2.ID, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, ifcvpc_pending)
	_, err = cdbm.NewInterfaceDAO(dbSession).Update(ctx, nil, cdbm.InterfaceUpdateInput{
		InterfaceID:        ifcvpc_pending.ID,
		RequestedIpAddress: &requestedIpAddress,
		InlineRoutingProfile: &cdbm.InterfaceInlineRoutingProfile{
			AllowedAnycastPrefixes: []string{"198.51.100.0/24"},
		},
	})
	assert.Nil(t, err)

	// Instance 16 starts with an initial TPM EK certificate and gets updated with a new certificate value
	machine16 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	instance16, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-16",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine16.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			// Start with an initial TPM EK certificate
			TpmEkCertificate: cutil.GetPtr("initial-cert-value"),
			CreatedBy:        tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance16.ID.String())
	assert.NoError(t, err)

	// Instance 17 starts with nil TPM EK certificate and gets updated with a certificate value
	machine17 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	instance17, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-17",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine17.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			// Start with nil TPM EK certificate
			TpmEkCertificate: nil,
			CreatedBy:        tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance17.ID.String())
	assert.NoError(t, err)

	// Sample base64 encoded TPM EK certificate (truncated for brevity but realistic format)
	tpmEKCertBase64 := "MIICXjCCAUYCFGvJgf1k9b2LqK2F3VzjRkGbXfGGMA0GCSqGSIb3DQEBCwUAMCMxITAfBgNVBAMMGFRQTSBFSyBDZXJ0aWZpY2F0ZSBUZXN0MB4XDTIzMTAwMTA4MDAzNVoXDTMzMDkyODA4MDAzNVowIzEhMB8GA1UEAwwYVFBNIEVLIENlcnRpZmljYXRlIFRlc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQC8uKdyQ1P+KqHGfyJLIvJhDzQ3rHnS"

	// Instance 18 receives updates from Site Controller, namely status update and Instance Subnet status, attribute updates
	machine18 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	instance18, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-18",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine18.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance18.ID.String())
	assert.NoError(t, err)

	interface18_1 := util.TestBuildInterface(t, dbSession, &instance18.ID, &subnet1.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, interface18_1)
	interface18_2 := util.TestBuildInterface(t, dbSession, &instance18.ID, &subnet2.ID, nil, false, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, interface18_2)
	interface18_3 := util.TestBuildInterface(t, dbSession, &instance18.ID, &subnet3.ID, nil, false, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)
	assert.NotNil(t, interface18_3)

	ibInterface18_1 := util.TestBuildInfiniBandInterface(t, dbSession, instance18.ID, site.ID, partition1.ID, "MT2910 Family [ConnectX-7]", 0, true, nil, cdbm.InfiniBandInterfaceStatusDeleting, false)
	assert.NotNil(t, ibInterface18_1)

	ibInterface18_2 := util.TestBuildInfiniBandInterface(t, dbSession, instance18.ID, site.ID, partition1.ID, "MT2910 Family [ConnectX-7]", 1, true, nil, cdbm.InfiniBandInterfaceStatusDeleting, false)
	assert.NotNil(t, ibInterface18_2)

	ibInterface18_3 := util.TestBuildInfiniBandInterface(t, dbSession, instance18.ID, site.ID, partition1.ID, "MT2910 Family [ConnectX-7]", 2, true, nil, cdbm.InfiniBandInterfaceStatusDeleting, false)
	assert.NotNil(t, ibInterface18_3)

	for _, ibd := range []*cdbm.InfiniBandInterface{ibInterface18_1, ibInterface18_2, ibInterface18_3} {
		_, err = dbSession.DB.Exec("UPDATE infiniband_interface SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), ibd.ID.String())
		assert.NoError(t, err)
	}

	// Instance 19 verifies that a device-less PF and VF sharing one VPC Prefix
	// reconcile independently by function identity and VF ID.
	instance19, err := instanceDAO.Create(
		ctx,
		nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-19",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine19.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusProvisioning,
			CreatedBy:                tnu.ID,
		},
	)
	assert.NoError(t, err)
	_, err = dbSession.DB.Exec(
		"UPDATE instance SET updated = ? WHERE id = ?",
		time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2),
		instance19.ID.String(),
	)
	assert.NoError(t, err)

	deviceLessFNNPhysicalInterface := util.TestBuildInterface(
		t,
		dbSession,
		&instance19.ID,
		nil,
		&vpcPrefix1.ID,
		true,
		nil,
		nil,
		nil,
		&tnu.ID,
		cdbm.InterfaceStatusPending,
	)
	assert.NotNil(t, deviceLessFNNPhysicalInterface)
	deviceLessFNNVirtualFunctionID := 5
	deviceLessFNNVirtualInterface := util.TestBuildInterface(
		t,
		dbSession,
		&instance19.ID,
		nil,
		&vpcPrefix1.ID,
		false,
		nil,
		nil,
		&deviceLessFNNVirtualFunctionID,
		&tnu.ID,
		cdbm.InterfaceStatusPending,
	)
	assert.NotNil(t, deviceLessFNNVirtualInterface)
	deviceLessFNNPhysicalMacAddress := "2F-FC-34-AE-9C-31"
	deviceLessFNNVirtualMacAddress := "2F-FC-34-AE-9C-32"

	// Build DPU Extension Services and Deployments for testing
	dpuExtensionService1 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-ext-service-1", site, tenant, "ovs-offload", cutil.GetPtr("1"), nil, []string{}, cdbm.DpuExtensionServiceStatusReady, ipu)
	assert.NotNil(t, dpuExtensionService1)

	dpuExtensionService2 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-ext-service-2", site, tenant, "ovs-offload", cutil.GetPtr("1"), nil, []string{}, cdbm.DpuExtensionServiceStatusReady, ipu)
	assert.NotNil(t, dpuExtensionService2)

	// Create DPU Extension Service Deployment for instance1 - will be updated to Running
	dpuExtServiceDeployment1 := util.TestBuildDpuExtensionServiceDeployment(t, dbSession, dpuExtensionService1.ID, site.ID, tenant.ID, instance1.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusPending, ipu)
	assert.NotNil(t, dpuExtServiceDeployment1)

	// Create DPU Extension Service Deployment for instance1 - will be deleted (missing from inventory)
	dpuExtServiceDeployment2 := util.TestBuildDpuExtensionServiceDeployment(t, dbSession, dpuExtensionService2.ID, site.ID, tenant.ID, instance1.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusRunning, ipu)
	assert.NotNil(t, dpuExtServiceDeployment2)

	// Set updated earlier than the inventory receipt interval for dpuExtServiceDeployment2 so it can be deleted
	_, err = dbSession.DB.Exec("UPDATE dpu_extension_service_deployment SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), dpuExtServiceDeployment2.ID.String())
	assert.NoError(t, err)

	instanceInventory := &corev1.InstanceInventory{
		NetworkSecurityGroupPropagations: []*corev1.NetworkSecurityGroupPropagationObjectStatus{
			&corev1.NetworkSecurityGroupPropagationObjectStatus{
				Id:      instance1.ID.String(),
				Status:  corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL,
				Details: cutil.GetPtr("nothing to see here"),
			},
		},
		Instances: []*corev1.Instance{
			{
				Id: &corev1.InstanceId{Value: instance1.ID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{
								FunctionType:     corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet1.ControllerNetworkSegmentID.String()},
							},
							{
								FunctionType:     corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
								NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet2.ControllerNetworkSegmentID.String()},
							},
						},
					},
					// InfiniBand config/status keys must align with reconcile map: controller IB partition id + device + device instance
					Infiniband: &corev1.InstanceInfinibandConfig{
						IbInterfaces: []*corev1.InstanceIBInterfaceConfig{
							{
								Device:         "MT2910 Family [ConnectX-7]",
								Vendor:         cutil.GetPtr("Mellanox Technologies"),
								DeviceInstance: 0,
								FunctionType:   corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								IbPartitionId:  &corev1.IBPartitionId{Value: partition1.ControllerIBPartitionID.String()},
							},
							{
								Device:         "MT2910 Family [ConnectX-7]",
								Vendor:         cutil.GetPtr("Mellanox Technologies"),
								DeviceInstance: 1,
								FunctionType:   corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								IbPartitionId:  &corev1.IBPartitionId{Value: partition1.ControllerIBPartitionID.String()},
							},
						},
					},
					DpuExtensionServices: &corev1.InstanceDpuExtensionServicesConfig{
						ServiceConfigs: []*corev1.InstanceDpuExtensionServiceConfig{
							{
								ServiceId: dpuExtensionService1.ID.String(),
								Version:   "1.0.0",
							},
						},
					},
					Nvlink: &corev1.InstanceNVLinkConfig{
						GpuConfigs: []*corev1.InstanceNVLinkGpuConfig{
							{
								DeviceInstance:     0,
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
							{
								DeviceInstance:     1,
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
							{
								DeviceInstance:     2,
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
							{
								DeviceInstance:     3,
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
					Network: &corev1.InstanceNetworkStatus{
						Interfaces: []*corev1.InstanceInterfaceStatus{
							{
								MacAddress: &macAddress,
								Addresses:  ipAddresses,
							},
							{
								VirtualFunctionId: &vfID,
								MacAddress:        &macAddress,
								Addresses:         ipAddresses,
							},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
					Infiniband: &corev1.InstanceInfinibandStatus{
						IbInterfaces: []*corev1.InstanceIBInterfaceStatus{
							{
								PfGuid: cutil.GetPtr("1070fd0300bd43ad"),
								Guid:   cutil.GetPtr("1070fd0300bd43ad"),
							},
							{
								PfGuid: cutil.GetPtr("c470bd0300ebe2b8"),
								Guid:   cutil.GetPtr("c470bd0300ebe2b8"),
							},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
					DpuExtensionServices: &corev1.InstanceDpuExtensionServicesStatus{
						DpuExtensionServices: []*corev1.InstanceDpuExtensionServiceStatus{
							{
								ServiceId:        dpuExtensionService1.ID.String(),
								Version:          "1.0.0",
								DeploymentStatus: corev1.DpuExtensionServiceDeploymentStatus_DPU_EXTENSION_SERVICE_RUNNING,
							},
						},
					},
					Nvlink: &corev1.InstanceNVLinkStatus{
						GpuStatuses: []*corev1.InstanceNVLinkGpuStatus{
							{
								GpuGuid:            cutil.GetPtr("a8f4c20500d71e9f"),
								DomainId:           &corev1.NVLinkDomainId{Value: uuid.New().String()},
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
							{
								GpuGuid:            cutil.GetPtr("b3e8d70400c52f1a"),
								DomainId:           &corev1.NVLinkDomainId{Value: uuid.New().String()},
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
							{
								GpuGuid:            cutil.GetPtr("c470bd0300ebe2b8"),
								DomainId:           &corev1.NVLinkDomainId{Value: uuid.New().String()},
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
							{
								GpuGuid:            cutil.GetPtr("d3e7d60400c52f1a"),
								DomainId:           &corev1.NVLinkDomainId{Value: uuid.New().String()},
								LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition1.ID.String()},
							},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
					Update: &corev1.InstanceUpdateStatus{
						UserApprovalReceived: false,
					},
				},
			},
			{
				Id:     &corev1.InstanceId{Value: instance5.ControllerInstanceID.String()},
				Config: &corev1.InstanceConfig{},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
				},
			},
			{
				Id:     &corev1.InstanceId{Value: instance6.ControllerInstanceID.String()},
				Config: &corev1.InstanceConfig{},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
				},
			},
			{
				Id:     &corev1.InstanceId{Value: instance7.ID.String()},
				Config: &corev1.InstanceConfig{},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
				},
			},
			{
				Id:     &corev1.InstanceId{Value: instance9.ID.String()},
				Config: &corev1.InstanceConfig{},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
				},
			},
			{
				Id: &corev1.InstanceId{Value: instance11.ControllerInstanceID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{
								FunctionType:     corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								NetworkSegmentId: nil,
								// VPC Prefix info
								NetworkDetails: &corev1.InstanceInterfaceConfig_VpcPrefixId{
									VpcPrefixId: &corev1.VpcPrefixId{Value: vpcPrefix1.ID.String()},
								},
								Device:    cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
								IpAddress: &requestedIpAddress,
								RoutingProfile: &corev1.InstanceInterfaceRoutingProfile{
									AllowedAnycastPrefixes: []*corev1.PrefixFilterPolicyEntry{
										{Prefix: routingProfilePrefixes[0]},
										{Prefix: routingProfilePrefixes[1]},
									},
								},
							},
							{
								FunctionType:     corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
								NetworkSegmentId: nil,
								// VPC Prefix info
								NetworkDetails: &corev1.InstanceInterfaceConfig_VpcPrefixId{
									VpcPrefixId: &corev1.VpcPrefixId{Value: vpcPrefix1.ID.String()},
								},
								Device:            cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
								VirtualFunctionId: &vfID,
							},
							{
								FunctionType:     corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								NetworkSegmentId: nil,
								// VPC Prefix info
								NetworkDetails: &corev1.InstanceInterfaceConfig_VpcPrefixId{
									VpcPrefixId: &corev1.VpcPrefixId{Value: vpcPrefix1.ID.String()},
								},
								Device:         cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
								DeviceInstance: 1,
							},
							{
								FunctionType:     corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
								NetworkSegmentId: nil,
								// VPC Prefix info
								NetworkDetails: &corev1.InstanceInterfaceConfig_VpcPrefixId{
									VpcPrefixId: &corev1.VpcPrefixId{Value: vpcPrefix1.ID.String()},
								},
								Device:            cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
								DeviceInstance:    1,
								VirtualFunctionId: &vfID,
							},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
					Network: &corev1.InstanceNetworkStatus{
						Interfaces: []*corev1.InstanceInterfaceStatus{
							{
								MacAddress: &macAddress,
								Addresses:  ipAddresses,
								Device:     cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
							},
							{
								MacAddress:        &macAddress,
								Addresses:         ipAddresses,
								Device:            cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
								VirtualFunctionId: &vfID,
							},
							{
								VirtualFunctionId: nil,
								MacAddress:        &macAddress,
								Addresses:         ipAddresses,
								Device:            cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
								DeviceInstance:    1,
							},
							{
								MacAddress:        &macAddress,
								Addresses:         ipAddresses,
								Device:            cutil.GetPtr("MT43244 BlueField-3 integrated ConnectX-7 network controller"),
								DeviceInstance:    1,
								VirtualFunctionId: &vfID,
							},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
					Update: &corev1.InstanceUpdateStatus{
						UserApprovalReceived: false,
					},
				},
			},
			{
				Id: &corev1.InstanceId{Value: instance15.ControllerInstanceID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{
								FunctionType:     corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								NetworkSegmentId: nil,
								// VPC Prefix info
								NetworkDetails: &corev1.InstanceInterfaceConfig_VpcPrefixId{
									VpcPrefixId: &corev1.VpcPrefixId{Value: vpcPrefix2.ID.String()},
								},
							},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
					Network: &corev1.InstanceNetworkStatus{
						Interfaces: []*corev1.InstanceInterfaceStatus{
							{
								VirtualFunctionId: &vfID,
								MacAddress:        &macAddress,
								Addresses:         ipAddresses,
							},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
					Update: &corev1.InstanceUpdateStatus{
						UserApprovalReceived: false,
					},
				},
			},
			{
				Id: &corev1.InstanceId{Value: instance19.ControllerInstanceID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{
								FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								NetworkDetails: &corev1.InstanceInterfaceConfig_VpcPrefixId{
									VpcPrefixId: &corev1.VpcPrefixId{Value: vpcPrefix1.ID.String()},
								},
							},
							{
								FunctionType: corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
								NetworkDetails: &corev1.InstanceInterfaceConfig_VpcPrefixId{
									VpcPrefixId: &corev1.VpcPrefixId{Value: vpcPrefix1.ID.String()},
								},
								VirtualFunctionId: cutil.GetPtr(uint32(deviceLessFNNVirtualFunctionID)),
							},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
					Network: &corev1.InstanceNetworkStatus{
						Interfaces: []*corev1.InstanceInterfaceStatus{
							{
								MacAddress: &deviceLessFNNPhysicalMacAddress,
								Addresses:  []string{"192.0.2.31"},
							},
							{
								VirtualFunctionId: cutil.GetPtr(uint32(deviceLessFNNVirtualFunctionID)),
								MacAddress:        &deviceLessFNNVirtualMacAddress,
								Addresses:         []string{"192.0.2.32"},
							},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
				},
			},
			{
				Id:     &corev1.InstanceId{Value: instance13.ID.String()},
				Config: &corev1.InstanceConfig{},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_PROVISIONING,
					},
				},
			},
			{
				Id:     &corev1.InstanceId{Value: instance14.ID.String()},
				Config: &corev1.InstanceConfig{},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
				},
			},
			{
				Id:               &corev1.InstanceId{Value: instance16.ControllerInstanceID.String()},
				Config:           &corev1.InstanceConfig{},
				TpmEkCertificate: &tpmEKCertBase64,
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
				},
			},
			{
				Id:               &corev1.InstanceId{Value: instance17.ControllerInstanceID.String()},
				Config:           &corev1.InstanceConfig{},
				TpmEkCertificate: &tpmEKCertBase64,
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
				},
			},
			{
				Id: &corev1.InstanceId{Value: instance18.ID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{
								FunctionType:     corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
								NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet1.ControllerNetworkSegmentID.String()},
							},
							{
								FunctionType:     corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
								NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet2.ControllerNetworkSegmentID.String()},
							},
						},
					},
					Infiniband: nil,
				},
				Status: &corev1.InstanceStatus{
					Tenant: &corev1.InstanceTenantStatus{
						State: corev1.TenantState_READY,
					},
					Network: &corev1.InstanceNetworkStatus{
						Interfaces: []*corev1.InstanceInterfaceStatus{
							{
								MacAddress: &macAddress,
								Addresses:  ipAddresses,
							},
							{
								VirtualFunctionId: &vfID,
								MacAddress:        &macAddress,
								Addresses:         ipAddresses,
							},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
					Infiniband: nil,
					Update: &corev1.InstanceUpdateStatus{
						UserApprovalReceived: false,
					},
				},
			},
		},
	}

	site2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	vpc2 := util.TestBuildVpc(t, dbSession, ip, site2, tenant, "test-vpc-2")

	// Build another 38 Machines for pagination
	pageMachines := []*cdbm.Machine{}
	for i := 0; i < 38; i++ {
		machine := util.TestBuildMachine(t, dbSession, ip.ID, site2.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
		pageMachines = append(pageMachines, machine)
	}

	allocation2 := util.TestBuildAllocation(t, dbSession, ip, tenant, site2, "test-allocation-2")
	instanceType2 := util.TestBuildInstanceType(t, dbSession, ip, site, "test-instance-type-2")
	_ = util.TestBuildAllocationContraints(t, dbSession, allocation2, cdbm.AllocationResourceTypeInstanceType, instanceType2.ID, cdbm.AllocationConstraintTypeReserved, 38, ipu)

	// Build Instance inventory that is paginated
	// Generate data for 34 Instances reported from Site Agent while Cloud has 38 Instances
	pagedIns := []*cdbm.Instance{}
	pagedInvIds := []string{}
	labels := map[string]string{}
	for i := 0; i < 38; i++ {

		// Making labels mismatch
		if i == 1 {
			labels = map[string]string{
				"west1": "gpu",
			}
		}

		ins, err := instanceDAO.Create(
			ctx, nil,
			cdbm.InstanceCreateInput{
				Name:                     fmt.Sprintf("Test Instance %d", i),
				Description:              cutil.GetPtr("Test description"),
				TenantID:                 tenant.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site2.ID,
				InstanceTypeID:           &instanceType2.ID,
				VpcID:                    vpc2.ID,
				MachineID:                &pageMachines[i].ID,
				ControllerInstanceID:     cutil.GetPtr(uuid.New()),
				Hostname:                 cutil.GetPtr("test.com"),
				OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				AlwaysBootWithCustomIpxe: true,
				UserData:                 cutil.GetPtr("userdata"),
				Labels:                   labels,
				Status:                   cdbm.InstanceStatusReady,
				CreatedBy:                tnu.ID,
			},
		)

		assert.NoError(t, err)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE instance SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), ins.ID.String())
		assert.NoError(t, err)

		// Update updated timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), ins.ID.String())
		assert.NoError(t, err)

		pagedIns = append(pagedIns, ins)
		pagedInvIds = append(pagedInvIds, ins.ControllerInstanceID.String())
	}

	pagedCtrlIns := []*corev1.Instance{}
	for i := 0; i < 34; i++ {
		ctrlIns := &corev1.Instance{
			Id:     &corev1.InstanceId{Value: pagedInvIds[i]},
			Config: &corev1.InstanceConfig{},
			Status: &corev1.InstanceStatus{
				Tenant: &corev1.InstanceTenantStatus{
					State: corev1.TenantState_READY,
				},
			},
		}

		if i == 1 {
			ctrlIns.Metadata = &corev1.Metadata{
				Name:        "Test Instance 1",
				Description: "Test description",
				Labels: []*corev1.Label{
					{
						Key:   "west1",
						Value: cutil.GetPtr("gpu1"),
					},
				},
			}
		}
		pagedCtrlIns = append(pagedCtrlIns, ctrlIns)
	}

	site3 := util.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)
	vpc3 := util.TestBuildVpc(t, dbSession, ip, site3, tenant, "test-vpc-3")

	machine12 := util.TestBuildMachine(t, dbSession, ip.ID, site2.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)

	allocation3 := util.TestBuildAllocation(t, dbSession, ip, tenant, site3, "test-allocation-3")
	instanceType3 := util.TestBuildInstanceType(t, dbSession, ip, site, "test-instance-type-3")
	_ = util.TestBuildAllocationContraints(t, dbSession, allocation3, cdbm.AllocationResourceTypeInstanceType, instanceType3.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)

	instance12, err := instanceDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-12",
			Description:              cutil.GetPtr("Test description"),
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site3.ID,
			InstanceTypeID:           &instanceType3.ID,
			VpcID:                    vpc3.ID,
			MachineID:                &machine12.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusTerminating,
			CreatedBy:                tnu.ID,
		},
	)
	assert.NoError(t, err)
	// Update creation timestamp to be earlier than inventory processing interval
	_, err = dbSession.DB.Exec("UPDATE instance SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instance12.ID.String())
	assert.NoError(t, err)

	// --- Site 4: NVLink Interface deletion strategy test scenarios ---
	site4 := util.TestBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, nil, ipu)
	vpc4 := util.TestBuildVpc(t, dbSession, ip, site4, tenant, "test-vpc-4")
	subnet4 := util.TestBuildSubnet(t, dbSession, tenant, vpc4, "testSubnet-4", cdbm.SubnetStatusPending, cutil.GetPtr(uuid.New()))

	nvllPartition4 := util.TestBuildNVLinkLogicalPartition(t, dbSession, "test-nvlinklpartition-4", nil, site4, tenant, cdbm.NVLinkLogicalPartitionStatusReady, false)

	gpuGuidDel1 := "gpu-guid-nvlink-del-1"
	gpuGuidDel2 := "gpu-guid-nvlink-del-2"
	gpuGuidDel3 := "gpu-guid-nvlink-del-3"
	gpuGuidDel4 := "gpu-guid-nvlink-del-4"
	gpuGuidDel5 := "gpu-guid-nvlink-del-5"

	// Instance A: Deleting NVLink Interface whose GPU GUID/Partition combo is NOT reported in controller status (should be deleted)
	machineNvlA := util.TestBuildMachine(t, dbSession, ip.ID, site4.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	nvlinkDelInstA, err := instanceDAO.Create(ctx, nil, cdbm.InstanceCreateInput{
		Name: "test-instance-nvlink-A", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site4.ID,
		InstanceTypeID: &instanceType.ID, VpcID: vpc4.ID, MachineID: &machineNvlA.ID, ControllerInstanceID: cutil.GetPtr(uuid.New()),
		Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID),
		Labels: map[string]string{}, Status: cdbm.InstanceStatusProvisioning, CreatedBy: tnu.ID,
	})
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlinkDelInstA.ID.String())
	assert.NoError(t, err)
	_ = util.TestBuildInterface(t, dbSession, &nvlinkDelInstA.ID, &subnet4.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)

	nvlifcDelA1 := util.TestBuildNVLinkInterface(t, dbSession, nvlinkDelInstA.ID, site4.ID, nvllPartition4.ID, cutil.GetPtr(""), 0, cutil.GetPtr(gpuGuidDel1), nil, cdbm.NVLinkInterfaceStatusDeleting)
	_, err = dbSession.DB.Exec("UPDATE nvlink_interface SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlifcDelA1.ID.String())
	assert.NoError(t, err)

	nvlifcDelA2 := util.TestBuildNVLinkInterface(t, dbSession, nvlinkDelInstA.ID, site4.ID, nvllPartition4.ID, cutil.GetPtr(""), 1, cutil.GetPtr(gpuGuidDel2), nil, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, nvlifcDelA2)

	// Instance B: Deleting NVLink Interface combo IS reported AND a Pending duplicate exists (should be deleted)
	machineNvlB := util.TestBuildMachine(t, dbSession, ip.ID, site4.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	nvlinkDelInstB, err := instanceDAO.Create(ctx, nil, cdbm.InstanceCreateInput{
		Name: "test-instance-nvlink-B", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site4.ID,
		InstanceTypeID: &instanceType.ID, VpcID: vpc4.ID, MachineID: &machineNvlB.ID, ControllerInstanceID: cutil.GetPtr(uuid.New()),
		Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID),
		Labels: map[string]string{}, Status: cdbm.InstanceStatusProvisioning, CreatedBy: tnu.ID,
	})
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlinkDelInstB.ID.String())
	assert.NoError(t, err)
	_ = util.TestBuildInterface(t, dbSession, &nvlinkDelInstB.ID, &subnet4.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)

	nvlifcDelB1 := util.TestBuildNVLinkInterface(t, dbSession, nvlinkDelInstB.ID, site4.ID, nvllPartition4.ID, cutil.GetPtr(""), 0, cutil.GetPtr(gpuGuidDel3), nil, cdbm.NVLinkInterfaceStatusDeleting)
	_, err = dbSession.DB.Exec("UPDATE nvlink_interface SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlifcDelB1.ID.String())
	assert.NoError(t, err)

	nvlifcDelB2 := util.TestBuildNVLinkInterface(t, dbSession, nvlinkDelInstB.ID, site4.ID, nvllPartition4.ID, cutil.GetPtr(""), 1, cutil.GetPtr(gpuGuidDel3), nil, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, nvlifcDelB2)

	// Instance C: Last deleting NVLink Interface, Site won't report GPUConfig or Status, should be deleted
	machineNvlC := util.TestBuildMachine(t, dbSession, ip.ID, site4.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	nvlinkDelInstC, err := instanceDAO.Create(ctx, nil, cdbm.InstanceCreateInput{
		Name: "test-instance-nvlink-C", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site4.ID,
		InstanceTypeID: &instanceType.ID, VpcID: vpc4.ID, MachineID: &machineNvlC.ID, ControllerInstanceID: cutil.GetPtr(uuid.New()),
		Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID),
		Labels: map[string]string{}, Status: cdbm.InstanceStatusProvisioning, CreatedBy: tnu.ID,
	})
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlinkDelInstC.ID.String())
	assert.NoError(t, err)
	_ = util.TestBuildInterface(t, dbSession, &nvlinkDelInstC.ID, &subnet4.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)

	nvlifcDelC1 := util.TestBuildNVLinkInterface(t, dbSession, nvlinkDelInstC.ID, site4.ID, nvllPartition4.ID, cutil.GetPtr(""), 0, cutil.GetPtr(gpuGuidDel4), nil, cdbm.NVLinkInterfaceStatusDeleting)
	_, err = dbSession.DB.Exec("UPDATE nvlink_interface SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlifcDelC1.ID.String())
	assert.NoError(t, err)
	nvlifcDelC1.Instance = nvlinkDelInstC

	// Instance D: Deleting NVLink Interface with stale inventory (recently updated, should NOT be deleted)
	machineNvlD := util.TestBuildMachine(t, dbSession, ip.ID, site4.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	nvlinkDelInstD, err := instanceDAO.Create(ctx, nil, cdbm.InstanceCreateInput{
		Name: "test-instance-nvlink-D", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site4.ID,
		InstanceTypeID: &instanceType.ID, VpcID: vpc4.ID, MachineID: &machineNvlD.ID, ControllerInstanceID: cutil.GetPtr(uuid.New()),
		Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID),
		Labels: map[string]string{}, Status: cdbm.InstanceStatusProvisioning, CreatedBy: tnu.ID,
	})
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlinkDelInstD.ID.String())
	assert.NoError(t, err)
	_ = util.TestBuildInterface(t, dbSession, &nvlinkDelInstD.ID, &subnet4.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)

	// NVLink Interface D1: NOT setting updated to old time - Instance stale guard should block deletion
	nvlifcDelD1 := util.TestBuildNVLinkInterface(t, dbSession, nvlinkDelInstD.ID, site4.ID, nvllPartition4.ID, cutil.GetPtr(""), 0, cutil.GetPtr(gpuGuidDel5), nil, cdbm.NVLinkInterfaceStatusDeleting)
	assert.NotNil(t, nvlifcDelD1)
	nvlifcDelD1.Instance = nvlinkDelInstD

	// Instance E: Deleting NVLink Interface when configs are NOT synced (should NOT be deleted)
	machineNvlE := util.TestBuildMachine(t, dbSession, ip.ID, site4.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	nvlinkDelInstE, err := instanceDAO.Create(ctx, nil, cdbm.InstanceCreateInput{
		Name: "test-instance-nvlink-E", TenantID: tenant.ID, InfrastructureProviderID: ip.ID, SiteID: site4.ID,
		InstanceTypeID: &instanceType.ID, VpcID: vpc4.ID, MachineID: &machineNvlE.ID, ControllerInstanceID: cutil.GetPtr(uuid.New()),
		Hostname: cutil.GetPtr("test.com"), OperatingSystemID: cutil.GetPtr(operatingSystem.ID),
		Labels: map[string]string{}, Status: cdbm.InstanceStatusProvisioning, CreatedBy: tnu.ID,
	})
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlinkDelInstE.ID.String())
	assert.NoError(t, err)
	_ = util.TestBuildInterface(t, dbSession, &nvlinkDelInstE.ID, &subnet4.ID, nil, true, nil, nil, nil, &tnu.ID, cdbm.InterfaceStatusPending)

	nvlifcDelE1 := util.TestBuildNVLinkInterface(t, dbSession, nvlinkDelInstE.ID, site4.ID, nvllPartition4.ID, cutil.GetPtr(""), 0, cutil.GetPtr(gpuGuidDel1), nil, cdbm.NVLinkInterfaceStatusDeleting)
	_, err = dbSession.DB.Exec("UPDATE nvlink_interface SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), nvlifcDelE1.ID.String())
	assert.NoError(t, err)
	nvlifcDelE1.Instance = nvlinkDelInstE

	nvlinkDelMacAddress := "2F-FC-34-AE-9C-2B"
	nvlinkDelInventory := &corev1.InstanceInventory{
		Instances: []*corev1.Instance{
			{
				Id: &corev1.InstanceId{Value: nvlinkDelInstA.ID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION, NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet4.ControllerNetworkSegmentID.String()}},
						},
					},
					Nvlink: &corev1.InstanceNVLinkConfig{
						GpuConfigs: []*corev1.InstanceNVLinkGpuConfig{
							{DeviceInstance: 1, LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition4.ID.String()}},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant:  &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
					Network: &corev1.InstanceNetworkStatus{Interfaces: []*corev1.InstanceInterfaceStatus{{MacAddress: &nvlinkDelMacAddress}}, ConfigsSynced: corev1.SyncState_SYNCED},
					Nvlink: &corev1.InstanceNVLinkStatus{
						GpuStatuses: []*corev1.InstanceNVLinkGpuStatus{
							{GpuGuid: cutil.GetPtr(gpuGuidDel2), LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition4.ID.String()}},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
				},
			},
			{
				Id: &corev1.InstanceId{Value: nvlinkDelInstB.ID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION, NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet4.ControllerNetworkSegmentID.String()}},
						},
					},
					Nvlink: &corev1.InstanceNVLinkConfig{
						GpuConfigs: []*corev1.InstanceNVLinkGpuConfig{
							{DeviceInstance: 1, LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition4.ID.String()}},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant:  &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
					Network: &corev1.InstanceNetworkStatus{Interfaces: []*corev1.InstanceInterfaceStatus{{MacAddress: &nvlinkDelMacAddress}}, ConfigsSynced: corev1.SyncState_SYNCED},
					Nvlink: &corev1.InstanceNVLinkStatus{
						GpuStatuses: []*corev1.InstanceNVLinkGpuStatus{
							{GpuGuid: cutil.GetPtr(gpuGuidDel3), LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition4.ID.String()}},
						},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
				},
			},
			{
				Id: &corev1.InstanceId{Value: nvlinkDelInstC.ID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION, NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet4.ControllerNetworkSegmentID.String()}},
						},
					},
					Nvlink: &corev1.InstanceNVLinkConfig{},
				},
				Status: &corev1.InstanceStatus{
					Tenant:  &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
					Network: &corev1.InstanceNetworkStatus{Interfaces: []*corev1.InstanceInterfaceStatus{{MacAddress: &nvlinkDelMacAddress}}, ConfigsSynced: corev1.SyncState_SYNCED},
					Nvlink:  &corev1.InstanceNVLinkStatus{},
				},
			},
			{
				Id: &corev1.InstanceId{Value: nvlinkDelInstD.ID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION, NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet4.ControllerNetworkSegmentID.String()}},
						},
					},
					Nvlink: &corev1.InstanceNVLinkConfig{
						GpuConfigs: []*corev1.InstanceNVLinkGpuConfig{
							{DeviceInstance: 0, LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition4.ID.String()}},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant:  &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
					Network: &corev1.InstanceNetworkStatus{Interfaces: []*corev1.InstanceInterfaceStatus{{MacAddress: &nvlinkDelMacAddress}}, ConfigsSynced: corev1.SyncState_SYNCED},
					Nvlink: &corev1.InstanceNVLinkStatus{
						GpuStatuses:   []*corev1.InstanceNVLinkGpuStatus{},
						ConfigsSynced: corev1.SyncState_SYNCED,
					},
				},
			},
			{
				Id: &corev1.InstanceId{Value: nvlinkDelInstE.ID.String()},
				Config: &corev1.InstanceConfig{
					Network: &corev1.InstanceNetworkConfig{
						Interfaces: []*corev1.InstanceInterfaceConfig{
							{FunctionType: corev1.InterfaceFunctionType_PHYSICAL_FUNCTION, NetworkSegmentId: &corev1.NetworkSegmentId{Value: subnet4.ControllerNetworkSegmentID.String()}},
						},
					},
					Nvlink: &corev1.InstanceNVLinkConfig{
						GpuConfigs: []*corev1.InstanceNVLinkGpuConfig{
							{DeviceInstance: 0, LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvllPartition4.ID.String()}},
						},
					},
				},
				Status: &corev1.InstanceStatus{
					Tenant:  &corev1.InstanceTenantStatus{State: corev1.TenantState_READY},
					Network: &corev1.InstanceNetworkStatus{Interfaces: []*corev1.InstanceInterfaceStatus{{MacAddress: &nvlinkDelMacAddress}}, ConfigsSynced: corev1.SyncState_SYNCED},
					Nvlink: &corev1.InstanceNVLinkStatus{
						GpuStatuses:   []*corev1.InstanceNVLinkGpuStatus{},
						ConfigsSynced: corev1.SyncState_PENDING,
					},
				},
			},
		},
	}

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	ifcDAO := cdbm.NewInterfaceDAO(dbSession)
	ibifcDAO := cdbm.NewInfiniBandInterfaceDAO(dbSession)
	nvifcDAO := cdbm.NewNVLinkInterfaceDAO(dbSession)
	mDAO := cdbm.NewMachineDAO(dbSession)
	skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(dbSession)

	// Mock UpdateInstance workflow from site-agent
	wrun := &tmocks.WorkflowRun{}
	wid := "test-workflow-id"
	wrun.On("GetID").Return(wid)

	workflowOptions1 := client.StartWorkflowOptions{
		ID:        "site-instance-update-metadata-" + instance1.ID.String(),
		TaskQueue: queue.SiteTaskQueue,
	}

	workflowOptions2 := client.StartWorkflowOptions{
		ID:        "site-instance-update-metadata-" + pagedIns[1].ID.String(),
		TaskQueue: queue.SiteTaskQueue,
	}

	mtc1 := &tmocks.Client{}
	mtc1.Mock.On("ExecuteWorkflow", context.Background(), workflowOptions1, "UpdateInstance",
		mock.Anything).Return(wrun, nil)

	mtc3 := &tmocks.Client{}
	mtc3.Mock.On("ExecuteWorkflow", context.Background(), workflowOptions2, "UpdateInstance",
		mock.Anything).Return(wrun, nil)

	mtc4 := &tmocks.Client{}

	assert.NotNil(t, tSiteClientPool)

	wtc := &tmocks.Client{}

	cfg := config.GetTestConfig()

	tests := []struct {
		name                                  string
		siteID                                uuid.UUID
		clientPoolSiteID                      string
		clientPoolClient                      *tmocks.Client
		instanceInventory                     *corev1.InstanceInventory
		updatedInstance                       *cdbm.Instance
		updatedVpcPrefixInstance              *cdbm.Instance
		updatedMultiDPUInstance               *cdbm.Instance
		nsgPropagationDetailsClearedInstances []*cdbm.Instance
		readyInstances                        []*cdbm.Instance
		deletingInstance                      *cdbm.Instance
		deletedInstances                      []*cdbm.Instance
		missingInstances                      []*cdbm.Instance
		restoredInstance                      *cdbm.Instance
		unpairedInstances                     []*cdbm.Instance
		bootCompletedInstances                []*cdbm.Instance
		unchangedInstances                    []*cdbm.Instance
		deletingInterfaces                    []*cdbm.Interface
		readyInterfaces                       []*cdbm.Interface
		clearedRequestedIpInterfaces          []*cdbm.Interface
		clearedInlineRoutingProfileInterfaces []*cdbm.Interface
		vpcPrefixInterfaces                   []*cdbm.Interface
		multiDPUInterfaces                    []*cdbm.Interface
		deviceLessFNNPhysicalInterface        *cdbm.Interface
		deviceLessFNNVirtualInterface         *cdbm.Interface
		deletedInfiniBandInterfaces           []*cdbm.InfiniBandInterface
		readyInfiniBandInterfaces             []*cdbm.InfiniBandInterface
		updatedDpuExtServiceDeployments       []*cdbm.DpuExtensionServiceDeployment
		deletedDpuExtServiceDeployments       []*cdbm.DpuExtensionServiceDeployment
		readyNVLinkInterfaces                 []*cdbm.NVLinkInterface
		deletedNVLinkInterfaces               []*cdbm.NVLinkInterface
		deletingNVLinkInterfaces              []*cdbm.NVLinkInterface
		requiredMetadataUpdate                bool
		metadataInstanceUpdate                *cdbm.Instance
		tpmCertificateUpdatedInstance         *cdbm.Instance
		expectErr                             bool
	}{
		{
			name:              "test Instance inventory processing error, non-existent site",
			siteID:            uuid.New(),
			instanceInventory: instanceInventory,
			expectErr:         true,
		},
		{
			name:                                  "test Instance inventory processing success including VPC prefix interfaces",
			siteID:                                site.ID,
			clientPoolSiteID:                      site.ID.String(),
			clientPoolClient:                      mtc1,
			instanceInventory:                     instanceInventory,
			updatedVpcPrefixInstance:              instance11,
			updatedInstance:                       instance1,
			deletingInstance:                      instance5,
			updatedMultiDPUInstance:               instance11,
			deletedInstances:                      []*cdbm.Instance{instance2, instance8},
			missingInstances:                      []*cdbm.Instance{instance3, instance4, instance10},
			restoredInstance:                      instance6,
			unpairedInstances:                     []*cdbm.Instance{instance7},
			bootCompletedInstances:                []*cdbm.Instance{instance9},
			unchangedInstances:                    []*cdbm.Instance{instance13, instance14},
			deletingInterfaces:                    []*cdbm.Interface{ifcvpc_deleting},
			readyInterfaces:                       []*cdbm.Interface{ifcvpc_pending},
			clearedRequestedIpInterfaces:          []*cdbm.Interface{ifcvpc_pending},
			clearedInlineRoutingProfileInterfaces: []*cdbm.Interface{ifcvpc_pending},
			deletedInfiniBandInterfaces:           []*cdbm.InfiniBandInterface{ibInterface3, ibInterface18_1, ibInterface18_2, ibInterface18_3},
			readyInfiniBandInterfaces:             []*cdbm.InfiniBandInterface{ibInterface1, ibInterface2},
			multiDPUInterfaces:                    []*cdbm.Interface{ifcvpc0, ifcvpc1, ifcvpc0_1, ifcvpc1_1},
			vpcPrefixInterfaces:                   []*cdbm.Interface{ifcvpc0, ifcvpc1, ifcvpc0_1, ifcvpc1_1},
			deviceLessFNNPhysicalInterface:        deviceLessFNNPhysicalInterface,
			deviceLessFNNVirtualInterface:         deviceLessFNNVirtualInterface,
			updatedDpuExtServiceDeployments:       []*cdbm.DpuExtensionServiceDeployment{dpuExtServiceDeployment1},
			deletedDpuExtServiceDeployments:       []*cdbm.DpuExtensionServiceDeployment{dpuExtServiceDeployment2},
			readyNVLinkInterfaces:                 []*cdbm.NVLinkInterface{nvlinkInterface1, nvlinkInterface2},
			deletedNVLinkInterfaces:               []*cdbm.NVLinkInterface{nvlinkInterface3},
			tpmCertificateUpdatedInstance:         instance16,
			expectErr:                             false,
		},
		{
			name:             "test Instance inventory processing success with nil TPM certificate update",
			siteID:           site.ID,
			clientPoolSiteID: site.ID.String(),
			clientPoolClient: mtc1,
			instanceInventory: &corev1.InstanceInventory{
				Instances: []*corev1.Instance{
					{
						Id:               &corev1.InstanceId{Value: instance17.ControllerInstanceID.String()},
						Config:           &corev1.InstanceConfig{},
						TpmEkCertificate: &tpmEKCertBase64,
						Status: &corev1.InstanceStatus{
							Tenant: &corev1.InstanceTenantStatus{
								State: corev1.TenantState_READY,
							},
						},
					},
				},
			},
			tpmCertificateUpdatedInstance: instance17,
			expectErr:                     false,
		},
		{
			name:             "test paged Instance inventory processing, empty inventory",
			siteID:           site3.ID,
			clientPoolSiteID: site3.ID.String(),
			clientPoolClient: mtc3,
			instanceInventory: &corev1.InstanceInventory{
				Instances:       []*corev1.Instance{},
				Timestamp:       timestamppb.Now(),
				InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				InventoryPage: &corev1.InventoryPage{
					CurrentPage: 1,
					PageSize:    25,
				},
			},
			deletedInstances: []*cdbm.Instance{instance12},
		},
		{
			name:             "test paged Instance inventory processing, first page",
			siteID:           site2.ID,
			clientPoolSiteID: site2.ID.String(),
			clientPoolClient: mtc3,
			instanceInventory: &corev1.InstanceInventory{
				Instances: pagedCtrlIns[0:10],
				Timestamp: timestamppb.Now(),
				InventoryPage: &corev1.InventoryPage{
					CurrentPage: 1,
					TotalPages:  4,
					PageSize:    10,
					TotalItems:  34,
					ItemIds:     pagedInvIds[0:34],
				},
			},
			readyInstances: pagedIns[0:34],
		},
		{
			name:             "test paged Instance inventory processing, last page",
			siteID:           site2.ID,
			clientPoolSiteID: site2.ID.String(),
			clientPoolClient: mtc3,
			instanceInventory: &corev1.InstanceInventory{
				Instances: pagedCtrlIns[30:34],
				Timestamp: timestamppb.Now(),
				InventoryPage: &corev1.InventoryPage{
					CurrentPage: 4,
					TotalPages:  4,
					PageSize:    10,
					TotalItems:  34,
					ItemIds:     pagedInvIds[0:34],
				},
			},
			readyInstances:   pagedIns[0:34],
			missingInstances: pagedIns[34:38],
		},
		{
			name:             "test paged Instance inventory processing success with initiation of Instance metadata update workflow, label value mismatched",
			siteID:           site2.ID,
			clientPoolSiteID: site2.ID.String(),
			clientPoolClient: mtc3,
			instanceInventory: &corev1.InstanceInventory{
				Instances: pagedCtrlIns[0:10],
				Timestamp: timestamppb.Now(),
				InventoryPage: &corev1.InventoryPage{
					CurrentPage: 1,
					TotalPages:  4,
					PageSize:    10,
					TotalItems:  34,
					ItemIds:     pagedInvIds[0:34],
				},
			},
			readyInstances:         pagedIns[0:34],
			requiredMetadataUpdate: true,
			metadataInstanceUpdate: pagedIns[1],
		},
		{
			name:                     "test NVLink Interface deletion strategy with synced/unsynced configs and stale inventory",
			siteID:                   site4.ID,
			clientPoolSiteID:         site4.ID.String(),
			clientPoolClient:         mtc4,
			instanceInventory:        nvlinkDelInventory,
			deletedNVLinkInterfaces:  []*cdbm.NVLinkInterface{nvlifcDelA1, nvlifcDelB1, nvlifcDelC1},
			readyNVLinkInterfaces:    []*cdbm.NVLinkInterface{nvlifcDelA2, nvlifcDelB2},
			deletingNVLinkInterfaces: []*cdbm.NVLinkInterface{nvlifcDelD1, nvlifcDelE1},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {

			ms := NewManageInstance(dbSession, tSiteClientPool, wtc, cfg)
			ms.siteClientPool.IDClientMap[tc.clientPoolSiteID] = tc.clientPoolClient

			_, err := ms.UpdateInstancesInDB(ctx, tc.siteID, tc.instanceInventory)
			assert.Equal(t, tc.expectErr, err != nil)

			if tc.expectErr {
				return
			}

			for _, inst := range tc.nsgPropagationDetailsClearedInstances {
				// If the VPC should not have propagation details according to the site
				// make sure the DB agrees.
				updatedInstance, _ := instanceDAO.GetByID(ctx, nil, inst.ID, nil)
				assert.Nil(t, updatedInstance.NetworkSecurityGroupPropagationDetails)
			}

			if tc.updatedInstance != nil {
				// Check that Instance status, Instance Subnet status and attributes were updated in DB for Instance 1
				updatedInstance, err := instanceDAO.GetByID(ctx, nil, tc.updatedInstance.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.InstanceStatusReady, updatedInstance.Status)

				// Instance Interfaces
				_, totalIfcs, err := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{updatedInstance.ID}}, paginator.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 3, totalIfcs)

				ifc1, err := ifcDAO.GetByID(ctx, nil, interface1.ID, nil)
				assert.Nil(t, err)
				if ifc1.SubnetID != nil {
					assert.Equal(t, subnet1.ID, *ifc1.SubnetID)
				}
				assert.True(t, ifc1.IsPhysical)
				assert.Equal(t, macAddress, *ifc1.MacAddress)
				assert.Equal(t, ipAddresses, ifc1.IPAddresses)
				assert.Equal(t, cdbm.InterfaceStatusReady, ifc1.Status)

				ifc2, err := ifcDAO.GetByID(ctx, nil, interface2.ID, nil)
				assert.Nil(t, err)
				if ifc2.SubnetID != nil {
					assert.Equal(t, subnet2.ID, *ifc2.SubnetID)
				}
				assert.False(t, ifc2.IsPhysical)
				assert.Equal(t, int(vfID), *ifc2.VirtualFunctionID)
				assert.Equal(t, macAddress, *ifc2.MacAddress)
				assert.Equal(t, ipAddresses, ifc2.IPAddresses)
				assert.Equal(t, cdbm.InterfaceStatusReady, ifc2.Status)

				ifc3, err := ifcDAO.GetByID(ctx, nil, interface3.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.InterfaceStatusError, ifc3.Status)

				// Instance IB Interfaces
				_, totalIbIfcs, err := ibifcDAO.GetAll(
					ctx,
					nil,
					cdbm.InfiniBandInterfaceFilterInput{
						InstanceIDs: []uuid.UUID{updatedInstance.ID},
					},
					paginator.PageInput{},
					nil,
				)
				assert.Nil(t, err)
				assert.Equal(t, 2, totalIbIfcs)

				ibIfc1, err := ibifcDAO.GetByID(ctx, nil, ibInterface1.ID, nil)
				assert.Nil(t, err)

				assert.Equal(t, partition1.ID, ibIfc1.InfiniBandPartitionID)
				assert.Equal(t, cdbm.InfiniBandInterfaceStatusReady, ibIfc1.Status)
				assert.True(t, ibIfc1.IsPhysical)
				assert.Equal(t, tc.instanceInventory.Instances[0].Status.Infiniband.IbInterfaces[0].PfGuid, ibIfc1.PhysicalGUID)
				assert.Equal(t, tc.instanceInventory.Instances[0].Status.Infiniband.IbInterfaces[0].Guid, ibIfc1.GUID)

				ibIfc2, err := ibifcDAO.GetByID(ctx, nil, ibInterface2.ID, nil)
				assert.Nil(t, err)

				assert.Equal(t, partition1.ID, ibIfc2.InfiniBandPartitionID)
				assert.Equal(t, cdbm.InfiniBandInterfaceStatusReady, ibIfc2.Status)
				assert.True(t, ibIfc2.IsPhysical)
				assert.Equal(t, tc.instanceInventory.Instances[0].Status.Infiniband.IbInterfaces[1].PfGuid, ibIfc2.PhysicalGUID)
				assert.Equal(t, tc.instanceInventory.Instances[0].Status.Infiniband.IbInterfaces[1].Guid, ibIfc2.GUID)

				if len(tc.instanceInventory.Instances) > 0 && tc.instanceInventory.Instances[0].Status.Update != nil {
					assert.Equal(t, !tc.instanceInventory.Instances[0].Status.Update.UserApprovalReceived, updatedInstance.IsUpdatePending)
				}

				// Instance NVLink Interfaces
				_, totalNvIfcs, err := nvifcDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{updatedInstance.ID}}, paginator.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 3, totalNvIfcs)

				nvIfc1, err := nvifcDAO.GetByID(ctx, nil, nvlinkInterface1.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, nvllPartition1.ID, nvIfc1.NVLinkLogicalPartitionID)
				assert.Equal(t, cdbm.NVLinkInterfaceStatusReady, nvIfc1.Status)

				nvIfc2, err := nvifcDAO.GetByID(ctx, nil, nvlinkInterface2.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, nvllPartition1.ID, nvIfc2.NVLinkLogicalPartitionID)
				assert.Equal(t, cdbm.NVLinkInterfaceStatusReady, nvIfc2.Status)
			}

			if tc.updatedVpcPrefixInstance != nil {
				// Check that Instance status, Instance VpcPrefix status and attributes were updated in DB for Instance 1
				updatedVpInstance, err := instanceDAO.GetByID(ctx, nil, tc.updatedVpcPrefixInstance.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.InstanceStatusReady, updatedVpInstance.Status)

				_, totalIfcs, err := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{updatedVpInstance.ID}}, paginator.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, len(tc.vpcPrefixInterfaces), totalIfcs)

				ifcvpc, err := ifcDAO.GetByID(ctx, nil, tc.vpcPrefixInterfaces[0].ID, nil)
				assert.Nil(t, err)
				if ifcvpc.VpcPrefixID != nil {
					assert.Equal(t, vpcPrefix1.ID, *ifcvpc.VpcPrefixID)
				}
				assert.True(t, ifcvpc.IsPhysical)
				require.NotNil(t, ifcvpc.RequestedIpAddress)
				assert.Equal(t, requestedIpAddress, *ifcvpc.RequestedIpAddress)
				require.NotNil(t, ifcvpc.InlineRoutingProfile)
				assert.Equal(t, routingProfilePrefixes, ifcvpc.InlineRoutingProfile.AllowedAnycastPrefixes)
				assert.Equal(t, macAddress, *ifcvpc.MacAddress)
				assert.Equal(t, ipAddresses, ifcvpc.IPAddresses)
				assert.Equal(t, cdbm.InterfaceStatusReady, ifcvpc.Status)
			}

			if tc.updatedMultiDPUInstance != nil {
				// Check that Instance status, Instance VpcPrefix status and attributes were updated in DB for Instance 1
				updatedMultiDPUInstance, err := instanceDAO.GetByID(ctx, nil, tc.updatedMultiDPUInstance.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.InstanceStatusReady, updatedMultiDPUInstance.Status)

				_, totalIfcs, err := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{updatedMultiDPUInstance.ID}}, paginator.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, len(tc.multiDPUInterfaces), totalIfcs)

				for _, ifc := range tc.multiDPUInterfaces {
					uifc, err := ifcDAO.GetByID(ctx, nil, ifc.ID, nil)
					assert.Nil(t, err)
					assert.Equal(t, cdbm.InterfaceStatusReady, uifc.Status)
					if uifc.Device != nil {
						assert.Equal(t, *uifc.Device, *ifc.Device)

						if ifc.DeviceInstance != nil {
							assert.Equal(t, *uifc.DeviceInstance, *ifc.DeviceInstance)
						} else {
							assert.Equal(t, *uifc.DeviceInstance, 0)
						}
					}
					if ifc.VirtualFunctionID != nil {
						assert.Equal(t, *uifc.VirtualFunctionID, *ifc.VirtualFunctionID)
					}
					assert.Equal(t, uifc.IsPhysical, ifc.IsPhysical)
					assert.Equal(t, *uifc.VpcPrefixID, *ifc.VpcPrefixID)
				}
			}

			if tc.deviceLessFNNPhysicalInterface != nil {
				physicalInterface, err := ifcDAO.GetByID(ctx, nil, tc.deviceLessFNNPhysicalInterface.ID, nil)
				require.NoError(t, err)
				assert.Equal(t, cdbm.InterfaceStatusReady, physicalInterface.Status)
				require.NotNil(t, physicalInterface.MacAddress)
				assert.Equal(t, deviceLessFNNPhysicalMacAddress, *physicalInterface.MacAddress)
				assert.Equal(t, []string{"192.0.2.31"}, physicalInterface.IPAddresses)

				virtualInterface, err := ifcDAO.GetByID(ctx, nil, tc.deviceLessFNNVirtualInterface.ID, nil)
				require.NoError(t, err)
				assert.Equal(t, cdbm.InterfaceStatusReady, virtualInterface.Status)
				require.NotNil(t, virtualInterface.MacAddress)
				assert.Equal(t, deviceLessFNNVirtualMacAddress, *virtualInterface.MacAddress)
				assert.Equal(t, []string{"192.0.2.32"}, virtualInterface.IPAddresses)
				require.NotNil(t, virtualInterface.VirtualFunctionID)
				assert.Equal(t, deviceLessFNNVirtualFunctionID, *virtualInterface.VirtualFunctionID)
			}

			for _, instPropStatus := range tc.instanceInventory.NetworkSecurityGroupPropagations {
				updatedInstance, _ := instanceDAO.GetByID(ctx, nil, uuid.MustParse(instPropStatus.Id), nil)

				// Prop details should not be nil
				assert.NotNil(t, updatedInstance.NetworkSecurityGroupPropagationDetails)

				// The details should match
				assert.Equal(
					t,
					updatedInstance.NetworkSecurityGroupPropagationDetails.NetworkSecurityGroupPropagationObjectStatus,
					instPropStatus,
					"\n%+v \n != \n %+v\n",
					updatedInstance.NetworkSecurityGroupPropagationDetails.NetworkSecurityGroupPropagationObjectStatus,
					instPropStatus,
				)
			}

			if tc.deletingInstance != nil {
				// Check that Instance 5, which was in Terminating state, DID get its state changed to Ready if Site inventory reports it as Ready
				ui, serr := instanceDAO.GetByID(ctx, nil, tc.deletingInstance.ID, nil)
				assert.NoError(t, serr)
				assert.Equal(t, cdbm.InstanceStatusReady, ui.Status)
			}

			for _, instance := range tc.readyInstances {
				ui, serr := instanceDAO.GetByID(ctx, nil, instance.ID, nil)
				assert.NoError(t, serr)
				assert.Equal(t, cdbm.InstanceStatusReady, ui.Status)
			}

			for _, instance := range tc.deletedInstances {
				// Check that Instance was deleted as it was in Terminating state and missing from Inventory
				_, err = instanceDAO.GetByID(ctx, nil, instance.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("Instance %s was not deleted", instance.Name))
				// Check Instance Subnets were deleted for Instance 2
				_, ifcCnt, err := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, paginator.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 0, ifcCnt)

				// Check that the machine isAssigned is set to false for Machine 2
				uMachine, err := mDAO.GetByID(ctx, nil, *instance.MachineID, nil, false)
				assert.Nil(t, err)
				assert.Equal(t, false, uMachine.IsAssigned)

				// Check that SSH Key Group Instance associations were deleted
				_, skgiaCnt, err := skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
					InstanceIDs: []uuid.UUID{instance.ID},
				}, paginator.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 0, skgiaCnt)
			}

			for _, instance := range tc.missingInstances {
				// Check that Instance has missing flag set
				ui, serr := instanceDAO.GetByID(ctx, nil, instance.ID, nil)
				assert.Nil(t, serr)

				if ui.ControllerInstanceID != nil {
					assert.True(t, ui.IsMissingOnSite)
					assert.Equal(t, cdbm.InstanceStatusError, ui.Status)

					sds, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{instance.ID.String()}}, cdbp.PageInput{})
					assert.Nil(t, err)
					assert.Equal(t, 1, len(sds), instance.Name)
				} else {
					assert.False(t, ui.IsMissingOnSite)
				}
			}

			if tc.restoredInstance != nil {
				// Check that Instance status is set to Ready
				ui, err := instanceDAO.GetByID(ctx, nil, tc.restoredInstance.ID, nil)
				assert.Nil(t, err)
				assert.False(t, ui.IsMissingOnSite)
				assert.Equal(t, cdbm.InstanceStatusReady, ui.Status)
			}

			for _, instance := range tc.unpairedInstances {
				// Check that Instance has controller ID set
				ui, serr := instanceDAO.GetByID(ctx, nil, instance.ID, nil)
				assert.Nil(t, serr)
				assert.NotNil(t, ui.ControllerInstanceID)
			}

			for _, instance := range tc.bootCompletedInstances {
				// Check that Instance power status is set to BootCompleted
				ui, serr := instanceDAO.GetByID(ctx, nil, instance.ID, nil)
				assert.Nil(t, serr)
				assert.Equal(t, cdbm.InstancePowerStatusBootCompleted, *ui.PowerStatus)
			}

			for _, instance := range tc.unchangedInstances {
				// Check that no new status detail has been created for Instance
				sds, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{instance.ID.String()}}, cdbp.PageInput{})
				assert.Nil(t, err)
				assert.Equal(t, 1, len(sds))
			}

			if tc.requiredMetadataUpdate {
				assert.True(t, len(tc.clientPoolClient.Calls) > 0)
				assert.Equal(t, len(tc.clientPoolClient.Calls[0].Arguments), 4)

				scReq := tc.clientPoolClient.Calls[0].Arguments[3].(*corev1.InstanceConfigUpdateRequest)
				assert.Equal(t, tc.metadataInstanceUpdate.ControllerInstanceID.String(), scReq.InstanceId.Value)
			}

			if tc.tpmCertificateUpdatedInstance != nil {
				// Verify that the TPM EK certificate was updated from nil to the base64 encoded value
				updatedInstance, err := instanceDAO.GetByID(ctx, nil, tc.tpmCertificateUpdatedInstance.ID, nil)
				assert.Nil(t, err)
				assert.NotNil(t, updatedInstance.TpmEkCertificate)
				assert.Equal(t, tpmEKCertBase64, *updatedInstance.TpmEkCertificate)
			}

			// Make sure the deleting interfaces are deleted
			for _, ifc := range tc.deletingInterfaces {
				_, err := ifcDAO.GetByID(ctx, nil, ifc.ID, nil)
				assert.NotNil(t, err)
				assert.Equal(t, cdb.ErrDoesNotExist, err)
			}

			for _, ifc := range tc.readyInterfaces {
				inteface, err := ifcDAO.GetByID(ctx, nil, ifc.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.InterfaceStatusReady, inteface.Status)
			}

			for _, ifc := range tc.clearedRequestedIpInterfaces {
				inteface, err := ifcDAO.GetByID(ctx, nil, ifc.ID, nil)
				assert.Nil(t, err)
				assert.Nil(t, inteface.RequestedIpAddress)
			}

			for _, ifc := range tc.clearedInlineRoutingProfileInterfaces {
				inteface, err := ifcDAO.GetByID(ctx, nil, ifc.ID, nil)
				assert.Nil(t, err)
				assert.Nil(t, inteface.InlineRoutingProfile)
			}

			if tc.deletedInfiniBandInterfaces != nil {
				for _, ibfc := range tc.deletedInfiniBandInterfaces {
					_, err := ibifcDAO.GetByID(ctx, nil, ibfc.ID, nil)
					assert.Equal(t, cdb.ErrDoesNotExist, err)
				}
			}

			if tc.readyInfiniBandInterfaces != nil {
				for _, ibfc := range tc.readyInfiniBandInterfaces {
					ibIfc, err := ibifcDAO.GetByID(ctx, nil, ibfc.ID, nil)
					assert.Nil(t, err)
					assert.Equal(t, cdbm.InfiniBandInterfaceStatusReady, ibIfc.Status)
				}
			}

			// Verify DPU Extension Service Deployment updates
			if tc.updatedDpuExtServiceDeployments != nil {
				desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(dbSession)
				for _, desd := range tc.updatedDpuExtServiceDeployments {
					updatedDesd, err := desdDAO.GetByID(ctx, nil, desd.ID, nil)
					assert.Nil(t, err)
					assert.Equal(t, cdbm.DpuExtensionServiceDeploymentStatusRunning, updatedDesd.Status)
				}
			}

			// Verify DPU Extension Service Deployment deletions
			if tc.deletedDpuExtServiceDeployments != nil {
				desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(dbSession)
				for _, desd := range tc.deletedDpuExtServiceDeployments {
					_, err := desdDAO.GetByID(ctx, nil, desd.ID, nil)
					assert.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("DPU Extension Service Deployment %s should have been deleted", desd.ID.String()))
				}
			}

			if tc.readyNVLinkInterfaces != nil {
				for _, nvIfc := range tc.readyNVLinkInterfaces {
					unvIfc, err := nvifcDAO.GetByID(ctx, nil, nvIfc.ID, nil)
					assert.Nil(t, err)
					assert.Equal(t, cdbm.NVLinkInterfaceStatusReady, unvIfc.Status)
					assert.Equal(t, nvIfc.DeviceInstance, unvIfc.DeviceInstance)
				}
			}

			if tc.deletedNVLinkInterfaces != nil {
				for _, nvIfc := range tc.deletedNVLinkInterfaces {
					_, err := nvifcDAO.GetByID(ctx, nil, nvIfc.ID, nil)
					assert.Equal(t, cdb.ErrDoesNotExist, err)
				}
			}

			if tc.deletingNVLinkInterfaces != nil {
				for _, nvIfc := range tc.deletingNVLinkInterfaces {
					unvIfc, err := nvifcDAO.GetByID(ctx, nil, nvIfc.ID, []string{cdbm.InstanceRelationName})
					require.NoError(t, err, "NVLink Interface: %s for Instance: %s should still exist", nvIfc.ID.String(), nvIfc.Instance.Name)
					assert.Equal(t, cdbm.NVLinkInterfaceStatusDeleting, unvIfc.Status)
				}
			}
		})
	}
}

// Metrics tests with various statusDetails scenarios

// Test Instance Metrics - CREATE operations
func Test_InstanceMetrics_Create_PendingToReady(t *testing.T) {
	// Case 1: pending -> ready (should emit metric with duration t2-t1)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageInstanceLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testInstanceID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour) // Use past time to avoid conflicts
	t1 := baseTime
	t2 := baseTime.Add(100 * time.Millisecond)
	expectedDuration := t2.Sub(t1)

	// t1: pending
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusPending, nil, t1)

	// t2: ready
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusReady, nil, t2)

	// Process create event
	ctx := context.Background()
	err := lifecycleMetrics.RecordInstanceStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testInstanceID, Created: &t2},
	})
	assert.NoError(t, err)

	// Verify metric was emitted with correct duration
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_instance_operation_latency_seconds", 1, map[string]string{
		"operation_type": "create",
		"from_status":    cdbm.InstanceStatusPending,
		"to_status":      cdbm.InstanceStatusReady,
	}, expectedDuration)
}

func Test_InstanceMetrics_Create_PendingErrorReady(t *testing.T) {
	// Case 2: pending -> error -> ready (should emit metric with duration t3-t1)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageInstanceLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testInstanceID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                             // pending
	t2 := baseTime.Add(50 * time.Millisecond)  // error
	t3 := baseTime.Add(100 * time.Millisecond) // ready
	expectedDuration := t3.Sub(t1)             // t3-t1

	// t1: pending
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusPending, nil, t1)

	// t2: error
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusError, nil, t2)

	// t3: ready
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusReady, nil, t3)

	// Process create event
	ctx := context.Background()
	err := lifecycleMetrics.RecordInstanceStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testInstanceID, Created: &t3},
	})
	assert.NoError(t, err)

	// Verify metric was emitted with correct duration
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_instance_operation_latency_seconds", 1, map[string]string{
		"operation_type": "create",
		"from_status":    cdbm.InstanceStatusPending,
		"to_status":      cdbm.InstanceStatusReady,
	}, expectedDuration)
}

func Test_InstanceMetrics_Create_ReadyErrorReady(t *testing.T) {
	// Case 3: pending -> ready -> error -> ready (should NOT emit metric - duplicate ready)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageInstanceLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testInstanceID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                             // pending (initial state)
	t2 := baseTime.Add(50 * time.Millisecond)  // ready
	t3 := baseTime.Add(100 * time.Millisecond) // error
	t4 := baseTime.Add(150 * time.Millisecond) // ready (duplicate)

	// t1: pending (initial state)
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusPending, nil, t1)

	// t2: ready
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusReady, nil, t2)

	// t3: error
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusError, nil, t3)

	// t4: ready (duplicate)
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusReady, nil, t4)

	// Process create event
	ctx := context.Background()
	err := lifecycleMetrics.RecordInstanceStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testInstanceID, Created: &t4},
	})
	assert.NoError(t, err)

	// Verify NO metric was emitted (duplicate ready status)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_instance_operation_latency_seconds", 0, nil, 0)
}

// Test Instance Metrics - DELETE operations
func Test_InstanceMetrics_Delete_TerminatingOnly(t *testing.T) {
	// Case 1: terminating (should emit metric with duration now-t1)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageInstanceLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testInstanceID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // terminating started
	deleteTime := baseTime.Add(200 * time.Millisecond) // delete happened 200ms later
	expectedDuration := deleteTime.Sub(t1)

	// t1: terminating
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusTerminating, nil, t1)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordInstanceStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testInstanceID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted with correct duration
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_instance_operation_latency_seconds", 1, map[string]string{
		"operation_type": "delete",
		"from_status":    cdbm.InstanceStatusTerminating,
		"to_status":      cdbm.InstanceStatusTerminated,
	}, expectedDuration)
}

func Test_InstanceMetrics_Delete_MultipleTerminating(t *testing.T) {
	// Case 2: terminating -> terminating -> terminating (should emit metric with duration now-t1)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageInstanceLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testInstanceID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // first terminating
	t2 := baseTime.Add(50 * time.Millisecond)          // second terminating
	t3 := baseTime.Add(100 * time.Millisecond)         // third terminating
	deleteTime := baseTime.Add(300 * time.Millisecond) // delete happened
	expectedDuration := deleteTime.Sub(t1)             // should use first terminating timestamp

	// t1: terminating
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusTerminating, nil, t1)

	// t2: terminating
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusTerminating, nil, t2)

	// t3: terminating
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusTerminating, nil, t3)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordInstanceStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testInstanceID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted (should use first terminating timestamp, duration 300ms)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_instance_operation_latency_seconds", 1, map[string]string{
		"operation_type": "delete",
		"from_status":    cdbm.InstanceStatusTerminating,
		"to_status":      cdbm.InstanceStatusTerminated,
	}, expectedDuration)
}

func Test_InstanceMetrics_Delete_NoTerminating(t *testing.T) {
	// Case 3: ready (no terminating, should NOT emit metric)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageInstanceLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testInstanceID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime
	deleteTime := baseTime.Add(100 * time.Millisecond)

	// t1: ready (no terminating status)
	util.TestBuildStatusDetailWithTime(t, dbSession, testInstanceID.String(), cdbm.InstanceStatusReady, nil, t1)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordInstanceStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testInstanceID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify NO metric was emitted (no terminating status found)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_instance_operation_latency_seconds", 0, nil, 0)
}

const testSpectrumXDevice = "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC"

type spectrumXInventoryFixture struct {
	dbSession *cdb.Session
	manager   ManageInstance
	site      *cdbm.Site
	tenant    *cdbm.Tenant
	instance  *cdbm.Instance
	partition *cdbm.SpectrumXPartition
	sxaDAO    cdbm.SpectrumXAttachmentDAO
}

func newSpectrumXInventoryFixture(t *testing.T) *spectrumXInventoryFixture {
	t.Helper()
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	tnOrg := "test-tenant-org-1"
	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, []string{"FORGE_TENANT_ADMIN"})
	tenant := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)

	site := util.TestBuildSite(t, dbSession, ip, "testSite", cdbm.SiteStatusRegistered, nil, ipu)
	vpc := util.TestBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	machine := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr("mcTypeTest"), cutil.GetPtr(true), cdbm.MachineStatusReady)
	allocation := util.TestBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	instanceType := util.TestBuildInstanceType(t, dbSession, ip, site, "testInstanceType")
	_ = util.TestBuildAllocationContraints(t, dbSession, allocation, cdbm.AllocationResourceTypeInstanceType, instanceType.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	operatingSystem := util.TestBuildOperatingSystem(t, dbSession, "testOS")

	instance, err := cdbm.NewInstanceDAO(dbSession).Create(ctx, nil, cdbm.InstanceCreateInput{
		Name:                     "test-instance",
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		InstanceTypeID:           &instanceType.ID,
		VpcID:                    vpc.ID,
		MachineID:                &machine.ID,
		ControllerInstanceID:     cutil.GetPtr(uuid.New()),
		Hostname:                 cutil.GetPtr("test.com"),
		OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
		IpxeScript:               cutil.GetPtr("ipxe"),
		Status:                   cdbm.InstanceStatusReady,
		PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusBootCompleted),
		CreatedBy:                tnu.ID,
	})
	require.NoError(t, err)

	// The activity skips an Instance touched more recently than the inventory it is
	// processing, so backdate it or none of the attachment branches are reached.
	past := time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval) * 2)
	_, err = dbSession.DB.Exec("UPDATE instance SET updated = ? WHERE id = ?", past, instance.ID.String())
	require.NoError(t, err)

	partition := util.TestBuildSpectrumXPartition(t, dbSession, "east-west-net", site, tenant, cutil.GetPtr(10200), cdbm.SpectrumXPartitionStatusReady, false)

	return &spectrumXInventoryFixture{
		dbSession: dbSession,
		manager:   NewManageInstance(dbSession, testTemporalSiteClientPool(t), &tmocks.Client{}, config.GetTestConfig()),
		site:      site,
		tenant:    tenant,
		instance:  instance,
		partition: partition,
		sxaDAO:    cdbm.NewSpectrumXAttachmentDAO(dbSession),
	}
}

// attachment inserts a SpectrumXAttachment row for the fixture's Instance and Partition.
func (f *spectrumXInventoryFixture) attachment(t *testing.T, deviceInstance int, status string) *cdbm.SpectrumXAttachment {
	t.Helper()
	return util.TestBuildSpectrumXAttachment(t, f.dbSession, f.instance.ID, f.site.ID, f.partition.ID, testSpectrumXDevice, deviceInstance, cdbm.SpectrumXAttachmentTypePhysical, status, false)
}

// age backdates `updated` past the stale inventory threshold, which is the gate the
// attachment deletion sweep sits behind.
func (f *spectrumXInventoryFixture) age(t *testing.T, attachmentID uuid.UUID) {
	t.Helper()
	past := time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval) * 2)
	_, err := f.dbSession.DB.Exec("UPDATE spectrumx_attachment SET updated = ? WHERE id = ?", past, attachmentID.String())
	require.NoError(t, err)
}

func (f *spectrumXInventoryFixture) get(t *testing.T, attachmentID uuid.UUID) *cdbm.SpectrumXAttachment {
	t.Helper()
	sxa, err := f.sxaDAO.Get(context.Background(), nil, attachmentID, nil)
	require.NoError(t, err)
	return sxa
}

// reportInventory drives one UpdateInstancesInDB iteration with the given SpectrumX config
// and status, keyed on the fixture Partition's controller ID.
func (f *spectrumXInventoryFixture) reportInventory(t *testing.T, deviceInstances []uint32, statuses []*corev1.InstanceSpxAttachmentStatus, syncState corev1.SyncState) error {
	t.Helper()

	attachments := []*corev1.InstanceSpxAttachment{}
	for _, di := range deviceInstances {
		attachments = append(attachments, &corev1.InstanceSpxAttachment{
			SpxPartitionId: &corev1.SpxPartitionId{Value: f.partition.ID.String()},
			Device:         testSpectrumXDevice,
			DeviceInstance: di,
			AttachmentType: corev1.SpxAttachmentType_Physical,
		})
	}

	controllerInstance := &corev1.Instance{
		Id: &corev1.InstanceId{Value: f.instance.ControllerInstanceID.String()},
		Config: &corev1.InstanceConfig{
			Spxconfig: &corev1.InstanceSpxConfig{SpxAttachments: attachments},
		},
		Status: &corev1.InstanceStatus{
			SpxStatus: &corev1.InstanceSpxStatus{
				AttachmentStatuses: statuses,
				ConfigsSynced:      syncState,
			},
		},
	}

	_, err := f.manager.UpdateInstancesInDB(context.Background(), f.site.ID, &corev1.InstanceInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Timestamp:       timestamppb.Now(),
		Instances:       []*corev1.Instance{controllerInstance},
		InventoryPage: &corev1.InventoryPage{
			TotalPages: 1, CurrentPage: 1, PageSize: 1, TotalItems: 1,
			ItemIds: []string{f.instance.ControllerInstanceID.String()},
		},
	})
	return err
}

// TestUpdateInstancesInDB_SpectrumXAttachmentReconciliation covers the attachment branches
// the Instance inventory activity owns. Each case reloads the persisted row after the
// iteration, because status, MAC and IP are the only observable effects.
func TestUpdateInstancesInDB_SpectrumXAttachmentReconciliation(t *testing.T) {
	t.Run("a synced attachment records its MAC and IP and becomes Ready", func(t *testing.T) {
		fx := newSpectrumXInventoryFixture(t)
		sxa := fx.attachment(t, 0, cdbm.SpectrumXAttachmentStatusPending)

		require.NoError(t, fx.reportInventory(t, []uint32{0}, []*corev1.InstanceSpxAttachmentStatus{
			{MacAddr: cutil.GetPtr("00:11:22:33:44:55"), IpAddress: cutil.GetPtr("192.0.2.15")},
		}, corev1.SyncState_SYNCED))

		persisted := fx.get(t, sxa.ID)
		assert.Equal(t, cdbm.SpectrumXAttachmentStatusReady, persisted.Status)
		require.NotNil(t, persisted.MacAddress)
		assert.Equal(t, "00:11:22:33:44:55", *persisted.MacAddress)
		require.NotNil(t, persisted.IPAddress)
		assert.Equal(t, "192.0.2.15", *persisted.IPAddress)
	})

	// An unsynced config means the Site has accepted the attachment but not applied it,
	// so the addresses can be recorded while the status stays Pending.
	t.Run("an unsynced attachment records addresses but stays Pending", func(t *testing.T) {
		fx := newSpectrumXInventoryFixture(t)
		sxa := fx.attachment(t, 0, cdbm.SpectrumXAttachmentStatusPending)

		require.NoError(t, fx.reportInventory(t, []uint32{0}, []*corev1.InstanceSpxAttachmentStatus{
			{MacAddr: cutil.GetPtr("00:11:22:33:44:55")},
		}, corev1.SyncState_PENDING))

		persisted := fx.get(t, sxa.ID)
		assert.Equal(t, cdbm.SpectrumXAttachmentStatusPending, persisted.Status)
		require.NotNil(t, persisted.MacAddress)
		assert.Equal(t, "00:11:22:33:44:55", *persisted.MacAddress)
	})

	// Config and status attachments are index-aligned by Core. A truncated status list
	// must not shift another attachment's MAC onto this row.
	t.Run("a status list shorter than the config list leaves the unmatched row alone", func(t *testing.T) {
		fx := newSpectrumXInventoryFixture(t)
		first := fx.attachment(t, 0, cdbm.SpectrumXAttachmentStatusPending)
		second := fx.attachment(t, 1, cdbm.SpectrumXAttachmentStatusPending)

		require.NoError(t, fx.reportInventory(t, []uint32{0, 1}, []*corev1.InstanceSpxAttachmentStatus{
			{MacAddr: cutil.GetPtr("00:11:22:33:44:55"), IpAddress: cutil.GetPtr("192.0.2.15")},
		}, corev1.SyncState_SYNCED))

		firstPersisted := fx.get(t, first.ID)
		require.NotNil(t, firstPersisted.MacAddress)
		assert.Equal(t, "00:11:22:33:44:55", *firstPersisted.MacAddress)

		secondPersisted := fx.get(t, second.ID)
		assert.Nil(t, secondPersisted.MacAddress, "the second attachment has no reported status, so it must stay empty")
		assert.Nil(t, secondPersisted.IPAddress)
	})

	t.Run("a Deleting attachment is removed once the Site config no longer carries it", func(t *testing.T) {
		fx := newSpectrumXInventoryFixture(t)
		sxa := fx.attachment(t, 0, cdbm.SpectrumXAttachmentStatusDeleting)
		fx.age(t, sxa.ID)

		require.NoError(t, fx.reportInventory(t, nil, nil, corev1.SyncState_SYNCED))

		_, err := fx.sxaDAO.Get(context.Background(), nil, sxa.ID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})

	// A row marked Deleting moments ago has not had time to reach the Site, so this
	// inventory cycle may predate the request.
	t.Run("a freshly marked Deleting attachment survives the cycle", func(t *testing.T) {
		fx := newSpectrumXInventoryFixture(t)
		sxa := fx.attachment(t, 0, cdbm.SpectrumXAttachmentStatusDeleting)

		require.NoError(t, fx.reportInventory(t, nil, nil, corev1.SyncState_SYNCED))

		assert.Equal(t, cdbm.SpectrumXAttachmentStatusDeleting, fx.get(t, sxa.ID).Status)
	})

	// A Ready attachment the Site still reports must not be swept just because another
	// attachment on the same Instance is being deleted.
	t.Run("deleting one attachment leaves the others in place", func(t *testing.T) {
		fx := newSpectrumXInventoryFixture(t)
		keep := fx.attachment(t, 0, cdbm.SpectrumXAttachmentStatusReady)
		remove := fx.attachment(t, 1, cdbm.SpectrumXAttachmentStatusDeleting)
		fx.age(t, remove.ID)

		require.NoError(t, fx.reportInventory(t, []uint32{0}, []*corev1.InstanceSpxAttachmentStatus{
			{MacAddr: cutil.GetPtr("00:11:22:33:44:55")},
		}, corev1.SyncState_SYNCED))

		assert.Equal(t, cdbm.SpectrumXAttachmentStatusReady, fx.get(t, keep.ID).Status)

		_, err := fx.sxaDAO.Get(context.Background(), nil, remove.ID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

		remaining, total, err := fx.sxaDAO.GetAll(context.Background(), nil, cdbm.SpectrumXAttachmentFilterInput{
			InstanceIDs: []uuid.UUID{fx.instance.ID},
		}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
		require.NoError(t, err)
		assert.Equal(t, 1, total)
		require.Len(t, remaining, 1)
		assert.Equal(t, keep.ID, remaining[0].ID)
	})
}
