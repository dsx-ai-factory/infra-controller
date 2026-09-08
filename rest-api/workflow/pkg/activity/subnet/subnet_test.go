// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"bytes"
	"context"
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
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	"os"

	"go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	"go.temporal.io/sdk/testsuite"

	"github.com/prometheus/client_golang/prometheus"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
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

func testSubnetInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testSubnetSetupSchema(t *testing.T, dbSession *cdb.Session) {
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
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Domain)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Subnet)(nil))
	assert.Nil(t, err)
	// setup ipam table
	ipamStorage := cipam.NewBunStorage(dbSession.DB, nil)
	assert.Nil(t, ipamStorage.ApplyDbSchema())
	assert.Nil(t, ipamStorage.DeleteAllPrefixes(context.Background(), ""))
}

// testSubnetSiteBuildInfrastructureProvider Building Infra Provider in DB
func testSubnetSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
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

// testSubnetBuildSite Building Site in DB
func testSubnetBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
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
func testSubnetBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
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

// testSubnetBuildUser Building User in DB
func testSubnetBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
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

// testSubnetBuildVPC Building VPC in DB
func testSubnetBuildVPC(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, tn *cdbm.Tenant, st *cdbm.Site, ct *uuid.UUID, lb map[string]string, user *cdbm.User) *cdbm.Vpc {
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
		Status:                    cdbm.VpcStatusPending,
		CreatedBy:                 *user,
	}

	vpc, err := vpcDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

