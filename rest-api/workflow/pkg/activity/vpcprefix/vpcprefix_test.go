// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import (
	"bytes"
	"context"
	"database/sql"
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	"os"

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

func testVpcPrefixInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testVPCPrefixSetupSchema(t *testing.T, dbSession *cdb.Session) {
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
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	// create VPCPrefix table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.VpcPrefix)(nil))
	assert.Nil(t, err)
	// ensure the shared IPAM schema exists even when another package reset the test DB
	ipamDB := cipam.NewBunStorage(dbSession.DB, nil)
	err = ipamDB.ApplyDbSchema()
	require.NoError(t, err)
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
func testVPCBuildVPC(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, tn *cdbm.Tenant, st *cdbm.Site, ct *uuid.UUID, lb map[string]string, user *cdbm.User, status string) *cdbm.Vpc {
	vpcDAO := cdbm.NewVpcDAO(dbSession)

	input := cdbm.VpcCreateInput{
		Name:                      name,
		Description:               cutil.GetPtr("Test VPC"),
		Org:                       tn.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
		ControllerVpcID:           ct,
		Labels:                    lb,
		Status:                    status,
		CreatedBy:                 *user,
	}

	vpc, err := vpcDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

// testVPCPrefixBuildIPBlock Building IPBlock in DB
func testVPCPrefixBuildIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, routingType, prefix string, blockSize int, protocolVersion string, fullGrant bool, status string, user *cdbm.User) *cdbm.IPBlock {
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)
	ipb, err := ipbDAO.Create(
		context.Background(),
		nil,
		cdbm.IPBlockCreateInput{
			Name:                     name,
			SiteID:                   site.ID,
			InfrastructureProviderID: ip.ID,
			TenantID:                 tenantID,
			RoutingType:              routingType,
			Prefix:                   prefix,
			PrefixLength:             blockSize,
			ProtocolVersion:          protocolVersion,
			FullGrant:                fullGrant,
			Status:                   status,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	return ipb
}

func testVPCBuildVPCPrefix(t *testing.T, dbSession *cdb.Session, name string, st *cdbm.Site, tenant *cdbm.Tenant, vpcID uuid.UUID, ipv4BlockID *uuid.UUID, prefix *string, prefixLength *int, status string, user *cdbm.User) *cdbm.VpcPrefix {
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)

	vpcprefix, err := vpcPrefixDAO.Create(context.Background(), nil, cdbm.VpcPrefixCreateInput{Name: name, TenantOrg: tenant.Org, SiteID: st.ID, VpcID: vpcID, TenantID: tenant.ID, IpBlockID: ipv4BlockID, Prefix: *prefix, PrefixLength: *prefixLength, Status: status, CreatedBy: user.ID})
	assert.Nil(t, err)

	return vpcprefix
}

func TestManageVpcPrefix_UpdateVpcPrefixesInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()

	testVPCPrefixSetupSchema(t, dbSession)

	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

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

	vpc1 := testVPCBuildVPC(t, dbSession, "test-vpc-1", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)

	ipb1 := testVPCPrefixBuildIPBlock(t, dbSession, "testipb", st, ip, &st.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu)
	assert.NotNil(t, ipb1)

	_, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb1.Prefix, ipb1.PrefixLength, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String())
	assert.Nil(t, err)

	vpcPrefix1 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-1", st, tn, vpc1.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	vpc2 := testVPCBuildVPC(t, dbSession, "test-vpc-2", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix2 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-2", st, tn, vpc2.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	vpc3 := testVPCBuildVPC(t, dbSession, "test-vpc-3", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix3 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-3", st, tn, vpc3.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusDeleting, tnu)

	vpc4 := testVPCBuildVPC(t, dbSession, "test-vpc-4", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix4 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-4", st, tn, vpc4.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusDeleting, tnu)

	// VPC Prefix 5 and 6 are missing and will be deleted
	prefix5, err := ipam.CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamStorage, ipb1, 28)
	assert.NoError(t, err)
	_, prefix5Len, err := ipam.ParseCidrIntoPrefixAndBlockSize(prefix5.Cidr)
	assert.NoError(t, err)

	vpc5 := testVPCBuildVPC(t, dbSession, "test-vpc-5", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix5 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-5", st, tn, vpc5.ID, &ipb1.ID, &prefix5.Cidr, &prefix5Len, cdbm.VpcPrefixStatusDeleting, tnu)
	vpcPrefix5.IPBlock = ipb1

	prefix6, err := ipam.CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamStorage, ipb1, 28)
	assert.NoError(t, err)
	_, prefix6Len, err := ipam.ParseCidrIntoPrefixAndBlockSize(prefix6.Cidr)
	assert.NoError(t, err)

	vpc6 := testVPCBuildVPC(t, dbSession, "test-vpc-6", ip, tn, st, nil, nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix6 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-6", st, tn, vpc6.ID, &ipb1.ID, &prefix6.Cidr, &prefix6Len, cdbm.VpcPrefixStatusDeleting, tnu)
	vpcPrefix6.IPBlock = ipb1

	vpc7 := testVPCBuildVPC(t, dbSession, "test-vpc-7", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix7 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-7", st, tn, vpc7.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE vpc_prefix SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), vpcPrefix7.ID.String())
	assert.NoError(t, err)

	vpc8 := testVPCBuildVPC(t, dbSession, "test-vpc-8", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix8 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-8", st, tn, vpc8.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	vpc9 := testVPCBuildVPC(t, dbSession, "test-vpc-9", ip, tn, st, nil, nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix9 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-9", st, tn, vpc9.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	vpc10 := testVPCBuildVPC(t, dbSession, "test-vpc-10", ip, tn, st, nil, nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix10 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-10", st, tn, vpc10.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusDeleting, tnu)

	vpc11 := testVPCBuildVPC(t, dbSession, "test-vpc-11", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix11 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-11", st, tn, vpc11.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	vpc12 := testVPCBuildVPC(t, dbSession, "test-vpc-12", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix12 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-12", st, tn, vpc12.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusProvisioning, tnu)

	vpc13 := testVPCBuildVPC(t, dbSession, "test-vpc-13", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix13 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-13", st, tn, vpc13.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)

	vpc14 := testVPCBuildVPC(t, dbSession, "test-vpc-14", ip, tn, st, cutil.GetPtr(uuid.New()), nil, tnu, cdbm.VpcStatusReady)
	vpcPrefix14 := testVPCBuildVPCPrefix(t, dbSession, "test-vpcprefix-14", st, tn, vpc14.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusDeleting, tnu)

	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE vpc_prefix SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), vpcPrefix11.ID.String())
	assert.NoError(t, err)

	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
	vpcPrefix8, err = vpcPrefixDAO.Update(ctx, nil, cdbm.VpcPrefixUpdateInput{VpcPrefixID: vpcPrefix8.ID, Status: cutil.GetPtr(cdbm.VpcStatusReady), IsMissingOnSite: cutil.GetPtr(true)})
	assert.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	// Build VpcPrefix inventory that is paginated
	// Generate data for 34 VpcPrefix reported from Site Agent while Cloud has 38 VpcPrefixes
	pagedVpcPrefixes := []*cdbm.VpcPrefix{}
	pagedInvIds := []string{}

	for i := 0; i < 38; i++ {
		vpc := testVPCBuildVPC(t, dbSession, fmt.Sprintf("test-vpc-paged-%d", i), ip, tn, st3, cutil.GetPtr(uuid.New()), map[string]string{}, tnu, cdbm.VpcStatusReady)
		vpcPrefix := testVPCBuildVPCPrefix(t, dbSession, fmt.Sprintf("test-vpc-prefix-paged-%d", i), st3, tn, vpc.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE vpc_prefix SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval*2)), vpcPrefix.ID.String())
		assert.NoError(t, err)
		pagedVpcPrefixes = append(pagedVpcPrefixes, vpcPrefix)
		pagedInvIds = append(pagedInvIds, vpcPrefix.ID.String())
	}

	pagedCtrlVpcPrefixes := []*corev1.VpcPrefix{}
	for i := 0; i < 34; i++ {
		ctrlVpcPrefix := &corev1.VpcPrefix{
			Id:   &corev1.VpcPrefixId{Value: pagedVpcPrefixes[i].ID.String()},
			Name: pagedVpcPrefixes[i].Name,
		}
		pagedCtrlVpcPrefixes = append(pagedCtrlVpcPrefixes, ctrlVpcPrefix)
	}

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                context.Context
		siteID             uuid.UUID
		vpcPrefixInventory *corev1.VpcPrefixInventory
	}

	tests := []struct {
		name                     string
		fields                   fields
		args                     args
		updatedVpcPrefix         *cdbm.VpcPrefix
		deletingVpcPrefixes      []*cdbm.VpcPrefix
		readyVpcPrefixes         []*cdbm.VpcPrefix
		deletedStatusVpcPrefixes []*cdbm.VpcPrefix
		deletedVpcPrefixes       []*cdbm.VpcPrefix
		missingVpcPrefixes       []*cdbm.VpcPrefix
		restoredVpcPrefixes      []*cdbm.VpcPrefix
		unpairedVpcPrefixes      []*cdbm.VpcPrefix
		wantErr                  bool
	}{
		{
			name: "test Vpc Prefix inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				vpcPrefixInventory: &corev1.VpcPrefixInventory{
					VpcPrefixes: []*corev1.VpcPrefix{},
				},
			},
			wantErr: true,
		},
		{
			name: "test Vpc Prefix inventory processing success",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				vpcPrefixInventory: &corev1.VpcPrefixInventory{
					VpcPrefixes: []*corev1.VpcPrefix{
						{
							Id:   &corev1.VpcPrefixId{Value: vpcPrefix1.ID.String()},
							Name: vpcPrefix1.ID.String(),
						},
						{
							Id:   &corev1.VpcPrefixId{Value: vpcPrefix2.ID.String()},
							Name: vpcPrefix2.ID.String(),
						},
						{
							Id:   &corev1.VpcPrefixId{Value: vpcPrefix3.ID.String()},
							Name: vpcPrefix3.ID.String(),
						},
						{
							Id:   &corev1.VpcPrefixId{Value: vpcPrefix4.ID.String()},
							Name: vpcPrefix4.ID.String(),
						},
						{
							Id:   &corev1.VpcPrefixId{Value: vpcPrefix8.ID.String()},
							Name: vpcPrefix8.ID.String(),
						},
						{
							Id:   &corev1.VpcPrefixId{Value: vpcPrefix9.ID.String()},
							Name: vpcPrefix9.ID.String(),
						},
						{
							Id:   &corev1.VpcPrefixId{Value: vpcPrefix10.ID.String()},
							Name: vpcPrefix10.ID.String(),
						},
						{
							Id:     &corev1.VpcPrefixId{Value: vpcPrefix12.ID.String()},
							Name:   vpcPrefix12.ID.String(),
							Status: &corev1.VpcPrefixStatus{TenantState: corev1.TenantState_READY},
						},
						{
							Id:     &corev1.VpcPrefixId{Value: vpcPrefix13.ID.String()},
							Name:   vpcPrefix13.ID.String(),
							Status: &corev1.VpcPrefixStatus{TenantState: corev1.TenantState_TERMINATING},
						},
						{
							Id:     &corev1.VpcPrefixId{Value: vpcPrefix14.ID.String()},
							Name:   vpcPrefix14.ID.String(),
							Status: &corev1.VpcPrefixStatus{TenantState: corev1.TenantState_TERMINATED},
						},
					},
				},
			},
			updatedVpcPrefix:         vpcPrefix12,
			deletingVpcPrefixes:      []*cdbm.VpcPrefix{vpcPrefix3, vpcPrefix4, vpcPrefix10, vpcPrefix13},
			deletedStatusVpcPrefixes: []*cdbm.VpcPrefix{vpcPrefix14},
			deletedVpcPrefixes:       []*cdbm.VpcPrefix{vpcPrefix5, vpcPrefix6},
			missingVpcPrefixes:       []*cdbm.VpcPrefix{vpcPrefix7, vpcPrefix11},
			restoredVpcPrefixes:      []*cdbm.VpcPrefix{vpcPrefix8},
			wantErr:                  false,
		},
		{
			name: "test paged Vpc Prefix inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				vpcPrefixInventory: &corev1.VpcPrefixInventory{
					VpcPrefixes:     []*corev1.VpcPrefix{},
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
			name: "test paged Vpc Prefix inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				vpcPrefixInventory: &corev1.VpcPrefixInventory{
					VpcPrefixes: pagedCtrlVpcPrefixes[0:10],
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
			readyVpcPrefixes: pagedVpcPrefixes[0:34],
		},
		{
			name: "test paged Vpc Prefix inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				vpcPrefixInventory: &corev1.VpcPrefixInventory{
					VpcPrefixes: pagedCtrlVpcPrefixes[30:34],
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
			readyVpcPrefixes:   pagedVpcPrefixes[0:34],
			missingVpcPrefixes: pagedVpcPrefixes[34:38],
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageVpcPrefix{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			err := mv.UpdateVpcPrefixesInDB(tt.args.ctx, tt.args.siteID, tt.args.vpcPrefixInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
			// Check that VPC Prefix status was updated in DB
			if tt.updatedVpcPrefix != nil {
				updatedVPCPrefix, _ := vpcPrefixDAO.GetByID(ctx, nil, tt.updatedVpcPrefix.ID, nil)
				assert.Equal(t, cdbm.VpcPrefixStatusReady, updatedVPCPrefix.Status)
			}

			for _, vpcPrefix := range tt.readyVpcPrefixes {
				rv, _ := vpcPrefixDAO.GetByID(ctx, nil, vpcPrefix.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.VpcPrefixStatusReady, rv.Status)
			}

			for _, vpcPrefix := range tt.deletingVpcPrefixes {
				rv, _ := vpcPrefixDAO.GetByID(ctx, nil, vpcPrefix.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.VpcPrefixStatusDeleting, rv.Status)
			}

			for _, vpcPrefix := range tt.deletedStatusVpcPrefixes {
				rv, _ := vpcPrefixDAO.GetByID(ctx, nil, vpcPrefix.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.VpcPrefixStatusDeleted, rv.Status)
			}

			for _, vpcPrefix := range tt.deletedVpcPrefixes {
				_, err = vpcPrefixDAO.GetByID(ctx, nil, vpcPrefix.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("VPC Prefix %s should have been deleted", vpcPrefix.Name))

				// Check that corresponding IPAM entry was removed
				if vpcPrefix.IPBlock.PrefixLength != vpcPrefix.PrefixLength {
					ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
					ipamer := cipam.NewWithStorage(ipamStorage)
					ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, ipb1.RoutingType, ipb1.InfrastructureProviderID.String(), ipb1.SiteID.String()))
					pref := ipamer.PrefixFrom(ctx, vpcPrefix.Prefix)
					assert.Nil(t, pref)
				}
			}

			for _, vpcprefix := range tt.missingVpcPrefixes {
				uv, _ := vpcPrefixDAO.GetByID(ctx, nil, vpcprefix.ID, nil)
				assert.True(t, uv.IsMissingOnSite)
			}

			for _, vpcprefix := range tt.restoredVpcPrefixes {
				rv, _ := vpcPrefixDAO.GetByID(ctx, nil, vpcprefix.ID, nil)
				assert.False(t, rv.IsMissingOnSite)
				assert.Equal(t, cdbm.VpcPrefixStatusReady, rv.Status)
			}

		})
	}

	regressionTests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "auto creates, replays, and restores VPC Prefixes",
			run:  testManageVpcPrefixUpdateVpcPrefixesInDBAutoCreatesAndRestores,
		},
	}
	for _, test := range regressionTests {
		t.Run(test.name, test.run)
	}
}

