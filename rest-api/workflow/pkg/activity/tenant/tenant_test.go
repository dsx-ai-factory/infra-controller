// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tenant

import (
	"context"
	"fmt"
	"os"
	"reflect"
	"testing"
	"time"

	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/uptrace/bun/extra/bundebug"
	tmocks "go.temporal.io/sdk/mocks"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
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

func testTenantInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testTenantSetupSchema(t *testing.T, dbSession *cdb.Session) {
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
}

func TestManageTenant_UpdateTenantsInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testTenantInitDB(t)
	defer dbSession.Close()

	testTenantSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	// Build Site
	st := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := cwu.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	st3 := cwu.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)

	// Build Tenant inventory that is paginated
	// Generate data for 34 Tenants reported from Site Agent while Cloud has 38 Tenants
	pagedTenants := []*cdbm.Tenant{}
	pagedInvIds := []string{}

	for i := 0; i < 38; i++ {
		tn := cwu.TestBuildTenant(t, dbSession, fmt.Sprintf("Test Tenant %v", i), fmt.Sprintf("test-tenant-org-%v", i), nil, ipu)
		cwu.TestBuildTenantSiteAssociation(t, dbSession, tn.Org, tn.ID, st.ID, ipu.ID)
		cwu.TestBuildTenantSiteAssociation(t, dbSession, tn.Org, tn.ID, st3.ID, ipu.ID)

		// Update creation timestamp to be earlier than inventory processing interval
		_, err := dbSession.DB.Exec("UPDATE tenant SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval*2)), tn.ID.String())
		assert.NoError(t, err)
		pagedTenants = append(pagedTenants, tn)
		pagedInvIds = append(pagedInvIds, tn.Org)
	}

	tenantsToUpdate := []*cdbm.Tenant{}
	pagedCtrlTenants := []*corev1.Tenant{}
	for i := 0; i < 34; i++ {
		ctrlTenant := &corev1.Tenant{
			OrganizationId: pagedTenants[i].Org,
			Metadata: &corev1.Metadata{
				Name: pagedTenants[i].Name,
			},
		}

		// Have an entries that are missing name
		if i%3 == 0 {
			if i < 20 {
				ctrlTenant.Metadata.Name = ""
				tenantsToUpdate = append(tenantsToUpdate, pagedTenants[i])
			}
		}
		pagedCtrlTenants = append(pagedCtrlTenants, ctrlTenant)
	}

	tenantsToCreate := pagedTenants[34:38]

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	// Mock UpdateTenant workflow from site-agent
	wrun := &tmocks.WorkflowRun{}
	wid := "test-workflow-id"
	wrun.On("GetID").Return(wid)

	mtc1 := &tmocks.Client{}
	mtc1.Mock.On("ExecuteWorkflow", context.Background(), mock.AnythingOfType("internal.StartWorkflowOptions"), "CreateTenant", mock.Anything).Return(wrun, nil)

	mtc2 := &tmocks.Client{}
	mtc2.Mock.On("ExecuteWorkflow", context.Background(), mock.AnythingOfType("internal.StartWorkflowOptions"), "UpdateTenant", mock.Anything).Return(wrun, nil)

	type fields struct {
		dbSession        *cdb.Session
		siteClientPool   *sc.ClientPool
		clientPoolClient *tmocks.Client
		env              *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx          context.Context
		siteID       uuid.UUID
		vpcInventory *corev1.TenantInventory
	}

	tests := []struct {
		name            string
		fields          fields
		args            args
		tenantsToCreate []*cdbm.Tenant
		tenantsToUpdate []*cdbm.Tenant
		wantErr         bool
	}{
		{
			name: "test Tenant inventory processing error, non-existent Site",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				vpcInventory: &corev1.TenantInventory{
					Tenants: []*corev1.Tenant{},
				},
			},
			wantErr: true,
		},
		{
			name: "test paged Tenant inventory processing, empty inventory",
			fields: fields{
				dbSession:        dbSession,
				siteClientPool:   tSiteClientPool,
				clientPoolClient: mtc1,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				vpcInventory: &corev1.TenantInventory{
					Tenants:         []*corev1.Tenant{},
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
				clientPoolClient: mtc2,
				env:              env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				vpcInventory: &corev1.TenantInventory{
					Tenants:   pagedCtrlTenants[0:20],
					Timestamp: timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  2,
						PageSize:    20,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			tenantsToCreate: []*cdbm.Tenant{},
			tenantsToUpdate: tenantsToUpdate,
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
				vpcInventory: &corev1.TenantInventory{
					Tenants:   pagedCtrlTenants[20:34],
					Timestamp: timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 2,
						TotalPages:  2,
						PageSize:    20,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			tenantsToCreate: tenantsToCreate,
			tenantsToUpdate: []*cdbm.Tenant{},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageTenant{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			mv.siteClientPool.IDClientMap[tt.args.siteID.String()] = tt.fields.clientPoolClient

			err := mv.UpdateTenantsInDB(tt.args.ctx, tt.args.siteID, tt.args.vpcInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			for _, tenant := range tt.tenantsToCreate {
				found := false
				for _, scCall := range tt.fields.clientPoolClient.Calls {
					scReq := scCall.Arguments[3].(*corev1.CreateTenantRequest)
					if scReq.OrganizationId == tenant.Org {
						found = true
					}
				}
				assert.True(t, found, fmt.Sprintf("CreateTenant workflow was not triggered for Tenant %v", tenant.Org))
			}

			for _, tenant := range tt.tenantsToUpdate {
				found := false
				for _, scCall := range tt.fields.clientPoolClient.Calls {
					scReq := scCall.Arguments[3].(*corev1.UpdateTenantRequest)
					if scReq.OrganizationId == tenant.Org {
						found = true
					}
				}
				assert.True(t, found, fmt.Sprintf("UpdateTenant workflow was not triggered for Tenant %v", tenant.Org))
			}
		})
	}
}

func TestNewManageTenant(t *testing.T) {
	type args struct {
		dbSession      *cdb.Session
		cfg            *config.Config
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
		want ManageTenant
	}{
		{
			name: "test new ManageTenant instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageTenant{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageTenant(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageTenant() = %v, want %v", got, tt.want)
			}
		})
	}
}