// testSubnetBuildDomain Building Domain in DB
func testSubnetBuildDomain(t *testing.T, dbSession *cdb.Session, hostname, org string, user *cdbm.User) *cdbm.Domain {
	domain := &cdbm.Domain{
		ID:        uuid.New(),
		Hostname:  hostname,
		Org:       org,
		Status:    cdbm.DomainStatusPending,
		CreatedBy: user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(domain).Exec(context.Background())
	assert.Nil(t, err)
	return domain
}

// testSubnetBuildSubnet Building Subnet in DB
func testSubnetBuildSubnet(t *testing.T, dbSession *cdb.Session, name string, tenant *cdbm.Tenant, vpc *cdbm.Vpc, domainID *uuid.UUID, ctrlSegmentID *uuid.UUID, routingType *string, ipv4prefix *string, ipv4gateway *string, ipv4BlockID *uuid.UUID, ipBlockSize int, status string, user *cdbm.User) *cdbm.Subnet {
	subnetDAO := cdbm.NewSubnetDAO(dbSession)

	subnet, err := subnetDAO.Create(context.Background(), nil, cdbm.SubnetCreateInput{
		Name:                       name,
		Description:                cutil.GetPtr("Test Subnet"),
		Org:                        tenant.Org,
		SiteID:                     vpc.SiteID,
		VpcID:                      vpc.ID,
		DomainID:                   domainID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: ctrlSegmentID,
		RoutingType:                routingType,
		IPv4Prefix:                 ipv4prefix,
		IPv4Gateway:                ipv4gateway,
		IPv4BlockID:                ipv4BlockID,
		PrefixLength:               ipBlockSize,
		Status:                     status,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)

	return subnet
}

// testSubnetBuildIPBlock Building IPBlock in DB
func testSubnetBuildIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, routingType, prefix string, blockSize int, protocolVersion string, fullGrant bool, status string, user *cdbm.User) *cdbm.IPBlock {
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

func testBuildNetworkSegment(id, name string, mtu *int32, state corev1.TenantState, segmentType corev1.NetworkSegmentType) *corev1.NetworkSegment {
	seg := &corev1.NetworkSegment{
		Id:       &corev1.NetworkSegmentId{Value: id},
		Metadata: &corev1.Metadata{Name: name},
		Config: &corev1.NetworkSegmentConfig{
			SegmentType: segmentType,
		},
		Status: &corev1.NetworkSegmentStatus{
			TenantState: state,
		},
	}
	if mtu != nil {
		seg.Config.Mtu = mtu
	}
	return seg
}

func TestManageSubnet_UpdateSubnetsInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()

	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)

	testSubnetSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{"FORGE_PROVIDER_ADMIN"}

	tnOrg := "test-tenant-org"
	tnOrgRoles := []string{"FORGE_TENANT_ADMIN"}

	ipu := testSubnetBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testSubnetSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	tnu := testSubnetBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn := testSubnetBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)

	st := testSubnetBuildSite(t, dbSession, ip, "test-site-1", ipu)
	assert.NotNil(t, st)

	al := testVPCSiteBuildAllocation(t, dbSession, st, tn, "test-allocation", ipu)
	assert.NotNil(t, al)

	vpc := testSubnetBuildVPC(t, dbSession, "test-vpc", ip, tn, st, nil, nil, tnu)
	assert.NotNil(t, vpc)

	ipb := testSubnetBuildIPBlock(t, dbSession, "testipb", st, ip, &tn.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.0.8.0", 22, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	assert.NotNil(t, ipb)

	_, err := ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipb.Prefix, ipb.PrefixLength, ipb.RoutingType, ipb.InfrastructureProviderID.String(), ipb.SiteID.String())
	assert.Nil(t, err)

	// Subnet 1 receives updates from Site Controller, namely status update
	subnet1 := testSubnetBuildSubnet(t, dbSession, "test-subnet-1", tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, cutil.GetPtr("192.0.1.0"), cutil.GetPtr("192.0.1.0"), nil, 24, cdbm.SubnetStatusProvisioning, tnu)

	// Subnet 2 & FG is in Deleting state and gets deleted when no longer present in Site Controller inventory
	sbPrefix, err := ipam.CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamStorage, ipb, 24)
	assert.NoError(t, err)
	ipv4Prefix, _, err := ipam.ParseCidrIntoPrefixAndBlockSize(sbPrefix.Cidr)
	assert.NoError(t, err)
	ipv4Gateway, err := ipam.GetFirstIPFromCidr(sbPrefix.Cidr)
	assert.NoError(t, err)
	subnet2 := testSubnetBuildSubnet(t, dbSession, "test-subnet-2", tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 24, cdbm.SubnetStatusDeleting, tnu)
	subnet2.IPv4Block = ipb

	// Full Grant subnet deletion
	ipv4PrefixFG := "193.1.1.0"
	ipv4GatewayFG := "193.1.1.1"

	ipbFG := testSubnetBuildIPBlock(t, dbSession, "test-ipb-full-grant", st, ip, &tn.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, ipv4PrefixFG, 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, ipu)
	_, err = ipam.CreateIpamEntryForIPBlock(ctx, ipamStorage, ipbFG.Prefix, ipbFG.PrefixLength, ipbFG.RoutingType, ip.ID.String(), st.ID.String())
	assert.NoError(t, err)
	_, err = ipam.CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamStorage, ipbFG, 24)
	assert.NoError(t, err)
	subnetFG := testSubnetBuildSubnet(t, dbSession, "test-subnet-FG", tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, &ipv4PrefixFG, &ipv4GatewayFG, cutil.GetPtr(ipbFG.ID), 24, cdbm.SubnetStatusDeleting, tnu)
	subnetFG.IPv4Block = ipbFG

	// Subnet 3 is missing from Site Controller inventory but was not requested by user to be deleted, hence gets missing flag set
	subnet3 := testSubnetBuildSubnet(t, dbSession, "test-subnet-3", tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, cutil.GetPtr("192.0.1.8"), cutil.GetPtr("192.0.1.8"), nil, 24, cdbm.SubnetStatusProvisioning, tnu)
	// Set created earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE subnet SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), subnet3.ID.String())
	assert.NoError(t, err)

	sbPrefix, err = ipam.CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamStorage, ipb, 26)
	assert.NoError(t, err)
	ipv4Prefix, _, err = ipam.ParseCidrIntoPrefixAndBlockSize(sbPrefix.Cidr)
	assert.NoError(t, err)
	ipv4Gateway, err = ipam.GetFirstIPFromCidr(sbPrefix.Cidr)
	assert.NoError(t, err)

	// Subnet 4 is missing from Site Controller inventory but does not have controller ID set, hence gets missing flag does not get set
	subnet4 := testSubnetBuildSubnet(t, dbSession, "test-subnet-4", tn, vpc, nil, nil, &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 26, cdbm.SubnetStatusProvisioning, tnu)

	// Subnet 5 is reported as Ready in Controller inventory but is being deleted, so does not get updated
	subnet5 := testSubnetBuildSubnet(t, dbSession, "test-subnet-5", tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 26, cdbm.SubnetStatusDeleting, tnu)

	// Subnet 10 is reported as Terminated in Controller inventory but remains Deleting until it disappears from inventory
	subnet10 := testSubnetBuildSubnet(t, dbSession, "test-subnet-10", tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 26, cdbm.SubnetStatusDeleting, tnu)

	// Subnet 6 was previously missing but is reported as Ready in Controller inventory
	subnet6 := testSubnetBuildSubnet(t, dbSession, "test-subnet-6", tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 26, cdbm.SubnetStatusError, tnu)

	// Subnet 7 is in Deleting state and has no controller ID, gets deleted on inventory update
	sbPrefix7, err := ipam.CreateChildIpamEntryForIPBlock(ctx, nil, dbSession, ipamStorage, ipb, 24)
	assert.NoError(t, err)
	ipv4Prefix7, _, err := ipam.ParseCidrIntoPrefixAndBlockSize(sbPrefix7.Cidr)
	assert.NoError(t, err)
	ipv4Gateway7, err := ipam.GetFirstIPFromCidr(sbPrefix7.Cidr)
	assert.NoError(t, err)
	subnet7 := testSubnetBuildSubnet(t, dbSession, "test-subnet-7", tn, vpc, nil, nil, &ipb.RoutingType, &ipv4Prefix7, &ipv4Gateway7, &ipb.ID, 24, cdbm.SubnetStatusDeleting, tnu)
	subnet7.IPv4Block = ipb

	// Subnet 8 & 9 do not have controller ID, but it was created and inventory returns them
	subnet8 := testSubnetBuildSubnet(t, dbSession, "test-subnet-8", tn, vpc, nil, nil, &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 26, cdbm.SubnetStatusProvisioning, tnu)

	// Subnet 9 does not have controller ID, but it was created and inventory returns it
	subnet9 := testSubnetBuildSubnet(t, dbSession, "test-subnet-9", tn, vpc, nil, nil, &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 26, cdbm.SubnetStatusDeleting, tnu)

	subnetDAO := cdbm.NewSubnetDAO(dbSession)
	_, err = subnetDAO.Update(ctx, nil, cdbm.SubnetUpdateInput{SubnetId: subnet6.ID, IsMissingOnSite: cutil.GetPtr(true)})
	assert.NoError(t, err)

	// Build Subnet inventory that is paginated
	// Generate data for 34 Subnets reported from Site Agent while Cloud has 38 Subnets
	pagedSubnets := []*cdbm.Subnet{}
	pagedInvIds := []string{}
	for i := 0; i < 38; i++ {
		subnet := testSubnetBuildSubnet(t, dbSession, fmt.Sprintf("test-vpc-paged-%d", i), tn, vpc, nil, cutil.GetPtr(uuid.New()), &ipb.RoutingType, &ipv4Prefix, &ipv4Gateway, &ipb.ID, 26, cdbm.SubnetStatusProvisioning, tnu)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE subnet SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), subnet.ID.String())
		assert.NoError(t, err)
		pagedSubnets = append(pagedSubnets, subnet)
		pagedInvIds = append(pagedInvIds, subnet.ControllerNetworkSegmentID.String())
	}

	pagedCtrlSubnets := []*corev1.NetworkSegment{}
	for i := 0; i < 34; i++ {
		ctrlSubnet := &corev1.NetworkSegment{
			Id:       &corev1.NetworkSegmentId{Value: pagedSubnets[i].ControllerNetworkSegmentID.String()},
			Metadata: &corev1.Metadata{Name: pagedSubnets[i].Name},
		}
		pagedCtrlSubnets = append(pagedCtrlSubnets, ctrlSubnet)
	}

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	wtc := &tmocks.Client{}

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	mtu := int32(1500)

	type fields struct {
		dbSession      *cdb.Session
		ipamStorage    cipam.Storage
		siteClientPool *sc.ClientPool
		tc             client.Client
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx             context.Context
		siteID          uuid.UUID
		subnetInventory *corev1.SubnetInventory
	}
	tests := []struct {
		name            string
		fields          fields
		args            args
		updatedSubnet   *cdbm.Subnet
		deletedSubnets  []*cdbm.Subnet
		deletingSubnets []*cdbm.Subnet
		missingSubnets  []*cdbm.Subnet
		restoredSubnet  *cdbm.Subnet
		unpairedSubnets []*cdbm.Subnet
		wantErr         bool
	}{
		{
			name: "test Subnet inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				ipamStorage:    ipamStorage,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				subnetInventory: &corev1.SubnetInventory{
					Segments: []*corev1.NetworkSegment{
						testBuildNetworkSegment(subnet1.ControllerNetworkSegmentID.String(), "", nil, corev1.TenantState_READY, corev1.NetworkSegmentType_TENANT),
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test Subnet inventory processing success",
			fields: fields{
				dbSession:      dbSession,
				ipamStorage:    ipamStorage,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				subnetInventory: &corev1.SubnetInventory{
					Segments: []*corev1.NetworkSegment{
						testBuildNetworkSegment(subnet1.ControllerNetworkSegmentID.String(), subnet1.Name, &mtu, corev1.TenantState_READY, corev1.NetworkSegmentType_TENANT),
						testBuildNetworkSegment(subnet5.ControllerNetworkSegmentID.String(), subnet5.Name, nil, corev1.TenantState_READY, corev1.NetworkSegmentType_TENANT),
						testBuildNetworkSegment(subnet10.ControllerNetworkSegmentID.String(), subnet10.Name, nil, corev1.TenantState_TERMINATED, corev1.NetworkSegmentType_TENANT),
						testBuildNetworkSegment(subnet6.ControllerNetworkSegmentID.String(), subnet6.Name, nil, corev1.TenantState_READY, corev1.NetworkSegmentType_TENANT),
						testBuildNetworkSegment(uuid.NewString(), subnet8.ID.String(), nil, corev1.TenantState_READY, corev1.NetworkSegmentType_TENANT),
						testBuildNetworkSegment(uuid.NewString(), subnet9.ID.String(), nil, corev1.TenantState_READY, corev1.NetworkSegmentType_TENANT),
					},
				},
			},
			updatedSubnet:   subnet1,
			deletedSubnets:  []*cdbm.Subnet{subnet2, subnetFG, subnet7},
			deletingSubnets: []*cdbm.Subnet{subnet5, subnet10},
			missingSubnets:  []*cdbm.Subnet{subnet3, subnet4},
			restoredSubnet:  subnet6,
			unpairedSubnets: []*cdbm.Subnet{subnet8, subnet9},
			wantErr:         false,
		},
		{
			name: "test paged Subnet inventory processing, empty inventory",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				subnetInventory: &corev1.SubnetInventory{
					Segments:        []*corev1.NetworkSegment{},
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
			name: "test paged Subnet inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				subnetInventory: &corev1.SubnetInventory{
					Segments:  pagedCtrlSubnets[0:10],
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
		},
		{
			name: "test paged Subnet inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				subnetInventory: &corev1.SubnetInventory{
					Segments:  pagedCtrlSubnets[30:34],
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
			missingSubnets: pagedSubnets[34:38],
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ms := NewManageSubnet(tt.fields.dbSession, tt.fields.siteClientPool, wtc)

			mtc := &tmocks.Client{}
			ms.siteClientPool.IDClientMap[vpc.SiteID.String()] = mtc

			_, err := ms.UpdateSubnetsInDB(tt.args.ctx, tt.args.siteID, tt.args.subnetInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			subnetDAO := cdbm.NewSubnetDAO(dbSession)

			// Check that Subnet status was updated in DB for Subnet 1
			if tt.updatedSubnet != nil {
				updatedSubnet, serr := subnetDAO.GetByID(ctx, nil, tt.updatedSubnet.ID, nil)
				assert.Nil(t, serr)
				assert.Equal(t, cdbm.SubnetStatusReady, updatedSubnet.Status)
				assert.Equal(t, *updatedSubnet.MTU, int(mtu))
			}

			for _, subnet := range tt.deletedSubnets {
				_, err = subnetDAO.GetByID(ctx, nil, subnet.ID, nil)
				require.Equal(t, cdb.ErrDoesNotExist, err, fmt.Sprintf("Subnet %s should have been deleted", subnet.Name))

				// Check that it's IPAM entry was removed
				if subnet.IPv4Block.PrefixLength != subnet.PrefixLength {
					ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
					ipamer := cipam.NewWithStorage(ipamStorage)
					ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(ctx, ipb.RoutingType, ipb.InfrastructureProviderID.String(), ipb.SiteID.String()))
					pref := ipamer.PrefixFrom(ctx, ipam.GetCidrForIPBlock(ctx, *subnet.IPv4Prefix, subnet.PrefixLength))
					assert.Nil(t, pref)
				}
			}

			// Check that Subnet 3, which is missing from Site inventory has missing flag set and status set to Error
			for _, subnet := range tt.missingSubnets {
				us, serr := subnetDAO.GetByID(ctx, nil, subnet.ID, nil)
				assert.Nil(t, serr)

				if us.ControllerNetworkSegmentID != nil {
					assert.True(t, us.IsMissingOnSite)
					assert.Equal(t, cdbm.SubnetStatusError, us.Status)
				} else {
					assert.False(t, us.IsMissingOnSite)
				}
			}

			// Check that Subnet 6, which was previously marked missing is now restored
			if tt.restoredSubnet != nil {
				us, serr := subnetDAO.GetByID(ctx, nil, tt.restoredSubnet.ID, nil)
				assert.Nil(t, serr)
				assert.NotNil(t, us.ControllerNetworkSegmentID)
				assert.False(t, us.IsMissingOnSite)
				assert.Equal(t, cdbm.SubnetStatusReady, us.Status)
			}

			// Check that Subnets in Deleting state remain Deleting while they are still present in Site inventory
			for _, subnet := range tt.deletingSubnets {
				us, err := subnetDAO.GetByID(ctx, nil, subnet.ID, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.SubnetStatusDeleting, us.Status)
			}

			// Check that Subnet 8 & 9, which previously did not have controller ID, now have it
			for _, subnet := range tt.unpairedSubnets {
				us, serr := subnetDAO.GetByID(ctx, nil, subnet.ID, nil)
				assert.Nil(t, serr)
				assert.NotNil(t, us.ControllerNetworkSegmentID)
			}
		})
	}

	regressionTests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "auto creates, replays, and restores Subnets",
			run:  testManageSubnetUpdateSubnetsInDBAutoCreatesAndRestores,
		},
	}
	for _, test := range regressionTests {
		t.Run(test.name, test.run)
	}
}

