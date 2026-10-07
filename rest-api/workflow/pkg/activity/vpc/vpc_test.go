// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpc

import (
	"context"
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	"os"

	"go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	"go.temporal.io/sdk/testsuite"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	"github.com/prometheus/client_golang/prometheus"
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

func testVPCInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testVPCSetupSchema(t *testing.T, dbSession *cdb.Session) {
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
	// create Network Security Group table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create NVLink Logical Partition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
}

// testVPCSiteBuildInfrastructureProvider Building Infra Provider in DB
func testVPCSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
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

// testVPCBuildSite Building Site in DB
func testVPCBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
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

// testVPCBuildTenant Building Tenant in DB
func testVPCBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
	tnDAO := cdbm.NewTenantDAO(dbSession)

	tn, err := tnDAO.Create(context.Background(), nil, cdbm.TenantCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Tenant"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return tn
}

// testVPCBuildUser Building User in DB
func testVPCBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
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

// testVPCSiteBuildAllocation Building Site Allocation in DB
func testVPCSiteBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
	alDAO := cdbm.NewAllocationDAO(dbSession)

	createInput := cdbm.AllocationCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("Test Allocation Description"),
		InfrastructureProviderID: st.InfrastructureProviderID,
		TenantID:                 tn.ID,
		SiteID:                   st.ID,
		Status:                   cdbm.AllocationStatusPending,
		CreatedBy:                user.ID,
	}
	al, err := alDAO.Create(context.Background(), nil, createInput)
	assert.Nil(t, err)

	return al
}