func TestNewManageVpcPrefix(t *testing.T) {
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
		want ManageVpcPrefix
	}{
		{
			name: "test new ManageVpcPrefix instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageVpcPrefix{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageVpcPrefix(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageVpcPrefix() = %v, want %v", got, tt.want)
			}
		})
	}
}

func testManageVpcPrefixUpdateVpcPrefixesInDBAutoCreatesAndRestores(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVPCPrefixSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testVPCBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testVPCBuildVPC(
		t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser, cdbm.VpcStatusReady,
	)
	controllerVpcID := parentVpc.ID
	ipBlock := testVPCPrefixBuildIPBlock(
		t, dbSession, "test-vpc-prefix-ip-block", site, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	_, err := ipam.CreateIpamEntryForIPBlock(
		ctx, ipamStorage, ipBlock.Prefix, ipBlock.PrefixLength, ipBlock.RoutingType,
		ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	)
	require.NoError(t, err)

	manager := ManageVpcPrefix{dbSession: dbSession}
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
	statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)

	controllerVpcPrefixID := uuid.MustParse("abcdef01-2345-4678-9abc-def012345678")
	controllerVpcPrefix := &corev1.VpcPrefix{
		Id:    &corev1.VpcPrefixId{Value: controllerVpcPrefixID.String()},
		VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
		Config: &corev1.VpcPrefixConfig{
			Prefix: "10.20.30.0/24",
		},
		Metadata: &corev1.Metadata{
			Name: "site-created-vpc-prefix",
		},
		Status: &corev1.VpcPrefixStatus{
			TenantState: corev1.TenantState_READY,
		},
	}
	inventory := &corev1.VpcPrefixInventory{
		VpcPrefixes: []*corev1.VpcPrefix{controllerVpcPrefix},
	}

	if !t.Run("auto creates VPC Prefix from inventory", func(t *testing.T) {
		var logOutput bytes.Buffer
		originalLogger := log.Logger
		log.Logger = zerolog.New(&logOutput)
		defer func() {
			log.Logger = originalLogger
		}()

		err := manager.UpdateVpcPrefixesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		assert.Contains(t, logOutput.String(), "created or undeleted VPC Prefix from Site inventory")

		created, err := vpcPrefixDAO.GetByID(ctx, nil, controllerVpcPrefixID, nil)
		require.NoError(t, err)
		assert.Equal(t, controllerVpcPrefixID, created.ID)
		assert.Equal(t, parentVpc.ID, created.VpcID)
		assert.Equal(t, tenant.ID, created.TenantID)
		assert.Equal(t, tenantOrg, created.Org)
		assert.Equal(t, site.ID, created.SiteID)
		assert.Equal(t, parentVpc.CreatedBy, created.CreatedBy)
		assert.Equal(t, cdbm.VpcPrefixStatusReady, created.Status)
		assert.Equal(t, "site-created-vpc-prefix", created.Name)
		assert.Equal(t, "10.20.30.0/24", created.Prefix)
		assert.Equal(t, 24, created.PrefixLength)
		require.NotNil(t, created.IPBlockID)
		assert.Equal(t, ipBlock.ID, *created.IPBlockID)

		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
		))
		assert.NotNil(t, ipamer.PrefixFrom(ctx, controllerVpcPrefix.Config.Prefix))

		statusDetails, _, statusErr := statusDetailDAO.GetAll(
			ctx,
			nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{created.ID.String()}},
			cdbp.PageInput{},
		)
		require.NoError(t, statusErr)
		require.NotEmpty(t, statusDetails)
		foundCreateMessage := false
		for i := range statusDetails {
			if statusDetails[i].Message != nil &&
				*statusDetails[i].Message == "VPC Prefix was found on Site, Ready for use" {
				foundCreateMessage = true
				break
			}
		}
		assert.True(t, foundCreateMessage)
	}) {
		t.FailNow()
	}

	if !t.Run("inventory replay is idempotent", func(t *testing.T) {
		err := manager.UpdateVpcPrefixesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		prefixes, count, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, prefixes, 1)
		assert.Equal(t, 1, count)
	}) {
		t.FailNow()
	}

	t.Run("uppercase inventory ID follows the recovery path", func(t *testing.T) {
		canonicalID := controllerVpcPrefix.Id.Value
		controllerVpcPrefix.Id.Value = strings.ToUpper(canonicalID)
		require.NotEqual(t, canonicalID, controllerVpcPrefix.Id.Value)
		defer func() {
			controllerVpcPrefix.Id.Value = canonicalID
		}()

		var logOutput bytes.Buffer
		originalLogger := log.Logger
		log.Logger = zerolog.New(&logOutput)
		defer func() {
			log.Logger = originalLogger
		}()

		err := manager.UpdateVpcPrefixesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)

		prefixes, count, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, prefixes, 1)
		assert.Equal(t, 1, count)
		assert.Contains(t, logOutput.String(), "created or undeleted VPC Prefix from Site inventory")
	})

	t.Run("inventory skips restore when Site reports TERMINATING", func(t *testing.T) {
		_, err := vpcPrefixDAO.Update(ctx, nil, cdbm.VpcPrefixUpdateInput{
			// Seed Deleting: that is the status the REST delete path leaves before soft-delete.
			VpcPrefixID:     controllerVpcPrefixID,
			Status:          cutil.GetPtr(cdbm.VpcPrefixStatusDeleting),
			IsMissingOnSite: cutil.GetPtr(true),
		})
		require.NoError(t, err)
		require.NoError(t, ipam.DeleteChildIpamEntryFromCidr(
			ctx, nil, dbSession, ipamStorage, ipBlock, controllerVpcPrefix.Config.Prefix,
		))
		require.NoError(t, vpcPrefixDAO.Delete(ctx, nil, controllerVpcPrefixID))
		// The undelete is deferred while the delete is newer than the staleness threshold, so
		// backdate it past that.
		cwu.TestInventoryAgeDeletedTimestamp(ctx, t, dbSession, (*cdbm.VpcPrefix)(nil), controllerVpcPrefixID)

		deleted, _, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)
		assert.Equal(t, cdbm.VpcPrefixStatusDeleting, deleted[0].Status)
		deletedAt := *deleted[0].Deleted

		terminatingInventory := &corev1.VpcPrefixInventory{
			VpcPrefixes: []*corev1.VpcPrefix{
				{
					Id:    &corev1.VpcPrefixId{Value: controllerVpcPrefixID.String()},
					VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
					Config: &corev1.VpcPrefixConfig{
						Prefix: controllerVpcPrefix.Config.Prefix,
					},
					Metadata: &corev1.Metadata{
						Name: "site-created-vpc-prefix",
					},
					Status: &corev1.VpcPrefixStatus{
						TenantState: corev1.TenantState_TERMINATING,
					},
				},
			},
		}
		err = manager.UpdateVpcPrefixesInDB(ctx, site.ID, terminatingInventory)
		require.NoError(t, err)

		stillDeleted, _, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, stillDeleted, 1)
		require.NotNil(t, stillDeleted[0].Deleted)
		assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)
		assert.Equal(t, cdbm.VpcPrefixStatusDeleting, stillDeleted[0].Status)

		_, err = vpcPrefixDAO.GetByID(ctx, nil, controllerVpcPrefixID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
		))
		assert.Nil(t, ipamer.PrefixFrom(ctx, controllerVpcPrefix.Config.Prefix))
	})

	t.Run("inventory restores soft-deleted VPC Prefix", func(t *testing.T) {
		var logOutput bytes.Buffer
		originalLogger := log.Logger
		log.Logger = zerolog.New(&logOutput)
		defer func() {
			log.Logger = originalLogger
		}()

		// Depends on the preceding TERMINATING subtest leaving the row soft-deleted with
		// Status=Deleting. Do not soft-delete here, that would hide failures in the prior case.
		existing, _, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, existing, 1)
		require.NotNil(t, existing[0].Deleted, "expected the prefix to be soft-deleted by the preceding subtest")
		require.Equal(t, cdbm.VpcPrefixStatusDeleting, existing[0].Status)

		deleted, _, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)
		assert.Equal(t, cdbm.VpcPrefixStatusDeleting, deleted[0].Status)

		err = manager.UpdateVpcPrefixesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		assert.Contains(t, logOutput.String(), "created or undeleted VPC Prefix from Site inventory")

		restored, err := vpcPrefixDAO.GetByID(ctx, nil, controllerVpcPrefixID, nil)
		require.NoError(t, err)
		assert.Nil(t, restored.Deleted)
		assert.False(t, restored.IsMissingOnSite)
		assert.Equal(t, cdbm.VpcPrefixStatusReady, restored.Status)
		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
		))
		assert.NotNil(t, ipamer.PrefixFrom(ctx, controllerVpcPrefix.Config.Prefix))

		statusDetails, _, statusErr := statusDetailDAO.GetAll(
			ctx,
			nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{restored.ID.String()}},
			cdbp.PageInput{},
		)
		require.NoError(t, statusErr)
		foundReadyMessage := false
		for i := range statusDetails {
			if statusDetails[i].Message != nil &&
				*statusDetails[i].Message == "VPC Prefix is ready for use" {
				foundReadyMessage = true
				break
			}
		}
		assert.True(t, foundReadyMessage)
	})

	t.Run("inventory skips restore when tenant organization differs", func(t *testing.T) {
		require.NoError(t, ipam.DeleteChildIpamEntryFromCidr(
			ctx, nil, dbSession, ipamStorage, ipBlock, controllerVpcPrefix.Config.Prefix,
		))
		require.NoError(t, vpcPrefixDAO.Delete(ctx, nil, controllerVpcPrefixID))
		deleted, _, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)
		deletedAt := *deleted[0].Deleted

		_, err = dbSession.DB.NewUpdate().
			Model((*cdbm.VpcPrefix)(nil)).
			Set("org = ?", "other-tenant-org").
			Where("id = ?", controllerVpcPrefixID).
			WhereAllWithDeleted().
			Exec(ctx)
		require.NoError(t, err)

		err = manager.UpdateVpcPrefixesInDB(ctx, site.ID, inventory)
		require.NoError(t, err)

		stillDeleted, _, err := vpcPrefixDAO.GetAll(
			ctx,
			nil,
			cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{controllerVpcPrefixID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, stillDeleted, 1)
		require.NotNil(t, stillDeleted[0].Deleted)
		assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)

		_, err = vpcPrefixDAO.GetByID(ctx, nil, controllerVpcPrefixID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})
}