func testManageSubnetUpdateSubnetsInDBAutoCreatesAndRestores(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testSubnetSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testSubnetBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testSubnetBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testSubnetBuildVPC(t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser)
	controllerVpcID := parentVpc.ID
	ipBlock := testSubnetBuildIPBlock(
		t, dbSession, "test-subnet-ip-block", site, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	_, err := ipam.CreateIpamEntryForIPBlock(
		ctx, ipamStorage, ipBlock.Prefix, ipBlock.PrefixLength, ipBlock.RoutingType,
		ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	)
	require.NoError(t, err)

	manager := ManageSubnet{dbSession: dbSession}
	subnetDAO := cdbm.NewSubnetDAO(dbSession)
	statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)

	controllerSegmentID := uuid.MustParse("abcdef01-2345-4678-9abc-def012345678")
	gateway := "10.20.30.1"
	controllerSegment := &corev1.NetworkSegment{
		Id: &corev1.NetworkSegmentId{Value: controllerSegmentID.String()},
		Config: &corev1.NetworkSegmentConfig{
			VpcId:       &corev1.VpcId{Value: controllerVpcID.String()},
			SegmentType: corev1.NetworkSegmentType_TENANT,
			Prefixes: []*corev1.NetworkPrefix{
				{Prefix: "10.20.30.0/24", Gateway: &gateway},
			},
		},
		Metadata: &corev1.Metadata{
			Name: "site-created-subnet",
		},
		Status: &corev1.NetworkSegmentStatus{
			TenantState: corev1.TenantState_READY,
		},
	}
	inventory := &corev1.SubnetInventory{
		Segments: []*corev1.NetworkSegment{controllerSegment},
	}

	if !t.Run("auto creates Subnet from inventory", func(t *testing.T) {
		var logOutput bytes.Buffer
		originalLogger := log.Logger
		log.Logger = zerolog.New(&logOutput)
		defer func() {
			log.Logger = originalLogger
		}()

		_, err := manager.UpdateSubnetsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		assert.Contains(t, logOutput.String(), "created or undeleted Subnet from Site inventory")

		created, err := subnetDAO.GetByID(ctx, nil, controllerSegmentID, nil)
		require.NoError(t, err)
		assert.Equal(t, controllerSegmentID, created.ID)
		assert.Equal(t, parentVpc.ID, created.VpcID)
		assert.Equal(t, tenant.ID, created.TenantID)
		assert.Equal(t, tenantOrg, created.Org)
		assert.Equal(t, site.ID, created.SiteID)
		assert.Equal(t, parentVpc.CreatedBy, created.CreatedBy)
		assert.Equal(t, cdbm.SubnetStatusReady, created.Status)
		assert.Equal(t, "site-created-subnet", created.Name)
		require.NotNil(t, created.IPv4Prefix)
		assert.Equal(t, "10.20.30.0", *created.IPv4Prefix)
		require.NotNil(t, created.IPv4Gateway)
		assert.Equal(t, gateway, *created.IPv4Gateway)
		assert.Equal(t, 24, created.PrefixLength)
		require.NotNil(t, created.IPv4BlockID)
		assert.Equal(t, ipBlock.ID, *created.IPv4BlockID)
		require.NotNil(t, created.ControllerNetworkSegmentID)
		assert.Equal(t, controllerSegmentID, *created.ControllerNetworkSegmentID)

		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
		))
		assert.NotNil(t, ipamer.PrefixFrom(ctx, controllerSegment.Config.Prefixes[0].Prefix))

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
				*statusDetails[i].Message == "Subnet was found on Site, Ready for use" {
				foundCreateMessage = true
				break
			}
		}
		assert.True(t, foundCreateMessage)
	}) {
		t.FailNow()
	}

	if !t.Run("inventory replay is idempotent", func(t *testing.T) {
		_, err := manager.UpdateSubnetsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		subnets, count, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, subnets, 1)
		assert.Equal(t, 1, count)
	}) {
		t.FailNow()
	}

	t.Run("uppercase inventory ID follows the recovery path", func(t *testing.T) {
		canonicalID := controllerSegment.Id.Value
		controllerSegment.Id.Value = strings.ToUpper(canonicalID)
		require.NotEqual(t, canonicalID, controllerSegment.Id.Value)
		defer func() {
			controllerSegment.Id.Value = canonicalID
		}()

		var logOutput bytes.Buffer
		originalLogger := log.Logger
		log.Logger = zerolog.New(&logOutput)
		defer func() {
			log.Logger = originalLogger
		}()

		_, err := manager.UpdateSubnetsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)

		subnets, count, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, subnets, 1)
		assert.Equal(t, 1, count)
		assert.Contains(t, logOutput.String(), "created or undeleted Subnet from Site inventory")
	})

	t.Run("inventory skips restore when Site reports TERMINATING", func(t *testing.T) {
		_, err := subnetDAO.Update(ctx, nil, cdbm.SubnetUpdateInput{
			SubnetId:        controllerSegmentID,
			Status:          cutil.GetPtr(cdbm.SubnetStatusDeleting),
			IsMissingOnSite: cutil.GetPtr(true),
		})
		require.NoError(t, err)
		require.NoError(t, ipam.DeleteChildIpamEntryFromCidr(
			ctx, nil, dbSession, ipamStorage, ipBlock, controllerSegment.Config.Prefixes[0].Prefix,
		))
		require.NoError(t, subnetDAO.Delete(ctx, nil, controllerSegmentID))

		deleted, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)
		assert.Equal(t, cdbm.SubnetStatusDeleting, deleted[0].Status)
		deletedAt := *deleted[0].Deleted

		terminatingInventory := &corev1.SubnetInventory{
			Segments: []*corev1.NetworkSegment{
				{
					Id: &corev1.NetworkSegmentId{Value: controllerSegmentID.String()},
					Config: &corev1.NetworkSegmentConfig{
						VpcId:       &corev1.VpcId{Value: controllerVpcID.String()},
						SegmentType: corev1.NetworkSegmentType_TENANT,
						Prefixes: []*corev1.NetworkPrefix{
							{Prefix: controllerSegment.Config.Prefixes[0].Prefix, Gateway: &gateway},
						},
					},
					Metadata: &corev1.Metadata{Name: "site-created-subnet"},
					Status: &corev1.NetworkSegmentStatus{
						TenantState: corev1.TenantState_TERMINATING,
					},
				},
			},
		}
		_, err = manager.UpdateSubnetsInDB(ctx, site.ID, terminatingInventory)
		require.NoError(t, err)

		stillDeleted, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, stillDeleted, 1)
		require.NotNil(t, stillDeleted[0].Deleted)
		assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)
		assert.Equal(t, cdbm.SubnetStatusDeleting, stillDeleted[0].Status)

		_, err = subnetDAO.GetByID(ctx, nil, controllerSegmentID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
		))
		assert.Nil(t, ipamer.PrefixFrom(ctx, controllerSegment.Config.Prefixes[0].Prefix))
	})

	t.Run("inventory defers restore when delete is newer than the inventory interval", func(t *testing.T) {
		recentlyDeletedSubnetID := uuid.New()
		recentlyDeletedPrefix := "10.20.31.0"
		recentlyDeletedCIDR := "10.20.31.0/24"
		recentlyDeletedGateway := "10.20.31.1"
		_, err := subnetDAO.Create(ctx, nil, cdbm.SubnetCreateInput{
			SubnetID:                   &recentlyDeletedSubnetID,
			Name:                       "recently-deleted-subnet",
			Org:                        tenant.Org,
			SiteID:                     site.ID,
			VpcID:                      parentVpc.ID,
			TenantID:                   tenant.ID,
			ControllerNetworkSegmentID: &recentlyDeletedSubnetID,
			RoutingType:                &ipBlock.RoutingType,
			IPv4Prefix:                 &recentlyDeletedPrefix,
			IPv4Gateway:                &recentlyDeletedGateway,
			IPv4BlockID:                &ipBlock.ID,
			PrefixLength:               24,
			Status:                     cdbm.SubnetStatusDeleting,
			CreatedBy:                  tenantUser.ID,
		})
		require.NoError(t, err)
		require.NoError(t, subnetDAO.Delete(ctx, nil, recentlyDeletedSubnetID))

		deleted, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{recentlyDeletedSubnetID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)
		deletedAt := *deleted[0].Deleted

		recentInventory := &corev1.SubnetInventory{
			Segments: []*corev1.NetworkSegment{
				{
					Id: &corev1.NetworkSegmentId{Value: recentlyDeletedSubnetID.String()},
					Config: &corev1.NetworkSegmentConfig{
						VpcId:       &corev1.VpcId{Value: controllerVpcID.String()},
						SegmentType: corev1.NetworkSegmentType_TENANT,
						Prefixes: []*corev1.NetworkPrefix{
							{Prefix: recentlyDeletedCIDR, Gateway: &recentlyDeletedGateway},
						},
					},
					Metadata: &corev1.Metadata{Name: "recently-deleted-subnet"},
					Status:   &corev1.NetworkSegmentStatus{TenantState: corev1.TenantState_READY},
				},
			},
		}
		_, err = manager.UpdateSubnetsInDB(ctx, site.ID, recentInventory)
		require.NoError(t, err)

		stillDeleted, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{recentlyDeletedSubnetID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, stillDeleted, 1)
		require.NotNil(t, stillDeleted[0].Deleted)
		assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)

		_, err = subnetDAO.GetByID(ctx, nil, recentlyDeletedSubnetID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
		))
		assert.Nil(t, ipamer.PrefixFrom(ctx, recentlyDeletedCIDR))
	})

	t.Run("inventory restores soft-deleted Subnet", func(t *testing.T) {
		var logOutput bytes.Buffer
		originalLogger := log.Logger
		log.Logger = zerolog.New(&logOutput)
		defer func() {
			log.Logger = originalLogger
		}()

		// Establish the soft-delete precondition here so this subtest can be
		// selected with -run without executing the preceding lifecycle subtests.
		existing, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		if len(existing) == 0 {
			created, createErr := subnetDAO.Create(ctx, nil, cdbm.SubnetCreateInput{
				SubnetID:                   &controllerSegmentID,
				Name:                       controllerSegment.GetMetadata().GetName(),
				Org:                        tenant.Org,
				SiteID:                     site.ID,
				VpcID:                      parentVpc.ID,
				TenantID:                   tenant.ID,
				ControllerNetworkSegmentID: &controllerSegmentID,
				RoutingType:                &ipBlock.RoutingType,
				IPv4Prefix:                 cutil.GetPtr("10.20.30.0"),
				IPv4Gateway:                &gateway,
				IPv4BlockID:                &ipBlock.ID,
				PrefixLength:               24,
				Status:                     cdbm.SubnetStatusDeleting,
				CreatedBy:                  tenantUser.ID,
			})
			require.NoError(t, createErr)
			existing = []cdbm.Subnet{*created}
		}
		require.Len(t, existing, 1)

		if existing[0].Deleted == nil {
			_, err = subnetDAO.Update(ctx, nil, cdbm.SubnetUpdateInput{
				SubnetId:        controllerSegmentID,
				Status:          cutil.GetPtr(cdbm.SubnetStatusDeleting),
				IsMissingOnSite: cutil.GetPtr(true),
			})
			require.NoError(t, err)

			setupIPAMer := cipam.NewWithStorage(ipamStorage)
			setupIPAMer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
				ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
			))
			if setupIPAMer.PrefixFrom(ctx, controllerSegment.Config.Prefixes[0].Prefix) != nil {
				require.NoError(t, ipam.DeleteChildIpamEntryFromCidr(
					ctx, nil, dbSession, ipamStorage, ipBlock, controllerSegment.Config.Prefixes[0].Prefix,
				))
			}
			require.NoError(t, subnetDAO.Delete(ctx, nil, controllerSegmentID))
		}

		deleted, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)
		require.Equal(t, cdbm.SubnetStatusDeleting, deleted[0].Status)
		// The undelete is deferred while the delete is newer than the staleness threshold, so
		// backdate it past that.
		util.TestInventoryAgeDeletedTimestamp(ctx, t, dbSession, (*cdbm.Subnet)(nil), controllerSegmentID)

		_, err = manager.UpdateSubnetsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)
		assert.Contains(t, logOutput.String(), "created or undeleted Subnet from Site inventory")

		restored, err := subnetDAO.GetByID(ctx, nil, controllerSegmentID, nil)
		require.NoError(t, err)
		assert.Nil(t, restored.Deleted)
		assert.False(t, restored.IsMissingOnSite)
		assert.Equal(t, cdbm.SubnetStatusReady, restored.Status)
		ipamer := cipam.NewWithStorage(ipamStorage)
		ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
			ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
		))
		assert.NotNil(t, ipamer.PrefixFrom(ctx, controllerSegment.Config.Prefixes[0].Prefix))

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
				*statusDetails[i].Message == "Subnet is ready for use" {
				foundReadyMessage = true
				break
			}
		}
		assert.True(t, foundReadyMessage)
	})

	t.Run("inventory skips restore when tenant organization differs", func(t *testing.T) {
		require.NoError(t, ipam.DeleteChildIpamEntryFromCidr(
			ctx, nil, dbSession, ipamStorage, ipBlock, controllerSegment.Config.Prefixes[0].Prefix,
		))
		require.NoError(t, subnetDAO.Delete(ctx, nil, controllerSegmentID))
		deleted, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, deleted, 1)
		require.NotNil(t, deleted[0].Deleted)
		deletedAt := *deleted[0].Deleted

		_, err = dbSession.DB.NewUpdate().
			Model((*cdbm.Subnet)(nil)).
			Set("org = ?", "other-tenant-org").
			Where("id = ?", controllerSegmentID).
			WhereAllWithDeleted().
			Exec(ctx)
		require.NoError(t, err)

		_, err = manager.UpdateSubnetsInDB(ctx, site.ID, inventory)
		require.NoError(t, err)

		stillDeleted, _, err := subnetDAO.GetAll(
			ctx,
			nil,
			cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{controllerSegmentID}, IncludeDeleted: true},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		require.Len(t, stillDeleted, 1)
		require.NotNil(t, stillDeleted[0].Deleted)
		assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)

		_, err = subnetDAO.GetByID(ctx, nil, controllerSegmentID, nil)
		assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
	})
}

