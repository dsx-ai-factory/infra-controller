// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcpeering

import (
	"context"
	"fmt"
	"reflect"
	"sync"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	"os"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

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

func testVpcPeeringInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testVpcPeeringSetupSchema(t *testing.T, dbSession *cdb.Session) {
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	require.NoError(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	require.NoError(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.VpcPeering)(nil))
	assert.Nil(t, err)
}

func testVpcPeeringSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
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

func testVpcPeeringBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
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

func testVpcPeeringBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
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

func testVpcPeeringBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
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

// testVPCPeeringBuildVPC Building VPC in DB
func testVpcPeeringBuildVPC(
	t *testing.T,
	dbSession *cdb.Session,
	name string,
	ip *cdbm.InfrastructureProvider,
	tn *cdbm.Tenant,
	st *cdbm.Site,
	networkVirtualizationType *string,
	ct *uuid.UUID,
	lb map[string]string,
	user *cdbm.User,
	status string,
) *cdbm.Vpc {
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

func testVpcPeeringBuildVpcPeering(
	t *testing.T,
	dbSession *cdb.Session,
	vpc1ID uuid.UUID,
	vpc2ID uuid.UUID,
	siteID uuid.UUID,
	isMultiTenant bool,
	createdByID uuid.UUID,
) *cdbm.VpcPeering {
	vpcPeeringDAO := cdbm.NewVpcPeeringDAO(dbSession)

	vpcPeering, err := vpcPeeringDAO.Create(
		context.Background(),
		nil,
		cdbm.VpcPeeringCreateInput{
			Vpc1ID:        vpc1ID,
			Vpc2ID:        vpc2ID,
			SiteID:        siteID,
			IsMultiTenant: isMultiTenant,
			CreatedByID:   createdByID,
		},
	)
	assert.Nil(t, err)

	return vpcPeering
}

type vpcPeeringStatusDetailFailureHook struct {
	enabled      bool
	entityID     string
	statusUpdate *bun.QueryEvent
	insertErr    error
}

func (hook *vpcPeeringStatusDetailFailureHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !hook.enabled || event.Model == nil || event.Operation() != "INSERT" {
		return ctx
	}
	detail, ok := event.Model.Value().(*cdbm.StatusDetail)
	if !ok || detail.EntityID != hook.entityID {
		return ctx
	}

	// Cancel only this INSERT so the transaction's context still permits rollback.
	ctx, cancel := context.WithCancel(ctx)
	cancel()
	return ctx
}

func (hook *vpcPeeringStatusDetailFailureHook) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if !hook.enabled || event.Model == nil {
		return
	}
	switch model := event.Model.Value().(type) {
	case *cdbm.VpcPeering:
		if event.Operation() == "UPDATE" {
			hook.statusUpdate = event
		}
	case *cdbm.StatusDetail:
		if event.Operation() == "INSERT" && model.EntityID == hook.entityID {
			hook.insertErr = event.Err
		}
	}
}

func TestManageVpcPeering_UpdateVpcPeeringsInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testVpcPeeringInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)

	// Setup users, site, tenant, vpcs
	org := "test-org"
	roles := []string{"FORGE_PROVIDER_ADMIN"}
	user := testVpcPeeringBuildUser(t, dbSession, uuid.NewString(), org, roles)
	ip := testVpcPeeringSiteBuildInfrastructureProvider(t, dbSession, "test-provider", org, user)
	site := testVpcPeeringBuildSite(t, dbSession, ip, "test-site", user)
	site2 := testVpcPeeringBuildSite(t, dbSession, ip, "test-site-2", user)
	site3 := testVpcPeeringBuildSite(t, dbSession, ip, "test-site-3", user)
	site4 := testVpcPeeringBuildSite(t, dbSession, ip, "test-site-4", user)
	tenant := testVpcPeeringBuildTenant(t, dbSession, "test-tenant", org, user)

	// Build VPCs
	vpc1 := testVpcPeeringBuildVPC(t, dbSession, "vpc-1", ip, tenant, site, nil, nil, nil, user, "READY")
	assert.NotNil(t, vpc1)
	vpc2 := testVpcPeeringBuildVPC(t, dbSession, "vpc-2", ip, tenant, site, nil, nil, nil, user, "READY")
	assert.NotNil(t, vpc2)
	vpc3 := testVpcPeeringBuildVPC(t, dbSession, "vpc-3", ip, tenant, site, nil, nil, nil, user, "READY")
	assert.NotNil(t, vpc3)
	recoveredVpc1 := testVpcPeeringBuildVPC(t, dbSession, "recovered-vpc-1", ip, tenant, site4, nil, nil, nil, user, cdbm.VpcStatusReady)
	recoveredVpc2 := testVpcPeeringBuildVPC(t, dbSession, "recovered-vpc-2", ip, tenant, site4, nil, nil, nil, user, cdbm.VpcStatusReady)
	recoveredVpcPeeringID := uuid.New()

	// Create VPC Peerings in DB
	vp1 := testVpcPeeringBuildVpcPeering(t, dbSession, vpc1.ID, vpc2.ID, site.ID, false, user.ID)
	assert.NotNil(t, vp1)
	vp2 := testVpcPeeringBuildVpcPeering(t, dbSession, vpc2.ID, vpc1.ID, site.ID, false, user.ID)
	assert.NotNil(t, vp2)
	_, err := dbSession.DB.Exec("UPDATE vpc_peering SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), vp2.ID)
	assert.NoError(t, err)
	vp3 := testVpcPeeringBuildVpcPeering(t, dbSession, vpc1.ID, vpc3.ID, site.ID, false, user.ID)
	assert.NotNil(t, vp3)
	_, err = dbSession.DB.Exec("UPDATE vpc_peering SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), vp3.ID)
	assert.NoError(t, err)
	vp4 := testVpcPeeringBuildVpcPeering(t, dbSession, vpc2.ID, vpc3.ID, site.ID, false, user.ID)
	assert.NotNil(t, vp4)
	_, err = dbSession.DB.Exec("UPDATE vpc_peering SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), vp4.ID)
	assert.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	pagedVpcPeerings := []*cdbm.VpcPeering{}
	pagedInvIds := []string{}
	pagedCtrlVpcPeerings := []*corev1.VpcPeering{}

	paged_vpc1 := testVpcPeeringBuildVPC(t, dbSession, fmt.Sprintf("test-vpc-paged-%d", 0), ip, tenant, site3, nil, nil, nil, user, "READY")
	curr_vpc := paged_vpc1

	// Cloud has 38 VPC Peerings, 34 of which are in the inventory
	for i := 0; i < 38; i++ {
		prev_vpc := curr_vpc
		curr_vpc = testVpcPeeringBuildVPC(t, dbSession, fmt.Sprintf("test-vpc-paged-%d", i+1), ip, tenant, site3, nil, nil, nil, user, "READY")
		vpcPeering := testVpcPeeringBuildVpcPeering(t, dbSession, prev_vpc.ID, curr_vpc.ID, site3.ID, false, user.ID)

		mvp := NewManageVpcPeering(dbSession, nil)
		// Set status to Ready
		err = mvp.updateVpcPeeringStatusInDB(ctx, nil, vpcPeering.ID, nil, cutil.GetPtr(cdbm.VpcPeeringStatusReady), cutil.GetPtr("VPC Peering was created in DB from site inventory"))
		assert.NoError(t, err)
		// Set created to 2x inventory interval ago
		_, err := dbSession.DB.Exec("UPDATE vpc_peering SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), vpcPeering.ID)
		assert.NoError(t, err)

		if i < 34 {
			ctrlVpcPeering := &corev1.VpcPeering{
				Id:        &corev1.VpcPeeringId{Value: vpcPeering.ID.String()},
				VpcId:     &corev1.VpcId{Value: prev_vpc.ID.String()},
				PeerVpcId: &corev1.VpcId{Value: curr_vpc.ID.String()},
			}
			pagedCtrlVpcPeerings = append(pagedCtrlVpcPeerings, ctrlVpcPeering)
		}
		pagedVpcPeerings = append(pagedVpcPeerings, vpcPeering)
		pagedInvIds = append(pagedInvIds, vpcPeering.ID.String())
	}

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                 context.Context
		siteID              uuid.UUID
		vpcPeeringInventory *corev1.VPCPeeringInventory
	}

	tests := []struct {
		name                string
		fields              fields
		args                args
		prepare             func(*testing.T)
		run                 func(*testing.T, ManageVpcPeering, args)
		concurrentDeletion  bool
		readyVpcPeerings    []*cdbm.VpcPeering
		deletingVpcPeerings []*cdbm.VpcPeering
		deletedVpcPeerings  []*cdbm.VpcPeering
		wantErr             bool
	}{
		{
			name: "test Vpc Peering inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{},
				},
			},
			wantErr: true,
		},
		{
			name: "test Vpc Peering inventory processing success on full inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: site.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{
						{Id: &corev1.VpcPeeringId{Value: vp1.ID.String()}},
						{Id: &corev1.VpcPeeringId{Value: vp2.ID.String()}},
						{Id: &corev1.VpcPeeringId{Value: vp3.ID.String()}},
						{Id: &corev1.VpcPeeringId{Value: vp4.ID.String()}},
					},
				},
			},
			readyVpcPeerings: []*cdbm.VpcPeering{vp1, vp2, vp3, vp4},
			wantErr:          false,
		},
		{
			name: "test Vpc Peering inventory processing success on partial inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: site.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{
						{Id: &corev1.VpcPeeringId{Value: vp1.ID.String()}},
					},
				},
			},
			readyVpcPeerings:   []*cdbm.VpcPeering{vp1},
			deletedVpcPeerings: []*cdbm.VpcPeering{vp2, vp3, vp4},
			wantErr:            false,
		},
		{
			name: "test paged Vpc Peering inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: site2.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{},
					Timestamp:   timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  0,
						PageSize:    25,
						TotalItems:  0,
						ItemIds:     []string{},
					},
				},
			},
			readyVpcPeerings:   []*cdbm.VpcPeering{vp1},
			deletedVpcPeerings: []*cdbm.VpcPeering{vp2, vp3, vp4},
			wantErr:            false,
		},
		{
			name: "test paged Vpc Peering inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: site3.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: pagedCtrlVpcPeerings[0:10],
					Timestamp:   timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyVpcPeerings: pagedVpcPeerings[0:34],
			wantErr:          false,
		},
		{
			name: "test paged Vpc Peering inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: site3.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: pagedCtrlVpcPeerings[30:34],
					Timestamp:   timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
			readyVpcPeerings:   pagedVpcPeerings[0:34],
			deletedVpcPeerings: pagedVpcPeerings[34:38],
		},
		{
			name: "test Vpc Peering inventory auto creates missing object",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: site4.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{
						{
							Id:        &corev1.VpcPeeringId{Value: recoveredVpcPeeringID.String()},
							VpcId:     &corev1.VpcId{Value: recoveredVpc1.ID.String()},
							PeerVpcId: &corev1.VpcId{Value: recoveredVpc2.ID.String()},
						},
					},
				},
			},
			readyVpcPeerings: []*cdbm.VpcPeering{{ID: recoveredVpcPeeringID}},
		},
		{
			name:   "Core deletion moves a Ready VPC Peering to Deleting",
			fields: fields{dbSession: dbSession},
			prepare: func(t *testing.T) {
				require.NoError(t, cdbm.NewVpcPeeringDAO(dbSession).UpdateStatusByID(ctx, nil, vp1.ID, cdbm.VpcPeeringStatusReady))
			},
			args: args{
				ctx:    ctx,
				siteID: site.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{{
						Id:    &corev1.VpcPeeringId{Value: vp1.ID.String()},
						State: corev1.VpcPeeringState_VPC_PEERING_STATE_DELETING,
					}},
				},
			},
			deletingVpcPeerings: []*cdbm.VpcPeering{vp1},
		},
		{
			name:   "delayed Ready inventory does not cancel a local deletion",
			fields: fields{dbSession: dbSession},
			prepare: func(t *testing.T) {
				require.NoError(t, cdbm.NewVpcPeeringDAO(dbSession).UpdateStatusByID(ctx, nil, vp1.ID, cdbm.VpcPeeringStatusDeleting))
			},
			args: args{
				ctx:    ctx,
				siteID: site.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{{
						Id:    &corev1.VpcPeeringId{Value: vp1.ID.String()},
						State: corev1.VpcPeeringState_VPC_PEERING_STATE_READY,
					}},
				},
			},
			deletingVpcPeerings: []*cdbm.VpcPeering{vp1},
		},
		{
			name:   "Ready inventory preserves a deletion committed while its update waits",
			fields: fields{dbSession: dbSession},
			prepare: func(t *testing.T) {
				require.NoError(t, cdbm.NewVpcPeeringDAO(dbSession).UpdateStatusByID(ctx, nil, vp1.ID, cdbm.VpcPeeringStatusConfiguring))
			},
			args: args{
				ctx:    ctx,
				siteID: site.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{{
						Id:    &corev1.VpcPeeringId{Value: vp1.ID.String()},
						State: corev1.VpcPeeringState_VPC_PEERING_STATE_READY,
					}},
				},
			},
			concurrentDeletion:  true,
			deletingVpcPeerings: []*cdbm.VpcPeering{vp1},
		},
		{
			name:   "status detail failure rolls back Ready and permits inventory retry",
			fields: fields{dbSession: dbSession},
			prepare: func(t *testing.T) {
				mv := NewManageVpcPeering(dbSession, nil)
				require.NoError(t, mv.updateVpcPeeringStatusInDB(ctx, nil, vp1.ID, nil,
					cutil.GetPtr(cdbm.VpcPeeringStatusConfiguring), cutil.GetPtr("VPC Peering configuration is pending")))
			},
			args: args{
				ctx:    ctx,
				siteID: site.ID,
				vpcPeeringInventory: &corev1.VPCPeeringInventory{
					VpcPeerings: []*corev1.VpcPeering{{
						Id:    &corev1.VpcPeeringId{Value: vp1.ID.String()},
						State: corev1.VpcPeeringState_VPC_PEERING_STATE_READY,
					}},
				},
			},
			run: func(t *testing.T, mv ManageVpcPeering, args args) {
				vpcPeeringDAO := cdbm.NewVpcPeeringDAO(dbSession)
				statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)
				filter := cdbm.StatusDetailFilterInput{EntityIDs: []string{vp1.ID.String()}}
				page := cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}
				before, err := vpcPeeringDAO.GetByID(args.ctx, nil, vp1.ID, nil)
				require.NoError(t, err)
				require.Equal(t, cdbm.VpcPeeringStatusConfiguring, before.Status)
				detailsBefore, _, err := statusDetailDAO.GetAll(args.ctx, nil, filter, page)
				require.NoError(t, err)
				require.NotEmpty(t, detailsBefore)

				hook := &vpcPeeringStatusDetailFailureHook{enabled: true, entityID: vp1.ID.String()}
				defer func() { hook.enabled = false }()
				dbSession.DB.AddQueryHook(hook)
				err = mv.UpdateVpcPeeringsInDB(args.ctx, args.siteID, args.vpcPeeringInventory)
				hook.enabled = false

				// Inventory logs entry failures and returns nil, so check the query
				// error to prove the injected failure reached the database call.
				require.NoError(t, err)
				require.NotNil(t, hook.statusUpdate)
				require.NoError(t, hook.statusUpdate.Err)
				require.NotNil(t, hook.statusUpdate.Result)
				updatedRows, err := hook.statusUpdate.Result.RowsAffected()
				require.NoError(t, err)
				require.EqualValues(t, 1, updatedRows)
				require.ErrorIs(t, hook.insertErr, context.Canceled)

				afterFailure, err := vpcPeeringDAO.GetByID(args.ctx, nil, vp1.ID, nil)
				require.NoError(t, err)
				assert.Equal(t, before.Status, afterFailure.Status)
				assert.Equal(t, before.Updated, afterFailure.Updated)
				detailsAfterFailure, _, err := statusDetailDAO.GetAll(args.ctx, nil, filter, page)
				require.NoError(t, err)
				assert.ElementsMatch(t, detailsBefore, detailsAfterFailure)

				require.NoError(t, mv.UpdateVpcPeeringsInDB(args.ctx, args.siteID, args.vpcPeeringInventory))
				afterRetry, err := vpcPeeringDAO.GetByID(args.ctx, nil, vp1.ID, nil)
				require.NoError(t, err)
				assert.Equal(t, cdbm.VpcPeeringStatusReady, afterRetry.Status)
				detailsAfterRetry, _, err := statusDetailDAO.GetAll(args.ctx, nil, filter, page)
				require.NoError(t, err)
				require.Len(t, detailsAfterRetry, len(detailsBefore)+1)
				assert.ElementsMatch(t, detailsBefore, detailsAfterRetry[1:])
				assert.Equal(t, vp1.ID.String(), detailsAfterRetry[0].EntityID)
				assert.Equal(t, cdbm.VpcPeeringStatusReady, detailsAfterRetry[0].Status)
				assert.Equal(t, cutil.GetPtr("VPC Peering was found on Site"), detailsAfterRetry[0].Message)
				assert.Equal(t, 1, detailsAfterRetry[0].Count)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageVpcPeering{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}
			if tt.prepare != nil {
				tt.prepare(t)
			}
			if tt.run != nil {
				tt.run(t, mv, tt.args)
				return
			}

			var err error
			if tt.concurrentDeletion {
				ctx, cancel := context.WithTimeout(tt.args.ctx, 10*time.Second)
				defer cancel()
				statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)
				filter := cdbm.StatusDetailFilterInput{EntityIDs: []string{vp1.ID.String()}}
				_, detailsBefore, detailErr := statusDetailDAO.GetAll(ctx, nil, filter, cdbp.PageInput{})
				require.NoError(t, detailErr)

				// DELETE holds this row while waiting for Core. Inventory reads
				// the committed `Configuring` row, then waits on the Ready UPDATE.
				var activity sync.WaitGroup
				defer activity.Wait()
				tx, txErr := cdb.BeginTx(ctx, dbSession, nil)
				require.NoError(t, txErr)
				committed := false
				defer func() {
					if !committed {
						assert.NoError(t, tx.Rollback())
					}
				}()
				var writerPID int
				require.NoError(t, tx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &writerPID))
				require.NoError(t, cdbm.NewVpcPeeringDAO(dbSession).UpdateStatusByID(ctx, tx, vp1.ID, cdbm.VpcPeeringStatusDeleting))
				activity.Go(func() {
					err = mv.UpdateVpcPeeringsInDB(ctx, tt.args.siteID, tt.args.vpcPeeringInventory)
				})
				require.Eventually(t, func() bool {
					var waiters int
					queryErr := dbSession.DB.NewSelect().ColumnExpr("count(*)").
						TableExpr("pg_catalog.pg_stat_activity").
						Where("? = ANY(pg_blocking_pids(pid))", writerPID).
						Scan(ctx, &waiters)
					return queryErr == nil && waiters > 0
				}, 5*time.Second, 10*time.Millisecond, "inventory did not wait for the deletion")
				require.NoError(t, tx.Commit())
				committed = true
				activity.Wait()

				_, detailsAfter, detailErr := statusDetailDAO.GetAll(ctx, nil, filter, cdbp.PageInput{})
				require.NoError(t, detailErr)
				assert.Equal(t, detailsBefore, detailsAfter, "skipped Ready update must not add a status detail")
			} else {
				err = mv.UpdateVpcPeeringsInDB(tt.args.ctx, tt.args.siteID, tt.args.vpcPeeringInventory)
			}
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			vpcPeeringDAO := cdbm.NewVpcPeeringDAO(dbSession)

			for _, vp := range tt.readyVpcPeerings {
				ready, err := vpcPeeringDAO.GetByID(ctx, nil, vp.ID, nil)
				assert.NoError(t, err)
				assert.Equal(t, cdbm.VpcPeeringStatusReady, ready.Status)
			}

			for _, vp := range tt.deletedVpcPeerings {
				_, err := vpcPeeringDAO.GetByID(ctx, nil, vp.ID, nil)
				assert.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("VPC Peering %s should have been deleted", vp.ID))
			}
			for _, vp := range tt.deletingVpcPeerings {
				deleting, err := vpcPeeringDAO.GetByID(ctx, nil, vp.ID, nil)
				require.NoError(t, err)
				assert.Equal(t, cdbm.VpcPeeringStatusDeleting, deleting.Status)
				assert.Nil(t, deleting.Deleted)
			}
		})
	}
}