// testVPCBuildVPC Building VPC in DB
func testVPCBuildVPC(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, tn *cdbm.Tenant, st *cdbm.Site, networkVirtualizationType *string, ct *uuid.UUID, lb map[string]string, user *cdbm.User, status string) *cdbm.Vpc {
	vpcDAO := cdbm.NewVpcDAO(dbSession)

	input := cdbm.VpcCreateInput{
		Name:                      name,
		Description:               cutil.GetPtr("Test VPC"),
		Org:                       st.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: networkVirtualizationType,
		ControllerVpcID:           ct,
		Labels:                    lb,
		Status:                    status,
		CreatedBy:                 *user,
	}

	vpc, err := vpcDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

func TestManageVpc_UpdateVpcsInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()

	testVPCSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := testVPCBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := testVPCBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := testVPCBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)

	st := testVPCBuildSite(t, dbSession, ip, "test-site", ipu)
	st2 := testVPCBuildSite(t, dbSession, ip, "test-site-2", ipu)
	st3 := testVPCBuildSite(t, dbSession, ip, "test-site-3", ipu)

	vpc1 := testVPCBuildVPC(t, dbSession, "test-vpc-1", ip, tn, st, cutil.GetPtr(""), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusProvisioning)

	vpc2 := testVPCBuildVPC(t, dbSession, "test-vpc-2", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusProvisioning)

	vpc3 := testVPCBuildVPC(t, dbSession, "test-vpc-3", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusDeleting)

	vpc4 := testVPCBuildVPC(t, dbSession, "test-vpc-4", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusDeleting)

	vpc5 := testVPCBuildVPC(t, dbSession, "test-vpc-5", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusDeleting)

	vpc6 := testVPCBuildVPC(t, dbSession, "test-vpc-6", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, nil, tnu, cdbm.VpcStatusDeleting)

	vpc7 := testVPCBuildVPC(t, dbSession, "test-vpc-7", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	// Set created earlier than the inventory receipt interval
	_, err := dbSession.DB.Exec("UPDATE vpc SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), vpc7.ID.String())
	assert.NoError(t, err)

	vpc8 := testVPCBuildVPC(t, dbSession, "test-vpc-8", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)

	vpc9 := testVPCBuildVPC(t, dbSession, "test-vpc-9", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, nil, tnu, cdbm.VpcStatusProvisioning)

	vpc10 := testVPCBuildVPC(t, dbSession, "test-vpc-10", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, nil, tnu, cdbm.VpcStatusDeleting)

	vpc11 := testVPCBuildVPC(t, dbSession, "test-vpc-11", ip, tn, st, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE vpc SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), vpc11.ID.String())
	assert.NoError(t, err)

	vpcDAO := cdbm.NewVpcDAO(dbSession)
	vpc8, err = vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{VpcID: vpc8.ID, Status: cutil.GetPtr(cdbm.VpcStatusError), IsMissingOnSite: cutil.GetPtr(true)})
	assert.NoError(t, err)
	vpc2, err = vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{
		VpcID:          vpc2.ID,
		RoutingProfile: cutil.GetPtr("EXTERNAL"),
		SlaacEnabled:   cutil.GetPtr(true),
	})
	assert.NoError(t, err)
	vpc3, err = vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{
		VpcID:        vpc3.ID,
		SlaacEnabled: cutil.GetPtr(true),
	})
	assert.NoError(t, err)
	// Seed cached profile data so an inventory omission must actively clear it.
	vpc2, err = vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{
		VpcID: vpc2.ID,
		RoutingProfileOverrides: &cdbm.VpcRoutingProfileOverrides{
			LeakDefaultRouteFromUnderlay: cutil.GetPtr(true),
		},
		EffectiveRoutingProfile: &cdbm.VpcEffectiveRoutingProfile{
			LeakDefaultRouteFromUnderlay: true,
			Internal:                     true,
			AccessTier:                   3,
		},
	})
	assert.NoError(t, err)

	vpc12 := testVPCBuildVPC(t, dbSession, "test-vpc-12", ip, tn, st, nil, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	// Set propagation details for VPC21.
	// We'll expect these to be cleared later.
	vpc12.NetworkSecurityGroupPropagationDetails = &cdbm.NetworkSecurityGroupPropagationDetails{
		NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{},
	}
	cwu.TestUpdateVPC(t, dbSession, vpc12)

	vpc13 := testVPCBuildVPC(t, dbSession, "test-vpc-13", ip, tn, st, nil, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)

	// Build real NSG rows so reconciliation exercises the VPC foreign-key column.
	networkSecurityGroupA := util.TestBuildNetworkSecurityGroup(t, dbSession, "test-nsg-a", st, tn, cdbm.NetworkSecurityGroupStatusReady, tnu)
	networkSecurityGroupB := util.TestBuildNetworkSecurityGroup(t, dbSession, "test-nsg-b", st, tn, cdbm.NetworkSecurityGroupStatusReady, tnu)
	vpc14 := testVPCBuildVPC(t, dbSession, "test-vpc-14", ip, tn, st, nil, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpc15 := testVPCBuildVPC(t, dbSession, "test-vpc-15", ip, tn, st, nil, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpc16 := testVPCBuildVPC(t, dbSession, "test-vpc-16", ip, tn, st, nil, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	reportedPowerResourceGroup := "reported-power-resource-group"
	existingPowerResourceGroup := "existing-power-resource-group"

	// Seed the replace and clear cases with the first NSG association.
	vpc15, err = vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{VpcID: vpc15.ID, NetworkSecurityGroupID: cutil.GetPtr(networkSecurityGroupA.ID)})
	require.NoError(t, err)
	vpc16, err = vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{VpcID: vpc16.ID, NetworkSecurityGroupID: cutil.GetPtr(networkSecurityGroupA.ID)})
	require.NoError(t, err)
	vpc15, err = vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{VpcID: vpc15.ID, PowerResourceGroup: &existingPowerResourceGroup})
	require.NoError(t, err)

	// Build VPC inventory that is paginated
	// Generate data for 34 VPCs reported from Site Agent while Cloud has 38 VPCs
	pagedVpcs := []*cdbm.Vpc{}
	pagedInvIds := []string{}
	labels := map[string]string{}
	for i := 0; i < 38; i++ {

		// Making labels mismatch
		if i == 1 {
			labels = map[string]string{
				"west1": "gpu",
			}
		}

		vpc := testVPCBuildVPC(t, dbSession, fmt.Sprintf("test-vpc-paged-%d", i), ip, tn, st3, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(uuid.New()), labels, tnu, cdbm.VpcStatusReady)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE vpc SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), vpc.ID.String())
		assert.NoError(t, err)
		pagedVpcs = append(pagedVpcs, vpc)
		pagedInvIds = append(pagedInvIds, vpc.ControllerVpcID.String())
	}

	pagedCtrlVpcs := []*corev1.Vpc{}
	for i := 0; i < 34; i++ {
		ctrlVpc := &corev1.Vpc{
			Id:   &corev1.VpcId{Value: pagedVpcs[i].ControllerVpcID.String()},
			Name: pagedVpcs[i].Name,
			Config: &corev1.VpcConfig{
				Vni: util.GetUint32Ptr(uint32(i)),
			},
			Status: &corev1.VpcStatus{
				Vni: util.GetUint32Ptr(uint32(i)),
			},
		}

		if i == 1 {
			ctrlVpc.Metadata = &corev1.Metadata{
				Name:        "Test VPC",
				Description: "Test description",
				Labels: []*corev1.Label{
					{
						Key:   "west1",
						Value: cutil.GetPtr("gpu1"),
					},
				},
			}
		}
		pagedCtrlVpcs = append(pagedCtrlVpcs, ctrlVpc)
	}

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	// Mock UpdateVpc workflow from site-agent
	wrun := &tmocks.WorkflowRun{}
	wid := "test-workflow-id"
	wrun.On("GetID").Return(wid)

	workflowOptions1 := client.StartWorkflowOptions{
		ID:        "site-vpc-update-metadata-" + pagedVpcs[1].ID.String(),
		TaskQueue: queue.SiteTaskQueue,
	}

	mtc1 := &tmocks.Client{}
	mtc1.Mock.On("ExecuteWorkflow", context.Background(), workflowOptions1, "UpdateVPC", mock.Anything).Return(wrun, nil)

	nwvt := corev1.VpcVirtualizationType_FNN
	evt := corev1.VpcVirtualizationType_ETHERNET_VIRTUALIZER

	type fields struct {
		dbSession        *cdb.Session
		siteClientPool   *sc.ClientPool
		clientPoolClient *tmocks.Client
		env              *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx          context.Context
		siteID       uuid.UUID
		vpcInventory *corev1.VPCInventory
	}

	tests := []struct {
		name                              string
		fields                            fields
		args                              args
		updatedVpc                        *cdbm.Vpc
		readyVpcs                         []*cdbm.Vpc
		deletedVpcs                       []*cdbm.Vpc
		missingVpcs                       []*cdbm.Vpc
		restoredVpcs                      []*cdbm.Vpc
		unpairedVpcs                      []*cdbm.Vpc
		nsgPropagationDetailsClearedVpcs  []*cdbm.Vpc
		networkVirtualizationUpdatedVpcs  []*cdbm.Vpc
		ethernetVirtualizationUpdatedVpcs []*cdbm.Vpc
		routingProfileUpdatedVpcs         []*cdbm.Vpc
		routingProfileClearedVpcs         []*cdbm.Vpc
		routingProfileStateUpdatedVpc     *cdbm.Vpc
		routingProfileStateClearedVpc     *cdbm.Vpc
		expectedSlaacEnabled              map[uuid.UUID]bool
		expectedNetworkSecurityGroupIDs   map[uuid.UUID]*string
		expectedPowerResourceGroups       map[uuid.UUID]*string
		readyStatusDetailVpcs             []*cdbm.Vpc
		requiredMetadataUpdate            bool
		metadataVpcUpdate                 *cdbm.Vpc
		wantErr                           bool
	}{
		{
			name: "test VPC inventory processing error, non-existent Site",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				vpcInventory: &corev1.VPCInventory{
					Vpcs: []*corev1.Vpc{},
				},
			},
			wantErr: true,
		},
		{
			name: "test VPC inventory processing success",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				vpcInventory: &corev1.VPCInventory{
					NetworkSecurityGroupPropagations: []*corev1.NetworkSecurityGroupPropagationObjectStatus{
						&corev1.NetworkSecurityGroupPropagationObjectStatus{
							Id:      vpc1.ID.String(),
							Status:  corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL,
							Details: cutil.GetPtr("nothing to see here"),
						},
					},
					Vpcs: []*corev1.Vpc{
						{
							Id:   &corev1.VpcId{Value: vpc1.ID.String()},
							Name: vpc1.ID.String(),
							Config: &corev1.VpcConfig{
								NetworkVirtualizationType: &nwvt,
								SlaacEnabled:              cutil.GetPtr(true),
								RoutingProfileType:        cutil.GetPtr("INTERNAL"),
								RoutingProfileOverrides: &corev1.VpcRoutingProfileOverrides{
									LeakDefaultRouteFromUnderlay: cutil.GetPtr(false),
									AllowedAnycastPrefixes: &corev1.PrefixFilterPolicyEntries{
										Values: []*corev1.PrefixFilterPolicyEntry{{Prefix: "192.0.2.1/24"}},
									},
								},
							},
							Status: &corev1.VpcStatus{
								EffectiveRoutingProfile: &corev1.VpcEffectiveRoutingProfile{
									LeakDefaultRouteFromUnderlay: true,
									Internal:                     true,
									AccessTier:                   8,
								},
							},
						},
						{
							Id:   &corev1.VpcId{Value: vpc2.ControllerVpcID.String()},
							Name: vpc2.ID.String(),
						},
						{
							Id:   &corev1.VpcId{Value: vpc3.ControllerVpcID.String()},
							Name: vpc3.ID.String(),
							Config: &corev1.VpcConfig{
								SlaacEnabled: cutil.GetPtr(false),
							},
						},
						{
							Id:   &corev1.VpcId{Value: vpc4.ControllerVpcID.String()},
							Name: vpc4.ID.String(),
						},
						{
							Id:   &corev1.VpcId{Value: vpc8.ControllerVpcID.String()},
							Name: vpc8.ID.String(),
						},
						// These unmatched entries intentionally omit tenant config,
						// so create-or-update from Site skips rather than creating them.
						{
							Id:   &corev1.VpcId{Value: uuid.NewString()},
							Name: vpc9.ID.String(),
						},
						{
							Id:   &corev1.VpcId{Value: uuid.NewString()},
							Name: vpc10.ID.String(),
						},
						{
							Id:   &corev1.VpcId{Value: vpc12.ControllerVpcID.String()},
							Name: vpc12.ID.String(),
							Config: &corev1.VpcConfig{
								NetworkVirtualizationType: &evt,
							},
						},
						{
							Id:   &corev1.VpcId{Value: vpc13.ControllerVpcID.String()},
							Name: vpc13.ID.String(),
							Config: &corev1.VpcConfig{
								NetworkVirtualizationType: &evt,
							},
						},
						{
							Id:   &corev1.VpcId{Value: vpc14.ControllerVpcID.String()},
							Name: vpc14.ID.String(),
							Config: &corev1.VpcConfig{
								NetworkSecurityGroupId: cutil.GetPtr(networkSecurityGroupA.ID),
								PowerResourceGroup:     &reportedPowerResourceGroup,
							},
						},
						{
							Id:   &corev1.VpcId{Value: vpc15.ControllerVpcID.String()},
							Name: vpc15.ID.String(),
							Config: &corev1.VpcConfig{
								NetworkSecurityGroupId: cutil.GetPtr(networkSecurityGroupB.ID),
							},
						},
						{
							Id:   &corev1.VpcId{Value: vpc16.ControllerVpcID.String()},
							Name: vpc16.ID.String(),
						},
					},
				},
			},
			updatedVpc:                        vpc1,
			nsgPropagationDetailsClearedVpcs:  []*cdbm.Vpc{vpc12},
			networkVirtualizationUpdatedVpcs:  []*cdbm.Vpc{vpc1},
			ethernetVirtualizationUpdatedVpcs: []*cdbm.Vpc{vpc12, vpc13},
			routingProfileUpdatedVpcs:         []*cdbm.Vpc{vpc1},
			routingProfileClearedVpcs:         []*cdbm.Vpc{vpc2},
			routingProfileStateUpdatedVpc:     vpc1,
			routingProfileStateClearedVpc:     vpc2,
			expectedSlaacEnabled: map[uuid.UUID]bool{
				vpc1.ID: true,
				vpc2.ID: true,
				vpc3.ID: false,
			},
			expectedNetworkSecurityGroupIDs: map[uuid.UUID]*string{
				vpc14.ID: cutil.GetPtr(networkSecurityGroupA.ID),
				vpc15.ID: cutil.GetPtr(networkSecurityGroupB.ID),
				vpc16.ID: nil,
			},
			expectedPowerResourceGroups: map[uuid.UUID]*string{
				vpc14.ID: &reportedPowerResourceGroup,
				vpc15.ID: nil,
				vpc16.ID: nil,
			},
			deletedVpcs:           []*cdbm.Vpc{vpc5, vpc6, vpc10},
			missingVpcs:           []*cdbm.Vpc{vpc7, vpc11},
			restoredVpcs:          []*cdbm.Vpc{vpc8},
			unpairedVpcs:          []*cdbm.Vpc{vpc9},
			readyStatusDetailVpcs: []*cdbm.Vpc{vpc1},
			wantErr:               false,
		},
		{
			name: "test paged VPC inventory processing, empty inventory",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				vpcInventory: &corev1.VPCInventory{
					Vpcs:            []*corev1.Vpc{},
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
				vpcInventory: &corev1.VPCInventory{
					Vpcs:      pagedCtrlVpcs[0:10],
					Timestamp: timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyVpcs: pagedVpcs[0:34],
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
				vpcInventory: &corev1.VPCInventory{
					Vpcs:      pagedCtrlVpcs[30:34],
					Timestamp: timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyVpcs:   pagedVpcs[0:34],
			missingVpcs: pagedVpcs[34:38],
		},
		{
			name: "test paged Instance inventory processing, initiate update VPC metadata workflow",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				vpcInventory: &corev1.VPCInventory{
					Vpcs:      pagedCtrlVpcs[0:10],
					Timestamp: timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyVpcs:              pagedVpcs[0:34],
			requiredMetadataUpdate: true,
			metadataVpcUpdate:      pagedVpcs[1],
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageVpc{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			mv.siteClientPool.IDClientMap[tt.args.siteID.String()] = tt.fields.clientPoolClient

			cwu.TestInventoryAgeUpdatedTimestamp(tt.args.ctx, t, dbSession, (*cdbm.Vpc)(nil))

			_, err := mv.UpdateVpcsInDB(tt.args.ctx, tt.args.siteID, tt.args.vpcInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			vpcDAO := cdbm.NewVpcDAO(dbSession)

			for _, vpcPropStatus := range tt.args.vpcInventory.NetworkSecurityGroupPropagations {
				updatedVPC, _ := vpcDAO.GetByID(ctx, nil, uuid.MustParse(vpcPropStatus.Id), nil)

				// Prop details should not be nil
				assert.NotNil(t, updatedVPC.NetworkSecurityGroupPropagationDetails)

				// The details should match
				assert.Equal(
					t,
					updatedVPC.NetworkSecurityGroupPropagationDetails.NetworkSecurityGroupPropagationObjectStatus,
					vpcPropStatus,
					"\n%+v \n != \n %+v\n",
					updatedVPC.NetworkSecurityGroupPropagationDetails.NetworkSecurityGroupPropagationObjectStatus,
					vpcPropStatus,
				)
			}

			for _, vpc := range tt.nsgPropagationDetailsClearedVpcs {
				// If the VPC should not have propagation details according to the site
				// make sure the DB agrees.
				updatedVPC, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				assert.Nil(t, updatedVPC.NetworkSecurityGroupPropagationDetails)
			}

			// Attach, replace, and clear must each persist when NSG is the only reported difference.
			for vpcID, expectedNetworkSecurityGroupID := range tt.expectedNetworkSecurityGroupIDs {
				updatedVPC, gerr := vpcDAO.GetByID(ctx, nil, vpcID, nil)
				require.NoError(t, gerr)
				assert.Equal(t, expectedNetworkSecurityGroupID, updatedVPC.NetworkSecurityGroupID)
			}

			// Site inventory sets, clears, or preserves nil power resource groups.
			for vpcID, expectedPowerResourceGroup := range tt.expectedPowerResourceGroups {
				updatedVPC, gerr := vpcDAO.GetByID(ctx, nil, vpcID, nil)
				require.NoError(t, gerr)
				assert.Equal(t, expectedPowerResourceGroup, updatedVPC.PowerResourceGroup)
			}

			// Check that VPC status was updated in DB for VPC1
			if tt.updatedVpc != nil {
				updatedVPC, _ := vpcDAO.GetByID(ctx, nil, tt.updatedVpc.ID, nil)
				assert.Equal(t, cdbm.VpcStatusReady, updatedVPC.Status)
			}

			for _, vpc := range tt.networkVirtualizationUpdatedVpcs {
				updatedNetworkVirtVPC, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				assert.Equal(t, nwvt.String(), *updatedNetworkVirtVPC.NetworkVirtualizationType)
			}

			for _, vpc := range tt.ethernetVirtualizationUpdatedVpcs {
				updatedEthernetVirtVPC, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				assert.Equal(t, evt.String(), *updatedEthernetVirtVPC.NetworkVirtualizationType)
			}

			for _, vpc := range tt.routingProfileUpdatedVpcs {
				updatedRoutingProfileVPC, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				assert.NotNil(t, updatedRoutingProfileVPC.RoutingProfile)
				assert.Equal(t, "INTERNAL", *updatedRoutingProfileVPC.RoutingProfile)
			}

			for _, vpc := range tt.routingProfileClearedVpcs {
				clearedRoutingProfileVPC, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				assert.Nil(t, clearedRoutingProfileVPC.RoutingProfile)
			}

			for vpcID, expectedSlaacEnabled := range tt.expectedSlaacEnabled {
				updatedSlaacVpc, gerr := vpcDAO.GetByID(ctx, nil, vpcID, nil)
				require.NoError(t, gerr)
				assert.Equal(t, expectedSlaacEnabled, updatedSlaacVpc.SlaacEnabled)
			}

			// Controller-reported desired and effective profiles must be cached together.
			if tt.routingProfileStateUpdatedVpc != nil {
				updatedProfileVPC, gerr := vpcDAO.GetByID(ctx, nil, tt.routingProfileStateUpdatedVpc.ID, nil)
				require.NoError(t, gerr)
				require.NotNil(t, updatedProfileVPC.RoutingProfileOverrides)
				require.NotNil(t, updatedProfileVPC.RoutingProfileOverrides.LeakDefaultRouteFromUnderlay)
				assert.False(t, *updatedProfileVPC.RoutingProfileOverrides.LeakDefaultRouteFromUnderlay)
				require.NotNil(t, updatedProfileVPC.RoutingProfileOverrides.AllowedAnycastPrefixes)
				assert.Equal(t, []string{"192.0.2.1/24"}, *updatedProfileVPC.RoutingProfileOverrides.AllowedAnycastPrefixes)
				require.NotNil(t, updatedProfileVPC.EffectiveRoutingProfile)
				assert.True(t, updatedProfileVPC.EffectiveRoutingProfile.LeakDefaultRouteFromUnderlay)
				assert.True(t, updatedProfileVPC.EffectiveRoutingProfile.Internal)
				assert.Equal(t, uint32(8), updatedProfileVPC.EffectiveRoutingProfile.AccessTier)
			}

			// Omission from inventory clears both stale cached values instead of preserving them.
			if tt.routingProfileStateClearedVpc != nil {
				clearedProfileVPC, gerr := vpcDAO.GetByID(ctx, nil, tt.routingProfileStateClearedVpc.ID, nil)
				require.NoError(t, gerr)
				assert.Nil(t, clearedProfileVPC.RoutingProfileOverrides)
				assert.Nil(t, clearedProfileVPC.EffectiveRoutingProfile)
			}

			for _, vpc := range tt.readyVpcs {
				rv, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.VpcStatusReady, rv.Status)
			}

			for _, vpc := range tt.deletedVpcs {
				_, err = vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("VPC %s should have been deleted", vpc.Name))
			}

			for _, vpc := range tt.missingVpcs {
				uv, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)

				if uv.ControllerVpcID != nil {
					assert.True(t, uv.IsMissingOnSite)
					assert.Equal(t, cdbm.VpcStatusError, uv.Status)
				} else {
					assert.False(t, uv.IsMissingOnSite)
				}
			}

			for _, vpc := range tt.unpairedVpcs {
				uv, err := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				require.NoError(t, err)
				assert.Nil(t, uv.ControllerVpcID)
				assert.Equal(t, vpc.Status, uv.Status)
			}

			for _, vpc := range tt.restoredVpcs {
				rv, _ := vpcDAO.GetByID(ctx, nil, vpc.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.VpcStatusReady, rv.Status)
			}

			if tt.requiredMetadataUpdate {
				assert.True(t, len(tt.fields.clientPoolClient.Calls) > 0)
				assert.Equal(t, len(tt.fields.clientPoolClient.Calls[0].Arguments), 4)

				scReq := tt.fields.clientPoolClient.Calls[0].Arguments[3].(*corev1.VpcUpdateRequest)
				assert.Equal(t, tt.metadataVpcUpdate.ID.String(), scReq.Id.Value)
			}

			statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)
			for _, vpc := range tt.readyStatusDetailVpcs {
				statusDetails, _, err := statusDetailDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{vpc.ID.String()}}, cdbp.PageInput{})
				require.NoError(t, err)
				require.Len(t, statusDetails, 1)
				assert.Equal(t, cdbm.VpcStatusReady, statusDetails[0].Status)
				require.NotNil(t, statusDetails[0].Message)
				assert.Equal(t, "VPC is ready for use", *statusDetails[0].Message)
			}
		})
	}
}