func TestNewManageSubnet(t *testing.T) {
	type args struct {
		dbSession      *cdb.Session
		ipamStorage    cipam.Storage
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
	ipamStorage := ipam.NewIpamStorage(dbSession.DB, nil)
	scp := sc.NewClientPool(tcfg)

	wtc := &tmocks.Client{}

	tests := []struct {
		name string
		args args
		want ManageSubnet
	}{
		{
			name: "test new ManageSubnet instantiation",
			args: args{
				dbSession:      dbSession,
				ipamStorage:    ipamStorage,
				siteClientPool: scp,
				tc:             wtc,
			},
			want: ManageSubnet{
				dbSession:      dbSession,
				siteClientPool: scp,
				tc:             wtc,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageSubnet(tt.args.dbSession, tt.args.siteClientPool, wtc); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageSubnet() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestManageSubnet_CreateOrUpdateSubnetFromSite(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testSubnetSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	site := testSubnetBuildSite(t, dbSession, provider, "test-site", providerUser)

	authorizedTenantOrg := "test-authorized-tenant"
	authorizedTenantUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), authorizedTenantOrg, []string{"FORGE_TENANT_ADMIN"})
	authorizedTenant := testSubnetBuildTenant(t, dbSession, "test-authorized-tenant", authorizedTenantOrg, authorizedTenantUser)

	parentVpc := testSubnetBuildVPC(t, dbSession, "parent-vpc", provider, authorizedTenant, site, nil, nil, authorizedTenantUser)
	controllerVpcID := parentVpc.ID
	ipBlock := testSubnetBuildIPBlock(
		t, dbSession, "test-subnet-ip-block", site, provider, &authorizedTenant.ID,
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
	existingSubnet := testSubnetBuildSubnet(
		t, dbSession, "existing-name", authorizedTenant, parentVpc, nil, nil,
		&ipBlock.RoutingType, cutil.GetPtr("10.0.0.0"), cutil.GetPtr("10.0.0.1"),
		&ipBlock.ID, 24, cdbm.SubnetStatusReady, authorizedTenantUser,
	)

	manager := ManageSubnet{dbSession: dbSession}

	tests := []struct {
		name              string
		controllerSegment *corev1.NetworkSegment
		wantSubnet        bool
		wantName          string
		wantNamePref      string
	}{
		{
			name: "unknown parent VPC is rejected",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: uuid.NewString()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.1.0.0/24", Gateway: cutil.GetPtr("10.1.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "unknown-vpc-subnet"},
			},
		},
		{
			name: "invalid Controller Segment ID",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: "not-a-uuid"},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.1.0.0/24", Gateway: cutil.GetPtr("10.1.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "invalid-id-subnet"},
			},
		},
		{
			name: "empty Prefix is rejected",
			controllerSegment: &corev1.NetworkSegment{
				Id:       &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config:   &corev1.NetworkSegmentConfig{VpcId: &corev1.VpcId{Value: controllerVpcID.String()}},
				Metadata: &corev1.Metadata{Name: "missing-prefix"},
			},
		},
		{
			name: "invalid Prefix CIDR is rejected",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "not-an-ip/24", Gateway: cutil.GetPtr("10.1.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "bad-cidr-subnet"},
			},
		},
		{
			name: "empty VPC ID is rejected",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.1.0.0/24", Gateway: cutil.GetPtr("10.1.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "missing-vpc-id"},
			},
		},
		{
			name: "no containing IP Block is rejected",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "192.168.1.0/24", Gateway: cutil.GetPtr("192.168.1.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "no-ip-block-subnet"},
			},
		},
		{
			name: "already allocated Site CIDR is rejected",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.5.0.0/24", Gateway: cutil.GetPtr("10.5.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "allocated-subnet"},
			},
		},
		{
			name: "renames Subnet when active name already exists",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.2.0.0/24", Gateway: cutil.GetPtr("10.2.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: existingSubnet.Name},
			},
			wantSubnet:   true,
			wantNamePref: existingSubnet.Name + "-recovered-",
		},
		{
			name: "assigns recovered name when metadata name is empty",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: controllerVpcID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.3.0.0/24", Gateway: cutil.GetPtr("10.3.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: ""},
			},
			wantSubnet:   true,
			wantNamePref: "recovered-",
		},
		{
			name: "creates Subnet using parent VPC REST ID as Site VPC ID",
			controllerSegment: &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: uuid.NewString()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.4.0.0/24", Gateway: cutil.GetPtr("10.4.0.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "rest-id-parent-vpc-subnet"},
			},
			wantSubnet: true,
			wantName:   "rest-id-parent-vpc-subnet",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			subnet := manager.createOrUpdateSubnetFromSite(ctx, site, tt.controllerSegment)
			if tt.wantSubnet {
				require.NotNil(t, subnet)
				assert.Equal(t, cdbm.SubnetStatusReady, subnet.Status)
				assert.Equal(t, authorizedTenant.ID, subnet.TenantID)
				assert.Equal(t, parentVpc.ID, subnet.VpcID)
				require.NotNil(t, subnet.IPv4BlockID)
				assert.Equal(t, ipBlock.ID, *subnet.IPv4BlockID)
				assert.NotNil(t, ipamer.PrefixFrom(ctx, tt.controllerSegment.Config.Prefixes[0].Prefix))
				if tt.wantName != "" {
					assert.Equal(t, tt.wantName, subnet.Name)
				}
				if tt.wantNamePref != "" {
					assert.True(t, len(subnet.Name) > len(tt.wantNamePref))
					assert.Equal(t, tt.wantNamePref, subnet.Name[:len(tt.wantNamePref)])
				}
			} else {
				assert.Nil(t, subnet)
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
			testDBSession := testSubnetInitDB(t)
			defer testDBSession.Close()
			testSubnetSetupSchema(t, testDBSession)

			testProviderOrg := "test-provider-org"
			testProviderUser := testSubnetBuildUser(t, testDBSession, uuid.NewString(), testProviderOrg, []string{"FORGE_PROVIDER_ADMIN"})
			testProvider := testSubnetSiteBuildInfrastructureProvider(t, testDBSession, "test-provider", testProviderOrg, testProviderUser)
			testTenantOrg := "test-tenant-org"
			testTenantUser := testSubnetBuildUser(t, testDBSession, uuid.NewString(), testTenantOrg, []string{"FORGE_TENANT_ADMIN"})
			testTenant := testSubnetBuildTenant(t, testDBSession, "test-tenant", testTenantOrg, testTenantUser)
			testSite := testSubnetBuildSite(t, testDBSession, testProvider, "test-site", testProviderUser)
			testParentVpc := testSubnetBuildVPC(t, testDBSession, "parent-vpc", testProvider, testTenant, testSite, nil, nil, testTenantUser)
			storedIPBlock := testSubnetBuildIPBlock(
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

			controllerSegmentID := uuid.New()
			testSubnetDAO := cdbm.NewSubnetDAO(testDBSession)
			_, err = testSubnetDAO.Create(testCtx, nil, cdbm.SubnetCreateInput{
				SubnetID:                   &controllerSegmentID,
				Name:                       "stored-ip-block-subnet",
				Org:                        testTenant.Org,
				SiteID:                     testSite.ID,
				VpcID:                      testParentVpc.ID,
				TenantID:                   testTenant.ID,
				ControllerNetworkSegmentID: &controllerSegmentID,
				RoutingType:                &storedIPBlock.RoutingType,
				IPv4Prefix:                 cutil.GetPtr("10.20.30.0"),
				IPv4Gateway:                cutil.GetPtr("10.20.30.1"),
				IPv4BlockID:                &storedIPBlock.ID,
				PrefixLength:               24,
				Status:                     cdbm.SubnetStatusDeleting,
				CreatedBy:                  testTenantUser.ID,
			})
			require.NoError(t, err)
			err = testSubnetDAO.Delete(testCtx, nil, controllerSegmentID)
			require.NoError(t, err)
			// The undelete is deferred while the delete is newer than the staleness threshold,
			// so backdate it past that.
			util.TestInventoryAgeDeletedTimestamp(testCtx, t, testDBSession, (*cdbm.Subnet)(nil), controllerSegmentID)

			if test.storedIPBlockStatus != cdbm.IPBlockStatusReady {
				_, err = cdbm.NewIPBlockDAO(testDBSession).Update(testCtx, nil, cdbm.IPBlockUpdateInput{
					IPBlockID: storedIPBlock.ID,
					Status:    &test.storedIPBlockStatus,
				})
				require.NoError(t, err)
			}
			if test.createMoreSpecificBlock {
				testSubnetBuildIPBlock(
					t, testDBSession, "more-specific-ip-block", testSite, testProvider, &testTenant.ID,
					cdbm.IPBlockRoutingTypeDatacenterOnly, "10.20.16.0", 20,
					cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, testTenantUser,
				)
			}

			controllerSegment := &corev1.NetworkSegment{
				Id: &corev1.NetworkSegmentId{Value: controllerSegmentID.String()},
				Config: &corev1.NetworkSegmentConfig{
					VpcId: &corev1.VpcId{Value: testParentVpc.ID.String()},
					Prefixes: []*corev1.NetworkPrefix{
						{Prefix: "10.20.30.0/24", Gateway: cutil.GetPtr("10.20.30.1")},
					},
				},
				Metadata: &corev1.Metadata{Name: "stored-ip-block-subnet"},
				Status: &corev1.NetworkSegmentStatus{
					TenantState: corev1.TenantState_READY,
				},
			}

			testManager := ManageSubnet{dbSession: testDBSession}
			restored := testManager.createOrUpdateSubnetFromSite(testCtx, testSite, controllerSegment)
			if !test.expectedRestore {
				assert.Nil(t, restored)
				deleted, _, getErr := testSubnetDAO.GetAll(
					testCtx,
					nil,
					cdbm.SubnetFilterInput{
						SubnetIDs:      []uuid.UUID{controllerSegmentID},
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
			require.NotNil(t, restored.IPv4BlockID)
			assert.Equal(t, storedIPBlock.ID, *restored.IPv4BlockID)

			testIPAMer := cipam.NewWithStorage(testIPAMStorage)
			testIPAMer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
				testCtx, storedIPBlock.RoutingType, storedIPBlock.InfrastructureProviderID.String(),
				storedIPBlock.SiteID.String(),
			))
			assert.NotNil(t, testIPAMer.PrefixFrom(testCtx, controllerSegment.Config.Prefixes[0].Prefix))
		})
	}

	regressionTests := []struct {
		name string
		run  func(*testing.T)
	}{
		{
			name: "rejects host bits in an equal-length CIDR",
			run:  testCreateOrUpdateSubnetRejectsHostBitsEqualLengthCIDR,
		},
		{
			name: "skips a fully granted IP Block",
			run:  testCreateOrUpdateSubnetSkipsFullGrantIPBlock,
		},
		{
			name: "skips undelete when the stored Prefix differs",
			run:  testCreateOrUpdateSubnetSkipsRestoreWhenPrefixDiffers,
		},
		{
			name: "skips a Controller Segment ID owned by another Site",
			run:  testCreateOrUpdateSubnetSkipsIDOwnedByDifferentSite,
		},
	}

	for _, test := range regressionTests {
		t.Run(test.name, test.run)
	}
}

