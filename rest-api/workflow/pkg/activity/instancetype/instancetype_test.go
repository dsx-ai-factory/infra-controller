// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instancetype

import (
	"context"
	"fmt"
	"reflect"
	"slices"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	"os"

	tmocks "go.temporal.io/sdk/mocks"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"go.temporal.io/sdk/testsuite"
)

// testTemporalSiteClientPool Building site client pool
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

func testInstanceTypeInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testInstanceTypeSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InstanceType)(nil))
	assert.Nil(t, err)
}

// testInstanceTypeSiteBuildInfrastructureProvider Building Infra Provider in DB
func testInstanceTypeSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)

	ip, err := ipDAO.Create(context.Background(), nil, cdbm.InfrastructureProviderCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Provider"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return ip
}

// testInstanceTypeBuildSite Building Site in DB
func testInstanceTypeBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
	stDAO := cdbm.NewSiteDAO(dbSession)

	st, err := stDAO.Create(context.Background(), nil, cdbm.SiteCreateInput{
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test Site"),
		Description:                 cutil.GetPtr("Test Site Description"),
		Org:                         ip.Org,
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:           false,
		IsSerialConsoleEnabled:      false,
		Status:                      cdbm.SiteStatusPending,
		CreatedBy:                   user.ID,
	})
	assert.Nil(t, err)

	return st
}

// testInstanceTypeBuildUser Building User in DB
func testInstanceTypeBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	u, err := uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
		AuxiliaryID: nil,
		StarfleetID: &starfleetID,
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: cdbm.OrgData{
			org: cdbm.Org{
				ID:      123,
				Name:    org,
				OrgType: "ENTERPRISE",
				Roles:   roles,
			},
		},
	})
	assert.Nil(t, err)

	return u
}

// testInstanceTypeBuildInstanceType Building InstanceType in DB
func testInstanceTypeBuildInstanceType(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, st *cdbm.Site, user *cdbm.User, status string) *cdbm.InstanceType {
	instanceTypeDAO := cdbm.NewInstanceTypeDAO(dbSession)

	instanceType, err := instanceTypeDAO.Create(context.Background(), nil, cdbm.InstanceTypeCreateInput{Name: name, Description: cutil.GetPtr("description"), InfrastructureProviderID: ip.ID, SiteID: &st.ID, Status: status, CreatedBy: user.ID})
	assert.Nil(t, err)

	return instanceType
}