func TestManageVpc_UpdateVpcsInDB_AutoCreatesAndRestores(t *testing.T) {
	ctx := context.Background()
	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	testVPCSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testVPCBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)

	siteClientPool := testTemporalSiteClientPool(t)
	siteClientPool.IDClientMap[site.ID.String()] = &tmocks.Client{}
	manager := ManageVpc{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}

	controllerVpcID := uuid.New()
	networkVirtualizationType := corev1.VpcVirtualizationType_FNN
	requestedVni := uint32(101)
	activeVni := uint32(202)
	routingProfile := "INTERNAL"
	controllerVpc := &corev1.Vpc{
		Id: &corev1.VpcId{Value: controllerVpcID.String()},
		Config: &corev1.VpcConfig{
			TenantOrganizationId:      tenantOrg,
			NetworkVirtualizationType: &networkVirtualizationType,
			SlaacEnabled:              cutil.GetPtr(true),
			RoutingProfileType:        &routingProfile,
			Vni:                       &requestedVni,
		},
		Status: &corev1.VpcStatus{Vni: &activeVni},
		Metadata: &corev1.Metadata{
			Name:        "site-created-vpc",
			Description: "created directly on Site",
			Labels: []*corev1.Label{
				{Key: "origin", Value: cutil.GetPtr("site")},
			},
		},
	}
	inventory := &corev1.VPCInventory{
		Vpcs: []*corev1.Vpc{controllerVpc},
	}

	vpcDAO := cdbm.NewVpcDAO(dbSession)
	statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)

	if !t.Run("auto creates VPC from inventory", func(t *testing.T) {
		_, err := manager.UpdateVpcsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)

		createdVpc, err := vpcDAO.GetByID(ctx, nil, controllerVpcID, nil)
		require.NoError(t, err)
		assert.Equal(t, controllerVpcID, createdVpc.ID)
		require.NotNil(t, createdVpc.ControllerVpcID)
		assert.Equal(t, controllerVpcID, *createdVpc.ControllerVpcID)
		assert.Equal(t, tenant.ID, createdVpc.TenantID)
		assert.Equal(t, site.ID, createdVpc.SiteID)
		assert.Equal(t, site.InfrastructureProviderID, createdVpc.InfrastructureProviderID)
		assert.Equal(t, site.ID, createdVpc.CreatedBy)
		assert.Equal(t, cdbm.VpcStatusReady, createdVpc.Status)
		assert.Equal(t, "site-created-vpc", createdVpc.Name)
		assert.Equal(t, cdbm.Labels{"origin": "site"}, createdVpc.Labels)
		require.NotNil(t, createdVpc.NetworkVirtualizationType)
		assert.Equal(t, cdbm.VpcFNN, *createdVpc.NetworkVirtualizationType)
		assert.True(t, createdVpc.SlaacEnabled)
		require.NotNil(t, createdVpc.RoutingProfile)
		assert.Equal(t, "INTERNAL", *createdVpc.RoutingProfile)
		require.NotNil(t, createdVpc.Vni)
		assert.Equal(t, 101, *createdVpc.Vni)
		require.NotNil(t, createdVpc.ActiveVni)
		assert.Equal(t, 202, *createdVpc.ActiveVni)

		statusDetails, _, statusErr := statusDetailDAO.GetAll(
			ctx,
			nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{createdVpc.ID.String()}},
			cdbp.PageInput{},
		)
		require.NoError(t, statusErr)
		require.Len(t, statusDetails, 1)
		require.NotNil(t, statusDetails[0].Message)
		assert.Equal(t, "VPC was found on Site, Ready for use", *statusDetails[0].Message)
	}) {
		t.FailNow()
	}

	if !t.Run("inventory replay is idempotent", func(t *testing.T) {
		_, err := manager.UpdateVpcsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		vpcs, count, err := vpcDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{controllerVpcID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, vpcs, 1)
		assert.Equal(t, 1, count)
	}) {
		t.FailNow()
	}

	t.Run("inventory restores soft-deleted VPC", func(t *testing.T) {
		nonReadyNVLink := cwu.TestBuildNVLinkLogicalPartition(
			t,
			dbSession,
			"test-restore-non-ready-nvlink",
			nil,
			site,
			tenant,
			cdbm.NVLinkLogicalPartitionStatusPending,
			false,
		)
		_, err := vpcDAO.Update(ctx, nil, cdbm.VpcUpdateInput{
			VpcID:                    controllerVpcID,
			NVLinkLogicalPartitionID: &nonReadyNVLink.ID,
			Status:                   cutil.GetPtr(cdbm.VpcStatusError),
			IsMissingOnSite:          cutil.GetPtr(true),
			ActiveVni:                cutil.GetPtr(1),
			RoutingProfile:           cutil.GetPtr("EXTERNAL"),
		})
		require.NoError(t, err)

		require.NoError(t, vpcDAO.DeleteByID(ctx, nil, controllerVpcID))
		deletedVpcs, _, err := vpcDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{controllerVpcID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deletedVpcs, 1)
		require.NotNil(t, deletedVpcs[0].Deleted)

		// The undelete is deferred while the delete is newer than the staleness threshold, so
		// backdate it past that.
		cwu.TestInventoryAgeDeletedTimestamp(ctx, t, dbSession, (*cdbm.Vpc)(nil), controllerVpcID)
		cwu.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.Vpc)(nil))
		_, err = manager.UpdateVpcsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		restoredVpc, err := vpcDAO.GetByID(ctx, nil, controllerVpcID, nil)
		require.NoError(t, err)
		assert.Nil(t, restoredVpc.Deleted)
		assert.False(t, restoredVpc.IsMissingOnSite)
		assert.Equal(t, cdbm.VpcStatusReady, restoredVpc.Status)
		assert.True(t, restoredVpc.SlaacEnabled)
		require.NotNil(t, restoredVpc.ControllerVpcID)
		assert.Equal(t, controllerVpcID, *restoredVpc.ControllerVpcID)
		require.NotNil(t, restoredVpc.Vni)
		assert.Equal(t, 101, *restoredVpc.Vni)
		require.NotNil(t, restoredVpc.ActiveVni)
		assert.Equal(t, 202, *restoredVpc.ActiveVni)
		require.NotNil(t, restoredVpc.RoutingProfile)
		assert.Equal(t, "INTERNAL", *restoredVpc.RoutingProfile)
		// Soft-deleted NVLink is preserved; inventory does not overwrite it.
		require.NotNil(t, restoredVpc.NVLinkLogicalPartitionID)
		assert.Equal(t, nonReadyNVLink.ID, *restoredVpc.NVLinkLogicalPartitionID)

		statusDetails, _, statusErr := statusDetailDAO.GetAll(
			ctx,
			nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{restoredVpc.ID.String()}},
			cdbp.PageInput{},
		)
		require.NoError(t, statusErr)
		foundReadyMessage := false
		for i := range statusDetails {
			if statusDetails[i].Message != nil &&
				*statusDetails[i].Message == "VPC is ready for use" {
				foundReadyMessage = true
				break
			}
		}
		assert.True(t, foundReadyMessage)
	})

	t.Run("inventory skips restore when tenant organization differs", func(t *testing.T) {
		require.NoError(t, vpcDAO.DeleteByID(ctx, nil, controllerVpcID))
		deletedVpcs, _, err := vpcDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{controllerVpcID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deletedVpcs, 1)
		require.NotNil(t, deletedVpcs[0].Deleted)
		deletedAt := *deletedVpcs[0].Deleted

		mismatchedInventory := &corev1.VPCInventory{
			Vpcs: []*corev1.Vpc{{
				Id: &corev1.VpcId{Value: controllerVpcID.String()},
				Config: &corev1.VpcConfig{
					TenantOrganizationId:      "other-tenant-org",
					NetworkVirtualizationType: &networkVirtualizationType,
					RoutingProfileType:        &routingProfile,
					Vni:                       &requestedVni,
				},
				Status: &corev1.VpcStatus{Vni: &activeVni},
				Metadata: &corev1.Metadata{
					Name:        "site-created-vpc",
					Description: "created directly on Site",
				},
			}},
		}
		_, err = manager.UpdateVpcsInDB(ctx, site.ID, mismatchedInventory)
		require.NoError(t, err)

		stillDeleted, _, err := vpcDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{controllerVpcID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, stillDeleted, 1)
		require.NotNil(t, stillDeleted[0].Deleted)
		assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)

		_, err = vpcDAO.GetByID(ctx, nil, controllerVpcID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})
}