func testCreateOrUpdateSubnetRejectsHostBitsEqualLengthCIDR(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testSubnetSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testSubnetBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testSubnetBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testSubnetBuildVPC(t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser)
	ipBlock := testSubnetBuildIPBlock(
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

	hostBitsSubnetID := uuid.New()
	manager := ManageSubnet{dbSession: dbSession}
	created := manager.createOrUpdateSubnetFromSite(ctx, site, &corev1.NetworkSegment{
		Id: &corev1.NetworkSegmentId{Value: hostBitsSubnetID.String()},
		Config: &corev1.NetworkSegmentConfig{
			VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
			Prefixes: []*corev1.NetworkPrefix{
				{Prefix: "10.20.0.1/16", Gateway: cutil.GetPtr("10.20.0.2")},
			},
		},
		Metadata: &corev1.Metadata{Name: "host-bits-subnet"},
		Status:   &corev1.NetworkSegmentStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	subnetDAO := cdbm.NewSubnetDAO(dbSession)
	_, err = subnetDAO.GetByID(ctx, nil, hostBitsSubnetID, nil)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

	ipBlockDAO := cdbm.NewIPBlockDAO(dbSession)
	reloadedIPBlock, err := ipBlockDAO.GetByID(ctx, nil, ipBlock.ID, nil)
	require.NoError(t, err)
	assert.False(t, reloadedIPBlock.FullGrant)

	childPrefix, err := ipam.CreateChildIpamEntryForIPBlock(
		ctx, nil, dbSession, ipamStorage, reloadedIPBlock, reloadedIPBlock.PrefixLength,
	)
	require.NoError(t, err)
	assert.Equal(t, "10.20.0.0/16", childPrefix.Cidr)
	reloadedIPBlock, err = ipBlockDAO.GetByID(ctx, nil, ipBlock.ID, nil)
	require.NoError(t, err)
	assert.True(t, reloadedIPBlock.FullGrant)
}

func testCreateOrUpdateSubnetSkipsFullGrantIPBlock(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testSubnetSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testSubnetBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testSubnetBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testSubnetBuildVPC(t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser)
	ipBlock := testSubnetBuildIPBlock(
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

	_, err = ipam.CreateChildIpamEntryForIPBlock(
		ctx, nil, dbSession, ipamStorage, ipBlock, ipBlock.PrefixLength,
	)
	require.NoError(t, err)
	ipBlockDAO := cdbm.NewIPBlockDAO(dbSession)
	fullGrantedIPBlock, err := ipBlockDAO.GetByID(ctx, nil, ipBlock.ID, nil)
	require.NoError(t, err)
	require.True(t, fullGrantedIPBlock.FullGrant)

	childSubnetID := uuid.New()
	manager := ManageSubnet{dbSession: dbSession}
	created := manager.createOrUpdateSubnetFromSite(ctx, site, &corev1.NetworkSegment{
		Id: &corev1.NetworkSegmentId{Value: childSubnetID.String()},
		Config: &corev1.NetworkSegmentConfig{
			VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
			Prefixes: []*corev1.NetworkPrefix{
				{Prefix: "10.20.30.0/24", Gateway: cutil.GetPtr("10.20.30.1")},
			},
		},
		Metadata: &corev1.Metadata{Name: "full-grant-child-subnet"},
		Status:   &corev1.NetworkSegmentStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	subnetDAO := cdbm.NewSubnetDAO(dbSession)
	_, err = subnetDAO.GetByID(ctx, nil, childSubnetID, nil)
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

func testCreateOrUpdateSubnetSkipsRestoreWhenPrefixDiffers(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testSubnetSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testSubnetBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := testSubnetBuildSite(t, dbSession, provider, "test-site", providerUser)

	parentVpc := testSubnetBuildVPC(t, dbSession, "parent-vpc", provider, tenant, site, nil, nil, tenantUser)
	ipBlock := testSubnetBuildIPBlock(
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
	subnetID := uuid.New()
	subnetDAO := cdbm.NewSubnetDAO(dbSession)
	_, err = subnetDAO.Create(ctx, nil, cdbm.SubnetCreateInput{
		SubnetID:                   &subnetID,
		Name:                       "stored-subnet",
		Org:                        tenant.Org,
		SiteID:                     site.ID,
		VpcID:                      parentVpc.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &subnetID,
		RoutingType:                &ipBlock.RoutingType,
		IPv4Prefix:                 cutil.GetPtr("10.20.30.0"),
		IPv4Gateway:                cutil.GetPtr("10.20.30.1"),
		IPv4BlockID:                &ipBlock.ID,
		PrefixLength:               24,
		Status:                     cdbm.SubnetStatusDeleting,
		CreatedBy:                  tenantUser.ID,
	})
	require.NoError(t, err)
	require.NoError(t, subnetDAO.Delete(ctx, nil, subnetID))

	deleted, _, err := subnetDAO.GetAll(
		ctx,
		nil,
		cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{subnetID}, IncludeDeleted: true},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	require.NoError(t, err)
	require.Len(t, deleted, 1)
	require.NotNil(t, deleted[0].Deleted)
	deletedAt := *deleted[0].Deleted

	manager := ManageSubnet{dbSession: dbSession}
	created := manager.createOrUpdateSubnetFromSite(ctx, site, &corev1.NetworkSegment{
		Id: &corev1.NetworkSegmentId{Value: subnetID.String()},
		Config: &corev1.NetworkSegmentConfig{
			VpcId: &corev1.VpcId{Value: parentVpc.ID.String()},
			Prefixes: []*corev1.NetworkPrefix{
				{Prefix: reportedCIDR, Gateway: cutil.GetPtr("10.20.31.1")},
			},
		},
		Metadata: &corev1.Metadata{Name: "drifted-subnet"},
		Status:   &corev1.NetworkSegmentStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	stillDeleted, _, err := subnetDAO.GetAll(
		ctx,
		nil,
		cdbm.SubnetFilterInput{SubnetIDs: []uuid.UUID{subnetID}, IncludeDeleted: true},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	require.NoError(t, err)
	require.Len(t, stillDeleted, 1)
	require.NotNil(t, stillDeleted[0].Deleted)
	assert.Equal(t, deletedAt, *stillDeleted[0].Deleted)
	require.NotNil(t, stillDeleted[0].IPv4Prefix)
	assert.Equal(t, "10.20.30.0", *stillDeleted[0].IPv4Prefix)

	_, err = subnetDAO.GetByID(ctx, nil, subnetID, nil)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)

	ipamer := cipam.NewWithStorage(ipamStorage)
	ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
		ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String(),
	))
	assert.Nil(t, ipamer.PrefixFrom(ctx, reportedCIDR))
	assert.Nil(t, ipamer.PrefixFrom(ctx, storedCIDR))
}

func testCreateOrUpdateSubnetSkipsIDOwnedByDifferentSite(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	providerOrg := "test-provider-org"
	providerUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), providerOrg, []string{"FORGE_PROVIDER_ADMIN"})
	provider := testSubnetSiteBuildInfrastructureProvider(t, dbSession, "test-provider", providerOrg, providerUser)
	tenantOrg := "test-tenant-org"
	tenantUser := testSubnetBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{"FORGE_TENANT_ADMIN"})
	tenant := testSubnetBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	siteA := testSubnetBuildSite(t, dbSession, provider, "test-site-a", providerUser)
	siteB := testSubnetBuildSite(t, dbSession, provider, "test-site-b", providerUser)

	vpcA := testSubnetBuildVPC(t, dbSession, "vpc-a", provider, tenant, siteA, nil, nil, tenantUser)
	vpcB := testSubnetBuildVPC(t, dbSession, "vpc-b", provider, tenant, siteB, nil, nil, tenantUser)

	ipBlockA := testSubnetBuildIPBlock(
		t, dbSession, "ip-block-a", siteA, provider, &tenant.ID,
		cdbm.IPBlockRoutingTypeDatacenterOnly, "10.10.0.0", 16,
		cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tenantUser,
	)
	ipBlockB := testSubnetBuildIPBlock(
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

	sharedSubnetID := uuid.New()
	prefixA := "10.10.1.0"
	prefixLengthA := 24
	existing, err := cdbm.NewSubnetDAO(dbSession).Create(ctx, nil, cdbm.SubnetCreateInput{
		SubnetID:                   &sharedSubnetID,
		Name:                       "site-a-subnet",
		Org:                        tenant.Org,
		SiteID:                     siteA.ID,
		VpcID:                      vpcA.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &sharedSubnetID,
		RoutingType:                &ipBlockA.RoutingType,
		IPv4Prefix:                 &prefixA,
		IPv4Gateway:                cutil.GetPtr("10.10.1.1"),
		IPv4BlockID:                &ipBlockA.ID,
		PrefixLength:               prefixLengthA,
		Status:                     cdbm.SubnetStatusReady,
		CreatedBy:                  tenantUser.ID,
	})
	require.NoError(t, err)
	require.Equal(t, sharedSubnetID, existing.ID)
	require.Equal(t, siteA.ID, existing.SiteID)

	manager := ManageSubnet{dbSession: dbSession}
	created := manager.createOrUpdateSubnetFromSite(ctx, siteB, &corev1.NetworkSegment{
		Id: &corev1.NetworkSegmentId{Value: sharedSubnetID.String()},
		Config: &corev1.NetworkSegmentConfig{
			VpcId: &corev1.VpcId{Value: vpcB.ID.String()},
			Prefixes: []*corev1.NetworkPrefix{
				{Prefix: "10.20.1.0/24", Gateway: cutil.GetPtr("10.20.1.1")},
			},
		},
		Metadata: &corev1.Metadata{Name: "site-b-collision-subnet"},
		Status:   &corev1.NetworkSegmentStatus{TenantState: corev1.TenantState_READY},
	})
	assert.Nil(t, created)

	subnetDAO := cdbm.NewSubnetDAO(dbSession)
	stillOriginal, err := subnetDAO.GetByID(ctx, nil, sharedSubnetID, nil)
	require.NoError(t, err)
	assert.Equal(t, siteA.ID, stillOriginal.SiteID)
	require.NotNil(t, stillOriginal.IPv4Prefix)
	assert.Equal(t, prefixA, *stillOriginal.IPv4Prefix)
	assert.Equal(t, "site-a-subnet", stillOriginal.Name)
	assert.Nil(t, stillOriginal.Deleted)

	siteBSubnets, _, err := subnetDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{
		SiteIDs: []uuid.UUID{siteB.ID},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	require.NoError(t, err)
	assert.Empty(t, siteBSubnets)

	ipamer := cipam.NewWithStorage(ipamStorage)
	ipamer.SetNamespace(ipam.GetIpamNamespaceForIPBlock(
		ctx, ipBlockB.RoutingType, ipBlockB.InfrastructureProviderID.String(), ipBlockB.SiteID.String(),
	))
	assert.Nil(t, ipamer.PrefixFrom(ctx, "10.20.1.0/24"))
}

// Test Subnet Metrics - CREATE operations
func Test_SubnetMetrics_Create_PendingToReady(t *testing.T) {
	// Case 1: pending -> ready (should emit metric with duration t2-t1)
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageSubnetLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testSubnetID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // pending started
	t2 := baseTime.Add(150 * time.Millisecond)         // ready achieved
	createTime := baseTime.Add(200 * time.Millisecond) // create event happened
	expectedDuration := t2.Sub(t1)                     // 150ms

	// t1: pending
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusPending, nil, t1)

	// t2: ready
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusReady, nil, t2)

	// Process create event
	ctx := context.Background()
	err := lifecycleMetrics.RecordSubnetStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testSubnetID, Created: &createTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted with correct duration (150ms)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_subnet_operation_latency_seconds", 1, map[string]string{
		"operation_type": "create",
		"from_status":    cdbm.SubnetStatusPending,
		"to_status":      cdbm.SubnetStatusReady,
	}, expectedDuration)
}

func Test_SubnetMetrics_Create_PendingErrorReady(t *testing.T) {
	// Case 2: pending -> error -> ready (should emit metric with duration t3-t1, ignoring error)
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageSubnetLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testSubnetID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // pending started
	t2 := baseTime.Add(100 * time.Millisecond)         // error occurred
	t3 := baseTime.Add(250 * time.Millisecond)         // ready achieved
	createTime := baseTime.Add(300 * time.Millisecond) // create event happened
	expectedDuration := t3.Sub(t1)                     // 250ms (ignoring error)

	// t1: pending
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusPending, nil, t1)

	// t2: error
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusError, nil, t2)

	// t3: ready
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusReady, nil, t3)

	// Process create event
	ctx := context.Background()
	err := lifecycleMetrics.RecordSubnetStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testSubnetID, Created: &createTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted with duration t3-t1 (250ms)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_subnet_operation_latency_seconds", 1, map[string]string{
		"operation_type": "create",
		"from_status":    cdbm.SubnetStatusPending,
		"to_status":      cdbm.SubnetStatusReady,
	}, expectedDuration)
}