func testInstanceTypeBuildMachineCapability(t *testing.T, dbSession *cdb.Session, iID *uuid.UUID, typ cdbm.MachineCapabilityType, name string, capacity *string, count *int, deviceType *cdbm.MachineCapabilityDeviceType) *cdbm.MachineCapability {
	mc := &cdbm.MachineCapability{
		ID:             uuid.New(),
		InstanceTypeID: iID,
		Type:           typ,
		Name:           name,
		Capacity:       capacity,
		Count:          count,
		DeviceType:     deviceType,
		Created:        cdb.GetCurTime(),
		Updated:        cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(mc).Exec(context.Background())
	assert.Nil(t, err)
	return mc
}

func TestManageInstanceType_UpdateInstanceTypesInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()

	testInstanceTypeSetupSchema(t, dbSession)

	instanceTypeDAO := cdbm.NewInstanceTypeDAO(dbSession)
	macCapDAO := cdbm.NewMachineCapabilityDAO(dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := testInstanceTypeBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := testInstanceTypeSiteBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := testInstanceTypeBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)

	st := testInstanceTypeBuildSite(t, dbSession, ip, "test-site", ipu)
	st2 := testInstanceTypeBuildSite(t, dbSession, ip, "test-site-2", ipu)
	st3 := testInstanceTypeBuildSite(t, dbSession, ip, "test-site-3", ipu)

	instanceType1 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-1", ip, st, tnu, cdbm.InstanceTypeStatusReady)

	instanceType2 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-2", ip, st, tnu, cdbm.InstanceTypeStatusReady)

	instanceType3 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-3", ip, st, tnu, cdbm.InstanceTypeStatusError)

	instanceType4 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-4", ip, st, tnu, cdbm.InstanceTypeStatusError)

	instanceType5 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-5", ip, st, tnu, cdbm.InstanceTypeStatusError)

	_, err := dbSession.DB.Exec("UPDATE instance_type SET deleted = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), instanceType5.ID.String())
	assert.NoError(t, err)

	instanceType6 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-6", ip, st, tnu, cdbm.InstanceTypeStatusError)

	_, err = dbSession.DB.Exec("UPDATE instance_type SET deleted = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), instanceType6.ID.String())
	assert.NoError(t, err)

	instanceType7 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-7", ip, st, tnu, cdbm.InstanceTypeStatusReady)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance_type SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instanceType7.ID.String())
	assert.NoError(t, err)

	instanceType8 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-8", ip, st, tnu, cdbm.InstanceTypeStatusReady)

	instanceType9 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-9", ip, st, tnu, cdbm.InstanceTypeStatusReady)

	instanceType10 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-10", ip, st, tnu, cdbm.InstanceTypeStatusError)

	instanceType11 := testInstanceTypeBuildInstanceType(t, dbSession, "test-instanceType-11", ip, st, tnu, cdbm.InstanceTypeStatusReady)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE instance_type SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), instanceType11.ID.String())
	assert.NoError(t, err)

	instanceType8, err = instanceTypeDAO.Update(ctx, nil, cdbm.InstanceTypeUpdateInput{ID: instanceType8.ID, Status: cutil.GetPtr(cdbm.InstanceTypeStatusError)})
	assert.NoError(t, err)

	siteSharedTypesMap := map[string]*corev1.InstanceType{}

	// Build InstanceType inventory that is paginated
	// Generate data for 34 InstanceTypes reported from Site Agent while Cloud has 38 InstanceTypes
	// One of the InstanceTypes on site doesn't exist in cloud.
	pagedInstanceTypes := []*cdbm.InstanceType{}
	pagedInvIds := []string{}
	for i := range 38 {
		instanceType := testInstanceTypeBuildInstanceType(t, dbSession, fmt.Sprintf("test-instanceType-paged-%d", i), ip, st3, tnu, cdbm.InstanceTypeStatusReady)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE instance_type SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), instanceType.ID.String())
		assert.NoError(t, err)
		pagedInstanceTypes = append(pagedInstanceTypes, instanceType)
	}

	// TODO: Need tests for instance type known to site and cloud where:
	// site has capabilities that cloud does not.  Cloud should get them.
	// Cloud has caps that site does not.  Cloud should lose them.

	pagedCtrlInstanceTypes := []*corev1.InstanceType{}
	for i := range 34 {

		ctrlInstanceType := &corev1.InstanceType{
			Id: pagedInstanceTypes[i].ID.String(),
			Metadata: &corev1.Metadata{
				Name: pagedInstanceTypes[i].Name,
			},
		}

		pagedCtrlInstanceTypes = append(pagedCtrlInstanceTypes, ctrlInstanceType)
		pagedInvIds = append(pagedInvIds, ctrlInstanceType.Id)

		siteSharedTypesMap[pagedInstanceTypes[i].ID.String()] = ctrlInstanceType
	}

	cloudUnknownType := &cdbm.InstanceType{ID: uuid.New(), Name: "unknown-to-cloud"}

	count := uint32(16)
	sharedNetworkCapabilityName := "ConnectX-8"
	spectrumXDeviceType := corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPECTRUM_X
	sameNameNetworkCapabilities := []*corev1.InstanceTypeMachineCapabilityFilterAttributes{
		{
			CapabilityType: corev1.MachineCapabilityType_CAP_TYPE_NETWORK,
			Name:           &sharedNetworkCapabilityName,
			Count:          &count,
		},
		{
			CapabilityType: corev1.MachineCapabilityType_CAP_TYPE_NETWORK,
			Name:           &sharedNetworkCapabilityName,
			Count:          &count,
			DeviceType:     &spectrumXDeviceType,
		},
	}
	staleGenericCount := 8
	staleSpectrumXCount := 4
	spectrumXDBDeviceType := cdbm.MachineCapabilityDeviceTypeSpectrumX
	testInstanceTypeBuildMachineCapability(t, dbSession, &pagedInstanceTypes[3].ID, cdbm.MachineCapabilityTypeNetwork, sharedNetworkCapabilityName, nil, &staleGenericCount, nil)
	testInstanceTypeBuildMachineCapability(t, dbSession, &pagedInstanceTypes[3].ID, cdbm.MachineCapabilityTypeNetwork, sharedNetworkCapabilityName, nil, &staleSpectrumXCount, &spectrumXDBDeviceType)

	siteKnownTypeCapabilities := []*corev1.InstanceTypeMachineCapabilityFilterAttributes{
		{
			CapabilityType: corev1.MachineCapabilityType_CAP_TYPE_CPU,
			Name:           cutil.GetPtr("xeon"),
			Count:          &count,
		},
	}
	siteKnownTypeCapabilities = append(siteKnownTypeCapabilities, sameNameNetworkCapabilities...)
	siteKnownType := &corev1.InstanceType{Id: cloudUnknownType.ID.String(), Metadata: &corev1.Metadata{
		Name: cloudUnknownType.Name,
	}, Attributes: &corev1.InstanceTypeAttributes{DesiredCapabilities: siteKnownTypeCapabilities}}

	// Add one more InstanceType to site that cloud won't know about
	pagedCtrlInstanceTypes = append(pagedCtrlInstanceTypes, siteKnownType)
	pagedInvIds = append(pagedInvIds, siteKnownType.Id)
	siteSharedTypesMap[siteKnownType.Id] = siteKnownType

	// Add some capability known to cloud but not site to
	// an instance type known to both cloud and site
	mc1 := testInstanceTypeBuildMachineCapability(t, dbSession, &pagedInstanceTypes[0].ID, cdbm.MachineCapabilityTypeCPU, "Intel(R) Xeon(R) Gold 6354 CPU @ 3.00GHz", nil, cutil.GetPtr(1), nil)
	assert.NotNil(t, mc1)

	// Make sure the "site" version won't match the cloud one.
	pagedCtrlInstanceTypes[0].Version = "anything-that-does-not-match"

	// Add a capability known to site but not cloud to
	// an instance type known to both cloud and site.
	pagedCtrlInstanceTypes[1].Attributes = &corev1.InstanceTypeAttributes{DesiredCapabilities: []*corev1.InstanceTypeMachineCapabilityFilterAttributes{
		{
			CapabilityType: corev1.MachineCapabilityType_CAP_TYPE_CPU,
			Name:           cutil.GetPtr("xeon"),
			Count:          &count,
		},
	}}

	mc2 := testInstanceTypeBuildMachineCapability(t, dbSession, &pagedInstanceTypes[2].ID, cdbm.MachineCapabilityTypeNetwork, "MT43245 BlueField-3 integrated ConnectX-7 network controller", nil, cutil.GetPtr(2), nil)
	assert.NotNil(t, mc2)

	// Make sure the "site" version won't match the cloud one.
	pagedCtrlInstanceTypes[1].Version = "anything-that-does-not-match"

	// Add a capability known to site but not cloud to
	// an instance type known to both cloud and site.
	deviceType := corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU
	pagedCtrlInstanceTypes[2].Attributes = &corev1.InstanceTypeAttributes{DesiredCapabilities: []*corev1.InstanceTypeMachineCapabilityFilterAttributes{
		{
			CapabilityType: corev1.MachineCapabilityType_CAP_TYPE_NETWORK,
			Name:           cutil.GetPtr("MT43245 BlueField-3 integrated ConnectX-7 network controller"),
			Count:          &count,
			DeviceType:     &deviceType,
		},
	}}

	// Exercise the update path with two capabilities that share a type and name
	// but have distinct device-type identities.
	pagedCtrlInstanceTypes[3].Attributes = &corev1.InstanceTypeAttributes{
		DesiredCapabilities: sameNameNetworkCapabilities,
	}
	pagedCtrlInstanceTypes[3].Version = "anything-that-does-not-match"

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	// Mock UpdateInstanceType workflow from site-agent
	wrun := &tmocks.WorkflowRun{}
	wid := "test-workflow-id"
	wrun.On("GetID").Return(wid)

	mtc1 := &tmocks.Client{}
	mtc1.Mock.On("ExecuteWorkflow", context.Background(), mock.Anything, "UpdateInstanceType", mock.Anything).Return(wrun, nil)

	// The ones that should, for now, be created on site to sync up
	// site and cloud as we transition SOT to site.
	mtc1.Mock.On("ExecuteWorkflow", context.Background(), mock.Anything, "CreateInstanceType", mock.Anything).Return(wrun, nil)

	type fields struct {
		dbSession        *cdb.Session
		siteClientPool   *sc.ClientPool
		clientPoolClient *tmocks.Client
		env              *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                   context.Context
		siteID                uuid.UUID
		instanceTypeInventory *corev1.InstanceTypeInventory
	}

	tests := []struct {
		name                    string
		fields                  fields
		args                    args
		readyInstanceTypes      []*cdbm.InstanceType
		deletedInstanceTypes    []*cdbm.InstanceType
		expectCapabilitiesMatch bool
		wantErr                 bool
	}{
		{
			name: "test InstanceType inventory processing error, non-existent Site",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				instanceTypeInventory: &corev1.InstanceTypeInventory{
					InstanceTypes: []*corev1.InstanceType{},
				},
			},
			wantErr: true,
		},
		{
			name: "test InstanceType inventory processing success",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				instanceTypeInventory: &corev1.InstanceTypeInventory{
					InstanceTypes: []*corev1.InstanceType{
						{
							Id:       instanceType1.ID.String(),
							Metadata: &corev1.Metadata{Name: instanceType1.ID.String()},
						},
						{
							Id:       instanceType2.ID.String(),
							Metadata: &corev1.Metadata{Name: instanceType2.ID.String()},
						},
						{
							Id:       instanceType3.ID.String(),
							Metadata: &corev1.Metadata{Name: instanceType3.ID.String()},
						},
						{
							Id:       instanceType4.ID.String(),
							Metadata: &corev1.Metadata{Name: instanceType4.ID.String()},
						},
						{
							Id:       instanceType8.ID.String(),
							Metadata: &corev1.Metadata{Name: instanceType8.ID.String()},
						},
						{
							Id:       uuid.NewString(),
							Metadata: &corev1.Metadata{Name: instanceType9.ID.String()},
						},
						{
							Id:       uuid.NewString(),
							Metadata: &corev1.Metadata{Name: instanceType10.ID.String()},
						},
					},
				},
			},
			// For now, nothing should be deleted.
			//deletedInstanceTypes: []*cdbm.InstanceType{instanceType5, instanceType6},
			wantErr: false,
		},
		{
			name: "test paged InstanceType inventory processing, empty inventory",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				instanceTypeInventory: &corev1.InstanceTypeInventory{
					InstanceTypes:   []*corev1.InstanceType{},
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  0,
						PageSize:    25,
						TotalItems:  0,
						ItemIds:     []string{},
					},
				},
			},
		},
		{
			name: "test paged Instance inventory processing, first page",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				instanceTypeInventory: &corev1.InstanceTypeInventory{
					InstanceTypes: pagedCtrlInstanceTypes[0:10],
					Timestamp:     timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  35,
						ItemIds:     pagedInvIds[0:35],
					},
				},
			},
			readyInstanceTypes:      pagedInstanceTypes[0:10],
			expectCapabilitiesMatch: true,
		},
		{
			name: "test paged Instance inventory processing, last page",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				instanceTypeInventory: &corev1.InstanceTypeInventory{
					InstanceTypes: pagedCtrlInstanceTypes[30:35],
					Timestamp:     timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  35,
						ItemIds:     pagedInvIds[0:35],
					},
				},
			},
			readyInstanceTypes:      append(pagedInstanceTypes[30:34], cloudUnknownType),
			expectCapabilitiesMatch: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageInstanceType{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			mv.siteClientPool.IDClientMap[tt.args.siteID.String()] = tt.fields.clientPoolClient

			cwu.TestInventoryAgeUpdatedTimestamp(tt.args.ctx, t, dbSession, (*cdbm.InstanceType)(nil))

			err := mv.UpdateInstanceTypesInDB(tt.args.ctx, tt.args.siteID, tt.args.instanceTypeInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			for _, instanceType := range tt.deletedInstanceTypes {
				_, err := instanceTypeDAO.GetByID(ctx, nil, instanceType.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("InstanceType %s (%s) should have been deleted", instanceType.Name, instanceType.ID))
			}

			for _, instanceType := range tt.readyInstanceTypes {
				it, err := instanceTypeDAO.GetByID(ctx, nil, instanceType.ID, nil)
				require.Nil(t, err, fmt.Sprintf("InstanceType %s (%s) should exist and not return err", instanceType.Name, instanceType.ID))
				require.NotNil(t, it, fmt.Sprintf("InstanceType %s (%s) should exist", instanceType.Name, instanceType.ID))

				if tt.expectCapabilitiesMatch {
					cloudCaps, tot, err := macCapDAO.GetAll(ctx, nil, nil, []uuid.UUID{instanceType.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil)
					assert.Nil(t, err, err)

					slices.SortFunc(cloudCaps, func(a, b cdbm.MachineCapability) int {
						if a.Index < b.Index {
							return -1
						}

						if a.Index > b.Index {
							return 1
						}

						return 0
					})

					siteInstanceType := siteSharedTypesMap[instanceType.ID.String()]
					require.NotNil(t, siteInstanceType)

					siteCaps := siteInstanceType.Attributes.GetDesiredCapabilities()

					// The set of both should be the same size.
					if assert.Equal(t, tot, len(siteCaps)) {
						for i := range tot {
							assert.Equal(t, cloudCaps[i].Name, *siteCaps[i].Name)
							if cloudCaps[i].Name == sharedNetworkCapabilityName {
								assert.Equal(t, int(siteCaps[i].GetCount()), *cloudCaps[i].Count)
							}
							if cloudCaps[i].Type == cdbm.MachineCapabilityTypeNetwork && cloudCaps[i].DeviceType != nil {
								var protoDeviceType corev1.MachineCapabilityDeviceType
								switch *cloudCaps[i].DeviceType {
								case cdbm.MachineCapabilityDeviceTypeDPU:
									protoDeviceType = corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU
								case cdbm.MachineCapabilityDeviceTypeNVLink:
									protoDeviceType = corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_NVLINK
								case cdbm.MachineCapabilityDeviceTypeSpectrumX:
									protoDeviceType = corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPECTRUM_X
								default:
									t.Fatalf("unsupported DeviceType %q in test fixture", *cloudCaps[i].DeviceType)
								}
								assert.Equal(t, *siteCaps[i].DeviceType, protoDeviceType)
							}
						}
					}
				}
			}
		})
	}
}

func TestNewManageInstanceType(t *testing.T) {
	type args struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
	}

	dbSession := &cdb.Session{}
	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := config.NewConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)
	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want ManageInstanceType
	}{
		{
			name: "test new ManageInstanceType instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageInstanceType{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageInstanceType(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageInstanceType() = %v, want %v", got, tt.want)
			}
		})
	}
}