func TestManageVpc_CreateOrUpdateVpcFromSite_SkipsIncompleteOwnership(t *testing.T) {
	ctx := context.Background()
	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	testVPCSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)
	manager := ManageVpc{dbSession: dbSession}

	authorizedTenantOrg := "test-authorized-tenant"
	authorizedTenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), authorizedTenantOrg, []string{"FORGE_TENANT_ADMIN"})
	authorizedTenant := testVPCBuildTenant(t, dbSession, "test-authorized-tenant", authorizedTenantOrg, authorizedTenantUser)

	otherTenantOrg := "test-other-tenant"
	otherTenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), otherTenantOrg, []string{"FORGE_TENANT_ADMIN"})
	otherTenant := testVPCBuildTenant(t, dbSession, "test-other-tenant", otherTenantOrg, otherTenantUser)
	otherTenantNVLink := cwu.TestBuildNVLinkLogicalPartition(
		t,
		dbSession,
		"test-other-tenant-nvlink",
		nil,
		site,
		otherTenant,
		cdbm.NVLinkLogicalPartitionStatusReady,
		false,
	)
	nonReadyNVLink := cwu.TestBuildNVLinkLogicalPartition(
		t,
		dbSession,
		"test-non-ready-nvlink",
		nil,
		site,
		authorizedTenant,
		cdbm.NVLinkLogicalPartitionStatusPending,
		false,
	)
	existingVpc := testVPCBuildVPC(
		t,
		dbSession,
		"test-existing-name",
		provider,
		authorizedTenant,
		site,
		cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
		cutil.GetPtr(uuid.New()),
		nil,
		authorizedTenantUser,
		cdbm.VpcStatusReady,
	)

	tests := []struct {
		name          string
		controllerVpc *corev1.Vpc
		wantVpc       bool
		wantName      string
		wantNamePref  string
		wantNVLinkID  *uuid.UUID
	}{
		{
			name: "unknown tenant organization",
			controllerVpc: &corev1.Vpc{
				Id:       &corev1.VpcId{Value: uuid.NewString()},
				Config:   &corev1.VpcConfig{TenantOrganizationId: "unknown-tenant-org"},
				Metadata: &corev1.Metadata{Name: "unknown-tenant-vpc"},
			},
		},
		{
			name: "invalid controller VPC ID",
			controllerVpc: &corev1.Vpc{
				Id:       &corev1.VpcId{Value: "not-a-uuid"},
				Config:   &corev1.VpcConfig{TenantOrganizationId: authorizedTenantOrg},
				Metadata: &corev1.Metadata{Name: "invalid-id-vpc"},
			},
		},
		{
			name: "NVLink Logical Partition from another tenant is rejected",
			controllerVpc: &corev1.Vpc{
				Id: &corev1.VpcId{Value: uuid.NewString()},
				Config: &corev1.VpcConfig{
					TenantOrganizationId: authorizedTenantOrg,
					DefaultNvlinkLogicalPartitionId: &corev1.NVLinkLogicalPartitionId{
						Value: otherTenantNVLink.ID.String(),
					},
				},
				Metadata: &corev1.Metadata{Name: "cross-tenant-nvlink-vpc"},
			},
		},
		{
			name: "creates VPC with non-Ready inventory NVLink Logical Partition",
			controllerVpc: &corev1.Vpc{
				Id: &corev1.VpcId{Value: uuid.NewString()},
				Config: &corev1.VpcConfig{
					TenantOrganizationId: authorizedTenantOrg,
					DefaultNvlinkLogicalPartitionId: &corev1.NVLinkLogicalPartitionId{
						Value: nonReadyNVLink.ID.String(),
					},
				},
				Metadata: &corev1.Metadata{Name: "non-ready-nvlink-vpc"},
			},
			wantVpc:      true,
			wantName:     "non-ready-nvlink-vpc",
			wantNVLinkID: &nonReadyNVLink.ID,
		},
		{
			name: "renames VPC when active name already exists",
			controllerVpc: &corev1.Vpc{
				Id:       &corev1.VpcId{Value: uuid.NewString()},
				Config:   &corev1.VpcConfig{TenantOrganizationId: authorizedTenantOrg},
				Metadata: &corev1.Metadata{Name: existingVpc.Name},
			},
			wantVpc:      true,
			wantNamePref: existingVpc.Name + "-recovered-",
		},
		{
			name: "assigns recovered name when metadata name is empty",
			controllerVpc: &corev1.Vpc{
				Id:       &corev1.VpcId{Value: uuid.NewString()},
				Config:   &corev1.VpcConfig{TenantOrganizationId: authorizedTenantOrg},
				Metadata: &corev1.Metadata{Name: ""},
			},
			wantVpc:      true,
			wantNamePref: "recovered-",
		},
		{
			name: "empty tenant organization is rejected",
			controllerVpc: &corev1.Vpc{
				Id:       &corev1.VpcId{Value: uuid.NewString()},
				Config:   &corev1.VpcConfig{TenantOrganizationId: ""},
				Metadata: &corev1.Metadata{Name: "missing-tenant-org-vpc"},
			},
		},
		{
			name: "creates VPC without Site allocation",
			controllerVpc: &corev1.Vpc{
				Id:       &corev1.VpcId{Value: uuid.NewString()},
				Config:   &corev1.VpcConfig{TenantOrganizationId: authorizedTenantOrg},
				Metadata: &corev1.Metadata{Name: "unallocated-tenant-vpc"},
			},
			wantVpc:  true,
			wantName: "unallocated-tenant-vpc",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vpc := manager.createOrUpdateVpcFromSite(
				ctx,
				site,
				tt.controllerVpc,
				nil,
			)
			if tt.wantVpc {
				require.NotNil(t, vpc)
				assert.Equal(t, cdbm.VpcStatusReady, vpc.Status)
				assert.Equal(t, authorizedTenant.ID, vpc.TenantID)
				if tt.wantName != "" {
					assert.Equal(t, tt.wantName, vpc.Name)
				}
				if tt.wantNamePref != "" {
					assert.True(t, strings.HasPrefix(vpc.Name, tt.wantNamePref), "name %q should have prefix %q", vpc.Name, tt.wantNamePref)
					assert.Len(t, strings.TrimPrefix(vpc.Name, tt.wantNamePref), 8)
				}
				if tt.wantNVLinkID != nil {
					require.NotNil(t, vpc.NVLinkLogicalPartitionID)
					assert.Equal(t, *tt.wantNVLinkID, *vpc.NVLinkLogicalPartitionID)
				}
				return
			}
			assert.Nil(t, vpc)
		})
	}
}