func Test_SubnetMetrics_Create_ReadyErrorReady(t *testing.T) {
	// Case 3: ready -> error -> ready (should NOT emit metric, duplicate ready)
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageSubnetLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testSubnetID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // ready (initial)
	t2 := baseTime.Add(100 * time.Millisecond)         // error occurred
	t3 := baseTime.Add(200 * time.Millisecond)         // ready (duplicate)
	createTime := baseTime.Add(300 * time.Millisecond) // create event happened

	// t1: ready (initial)
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusReady, nil, t1)

	// t2: error
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusError, nil, t2)

	// t3: ready (duplicate)
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusReady, nil, t3)

	// Process create event
	ctx := context.Background()
	err := lifecycleMetrics.RecordSubnetStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testSubnetID, Created: &createTime},
	})
	assert.NoError(t, err)

	// Verify NO metric was emitted (duplicate ready, no pending->ready transition)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_subnet_operation_latency_seconds", 0, nil, 0)
}

// Test Subnet Metrics - DELETE operations
func Test_SubnetMetrics_Delete_DeletingOnly(t *testing.T) {
	// Case 1: deleting (should emit metric with duration now-t1)
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageSubnetLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testSubnetID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // deleting started
	deleteTime := baseTime.Add(180 * time.Millisecond) // delete happened 180ms later
	expectedDuration := deleteTime.Sub(t1)

	// t1: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusDeleting, nil, t1)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordSubnetStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testSubnetID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted with correct duration (180ms)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_subnet_operation_latency_seconds", 1, map[string]string{
		"operation_type": "delete",
		"from_status":    cdbm.SubnetStatusDeleting,
		"to_status":      cdbm.SubnetStatusDeleted,
	}, expectedDuration)
}

