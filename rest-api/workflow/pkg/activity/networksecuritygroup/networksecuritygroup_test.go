// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package networksecuritygroup

import (
	"context"
	"fmt"
	"reflect"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
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

func testNetworkSecurityGroupInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testNetworkSecurityGroupSetupSchema(t *testing.T, dbSession *cdb.Session) {
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
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
}

// testNetworkSecurityGroupSiteBuildInfrastructureProvider Building Infra Provider in DB
func testNetworkSecurityGroupSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
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

// testNetworkSecurityGroupBuildSite Building Site in DB
func testNetworkSecurityGroupBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
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

// testSubnetBuildTenant Building Tenant in DB
func testNetworkSecurityGroupBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
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

// testNetworkSecurityGroupBuildUser Building User in DB
func testNetworkSecurityGroupBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
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

// testNetworkSecurityGroupBuildNetworkSecurityGroup Building NetworkSecurityGroup in DB
func testNetworkSecurityGroupBuildNetworkSecurityGroup(t *testing.T, dbSession *cdb.Session, name string, st *cdbm.Site, tn *cdbm.Tenant, user *cdbm.User, status string) *cdbm.NetworkSecurityGroup {
	networkSecurityGroupDAO := cdbm.NewNetworkSecurityGroupDAO(dbSession)

	networkSecurityGroup, err := networkSecurityGroupDAO.Create(context.Background(), nil, cdbm.NetworkSecurityGroupCreateInput{Name: name, Description: cutil.GetPtr("description"), TenantID: tn.ID, SiteID: st.ID, Status: status, CreatedByID: user.ID})
	assert.Nil(t, err)

	return networkSecurityGroup
}

func TestManageNetworkSecurityGroup_UpdateNetworkSecurityGroupsInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testNetworkSecurityGroupInitDB(t)
	defer dbSession.Close()

	testNetworkSecurityGroupSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := testNetworkSecurityGroupBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := testNetworkSecurityGroupSiteBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := testNetworkSecurityGroupBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)

	st := testNetworkSecurityGroupBuildSite(t, dbSession, ip, "test-site", ipu)
	st2 := testNetworkSecurityGroupBuildSite(t, dbSession, ip, "test-site-2", ipu)
	st3 := testNetworkSecurityGroupBuildSite(t, dbSession, ip, "test-site-3", ipu)

	tn := testNetworkSecurityGroupBuildTenant(t, dbSession, "test tenant1", tnOrg, tnu)

	networkSecurityGroup1 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-1", st, tn, tnu, cdbm.NetworkSecurityGroupStatusReady)

	networkSecurityGroup2 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-2", st, tn, tnu, cdbm.NetworkSecurityGroupStatusReady)

	networkSecurityGroup3 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-3", st, tn, tnu, cdbm.NetworkSecurityGroupStatusError)

	networkSecurityGroup4 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-4", st, tn, tnu, cdbm.NetworkSecurityGroupStatusError)

	networkSecurityGroup5 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-5", st, tn, tnu, cdbm.NetworkSecurityGroupStatusError)

	_, err := dbSession.DB.Exec("UPDATE network_security_group SET deleted = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), networkSecurityGroup5.ID)
	assert.NoError(t, err)

	networkSecurityGroup6 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-6", st, tn, tnu, cdbm.NetworkSecurityGroupStatusError)

	_, err = dbSession.DB.Exec("UPDATE network_security_group SET deleted = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), networkSecurityGroup6.ID)
	assert.NoError(t, err)

	networkSecurityGroup7 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-7", st, tn, tnu, cdbm.NetworkSecurityGroupStatusReady)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE network_security_group SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), networkSecurityGroup7.ID)
	assert.NoError(t, err)

	networkSecurityGroup8 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-8", st, tn, tnu, cdbm.NetworkSecurityGroupStatusReady)

	networkSecurityGroup9 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-9", st, tn, tnu, cdbm.NetworkSecurityGroupStatusReady)

	networkSecurityGroup10 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-10", st, tn, tnu, cdbm.NetworkSecurityGroupStatusError)

	networkSecurityGroup11 := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, "test-networkSecurityGroup-11", st, tn, tnu, cdbm.NetworkSecurityGroupStatusReady)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE network_security_group SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), networkSecurityGroup11.ID)
	assert.NoError(t, err)

	networkSecurityGroupDAO := cdbm.NewNetworkSecurityGroupDAO(dbSession)
	networkSecurityGroup8, err = networkSecurityGroupDAO.Update(ctx, nil, cdbm.NetworkSecurityGroupUpdateInput{NetworkSecurityGroupID: networkSecurityGroup8.ID, Status: cutil.GetPtr(cdbm.NetworkSecurityGroupStatusError)})
	assert.NoError(t, err)

	// Build NetworkSecurityGroup inventory that is paginated
	// Generate data for 35 NetworkSecurityGroups reported from Site Agent while Cloud has 38 NetworkSecurityGroups
	// One of the NetworkSecurityGroups on site doesn't exist in cloud.
	pagedNetworkSecurityGroups := []*cdbm.NetworkSecurityGroup{}
	pagedInvIds := []string{}
	for i := 0; i < 38; i++ {
		networkSecurityGroup := testNetworkSecurityGroupBuildNetworkSecurityGroup(t, dbSession, fmt.Sprintf("test-networkSecurityGroup-paged-%d", i), st3, tn, tnu, cdbm.NetworkSecurityGroupStatusReady)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE network_security_group SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), networkSecurityGroup.ID)
		assert.NoError(t, err)
		pagedNetworkSecurityGroups = append(pagedNetworkSecurityGroups, networkSecurityGroup)
		pagedInvIds = append(pagedInvIds, networkSecurityGroup.ID)
	}

	// We stop short by 4 records to similate records unknown to
	// the controller.
	pagedCtrlNetworkSecurityGroups := []*corev1.NetworkSecurityGroup{}
	for i := 0; i < 34; i++ {
		ctrlNetworkSecurityGroup := &corev1.NetworkSecurityGroup{
			Id: pagedNetworkSecurityGroups[i].ID,
			Metadata: &corev1.Metadata{
				Name: pagedNetworkSecurityGroups[i].Name,
			},
		}

		pagedCtrlNetworkSecurityGroups = append(pagedCtrlNetworkSecurityGroups, ctrlNetworkSecurityGroup)
	}

	// Update the last one on the "controller"
	// so we can later check if the cloud got the update.
	updatedGroup := pagedCtrlNetworkSecurityGroups[len(pagedCtrlNetworkSecurityGroups)-1]
	updatedGroup.Metadata.Description = "UPDATED!"

	// Similate a group known to site but not known to cloud.
	cloudUnknownGroup := &cdbm.NetworkSecurityGroup{ID: uuid.NewString(), Name: "unknown-to-cloud", TenantOrg: tn.Org}

	idStr := "anything"

	siteKnownGroup := &corev1.NetworkSecurityGroup{Id: cloudUnknownGroup.ID, Metadata: &corev1.Metadata{
		Name: cloudUnknownGroup.Name,
	}, TenantOrganizationId: cloudUnknownGroup.TenantOrg,
		Attributes: &corev1.NetworkSecurityGroupAttributes{StatefulEgress: true, Rules: []*corev1.NetworkSecurityGroupRuleAttributes{
			{
				Id: &idStr,
			},
		}}}

	// Add one more NetworkSecurityGroup to site that cloud won't know about
	pagedCtrlNetworkSecurityGroups = append(pagedCtrlNetworkSecurityGroups, siteKnownGroup)

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	// Mock UpdateNetworkSecurityGroup workflow from site-agent
	wrun := &tmocks.WorkflowRun{}
	wid := "test-workflow-id"
	wrun.On("GetID").Return(wid)

	workflowOptions1 := client.StartWorkflowOptions{
		ID:        "site-networkSecurityGroup-update-metadata-" + pagedNetworkSecurityGroups[1].ID,
		TaskQueue: queue.SiteTaskQueue,
	}

	mtc1 := &tmocks.Client{}
	mtc1.Mock.On("ExecuteWorkflow", context.Background(), workflowOptions1, "UpdateNetworkSecurityGroup", mock.Anything).Return(wrun, nil)

	type fields struct {
		dbSession        *cdb.Session
		siteClientPool   *sc.ClientPool
		clientPoolClient *tmocks.Client
		env              *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                           context.Context
		siteID                        uuid.UUID
		networkSecurityGroupInventory *corev1.NetworkSecurityGroupInventory
	}

	tests := []struct {
		name                         string
		fields                       fields
		args                         args
		readyNetworkSecurityGroups   []*cdbm.NetworkSecurityGroup
		deletedNetworkSecurityGroups []*cdbm.NetworkSecurityGroup
		updatedNetworkSecurityGroups []*corev1.NetworkSecurityGroup
		wantErr                      bool
	}{
		{
			name: "test NetworkSecurityGroup inventory processing error, non-existent Site",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				networkSecurityGroupInventory: &corev1.NetworkSecurityGroupInventory{
					NetworkSecurityGroups: []*corev1.NetworkSecurityGroup{},
				},
			},
			wantErr: true,
		},
		{
			name: "test NetworkSecurityGroup inventory processing success",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				networkSecurityGroupInventory: &corev1.NetworkSecurityGroupInventory{
					NetworkSecurityGroups: []*corev1.NetworkSecurityGroup{
						{
							Id:       networkSecurityGroup1.ID,
							Metadata: &corev1.Metadata{Name: networkSecurityGroup1.ID},
						},
						{
							Id:       networkSecurityGroup2.ID,
							Metadata: &corev1.Metadata{Name: networkSecurityGroup2.ID},
						},
						{
							Id:       networkSecurityGroup3.ID,
							Metadata: &corev1.Metadata{Name: networkSecurityGroup3.ID},
						},
						{
							Id:       networkSecurityGroup4.ID,
							Metadata: &corev1.Metadata{Name: networkSecurityGroup4.ID},
						},
						{
							Id:       networkSecurityGroup8.ID,
							Metadata: &corev1.Metadata{Name: networkSecurityGroup8.ID},
						},
						{
							Id:       uuid.NewString(),
							Metadata: &corev1.Metadata{Name: networkSecurityGroup9.ID},
						},
						{
							Id:       uuid.NewString(),
							Metadata: &corev1.Metadata{Name: networkSecurityGroup10.ID},
						},
					},
				},
			},
			deletedNetworkSecurityGroups: []*cdbm.NetworkSecurityGroup{networkSecurityGroup5, networkSecurityGroup6},
			wantErr:                      false,
		},
		{
			name: "test paged NetworkSecurityGroup inventory processing, empty inventory",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				networkSecurityGroupInventory: &corev1.NetworkSecurityGroupInventory{
					NetworkSecurityGroups: []*corev1.NetworkSecurityGroup{},
					Timestamp:             timestamppb.Now(),
					InventoryStatus:       corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
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
				networkSecurityGroupInventory: &corev1.NetworkSecurityGroupInventory{
					NetworkSecurityGroups: pagedCtrlNetworkSecurityGroups[0:10],
					Timestamp:             timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyNetworkSecurityGroups: pagedNetworkSecurityGroups[0:34],
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
				networkSecurityGroupInventory: &corev1.NetworkSecurityGroupInventory{
					NetworkSecurityGroups: pagedCtrlNetworkSecurityGroups[30:34],
					Timestamp:             timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyNetworkSecurityGroups: pagedNetworkSecurityGroups[0:34],
		},
		{
			name: "test paged Instance inventory processing, last page, with an ID unknown to cloud",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				networkSecurityGroupInventory: &corev1.NetworkSecurityGroupInventory{
					NetworkSecurityGroups: pagedCtrlNetworkSecurityGroups[30:35],
					Timestamp:             timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  35,
						ItemIds:     pagedInvIds[30:35],
					},
				},
			},
			// Check that the type unknown to cloud has become known to cloud.  I.e.,
			// it made it to the cloud DB.
			readyNetworkSecurityGroups:   append(pagedNetworkSecurityGroups[30:34], cloudUnknownGroup),
			updatedNetworkSecurityGroups: []*corev1.NetworkSecurityGroup{updatedGroup},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageNetworkSecurityGroup{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			mv.siteClientPool.IDClientMap[tt.args.siteID.String()] = tt.fields.clientPoolClient

			cwu.TestInventoryAgeUpdatedTimestamp(tt.args.ctx, t, dbSession, (*cdbm.NetworkSecurityGroup)(nil))

			err := mv.UpdateNetworkSecurityGroupsInDB(tt.args.ctx, tt.args.siteID, tt.args.networkSecurityGroupInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			for _, networkSecurityGroup := range tt.deletedNetworkSecurityGroups {
				_, err := networkSecurityGroupDAO.GetByID(ctx, nil, networkSecurityGroup.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("NetworkSecurityGroup %s (%s) should have been deleted", networkSecurityGroup.Name, networkSecurityGroup.ID))
			}

			for _, networkSecurityGroup := range tt.readyNetworkSecurityGroups {
				it, err := networkSecurityGroupDAO.GetByID(ctx, nil, networkSecurityGroup.ID, nil)
				require.Nil(t, err, fmt.Sprintf("NetworkSecurityGroup %s (%s) should exist and not return err", networkSecurityGroup.Name, networkSecurityGroup.ID))
				require.NotNil(t, it, fmt.Sprintf("NetworkSecurityGroup %s (%s) should exist", networkSecurityGroup.Name, networkSecurityGroup.ID))
			}

			for _, networkSecurityGroup := range tt.updatedNetworkSecurityGroups {
				it, err := networkSecurityGroupDAO.GetByID(ctx, nil, networkSecurityGroup.Id, nil)
				require.Nil(t, err, fmt.Sprintf("NetworkSecurityGroup %s (%s) should exist and not return err", networkSecurityGroup.Metadata.Name, networkSecurityGroup.Id))
				require.NotNil(t, it, fmt.Sprintf("NetworkSecurityGroup %s (%s) should exist", networkSecurityGroup.Metadata.Name, networkSecurityGroup.Id))

				assert.Equal(t, networkSecurityGroup.Metadata.Description, *it.Description)
				assert.Equal(t, networkSecurityGroup.GetAttributes().GetStatefulEgress(), it.StatefulEgress)
			}
		})
	}

}

func TestNewManageNetworkSecurityGroup(t *testing.T) {
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
		want ManageNetworkSecurityGroup
	}{
		{
			name: "test new ManageNetworkSecurityGroup instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageNetworkSecurityGroup{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageNetworkSecurityGroup(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageNetworkSecurityGroup() = %v, want %v", got, tt.want)
			}
		})
	}
}