func TestNewManageVpc(t *testing.T) {
	type args struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		tc             client.Client
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

	wtc := &tmocks.Client{}

	tests := []struct {
		name string
		args args
		want ManageVpc
	}{
		{
			name: "test new ManageVpc instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
				tc:             wtc,
			},
			want: ManageVpc{
				dbSession:      dbSession,
				siteClientPool: scp,
				tc:             wtc,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageVpc(tt.args.dbSession, tt.args.siteClientPool, tt.args.tc); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageVpc() = %v, want %v", got, tt.want)
			}
		})
	}
}

// Test VPC Metrics - DELETE operations
func Test_VpcMetrics_Delete_DeletingOnly(t *testing.T) {
	// Case 1: deleting (should emit metric with duration now-t1)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageVpcLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testVpcID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // deleting started
	deleteTime := baseTime.Add(200 * time.Millisecond) // delete happened 200ms later
	expectedDuration := deleteTime.Sub(t1)

	// t1: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testVpcID.String(), cdbm.VpcStatusDeleting, nil, t1)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordVpcStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testVpcID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted with correct duration (200ms)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_vpc_operation_latency_seconds", 1, map[string]string{
		"operation_type": "delete",
		"from_status":    cdbm.VpcStatusDeleting,
		"to_status":      "Deleted",
	}, expectedDuration)
}