func TestManageVpcPrefix_CreateOrUpdateVpcPrefixFromSite(t *testing.T) {
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVPCPrefixSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)

	authorizedTenantOrg := "test-authorized-tenant"
	authorizedTenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), authorizedTenantOrg, []string{"FORGE_TENANT_ADMIN"})
	authorizedTenant := testVPCBuildTenant(t, dbSession, "test-authorized-tenant", authorizedTenantOrg, authorizedTenantUser)

	parentVpc := testVPCBuildVPC(
		t, dbSession, "parent-vpc", provider, authorizedTenant, site, nil, nil, authorizedTenantUser, cdbm.VpcStatusReady,
	)
	controllerVpcID := parentVpc.ID
	ipBlock := testVPCPrefixBuildIPBlock(
		t, dbSession, "test-vpc-prefix-ip-block", site, provider, &authorizedTenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.0.0.0", 8,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, authorizedTenantUser,
	)
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	_, err := ipam.CreateIpamEntryForIPBlock(
		ctx, ipamStorage, ipBlock.Prefix, ipBlock.PrefixLength, ipBlock.RoutingType,
		ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	)
	require.NoError(t, err)
	ipamer := cipam.NewWithStorage(ipamStorage)
	ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
		ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	))
	_, err = ipamer.AcquireSpecificChildPrefix(ctx, "10.0.0.0/8", "10.5.0.0/24")
	require.NoError(t, err)
	existingPrefix := testVPCBuildVPCPrefix(
		t, dbSession, "existing-name", site, authorizedTenant, parentVpc.ID, nil,
		cutil.GetPtr("10.0.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, authorizedTenantUser,
	)

	manager := ManageVpcPrefix{dbSession: dbSession}

	tests := []struct {
		name                string
		controllerVpcPrefix *corev1.VpcPrefix
		wantVpcPrefix       bool
		wantName            string
		wantNamePref        string
	}{
		{
			name: "unknown parent VPC is rejected",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:    &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId: &corev1.VpcId{Value: uuid.NewString()},
				Config: &corev1.VpcPrefixConfig{
					Prefix: "10.1.0.0/24",
				},
				Metadata: &corev1.Metadata{Name: "unknown-vpc-prefix"},
			},
		},
		{
			name: "invalid controller VPC Prefix ID",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:    &corev1.VpcPrefixId{Value: "not-a-uuid"},
				VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
				Config: &corev1.VpcPrefixConfig{
					Prefix: "10.1.0.0/24",
				},
				Metadata: &corev1.Metadata{Name: "invalid-id-prefix"},
			},
		},
		{
			name: "empty prefix is rejected",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:       &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId:    &corev1.VpcId{Value: controllerVpcID.String()},
				Config:   &corev1.VpcPrefixConfig{Prefix: ""},
				Metadata: &corev1.Metadata{Name: "missing-prefix"},
			},
		},
		{
			name: "invalid prefix CIDR is rejected",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:       &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId:    &corev1.VpcId{Value: controllerVpcID.String()},
				Config:   &corev1.VpcPrefixConfig{Prefix: "not-a-cidr"},
				Metadata: &corev1.Metadata{Name: "bad-cidr-prefix"},
			},
		},
		{
			name: "empty VPC ID is rejected",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:       &corev1.VpcPrefixId{Value: uuid.NewString()},
				Config:   &corev1.VpcPrefixConfig{Prefix: "10.1.0.0/24"},
				Metadata: &corev1.Metadata{Name: "missing-vpc-id"},
			},
		},
		{
			name: "no containing IP Block is rejected",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:       &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId:    &corev1.VpcId{Value: controllerVpcID.String()},
				Config:   &corev1.VpcPrefixConfig{Prefix: "192.168.1.0/24"},
				Metadata: &corev1.Metadata{Name: "no-ip-block-prefix"},
			},
		},
		{
			name: "already allocated Site CIDR is rejected",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:       &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId:    &corev1.VpcId{Value: controllerVpcID.String()},
				Config:   &corev1.VpcPrefixConfig{Prefix: "10.5.0.0/24"},
				Metadata: &corev1.Metadata{Name: "allocated-prefix"},
			},
		},
		{
			name: "renames VPC Prefix when active name already exists",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:    &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
				Config: &corev1.VpcPrefixConfig{
					Prefix: "10.2.0.0/24",
				},
				Metadata: &corev1.Metadata{Name: existingPrefix.Name},
			},
			wantVpcPrefix: true,
			wantNamePref:  existingPrefix.Name + "-recovered-",
		},
		{
			name: "assigns recovered name when metadata name is empty",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:    &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
				Config: &corev1.VpcPrefixConfig{
					Prefix: "10.3.0.0/24",
				},
				Metadata: &corev1.Metadata{Name: ""},
			},
			wantVpcPrefix: true,
			wantNamePref:  "recovered-",
		},
		{
			name: "creates VPC Prefix using parent VPC REST ID as site VPC ID",
			controllerVpcPrefix: &corev1.VpcPrefix{
				Id:    &corev1.VpcPrefixId{Value: uuid.NewString()},
				VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
				Config: &corev1.VpcPrefixConfig{
					Prefix: "10.4.0.0/24",
				},
				Metadata: &corev1.Metadata{Name: "rest-id-parent-vpc-prefix"},
			},
			wantVpcPrefix: true,
			wantName:      "rest-id-parent-vpc-prefix",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vpcPrefix := manager.createOrUpdateVpcPrefixFromSite(ctx, site, tt.controllerVpcPrefix)
			if tt.wantVpcPrefix {
				require.NotNil(t, vpcPrefix)
				assert.Equal(t, cdbm.VpcPrefixStatusReady, vpcPrefix.Status)
				assert.Equal(t, authorizedTenant.ID, vpcPrefix.TenantID)
				assert.Equal(t, parentVpc.ID, vpcPrefix.VpcID)
				require.NotNil(t, vpcPrefix.IPBlockID)
				assert.Equal(t, ipBlock.ID, *vpcPrefix.IPBlockID)
				assert.NotNil(t, ipamer.PrefixFrom(ctx, tt.controllerVpcPrefix.Config.Prefix))
				if tt.wantName != "" {
					assert.Equal(t, tt.wantName, vpcPrefix.Name)
				}
				if tt.wantNamePref != "" {
					assert.True(t, len(vpcPrefix.Name) > len(tt.wantNamePref))
					assert.Equal(t, tt.wantNamePref, vpcPrefix.Name[:len(tt.wantNamePref)])
				}
			} else {
				assert.Nil(t, vpcPrefix)
			}
		})
	}

	storedIPBlockTests := []struct {
		name                    string
		storedIPBlockStatus     string
		createMoreSpecificBlock bool
		expectedRestore         bool
	}{
		{
			name:                    "undelete ignores a newer more-specific IP Block",
			storedIPBlockStatus:     cdbm.IPBlockStatusReady,
			createMoreSpecificBlock: true,
			expectedRestore:         true,
		},
		{
			name:                "undelete skips a stored IP Block that is not Ready",
			storedIPBlockStatus: cdbm.IPBlockStatusError,
		},
	}

	for _, test := range storedIPBlockTests {
		t.Run(test.name, func(t *testing.T) {
			testCtx := context.Background()
			testDBSession := testVpcPrefixInitDB(t)
			defer testDBSession.Close()
			testVPCPrefixSetupSchema(t, testDBSession)

			testProviderOrg := "test-provider-org"
			testProviderUser := testVPCBuildUser(t, testDBSession, uuid.NewString(), testProviderOrg, []string{"FORGE_PROVIDER_ADMIN"})
			testProvider := testVPCSiteBuildInfrastructureProvider(t, testDBSession, "test-provider", testProviderOrg, testProviderUser)
			testTenantOrg := "test-tenant-org"
			testTenantUser := testVPCBuildUser(t, testDBSession, uuid.NewString(), testTenantOrg, []string{"FORGE_TENANT_ADMIN"})
			testTenant := testVPCBuildTenant(t, testDBSession, "test-tenant", testTenantOrg, testTenantUser)
			testSite := testVPCBuildSite(t, testDBSession, testProvider, "test-site", testProviderUser)
			testParentVpc := testVPCBuildVPC(
				t, testDBSession, "parent-vpc", testProvider, testTenant, testSite, nil, nil, testTenantUser, cdbm.VpcStatusReady,
			)
			storedIPBlock := testVPCPrefixBuildIPBlock(
				t, testDBSession, "stored-ip-block", testSite, testProvider, &testTenant.ID,
				cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
				cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, testTenantUser,
			)

			testIPAMStorage := ipam.NewIpamStorage(testDBSession.DB, nil)
			_, err := ipam.CreateIpamEntryForIPBlock(
				testCtx, testIPAMStorage, storedIPBlock.Prefix, storedIPBlock.PrefixLength,
				storedIPBlock.RoutingType, storedIPBlock.InfrastructureProviderID.String(),
				storedIPBlock.SiteID.String(),
			)
			require.NoError(t, err)

			controllerVpcPrefixID := uuid.New()
			testVpcPrefixDAO := cdbm.NewVpcPrefixDAO(testDBSession)
			_, err = testVpcPrefixDAO.Create(testCtx, nil, cdbm.VpcPrefixCreateInput{
				VpcPrefixID:  &controllerVpcPrefixID,
				Name:         "stored-ip-block-prefix",
				TenantOrg:    testTenant.Org,
				SiteID:       testSite.ID,
				VpcID:        testParentVpc.ID,
				TenantID:     testTenant.ID,
				IpBlockID:    &storedIPBlock.ID,
				Prefix:       "10.20.30.0/24",
				PrefixLength: 24,
				Status:       cdbm.VpcPrefixStatusDeleting,
				CreatedBy:    testTenantUser.ID,
			})
			require.NoError(t, err)
			err = testVpcPrefixDAO.Delete(testCtx, nil, controllerVpcPrefixID)
			require.NoError(t, err)
			// The undelete is deferred while the delete is newer than the staleness threshold,
			// so backdate it past that.
			cwu.TestInventoryAgeDeletedTimestamp(testCtx, t, testDBSession, (*cdbm.VpcPrefix)(nil), controllerVpcPrefixID)

			if test.storedIPBlockStatus != cdbm.IPBlockStatusReady {
				_, err = cdbm.NewIPBlockDAO(testDBSession).Update(testCtx, nil, cdbm.IPBlockUpdateInput{
					IPBlockID: storedIPBlock.ID,
					Status:    &test.storedIPBlockStatus,
				})
				require.NoError(t, err)
			}
			if test.createMoreSpecificBlock {
				testVPCPrefixBuildIPBlock(
					t, testDBSession, "more-specific-ip-block", testSite, testProvider, &testTenant.ID,
					cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.16.0", 20,
					cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, testTenantUser,
				)
			}

			controllerVpcPrefix := &corev1.VpcPrefix{
				Id:    &corev1.VpcPrefixId{Value: controllerVpcPrefixID.String()},
				VpcId: &corev1.VpcId{Value: testParentVpc.ID.String()},
				Config: &corev1.VpcPrefixConfig{
					Prefix: "10.20.30.0/24",
				},
				Metadata: &corev1.Metadata{Name: "stored-ip-block-prefix"},
				Status: &corev1.VpcPrefixStatus{
					TenantState: corev1.TenantState_READY,
				},
			}

			testManager := ManageVpcPrefix{dbSession: testDBSession}
			restored := testManager.createOrUpdateVpcPrefixFromSite(testCtx, testSite, controllerVpcPrefix)
			if !test.expectedRestore {
				assert.Nil(t, restored)
				deleted, _, getErr := testVpcPrefixDAO.GetAll(
					testCtx,
					nil,
					cdbm.VpcPrefixFilterInput{
						VpcPrefixIDs:   []uuid.UUID{controllerVpcPrefixID},
						IncludeDeleted: true,
					},
					cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
					nil,
				)
				require.NoError(t, getErr)
				require.Len(t, deleted, 1)
				assert.NotNil(t, deleted[0].Deleted)
				return
			}
			require.NotNil(t, restored)
			require.NotNil(t, restored.IPBlockID)
			assert.Equal(t, storedIPBlock.ID, *restored.IPBlockID)

			testIPAMer := cipam.NewWithStorage(testIPAMStorage)
			testIPAMer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
				testCtx, storedIPBlock.RoutingType, storedIPBlock.InfrastructureProviderID.String(),
				storedIPBlock.SiteID.String(),
			))
			assert.NotNil(t, testIPAMer.PrefixFrom(testCtx, controllerVpcPrefix.Config.Prefix))
		})
	}

	regressionTests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "rejects host bits in an equal-length CIDR",
			run:  testCreateOrUpdateVpcPrefixRejectsHostBitsEqualLengthCIDR,
		},
		{
			name: "skips a fully granted IP Block",
			run:  testCreateOrUpdateVpcPrefixSkipsFullGrantIPBlock,
		},
		{
			name: "skips undelete when the stored Prefix differs",
			run:  testCreateOrUpdateVpcPrefixSkipsRestoreWhenPrefixDiffers,
		},
		{
			name: "skips a controller ID owned by another Site",
			run:  testCreateOrUpdateVpcPrefixSkipsIDOwnedByDifferentSite,
		},
	}

	for _, test := range regressionTests {
		t.Run(test.name, test.run)
	}
}