func Test_SubnetMetrics_Delete_MultipleDeletingTerminating(t *testing.T) {
	// Case 2: deleting -> deleting -> deleting (should emit metric with duration now-t1)
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageSubnetLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testSubnetID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime                                     // first deleting
	t2 := baseTime.Add(60 * time.Millisecond)          // second deleting
	t3 := baseTime.Add(120 * time.Millisecond)         // third deleting
	deleteTime := baseTime.Add(350 * time.Millisecond) // delete happened
	expectedDuration := deleteTime.Sub(t1)             // should use first deleting timestamp

	// t1: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusDeleting, nil, t1)

	// t2: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusDeleting, nil, t2)

	// t3: deleting
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusDeleting, nil, t3)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordSubnetStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testSubnetID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify metric was emitted (should use first deleting timestamp, duration 350ms)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_subnet_operation_latency_seconds", 1, map[string]string{
		"operation_type": "delete",
		"from_status":    cdbm.SubnetStatusDeleting,
		"to_status":      cdbm.SubnetStatusDeleted,
	}, expectedDuration)
}

func Test_SubnetMetrics_Delete_NoDeleting(t *testing.T) {
	// Case 3: ready (no deleting, should NOT emit metric)
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	site := util.TestSetupSite(t, dbSession)
	reg := prometheus.NewRegistry()
	lifecycleMetrics := NewManageSubnetLifecycleMetrics(reg, dbSession, "nico_rest_workflow")
	testSubnetID := uuid.New()

	// Set precise timestamps
	baseTime := time.Now().Add(-1 * time.Hour)
	t1 := baseTime
	deleteTime := baseTime.Add(120 * time.Millisecond)

	// t1: ready (no deleting status)
	util.TestBuildStatusDetailWithTime(t, dbSession, testSubnetID.String(), cdbm.SubnetStatusReady, nil, t1)

	// Process delete event
	ctx := context.Background()
	err := lifecycleMetrics.RecordSubnetStatusTransitionMetrics(ctx, site.ID, []cwm.InventoryObjectLifecycleEvent{
		{ObjectID: testSubnetID, Deleted: &deleteTime},
	})
	assert.NoError(t, err)

	// Verify NO metric was emitted (no deleting status found)
	util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_subnet_operation_latency_seconds", 0, nil, 0)
}