func Test_VpcMetrics_Delete_MultipleDeleting(t *testing.T) {
	// Case 2: deleting -> deleting -> deleting (should emit metric with duration now-t1)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageVpcLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testVpcID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // first deleting
	t2 := baseTime.Add(50 * time.Millisecond)          // second deleting
	t3 := baseTime.Add(100 * time.Millisecond)         // third deleting
	deleteTime := baseTime.Add(300 * time.Millisecond) // delete happened
	expectedDuration := deleteTime.Sub(t1)             // should use first deleting timestamp

	// t1: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testVpcID.String(), cdbm.VpcStatusDeleting, nil, t1)

	// t2: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testVpcID.String(), cdbm.VpcStatusDeleting, nil, t2)

	// t3: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testVpcID.String(), cdbm.VpcStatusDeleting, nil, t3)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordVpcStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testVpcID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted (should use first deleting timestamp, duration 300ms)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_vpc_operation_latency_seconds", 1, map[string]string{
		"operation_type": "delete",
		"from_status":    cdbm.VpcStatusDeleting,
		"to_status":      "Deleted",
	}, expectedDuration)
}

func Test_VpcMetrics_Delete_NoDeleting(t *testing.T) {
	// Case 3: ready (no deleting, should NOT emit metric)
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()
	util.TestSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageVpcLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testVpcID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime
	deleteTime := baseTime.Add(100 * time.Millisecond)

	// t1: ready (no deleting status)
	util.TestBuildStatusDetailWithTime(t, dbSession, testVpcID.String(), cdbm.VpcStatusReady, nil, t1)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordVpcStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testVpcID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify NO metric was emitted (no deleting status found)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_vpc_operation_latency_seconds", 0, nil, 0)
}