func TestManageVpcPeering_CreateOrUpdateVpcPeeringFromSite(t *testing.T) {
	type fixture struct {
		ctx                  context.Context
		dbSession            *cdb.Session
		site                 *cdbm.Site
		tenant               *cdbm.Tenant
		vpc1                 *cdbm.Vpc
		vpc2                 *cdbm.Vpc
		vpcPeeringID         uuid.UUID
		controllerVpcPeering *corev1.VpcPeering
		manager              ManageVpcPeering
	}

	newFixture := func(t *testing.T) *fixture {
		t.Helper()

		ctx := context.Background()
		dbSession := testVpcPeeringInitDB(t)
		t.Cleanup(dbSession.Close)
		testVpcPeeringSetupSchema(t, dbSession)

		org := "test-recovery-org"
		user := testVpcPeeringBuildUser(t, dbSession, uuid.NewString(), org, []string{"FORGE_PROVIDER_ADMIN"})
		infrastructureProvider := testVpcPeeringSiteBuildInfrastructureProvider(t, dbSession, "test-provider", org, user)
		site := testVpcPeeringBuildSite(t, dbSession, infrastructureProvider, "test-site", user)
		tenant := testVpcPeeringBuildTenant(t, dbSession, "test-tenant", org, user)
		vpc1 := testVpcPeeringBuildVPC(t, dbSession, "vpc-1", infrastructureProvider, tenant, site, nil, nil, nil, user, cdbm.VpcStatusReady)
		vpc2 := testVpcPeeringBuildVPC(t, dbSession, "vpc-2", infrastructureProvider, tenant, site, nil, nil, nil, user, cdbm.VpcStatusReady)
		vpcPeeringID := uuid.New()

		return &fixture{
			ctx:          ctx,
			dbSession:    dbSession,
			site:         site,
			tenant:       tenant,
			vpc1:         vpc1,
			vpc2:         vpc2,
			vpcPeeringID: vpcPeeringID,
			controllerVpcPeering: &corev1.VpcPeering{
				Id:        &corev1.VpcPeeringId{Value: vpcPeeringID.String()},
				VpcId:     &corev1.VpcId{Value: vpc1.ID.String()},
				PeerVpcId: &corev1.VpcId{Value: vpc2.ID.String()},
			},
			manager: NewManageVpcPeering(dbSession, nil),
		}
	}

	prepareDeletedPeering := func(t *testing.T, f *fixture) {
		t.Helper()
		dao := cdbm.NewVpcPeeringDAO(f.dbSession)
		_, err := dao.Create(f.ctx, nil, cdbm.VpcPeeringCreateInput{
			VpcPeeringID: &f.vpcPeeringID,
			Vpc1ID:       f.vpc1.ID,
			Vpc2ID:       f.vpc2.ID,
			SiteID:       f.site.ID,
			TenantID:     &f.tenant.ID,
			Status:       cdbm.VpcPeeringStatusDeleting,
			CreatedByID:  f.site.CreatedBy,
		})
		require.NoError(t, err)
		require.NoError(t, dao.Delete(f.ctx, nil, f.vpcPeeringID))

		// Recovery waits until the deletion is older than the inventory interval.
		cwu.TestInventoryAgeDeletedTimestamp(f.ctx, t, f.dbSession, (*cdbm.VpcPeering)(nil), f.vpcPeeringID)
	}

	tests := []struct {
		name              string
		prepare           func(*testing.T, *fixture)
		request           func(*fixture) *corev1.VpcPeering
		expectedRecovered bool
		reportedState     corev1.VpcPeeringState
		expectedStatus    string
		expectedMessage   string
	}{
		{
			name: "skips VPC Peering with missing VPC",
			request: func(f *fixture) *corev1.VpcPeering {
				return &corev1.VpcPeering{
					Id:        &corev1.VpcPeeringId{Value: uuid.NewString()},
					VpcId:     &corev1.VpcId{Value: uuid.NewString()},
					PeerVpcId: &corev1.VpcId{Value: f.vpc2.ID.String()},
				}
			},
		},
		{
			name:              "creates VPC Peering from Site inventory",
			expectedRecovered: true,
			expectedStatus:    cdbm.VpcPeeringStatusReady,
			expectedMessage:   "VPC Peering was found on Site",
		},
		{
			name:              "recovers a missing VPC Peering while Core is deleting it",
			expectedRecovered: true,
			reportedState:     corev1.VpcPeeringState_VPC_PEERING_STATE_DELETING,
			expectedStatus:    cdbm.VpcPeeringStatusDeleting,
		},
		{
			name:              "undeletes VPC Peering with the Deleting status reported by Core",
			prepare:           prepareDeletedPeering,
			expectedRecovered: true,
			reportedState:     corev1.VpcPeeringState_VPC_PEERING_STATE_DELETING,
			expectedStatus:    cdbm.VpcPeeringStatusDeleting,
		},
		{
			name:              "undeletes a Deleting VPC Peering as Ready when reported by Core",
			prepare:           prepareDeletedPeering,
			expectedRecovered: true,
			reportedState:     corev1.VpcPeeringState_VPC_PEERING_STATE_READY,
			expectedStatus:    cdbm.VpcPeeringStatusReady,
			expectedMessage:   "VPC Peering was found on Site",
		},
		{
			// The delete is newer than the interval, so this inventory may predate it and
			// undeleting would revive a VPC Peering the snapshot never saw removed.
			name: "does not undelete a VPC Peering deleted within the inventory interval",
			prepare: func(t *testing.T, f *fixture) {
				t.Helper()

				vpcPeeringDAO := cdbm.NewVpcPeeringDAO(f.dbSession)
				_, err := vpcPeeringDAO.Create(f.ctx, nil, cdbm.VpcPeeringCreateInput{
					VpcPeeringID: &f.vpcPeeringID,
					Vpc1ID:       f.vpc1.ID,
					Vpc2ID:       f.vpc2.ID,
					SiteID:       f.site.ID,
					TenantID:     &f.tenant.ID,
					Status:       cdbm.VpcPeeringStatusDeleting,
					CreatedByID:  f.site.CreatedBy,
				})
				require.NoError(t, err)
				require.NoError(t, vpcPeeringDAO.Delete(f.ctx, nil, f.vpcPeeringID))
			},
			expectedRecovered: false,
		},
		{
			// The REST create path answers a duplicate pair with 409, so recovery must not add a
			// second peering for the same VPCs under a different ID.
			name: "skips VPC Peering when another already connects the same VPCs",
			prepare: func(t *testing.T, f *fixture) {
				t.Helper()

				existingID := uuid.New()
				_, err := cdbm.NewVpcPeeringDAO(f.dbSession).Create(f.ctx, nil, cdbm.VpcPeeringCreateInput{
					VpcPeeringID: &existingID,
					Vpc1ID:       f.vpc1.ID,
					Vpc2ID:       f.vpc2.ID,
					SiteID:       f.site.ID,
					TenantID:     &f.tenant.ID,
					Status:       cdbm.VpcPeeringStatusReady,
					CreatedByID:  f.site.CreatedBy,
				})
				require.NoError(t, err)
			},
			expectedRecovered: false,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			f := newFixture(t)
			if test.prepare != nil {
				test.prepare(t, f)
			}

			request := f.controllerVpcPeering
			request.State = test.reportedState
			if test.request != nil {
				request = test.request(f)
			}

			recovered := f.manager.createOrUpdateVpcPeeringFromSite(f.ctx, f.site, request)
			if !test.expectedRecovered {
				assert.Nil(t, recovered)
				return
			}

			require.NotNil(t, recovered)
			assert.Equal(t, f.vpcPeeringID, recovered.ID)
			assert.Equal(t, f.vpc1.ID, recovered.Vpc1ID)
			assert.Equal(t, f.vpc2.ID, recovered.Vpc2ID)
			assert.Equal(t, f.site.ID, recovered.SiteID)
			assert.False(t, recovered.IsMultiTenant)
			assert.Nil(t, recovered.InfrastructureProviderID)
			require.NotNil(t, recovered.TenantID)
			assert.Equal(t, f.tenant.ID, *recovered.TenantID)
			assert.Equal(t, test.expectedStatus, recovered.Status)
			assert.Equal(t, f.site.CreatedBy, recovered.CreatedBy)
			assert.Nil(t, recovered.Deleted)
			if test.expectedMessage != "" {
				statusDetails, _, err := cdbm.NewStatusDetailDAO(f.dbSession).GetAll(f.ctx, nil,
					cdbm.StatusDetailFilterInput{EntityIDs: []string{recovered.ID.String()}}, cdbp.PageInput{})
				require.NoError(t, err)
				require.Len(t, statusDetails, 1)
				assert.Equal(t, test.expectedStatus, statusDetails[0].Status)
				require.NotNil(t, statusDetails[0].Message)
				assert.Equal(t, test.expectedMessage, *statusDetails[0].Message)
			}
		})
	}
}

func TestNewManageVpcPeering(t *testing.T) {
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
		want ManageVpcPeering
	}{
		{
			name: "test new ManageVpcPeering instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageVpcPeering{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageVpcPeering(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageVpcPeering() = %v, want %v", got, tt.want)
			}
		})
	}
}