func testCreateOrUpdateVpcPrefixRejectsHostBitsEqualLengthCIDR(t *testing.T) {
	// Regression: a non-canonical equal-length CIDR (host bits set) must not leave the
	// parent IPBlock with FullGrant=true after a soft-skip commits the transaction.
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVPCPrefixSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testVPCBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testVPCBuildVPC(
		t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser, cdbm.VpcStatusReady,
	)
	ipBlock := testVPCPrefixBuildIPBlock(
		t, dbSession, "test-full-grant-ip-block", site, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	_, err := ipam.CreateIpamEntryForIPBlock(
		ctx, ipamStorage, ipBlock.Prefix, ipBlock.PrefixLength, ipBlock.RoutingType,
		ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	)
	require.NoError(t, err)

	hostBitsVpcPrefixID := uuid.New()
	manager := ManageVpcPrefix{dbSession: dbSession}
	created := manager.createOrUpdateVpcPrefixFromSite(ctx, site, &corev1.VpcPrefix{
		Id:    &corev1.VpcPrefixId{Value: hostBitsVpcPrefixID.String()},
		VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
		Config: &corev1.VpcPrefixConfig{
			// netip.ParsePrefix accepts this; Masked() form is 10.20.0.0/16.
			Prefix: "10.20.0.1/16",
		},
		Metadata: &corev1.Metadata{Name: "host-bits-vpc-prefix"},
		Status:   &corev1.VpcPrefixStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
	_, err = vpcPrefixDAO.GetByID(ctx, nil, hostBitsVpcPrefixID, nil)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

	ipBlockDAO := cdbm.NewIPBlockDAO(dbSession)
	reloadedIPBlock, err := ipBlockDAO.GetByID(ctx, nil, ipBlock.ID, nil)
	require.NoError(t, err)
	assert.False(t, reloadedIPBlock.FullGrant)

	// A normal REST-style full-grant create on the same IPBlock must still succeed.
	childPrefix, err := ipam.CreateChildIpamEntryForIPBlock(
		ctx, nil, dbSession, ipamStorage, reloadedIPBlock, reloadedIPBlock.PrefixLength,
	)
	require.NoError(t, err)
	assert.Equal(t, "10.20.0.0/16", childPrefix.Cidr)
	reloadedIPBlock, err = ipBlockDAO.GetByID(ctx, nil, ipBlock.ID, nil)
	require.NoError(t, err)
	assert.True(t, reloadedIPBlock.FullGrant)
}

func testCreateOrUpdateVpcPrefixSkipsFullGrantIPBlock(t *testing.T) {
	// Regression: FullGrant lives only in the REST DB, so the ipam library reports a fully
	// granted IPBlock as empty. Recovery must not carve a child CIDR out of it.
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVPCPrefixSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testVPCBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testVPCBuildVPC(
		t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser, cdbm.VpcStatusReady,
	)
	ipBlock := testVPCPrefixBuildIPBlock(
		t, dbSession, "test-full-grant-ip-block", site, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	_, err := ipam.CreateIpamEntryForIPBlock(
		ctx, ipamStorage, ipBlock.Prefix, ipBlock.PrefixLength, ipBlock.RoutingType,
		ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	)
	require.NoError(t, err)

	// Full-grant the whole IPBlock the way the REST create path does.
	_, err = ipam.CreateChildIpamEntryForIPBlock(
		ctx, nil, dbSession, ipamStorage, ipBlock, ipBlock.PrefixLength,
	)
	require.NoError(t, err)
	ipBlockDAO := cdbm.NewIPBlockDAO(dbSession)
	fullGrantedIPBlock, err := ipBlockDAO.GetByID(ctx, nil, ipBlock.ID, nil)
	require.NoError(t, err)
	require.True(t, fullGrantedIPBlock.FullGrant)

	childVpcPrefixID := uuid.New()
	manager := ManageVpcPrefix{dbSession: dbSession}
	created := manager.createOrUpdateVpcPrefixFromSite(ctx, site, &corev1.VpcPrefix{
		Id:    &corev1.VpcPrefixId{Value: childVpcPrefixID.String()},
		VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
		Config: &corev1.VpcPrefixConfig{
			Prefix: "10.20.30.0/24",
		},
		Metadata: &corev1.Metadata{Name: "full-grant-child-vpc-prefix"},
		Status:   &corev1.VpcPrefixStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
	_, err = vpcPrefixDAO.GetByID(ctx, nil, childVpcPrefixID, nil)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

	ipamer := cipam.NewWithStorage(ipamStorage)
	ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
		ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	))
	assert.Nil(t, ipamer.PrefixFrom(ctx, "10.20.30.0/24"))

	reloadedIPBlock, err := ipBlockDAO.GetByID(ctx, nil, ipBlock.ID, nil)
	require.NoError(t, err)
	assert.True(t, reloadedIPBlock.FullGrant)
}

func testCreateOrUpdateVpcPrefixSkipsRestoreWhenPrefixDiffers(t *testing.T) {
	// Soft-deleted row stores 10.20.30.0/24; Site reports the same controller UUID with a
	// different CIDR in the same IPBlock. Undelete must not acquire the reported CIDR.
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVPCPrefixSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testVPCBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testVPCBuildVPC(
		t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser, cdbm.VpcStatusReady,
	)
	ipBlock := testVPCPrefixBuildIPBlock(
		t, dbSession, "test-prefix-mismatch-ip-block", site, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	_, err := ipam.CreateIpamEntryForIPBlock(
		ctx, ipamStorage, ipBlock.Prefix, ipBlock.PrefixLength, ipBlock.RoutingType,
		ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	)
	require.NoError(t, err)

	storedCIDR := "10.20.30.0/24"
	reportedCIDR := "10.20.31.0/24"
	vpcPrefixID := uuid.New()
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
	_, err = vpcPrefixDAO.Create(ctx, nil, cdbm.VpcPrefixCreateInput{
		VpcPrefixID:  &vpcPrefixID,
		Name:         "stored-vpc-prefix",
		TenantOrg:    tenant.Org,
		SiteID:       site.ID,
		VpcID:        parentVpc.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipBlock.ID,
		Prefix:       storedCIDR,
		PrefixLength: 24,
		Status:       cdbm.VpcPrefixStatusDeleting,
		CreatedBy:    tenantUser.ID,
	})
	require.NoError(t, err)
	require.NoError(t, vpcPrefixDAO.Delete(ctx, nil, vpcPrefixID))

	deleted, _, err := vpcPrefixDAO.GetAll(
		ctx,
		nil,
		cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{vpcPrefixID}, IncludeDeleted: true},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	require.NoError(t, err)
	require.Len(t, deleted, 1)
	require.NotNil(t, deleted[0].Deleted)
	deletedAt := *deleted[0].Deleted

	manager := ManageVpcPrefix{dbSession: dbSession}
	created := manager.createOrUpdateVpcPrefixFromSite(ctx, site, &corev1.VpcPrefix{
		Id:    &corev1.VpcPrefixId{Value: vpcPrefixID.String()},
		VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
		Config: &corev1.VpcPrefixConfig{
			Prefix: reportedCIDR,
		},
		Metadata: &corev1.Metadata{Name: "drifted-vpc-prefix"},
		Status:   &corev1.VpcPrefixStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	stillDeleted, _, err := vpcPrefixDAO.GetAll(
		ctx,
		nil,
		cdbm.VpcPrefixFilterInput{VpcPrefixIDs: []uuid.UUID{vpcPrefixID}, IncludeDeleted: true},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	require.NoError(t, err)
	require.Len(t, stillDeleted, 1)
	require.NotNil(t, stillDeleted[0].Deleted)
	assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)
	assert.Equal(t, storedCIDR, stillDeleted[0].Prefix)

	_, err = vpcPrefixDAO.GetByID(ctx, nil, vpcPrefixID, nil)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

	ipamer := cipam.NewWithStorage(ipamStorage)
	ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
		ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	))
	assert.Nil(t, ipamer.PrefixFrom(ctx, reportedCIDR))
	assert.Nil(t, ipamer.PrefixFrom(ctx, storedCIDR))
}

func TestManageVpcPrefix_DeleteVpcPrefixFromDB(t *testing.T) {
	tests := []struct {
		name          string
		clearRelation bool
		expectedError bool
	}{
		{
			name: "deletes with a loaded IP Block relation",
		},
		{
			name:          "rejects a missing IP Block relation",
			clearRelation: true,
			expectedError: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			ctx := context.Background()
			dbSession := testVpcPrefixInitDB(t)
			defer dbSession.Close()
			testVPCPrefixSetupSchema(t, dbSession)

			providerOrg := "test-provider-org"
			providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
			provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
			tenantOrg := "test-tenant-org"
			tenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
			tenant := testVPCBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
			site := testVPCBuildSite(t, dbSession, provider, "test-site", providerUser)
			parentVpc := testVPCBuildVPC(
				t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser, cdbm.VpcStatusReady,
			)
			ipBlock := testVPCPrefixBuildIPBlock(
				t, dbSession, "delete-ip-block", site, provider, &tenant.ID,
				cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
				cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
			)

			ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
			_, err := ipam.CreateIpamEntryForIPBlock(
				ctx, ipamStorage, ipBlock.Prefix, ipBlock.PrefixLength, ipBlock.RoutingType,
				ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
			)
			require.NoError(t, err)

			prefix := "10.20.30.0/24"
			prefixLength := 24
			_, err = ipam.AcquireSpecificChildIpamEntryForIPBlock(
				ctx, nil, dbSession, ipamStorage, ipBlock, prefix,
			)
			require.NoError(t, err)

			vpcPrefix := testVPCBuildVPCPrefix(
				t, dbSession, "delete-vpc-prefix", site, tenant, parentVpc.ID, &ipBlock.ID,
				&prefix, &prefixLength, cdbm.VpcPrefixStatusDeleting, tenantUser,
			)
			vpcPrefix, err = cdbm.NewVpcPrefixDAO(dbSession).GetByID(
				ctx, nil, vpcPrefix.ID, []string{cdbm.IPBlockRelationName},
			)
			require.NoError(t, err)
			require.NotNil(t, vpcPrefix.IPBlock)
			require.NotNil(t, vpcPrefix.IPBlockID)

			if test.clearRelation {
				vpcPrefix.IPBlock = nil
			}

			manager := ManageVpcPrefix{dbSession: dbSession}
			tx, err := cdb.BeginTx(ctx, dbSession, &sql.TxOptions{})
			require.NoError(t, err)
			err = manager.deleteVpcPrefixFromDB(ctx, tx, vpcPrefix, zerolog.Nop())
			assert.Equal(t, test.expectedError, err != nil)
			if test.expectedError {
				require.NoError(t, tx.Rollback())
				stillActive, getErr := cdbm.NewVpcPrefixDAO(dbSession).GetByID(ctx, nil, vpcPrefix.ID, nil)
				require.NoError(t, getErr)
				assert.Nil(t, stillActive.Deleted)
			} else {
				require.NoError(t, tx.Commit())
				_, getErr := cdbm.NewVpcPrefixDAO(dbSession).GetByID(ctx, nil, vpcPrefix.ID, nil)
				assert.ErrorIs(t, getErr, cdb.ErrDoesNotExist)

				ipamer := cipam.NewWithStorage(ipamStorage)
				ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
					ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
				))
				assert.Nil(t, ipamer.PrefixFrom(ctx, prefix))
			}
		})
	}
}

func testCreateOrUpdateVpcPrefixSkipsIDOwnedByDifferentSite(t *testing.T) {
	// Same controller UUID under another site must skip cleanly, not unique-constraint-fail every cycle.
	ctx := context.Background()
	dbSession := testVpcPrefixInitDB(t)
	defer dbSession.Close()
	testVPCPrefixSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testVPCBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testVPCSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testVPCBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testVPCBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	siteA := testVPCBuildSite(t, dbSession, provider, "test-site-a", providerUser)
	siteB := testVPCBuildSite(t, dbSession, provider, "test-site-b", providerUser)

	vpcA := testVPCBuildVPC(t, dbSession, "vpc-a", provider, tenant, siteA, nil, nil, tenantUser, cdbm.VpcStatusReady)
	vpcB := testVPCBuildVPC(t, dbSession, "vpc-b", provider, tenant, siteB, nil, nil, tenantUser, cdbm.VpcStatusReady)

	ipBlockA := testVPCPrefixBuildIPBlock(
		t, dbSession, "ip-block-a", siteA, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.10.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipBlockB := testVPCPrefixBuildIPBlock(
		t, dbSession, "ip-block-b", siteB, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	_, err := ipam.CreateIpamEntryForIPBlock(
		ctx, ipamStorage, ipBlockB.Prefix, ipBlockB.PrefixLength, ipBlockB.RoutingType,
		ipBlockB.InfrastructureProviderID.String(), ipBlockB.SiteID.String(),
	)
	require.NoError(t, err)

	sharedVpcPrefixID := uuid.New()
	prefixA := "10.10.1.0/24"
	prefixLengthA := 24
	existing, err := cdbm.NewVpcPrefixDAO(dbSession).Create(ctx, nil, cdbm.VpcPrefixCreateInput{
		VpcPrefixID:  &sharedVpcPrefixID,
		Name:         "site-a-vpc-prefix",
		TenantOrg:    tenant.Org,
		SiteID:       siteA.ID,
		VpcID:        vpcA.ID,
		TenantID:     tenant.ID,
		IpBlockID:    &ipBlockA.ID,
		Prefix:       prefixA,
		PrefixLength: prefixLengthA,
		Status:       cdbm.VpcPrefixStatusReady,
		CreatedBy:    tenantUser.ID,
	})
	require.NoError(t, err)
	require.Equal(t, sharedVpcPrefixID, existing.ID)
	require.Equal(t, siteA.ID, existing.SiteID)

	manager := ManageVpcPrefix{dbSession: dbSession}
	created := manager.createOrUpdateVpcPrefixFromSite(ctx, siteB, &corev1.VpcPrefix{
		Id:    &corev1.VpcPrefixId{Value: sharedVpcPrefixID.String()},
		VpcId: &corev1.VpcId{Value: vpcB.ID.String()},
		Config: &corev1.VpcPrefixConfig{
			Prefix: "10.20.1.0/24",
		},
		Metadata: &corev1.Metadata{Name: "site-b-collision-vpc-prefix"},
		Status:   &corev1.VpcPrefixStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)
	stillOriginal, err := vpcPrefixDAO.GetByID(ctx, nil, sharedVpcPrefixID, nil)
	require.NoError(t, err)
	assert.Equal(t, siteA.ID, stillOriginal.SiteID)
	assert.Equal(t, prefixA, stillOriginal.Prefix)
	assert.Equal(t, "site-a-vpc-prefix", stillOriginal.Name)
	assert.Nil(t, stillOriginal.Deleted)

	siteBPrefixes, _, err := vpcPrefixDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{
		SiteIDs: []uuid.UUID{siteB.ID},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	require.NoError(t, err)
	assert.Empty(t, siteBPrefixes)

	ipamer := cipam.NewWithStorage(ipamStorage)
	ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
		ctx, ipBlockB.RoutingType, ipBlockB.InfrastructureProviderID.String(), ipBlockB.SiteID.String(),
	))
	assert.Nil(t, ipamer.PrefixFrom(ctx, "10.20.1.0/24"))
}
