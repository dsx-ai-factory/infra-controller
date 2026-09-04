// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"
	"google.golang.org/protobuf/encoding/protojson"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"

	"go.temporal.io/api/enums/v1"
	temporalClient "go.temporal.io/sdk/client"
	tsdkConverter "go.temporal.io/sdk/converter"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
)

type failingSiteTemporalClientPool struct {
	err error
}

func (f failingSiteTemporalClientPool) GetClientByID(uuid.UUID) (temporalClient.Client, error) {
	return nil, f.err
}

func testMachineInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testMachineBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, org, name string) *cdbm.InfrastructureProvider {
	ip := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: name,
		Org:  org,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testMachineBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, status string) *cdbm.Site {
	st := &cdbm.Site{
		ID:                          uuid.New(),
		Name:                        name,
		Org:                         ip.Org,
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:           false,
		SerialConsoleHostname:       cutil.GetPtr("TestSshHostname"),
		Status:                      status,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testMachineBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, orgs []string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	OrgData := cdbm.OrgData{}
	for _, org := range orgs {
		OrgData[org] = cdbm.Org{
			ID:          123,
			Name:        org,
			DisplayName: org,
			OrgType:     "ENTERPRISE",
			Roles:       roles,
		}
	}
	u, err := uDAO.Create(
		context.Background(),
		nil,
		cdbm.UserCreateInput{
			AuxiliaryID: nil,
			StarfleetID: &starfleetID,
			Email:       cutil.GetPtr("jdoe@test.com"),
			FirstName:   cutil.GetPtr("John"),
			LastName:    cutil.GetPtr("Doe"),
			OrgData:     OrgData,
		},
	)
	assert.Nil(t, err)

	return u
}

func testMachineBuildMachine(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, instanceTypeID *uuid.UUID, controllerMachineType *string, isAssigned bool, isMissingOnSite bool, status string) *cdbm.Machine {
	mid := uuid.NewString()

	m := &cdbm.Machine{
		ID:                       mid,
		InfrastructureProviderID: ip,
		SiteID:                   site,
		InstanceTypeID:           instanceTypeID,
		ControllerMachineID:      mid,
		ControllerMachineType:    controllerMachineType,
		Metadata:                 nil,
		DefaultMacAddress:        cutil.GetPtr("00:1B:44:11:3A:B7"),
		IsAssigned:               isAssigned,
		IsMissingOnSite:          isMissingOnSite,
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func testMachineBuildMachineCapability(t *testing.T, dbSession *cdb.Session, mID *string, capabilityType cdbm.MachineCapabilityType, name string, capacity *string, count *int) *cdbm.MachineCapability {
	mc := &cdbm.MachineCapability{
		ID:             uuid.New(),
		MachineID:      mID,
		InstanceTypeID: nil,
		Type:           capabilityType,
		Name:           name,
		Capacity:       capacity,
		Count:          count,
		Created:        cdb.GetCurTime(),
		Updated:        cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(mc).Exec(context.Background())
	assert.Nil(t, err)
	return mc
}

func testMachineBuildMachineInterface(t *testing.T, dbSession *cdb.Session, mID string) *cdbm.MachineInterface {
	mi := &cdbm.MachineInterface{
		ID:                    uuid.New(),
		MachineID:             mID,
		ControllerInterfaceID: cutil.GetPtr(uuid.New()),
		ControllerSegmentID:   cutil.GetPtr(uuid.New()),
		Hostname:              cutil.GetPtr("test.com"),
		IsPrimary:             true,
		SubnetID:              nil,
		MacAddress:            cutil.GetPtr("00:00:00:00:00:00"),
		IPAddresses:           []string{"192.168.0.1, 172.168.0.1"},
		Created:               cdb.GetCurTime(),
		Updated:               cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(mi).Exec(context.Background())
	assert.Nil(t, err)
	return mi
}

func testMachineBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID string, status string, message *string) *cdbm.StatusDetail {
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)

	ssd, err := sdDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID, Status: status, Message: message})
	assert.NoError(t, err)

	return ssd
}

func testMachineBuildTenant(t *testing.T, dbSession *cdb.Session, org, name string) *cdbm.Tenant {
	tenant := &cdbm.Tenant{
		ID:             uuid.New(),
		Name:           name,
		Org:            org,
		OrgDisplayName: &name,
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testMachineEnableTenantAccountTargetedInstanceCreation(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenantID uuid.UUID) {
	taDAO := cdbm.NewTenantAccountDAO(dbSession)
	tas, _, err := taDAO.GetAll(context.Background(), nil, cdbm.TenantAccountFilterInput{
		InfrastructureProviderID: &ip.ID,
		TenantIDs:                []uuid.UUID{tenantID},
		Statuses:                 []string{cdbm.TenantAccountStatusReady},
	}, cdbp.PageInput{Limit: cutil.GetPtr(1)}, nil)
	require.NoError(t, err)
	require.NotEmpty(t, tas)

	_, err = taDAO.Update(context.Background(), nil, cdbm.TenantAccountUpdateInput{
		TenantAccountID: tas[0].ID,
		Config: &cdbm.TenantAccountConfig{
			TargetedInstanceCreation: true,
		},
	})
	assert.Nil(t, err)
}

func testMachineBuildVpc(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, tenant *cdbm.Tenant, org, name string) *cdbm.Vpc {
	vpc := &cdbm.Vpc{
		ID:                       uuid.New(),
		Name:                     name,
		Org:                      org,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		TenantID:                 tenant.ID,
		Status:                   cdbm.VpcStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func testMachineBuildInstanceType(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, name string) *cdbm.InstanceType {
	instanceType := &cdbm.InstanceType{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		InfinityResourceTypeID:   cutil.GetPtr(uuid.New()),
		SiteID:                   cutil.GetPtr(site.ID),
		Status:                   cdbm.InstanceTypeStatusPending,
	}
	_, err := dbSession.DB.NewInsert().Model(instanceType).Exec(context.Background())
	assert.Nil(t, err)
	return instanceType
}

func testMachineBuildAllocation(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, site *cdbm.Site, name string) *cdbm.Allocation {
	allocation := &cdbm.Allocation{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site.ID,
		Status:                   cdbm.AllocationStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(allocation).Exec(context.Background())
	assert.Nil(t, err)
	return allocation
}

func testMachineBuildAllocationContraints(t *testing.T, dbSession *cdb.Session, al *cdbm.Allocation, rt string, rtID uuid.UUID, ct string, cv int, user *cdbm.User) *cdbm.AllocationConstraint {
	alctDAO := cdbm.NewAllocationConstraintDAO(dbSession)

	alct, err := alctDAO.Create(context.Background(), nil, cdbm.AllocationConstraintCreateInput{
		AllocationID: al.ID, ResourceType: rt, ResourceTypeID: rtID,
		ConstraintType: ct, ConstraintValue: cv, CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	return alct
}

func testMachineBuildOperatingSystem(t *testing.T, dbSession *cdb.Session, name string, tenantId uuid.UUID, user *cdbm.User) *cdbm.OperatingSystem {
	operatingSystem := &cdbm.OperatingSystem{
		ID:        uuid.New(),
		Name:      name,
		TenantID:  &tenantId,
		Status:    cdbm.OperatingSystemStatusPending,
		CreatedBy: user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

func TestMachineHandler_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipunone := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, []string{})
	ipu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)
	ipuv := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipViewerRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnOrg3 := "test-tn-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2}, tnRoles)
	tnuo3 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg3}, tnRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg2, "infra-provider-2")
	ip3 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg3, "infra-provider-3")

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	site2 := testMachineBuildSite(t, dbSession, ip2, "test-site-2", cdbm.SiteStatusRegistered)
	site3 := testMachineBuildSite(t, dbSession, ip3, "test-site-3", cdbm.SiteStatusRegistered)
	siteo3 := testMachineBuildSite(t, dbSession, ip3, "test-site-o3", cdbm.SiteStatusRegistered)

	tenant := testMachineBuildTenant(t, dbSession, tnOrg1, "test-tenant-1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "test-tenant-2")
	tenant3 := testMachineBuildTenant(t, dbSession, tnOrg3, "test-tenant-o3")
	_ = common.TestBuildTenantAccount(t, dbSession, ip3, &tenant3.ID, tnOrg3, cdbm.TenantAccountStatusReady, tnuo3)
	testMachineEnableTenantAccountTargetedInstanceCreation(t, dbSession, ip3, tenant3.ID)

	common.TestBuildTenantSite(t, dbSession, tenant, site, ipu)
	common.TestBuildTenantSite(t, dbSession, tenant2, site2, ipu)
	common.TestBuildTenantSite(t, dbSession, tenant2, site3, ipu)
	common.TestBuildTenantSite(t, dbSession, tenant2, siteo3, ipu)

	ist := testMachineBuildInstanceType(t, dbSession, ip, site, "instance-type-1")
	ist2 := testMachineBuildInstanceType(t, dbSession, ip, site2, "instance-type-2")
	ist3 := testMachineBuildInstanceType(t, dbSession, ip, site3, "instance-type-3")

	m := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ist.ID), nil, false, false, cdbm.MachineStatusInUse)
	m2 := testMachineBuildMachine(t, dbSession, ip2.ID, site2.ID, cutil.GetPtr(ist2.ID), nil, false, false, cdbm.MachineStatusInUse)
	m3 := testMachineBuildMachine(t, dbSession, ip3.ID, site3.ID, cutil.GetPtr(ist3.ID), nil, true, false, cdbm.MachineStatusInitializing)
	m4 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ist.ID), nil, true, false, cdbm.MachineStatusInitializing)
	mo3 := testMachineBuildMachine(t, dbSession, ip3.ID, siteo3.ID, nil, nil, true, false, cdbm.MachineStatusInitializing)

	// Mangle the machine metadata to make sure we can still deal with it gracefully.
	dbSession.DB.Exec(fmt.Sprintf(`update machine set metadata='{"random_junk": "is_it_cake?"}' where id='%s'`, m4.ID))

	mc11 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0GHz"), cutil.GetPtr(2))
	assert.NotNil(t, mc11)
	mc12 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeMemory, "Corsair Venegeance LPX", cutil.GetPtr("16GB DDR4"), cutil.GetPtr(4))
	assert.NotNil(t, mc12)
	mi1 := testMachineBuildMachineInterface(t, dbSession, m.ID)
	assert.NotNil(t, mi1)

	mc21 := testMachineBuildMachineCapability(t, dbSession, &m2.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0GHz"), cutil.GetPtr(2))
	assert.NotNil(t, mc21)
	mi2 := testMachineBuildMachineInterface(t, dbSession, m2.ID)
	assert.NotNil(t, mi2)

	testMachineBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusInUse, nil)
	testMachineBuildStatusDetail(t, dbSession, m2.ID, cdbm.MachineStatusInUse, nil)
	testMachineBuildStatusDetail(t, dbSession, m3.ID, cdbm.MachineStatusInitializing, nil)

	// Build Instances
	insDAO := cdbm.NewInstanceDAO(dbSession)

	vpc := testMachineBuildVpc(t, dbSession, ip, site, tenant, tnOrg1, "test-vpc-1")
	allocation := testMachineBuildAllocation(t, dbSession, ip, tenant, site, "test-allocation-1")
	_ = testInstanceSiteBuildAllocationContraints(t, dbSession, allocation, cdbm.AllocationResourceTypeInstanceType, ist.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	os := testMachineBuildOperatingSystem(t, dbSession, "test-os-1", tenant.ID, tnu)

	ins, err := insDAO.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-1",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ist.ID,
			VpcID:                    vpc.ID,
			MachineID:                &m.ID,
			OperatingSystemID:        cutil.GetPtr(os.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("test-user-data"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.NoError(t, err)

	vpc2 := testMachineBuildVpc(t, dbSession, ip2, site2, tenant2, tnOrg2, "test-vpc-2")
	allocation2 := testMachineBuildAllocation(t, dbSession, ip2, tenant2, site2, "test-allocation-2")
	_ = testInstanceSiteBuildAllocationContraints(t, dbSession, allocation2, cdbm.AllocationResourceTypeInstanceType, ist2.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	os2 := testMachineBuildOperatingSystem(t, dbSession, "test-os-2", tenant2.ID, tnu)

	ins2, _ := insDAO.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-2",
			TenantID:                 tenant2.ID,
			InfrastructureProviderID: ip2.ID,
			SiteID:                   site2.ID,
			InstanceTypeID:           &ist2.ID,
			VpcID:                    vpc2.ID,
			MachineID:                &m2.ID,
			OperatingSystemID:        cutil.GetPtr(os2.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("test-user-data"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)

	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		mID                               string
		expectedErr                       bool
		expectedStatus                    int
		expectedID                        string
		expectedSdCnt                     int
		expectedMiCnt                     int
		expectedMcCnt                     int
		expectInstanceID                  *uuid.UUID
		expectTenantID                    *uuid.UUID
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when user not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			mID:            m.ID,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when user not found in org",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			mID:            m.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when infrastructure provider not found for org",
			reqOrgName:     tnOrg1,
			user:           tnu,
			mID:            m3.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when machine id not found",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            uuid.New().String(),
			expectedErr:    true,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when machine's infrastructure provider does not match org's infra provider and there is no Tenant",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            m2.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "success case for infra provider admin",
			reqOrgName:         ipOrg1,
			user:               ipu,
			mID:                m.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedID:         m.ID,
			expectedSdCnt:      1,
			expectedMiCnt:      1,
			expectedMcCnt:      2,
			verifyChildSpanner: true,
		},
		{
			name:               "success case for infra provider admin even when the metadata of the machine is mangled",
			reqOrgName:         ipOrg1,
			user:               ipu,
			mID:                m4.ID,
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedID:         m4.ID,
			expectedSdCnt:      0,
			expectedMiCnt:      0,
			expectedMcCnt:      0,
			verifyChildSpanner: true,
		},
		{
			name:           "failure case when user missing org role",
			reqOrgName:     ipOrg1,
			user:           ipunone,
			mID:            m.ID,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			expectedID:     m.ID,
			expectedSdCnt:  1,
			expectedMiCnt:  1,
			expectedMcCnt:  2,
		},
		{
			name:           "success case when user has Provider viewer role",
			reqOrgName:     ipOrg1,
			user:           ipuv,
			mID:            m.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedID:     m.ID,
			expectedSdCnt:  1,
			expectedMiCnt:  1,
			expectedMcCnt:  2,
		},
		{
			name:             "success case for Tenant when they have an associated Instance",
			reqOrgName:       tnOrg1,
			user:             tnu,
			mID:              m.ID,
			expectedErr:      false,
			expectedStatus:   http.StatusOK,
			expectedID:       m.ID,
			expectedSdCnt:    1,
			expectedMiCnt:    1,
			expectedMcCnt:    2,
			expectInstanceID: &ins.ID,
			expectTenantID:   &tenant.ID,
		},
		{
			name:             "success case for Tenant when they have an associated Instance - additional",
			reqOrgName:       tnOrg2,
			user:             tnu,
			mID:              m2.ID,
			expectedErr:      false,
			expectedStatus:   http.StatusOK,
			expectedID:       m2.ID,
			expectedSdCnt:    1,
			expectedMiCnt:    1,
			expectedMcCnt:    1,
			expectInstanceID: &ins2.ID,
			expectTenantID:   &tenant2.ID,
		},
		{
			name:           "success case for Tenant with TenantAdmin role and with TargetedInstanceCreation capability",
			reqOrgName:     tnOrg3,
			user:           tnuo3,
			mID:            mo3.ID,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedID:     mo3.ID,
		},
		{
			name:             "success case for Provider with Instance returned",
			reqOrgName:       ipOrg1,
			user:             ipu,
			mID:              m.ID,
			expectedErr:      false,
			expectedStatus:   http.StatusOK,
			expectedID:       m.ID,
			expectedSdCnt:    1,
			expectedMiCnt:    1,
			expectedMcCnt:    2,
			expectInstanceID: &ins.ID,
			expectTenantID:   &tenant.ID,
		},
		{
			name:                              "successfully get machine include relation in the case for infra provider",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			mID:                               m.ID,
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedID:                        m.ID,
			expectedSdCnt:                     1,
			expectedMiCnt:                     1,
			expectedMcCnt:                     2,
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.SiteRelationName),
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			q := req.URL.Query()

			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req.URL.RawQuery = q.Encode()
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.mID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			mh := GetMachineHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := mh.Handle(ec)
			assert.Nil(t, err)
			if rec.Code != tc.expectedStatus {
				fmt.Println(rec.Body.String())
			}
			assert.Equal(t, tc.expectedErr, rec.Code != http.StatusOK, fmt.Sprintf("Response body: %v", rec.Body.String()))
			assert.Equal(t, tc.expectedStatus, rec.Code)
			if !tc.expectedErr {
				rsp := &model.APIMachine{}
				b := rec.Body.Bytes()
				fmt.Println(string(b))
				err := json.Unmarshal(b, rsp)
				assert.Nil(t, err)
				assert.Equal(t, tc.expectedID, rsp.ID)
				assert.Equal(t, tc.expectedSdCnt, len(rsp.StatusHistory))
				assert.Equal(t, tc.expectedMcCnt, len(rsp.MachineCapabilities))
				assert.Equal(t, tc.expectedMiCnt, len(rsp.MachineInterfaces))

				if tc.expectInstanceID != nil {
					assert.Equal(t, tc.expectInstanceID.String(), *rsp.InstanceID)
					assert.NotNil(t, rsp.Instance)
				}
				if tc.expectTenantID != nil {
					assert.Equal(t, tc.expectTenantID.String(), *rsp.TenantID)
					assert.NotNil(t, rsp.Tenant)
				}

				if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil {
					if tc.expectedInfrastructureProviderOrg != nil {
						assert.Equal(t, *tc.expectedInfrastructureProviderOrg, rsp.InfrastructureProvider.Org)
					}
					if tc.expectedSiteName != nil {
						assert.Equal(t, *tc.expectedSiteName, rsp.Site.Name)
					}
				} else {
					assert.Nil(t, rsp.InfrastructureProvider)
					assert.Nil(t, rsp.Site)
				}
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

}

func TestMachineHandler_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipOrg4 := "test-ip-org-4"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipunone := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg4}, []string{})
	ipu := testMachineBuildUser(t, dbSession, "TestMachineHandler_GetAll", []string{ipOrg1, ipOrg2, ipOrg3, ipOrg4}, ipRoles)
	ipuv := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3, ipOrg4}, ipViewerRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg2, "infraProvider2")
	ip4 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg4, "infraProvider4")

	site := testMachineBuildSite(t, dbSession, ip, "testSite1", cdbm.SiteStatusRegistered)
	site2 := testMachineBuildSite(t, dbSession, ip2, "testSite2", cdbm.SiteStatusRegistered)
	site3 := testMachineBuildSite(t, dbSession, ip4, "testSite3", cdbm.SiteStatusRegistered)

	tnOrg1 := "test-tn-org-1"
	tnRoles := []string{authz.TenantAdminRole}
	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles)

	tenant := testMachineBuildTenant(t, dbSession, tnOrg1, "test-tenant")
	vpc := testMachineBuildVpc(t, dbSession, ip, site, tenant, tnOrg1, "test-vpc")
	al := testMachineBuildAllocation(t, dbSession, ip, tenant, site, "test-allocation")

	tnOrg2 := "test-tn-org-2"
	ipt2 := testMachineBuildInfrastructureProvider(t, dbSession, tnOrg2, "infraProviderT2")
	siteT2 := testMachineBuildSite(t, dbSession, ipt2, "testSiteT2", cdbm.SiteStatusRegistered)
	tnu2 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg2}, tnRoles)
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "test-tenant2")
	_ = common.TestBuildTenantAccount(t, dbSession, ipt2, &tenant2.ID, tnOrg2, cdbm.TenantAccountStatusReady, tnu2)

	it1 := common.TestBuildInstanceType(t, dbSession, "test-instance-1", cutil.GetPtr(uuid.New()), site, map[string]string{
		"name":        "test-instance-type-1",
		"description": "Test Instance Type 1 Description",
	}, ipu)
	it2 := common.TestBuildInstanceType(t, dbSession, "test-instance-2", cutil.GetPtr(uuid.New()), site, map[string]string{
		"name":        "test-instance-type-2",
		"description": "Test Instance Type 2 Description",
	}, ipu)
	_ = testInstanceSiteBuildAllocationContraints(t, dbSession, al, cdbm.AllocationResourceTypeInstanceType, it1.ID, cdbm.AllocationConstraintTypeReserved, 20, ipu)

	os := testMachineBuildOperatingSystem(t, dbSession, "test-os", tenant.ID, tnu)
	isd := cdbm.NewInstanceDAO(dbSession)

	totalCount := 30

	ms := []cdbm.Machine{}
	inss := []cdbm.Instance{}

	for i := 0; i < totalCount; i++ {
		var ipID, siteID uuid.UUID
		var itID *uuid.UUID
		isAssigned := false

		if i%2 == 0 {
			ipID = ip.ID
			siteID = site.ID
			itID = &it1.ID
			isAssigned = true
			if i%3 == 0 {
				itID = &it2.ID
			}
		} else {
			ipID = ip2.ID
			siteID = site2.ID
		}

		status := cdbm.MachineStatusInitializing
		if i%3 == 0 {
			status = cdbm.MachineStatusReady
		}

		m := testMachineBuildMachine(t, dbSession, ipID, siteID, itID, cutil.GetPtr(fmt.Sprintf("controller-machine-type-%02d", i)), isAssigned, false, status)

		mc1 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0GHz"), cutil.GetPtr(2))
		assert.NotNil(t, mc1)
		mc2 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeMemory, "Corsair Venegeance LPX", cutil.GetPtr("16GB DDR4"), cutil.GetPtr(4))
		assert.NotNil(t, mc2)
		mi1 := testMachineBuildMachineInterface(t, dbSession, m.ID)
		assert.NotNil(t, mi1)

		common.TestBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is being initialized"))
		common.TestBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusReady, cutil.GetPtr("Machine is ready for assignment"))

		// Create MachineInstanceType
		if i%2 == 0 {
			mit1 := common.TestBuildMachineInstanceType(t, dbSession, m, it1)
			assert.NotNil(t, mit1)
		}

		ms = append(ms, *m)

		// Create Instances for Machines on Site 1
		if i%2 == 0 {
			ins, err := isd.Create(
				context.Background(), nil,
				cdbm.InstanceCreateInput{
					Name:                     fmt.Sprintf("test-instance-%v", i),
					TenantID:                 tenant.ID,
					InfrastructureProviderID: ip.ID,
					SiteID:                   site.ID,
					InstanceTypeID:           &it1.ID,
					VpcID:                    vpc.ID,
					MachineID:                &m.ID,
					OperatingSystemID:        &os.ID,
					IpxeScript:               cutil.GetPtr("ipxe"),
					AlwaysBootWithCustomIpxe: true,
					UserData:                 cutil.GetPtr("test-user-data"),
					Labels:                   map[string]string{},
					Status:                   cdbm.InstanceStatusPending,
					CreatedBy:                tnu.ID,
				},
			)
			assert.Nil(t, err)
			assert.NotNil(t, ins)
			inss = append(inss, *ins)
		}
	}

	// Build Targeted Instance
	tnOrg4 := "test-tn-org-4"
	tnu4 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg4}, tnRoles)

	tnOrg5 := "test-tn-org-5"
	tnu5 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg5}, tnRoles)

	tnOrg6 := "test-tn-org-6"
	tnu6 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg6}, tnRoles)

	tenant4 := testMachineBuildTenant(t, dbSession, tnOrg4, "test-tenant-4")
	tenant5 := testMachineBuildTenant(t, dbSession, tnOrg5, "test-tenant-5")
	tenant6 := testMachineBuildTenant(t, dbSession, tnOrg6, "test-tenant-6")
	_ = common.TestBuildTenantAccount(t, dbSession, ip4, &tenant4.ID, tnOrg4, cdbm.TenantAccountStatusReady, tnu4)
	_ = common.TestBuildTenantAccount(t, dbSession, ip4, &tenant5.ID, tnOrg5, cdbm.TenantAccountStatusReady, tnu5)
	_ = common.TestBuildTenantAccount(t, dbSession, ip4, &tenant6.ID, tnOrg6, cdbm.TenantAccountStatusReady, tnu6)
	tenantIDsBeyondDefaultPage := make([]string, 0, cdbp.DefaultLimit+1)
	for i := range cdbp.DefaultLimit + 1 {
		tenantOrg := fmt.Sprintf("test-pagination-tenant-org-%d", i)
		paginationTenant := testMachineBuildTenant(t, dbSession, tenantOrg, fmt.Sprintf("test-pagination-tenant-%d", i))
		_ = common.TestBuildTenantAccount(t, dbSession, ip4, &paginationTenant.ID, tenantOrg, cdbm.TenantAccountStatusReady, ipu)
		tenantIDsBeyondDefaultPage = append(tenantIDsBeyondDefaultPage, paginationTenant.ID.String())
	}
	vpc4 := testMachineBuildVpc(t, dbSession, ip, site, tenant4, tnOrg4, "test-vpc-4")

	os4 := testMachineBuildOperatingSystem(t, dbSession, "test-os-4", tenant4.ID, tnu4)
	assert.NotNil(t, os4)
	os5 := testMachineBuildOperatingSystem(t, dbSession, "test-os-5", tenant5.ID, tnu5)
	assert.NotNil(t, os5)

	m31 := testMachineBuildMachine(t, dbSession, ip4.ID, site3.ID, nil, nil, false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m31)
	common.TestBuildStatusDetail(t, dbSession, m31.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is being initialized"))
	common.TestBuildStatusDetail(t, dbSession, m31.ID, cdbm.MachineStatusReady, cutil.GetPtr("Machine is ready for assignment"))

	m32 := testMachineBuildMachine(t, dbSession, ip4.ID, site3.ID, nil, nil, false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m32)
	common.TestBuildStatusDetail(t, dbSession, m32.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is being initialized"))
	common.TestBuildStatusDetail(t, dbSession, m32.ID, cdbm.MachineStatusReady, cutil.GetPtr("Machine is ready for assignment"))

	m33 := testMachineBuildMachine(t, dbSession, ip4.ID, site3.ID, nil, nil, false, true, cdbm.MachineStatusError)
	assert.NotNil(t, m33)
	common.TestBuildStatusDetail(t, dbSession, m33.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is being initialized"))
	common.TestBuildStatusDetail(t, dbSession, m33.ID, cdbm.MachineStatusError, cutil.GetPtr("Machine is missing on Site"))

	m34 := testMachineBuildMachine(t, dbSession, ip4.ID, site3.ID, nil, nil, false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m34)
	common.TestBuildStatusDetail(t, dbSession, m34.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is being initialized"))
	common.TestBuildStatusDetail(t, dbSession, m34.ID, cdbm.MachineStatusReady, cutil.GetPtr("Machine is ready for assignment"))

	ins31, err := isd.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     fmt.Sprintf("test-instance-targeted-31"),
			TenantID:                 tenant4.ID,
			InfrastructureProviderID: ip4.ID,
			SiteID:                   site3.ID,
			InstanceTypeID:           nil,
			VpcID:                    vpc4.ID,
			MachineID:                &m31.ID,
			OperatingSystemID:        &os4.ID,
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("test-user-data"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu4.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ins31)

	ins32, err := isd.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     fmt.Sprintf("test-instance-targeted-32"),
			TenantID:                 tenant4.ID,
			InfrastructureProviderID: ip4.ID,
			SiteID:                   site3.ID,
			InstanceTypeID:           nil,
			VpcID:                    vpc4.ID,
			MachineID:                &m32.ID,
			OperatingSystemID:        &os4.ID,
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("test-user-data"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu4.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ins32)

	ins34, err := isd.Create(

		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     fmt.Sprintf("test-instance-targeted-34"),
			TenantID:                 tenant5.ID,
			InfrastructureProviderID: ip4.ID,
			SiteID:                   site3.ID,
			InstanceTypeID:           nil,
			VpcID:                    vpc4.ID,
			MachineID:                &m34.ID,
			OperatingSystemID:        &os5.ID,
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("test-user-data"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu5.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, ins34)

	tnOrg7 := "test-tn-org-7"
	tnu7 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg7}, tnRoles)
	tenant7 := testMachineBuildTenant(t, dbSession, tnOrg7, "test-tenant7")
	_ = common.TestBuildTenantAccount(t, dbSession, ip, &tenant7.ID, tnOrg7, cdbm.TenantAccountStatusReady, tnu7)
	testMachineEnableTenantAccountTargetedInstanceCreation(t, dbSession, ip, tenant7.ID)

	tnOrg8 := "test-tn-org-8"
	tnu8 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg8}, tnRoles)
	_ = testMachineBuildTenant(t, dbSession, tnOrg8, "test-tenant8")
	cfg := common.GetTestConfig()
	tempClient := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name                              string
		reqOrgName                        string
		user                              *cdbm.User
		querySiteID                       *string
		queryInstanceTypeID               []string
		queryID                           []string
		queryTenantID                     []string
		queryCapabilityType               *string
		queryCapabilityName               []string
		queryStatus                       []string
		querySearch                       *string
		queryHasInstanceType              *bool
		queryHasInstance                  *bool
		queryIsMissingOnSite              *bool
		queryIncludeMetadata              *bool
		queryIncludeRelations1            *string
		queryIncludeRelations2            *string
		queryIncludeRelations3            *string
		pageNumber                        *int
		pageSize                          *int
		orderBy                           *string
		expectedErr                       bool
		expectedStatus                    int
		expectedCnt                       int
		expectedTotal                     *int
		expectedFirstEntry                *cdbm.Machine
		expectedInfrastructureProviderOrg *string
		expectedSiteName                  *string
		expectedInstanceTypeName          *string
		expectedTargetedInstance          bool
		expectInstance                    bool
		expectedTenant                    *string
		verifyChildSpanner                bool
	}{
		{
			name:           "error when User is not found in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			expectedErr:    true,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "error when User does not have org membership",
			reqOrgName:     "SomeOrg",
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "error when Infrastructure Provider is not set up for org",
			reqOrgName:     ipOrg3,
			user:           ipu,
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when Site ID specified in query is an invalid UUID",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr("bad#uuid$str"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "error when non-existent Site ID is specified in query",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(uuid.New().String()),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:                "error when non-existent Instance Type ID specified in query",
			reqOrgName:          ipOrg1,
			user:                ipu,
			queryInstanceTypeID: []string{uuid.New().String()},
			expectedErr:         true,
			expectedStatus:      http.StatusBadRequest,
		},
		{
			name:           "error when Site's Provider does not match org's Provider",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(site2.ID.String()),
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "success case when Site ID specified in query",
			reqOrgName:         ipOrg1,
			user:               ipu,
			querySiteID:        cutil.GetPtr(site.ID.String()),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        totalCount / 2,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectInstance:     true,
			expectedTenant:     cutil.GetPtr(tenant.ID.String()),
			verifyChildSpanner: true,
		},
		{
			name:               "failure case when user missing org role",
			reqOrgName:         ipOrg1,
			user:               ipunone,
			querySiteID:        cutil.GetPtr(site.ID.String()),
			expectedErr:        true,
			expectedStatus:     http.StatusForbidden,
			expectedCnt:        totalCount / 2,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectInstance:     true,
			expectedTenant:     cutil.GetPtr(tenant.ID.String()),
			verifyChildSpanner: true,
		},
		{
			name:               "success case when user has Provider viewer role",
			reqOrgName:         ipOrg1,
			user:               ipuv,
			querySiteID:        cutil.GetPtr(site.ID.String()),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        totalCount / 2,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectInstance:     true,
			expectedTenant:     cutil.GetPtr(tenant.ID.String()),
			verifyChildSpanner: true,
		},
		{
			name:               "failure case when Tenant has no effective TargetedInstanceCreation for Site",
			reqOrgName:         tnOrg2,
			user:               tnu2,
			querySiteID:        cutil.GetPtr(siteT2.ID.String()),
			expectedErr:        true,
			expectedStatus:     http.StatusForbidden,
			verifyChildSpanner: true,
		},
		{
			name:           "success case when Tenant has TargetedInstanceCreation capability",
			reqOrgName:     tnOrg7,
			user:           tnu7,
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    totalCount / 2,
			expectedTotal:  cutil.GetPtr(totalCount / 2),
		},
		{
			name:           "failure case when Tenant has no effective TargetedInstanceCreation on any Site",
			reqOrgName:     tnOrg8,
			user:           tnu8,
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:                "success case when Instance Type ID specified in query",
			reqOrgName:          ipOrg1,
			user:                ipu,
			queryInstanceTypeID: []string{it1.ID.String()},
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			expectedCnt:         10,
			expectedTotal:       cutil.GetPtr(10),
		},
		{
			name:                "success case when multiple Instance Type IDs specified in query",
			reqOrgName:          ipOrg1,
			user:                ipu,
			queryInstanceTypeID: []string{it1.ID.String(), it2.ID.String()},
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			expectedCnt:         totalCount / 2,
			expectedTotal:       cutil.GetPtr(totalCount / 2),
		},

		{
			name:                 "success case when hasInstanceType is true in query",
			reqOrgName:           ipOrg1,
			user:                 ipu,
			queryHasInstanceType: cutil.GetPtr(true),
			expectedErr:          false,
			expectedStatus:       http.StatusOK,
			expectedCnt:          totalCount / 2,
			expectedTotal:        cutil.GetPtr(totalCount / 2),
		},
		{
			name:                 "success case when hasInstanceType is true and Site ID is specified in query",
			reqOrgName:           ipOrg1,
			user:                 ipu,
			querySiteID:          cutil.GetPtr(site.ID.String()),
			queryHasInstanceType: cutil.GetPtr(true),
			expectedErr:          false,
			expectedStatus:       http.StatusOK,
			expectedCnt:          totalCount / 2,
			expectedTotal:        cutil.GetPtr(totalCount / 2),
		},
		{
			name:                 "success case when hasInstanceType is false in query",
			reqOrgName:           ipOrg2,
			user:                 ipu,
			queryHasInstanceType: cutil.GetPtr(false),
			expectedErr:          false,
			expectedStatus:       http.StatusOK,
			expectedCnt:          totalCount / 2,
			expectedTotal:        cutil.GetPtr(totalCount / 2),
		},
		{
			name:                 "success case when hasInstanceType is set to false and Site ID is specified in query",
			reqOrgName:           ipOrg2,
			user:                 ipu,
			querySiteID:          cutil.GetPtr(site2.ID.String()),
			queryHasInstanceType: cutil.GetPtr(false),
			expectedErr:          false,
			expectedStatus:       http.StatusOK,
			expectedCnt:          totalCount / 2,
			expectedTotal:        cutil.GetPtr(totalCount / 2),
		},
		{
			name:               "success when pagination params are specified",
			reqOrgName:         ipOrg1,
			user:               ipu,
			querySiteID:        cutil.GetPtr(site.ID.String()),
			pageNumber:         cutil.GetPtr(1),
			pageSize:           cutil.GetPtr(10),
			orderBy:            cutil.GetPtr("CREATED_DESC"),
			expectedErr:        false,
			expectedStatus:     http.StatusOK,
			expectedCnt:        10,
			expectedTotal:      cutil.GetPtr(totalCount / 2),
			expectedFirstEntry: &ms[28],
		},
		{
			name:           "failure when invalid pagination params are specified",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			pageNumber:     cutil.GetPtr(1),
			pageSize:       cutil.GetPtr(10),
			orderBy:        cutil.GetPtr("TEST_ASC"),
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:                              "success when include relation with site",
			reqOrgName:                        ipOrg1,
			user:                              ipu,
			querySiteID:                       cutil.GetPtr(site.ID.String()),
			queryInstanceTypeID:               []string{it1.ID.String()},
			expectedErr:                       false,
			expectedStatus:                    http.StatusOK,
			expectedCnt:                       10,
			expectedTotal:                     cutil.GetPtr(10),
			queryIncludeRelations1:            cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelations2:            cutil.GetPtr(cdbm.SiteRelationName),
			queryIncludeRelations3:            cutil.GetPtr(cdbm.InstanceTypeRelationName),
			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedSiteName:                  cutil.GetPtr(site.Name),
			expectedInstanceTypeName:          cutil.GetPtr(it1.Name),
		},
		{
			name:           "success when MachineStatusInitializing status is specified",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			queryStatus:    []string{cdbm.MachineStatusInitializing},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    10,
			expectedTotal:  cutil.GetPtr(10),
		},
		{
			name:           "success when multiple statuses are specified",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			queryStatus:    []string{cdbm.MachineStatusInitializing, cdbm.MachineStatusReady},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    15,
			expectedTotal:  cutil.GetPtr(15),
		},
		{
			name:           "success when MachineStatusReady status is specified query search",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySearch:    cutil.GetPtr(cdbm.MachineStatusReady),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    5,
			expectedTotal:  cutil.GetPtr(5),
		},
		{
			name:           "success when Machine id is specified query search",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySearch:    cutil.GetPtr(ms[0].ID),
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    1,
			expectedTotal:  cutil.GetPtr(1),
		},
		{
			name:           "success when BadStatus status is specified",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			queryStatus:    []string{"BadStatus"},
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "success when machine id is specified in query",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			queryID:        []string{ms[0].ID},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    1,
			expectedTotal:  cutil.GetPtr(1),
		},
		{
			name:           "success when multiple machine id's specified in query",
			reqOrgName:     ipOrg1,
			user:           ipu,
			querySiteID:    cutil.GetPtr(site.ID.String()),
			queryID:        []string{ms[0].ID, ms[2].ID},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    2,
			expectedTotal:  cutil.GetPtr(2),
		},
		{
			name:                "success when valid capability type is specified in query",
			reqOrgName:          ipOrg1,
			user:                ipu,
			queryCapabilityType: cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeCPU),
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			expectedCnt:         totalCount / 2,
		},
		{
			name:                "failure when invalid capability type params are specified",
			reqOrgName:          ipOrg1,
			user:                ipu,
			querySiteID:         cutil.GetPtr(site.ID.String()),
			queryCapabilityType: cutil.GetPtr("ETHERNET"),
			expectedErr:         true,
			expectedStatus:      http.StatusBadRequest,
			expectedCnt:         0,
		},
		{
			name:                "success when valid capability name is specified in query",
			reqOrgName:          ipOrg1,
			user:                ipu,
			queryCapabilityName: []string{"AMD Opteron Series x10"},
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			expectedCnt:         totalCount / 2,
		},
		{
			name:                "success when multiple valid capability names are specified in query",
			reqOrgName:          ipOrg1,
			user:                ipu,
			queryCapabilityName: []string{"AMD Opteron Series x10", "Corsair Venegeance LPX"},
			expectedErr:         false,
			expectedStatus:      http.StatusOK,
			expectedCnt:         totalCount / 2,
		},
		{
			name:                     "success case when Site ID specified in query in case of targeted instance",
			reqOrgName:               ipOrg4,
			user:                     ipu,
			querySiteID:              cutil.GetPtr(site3.ID.String()),
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedCnt:              4,
			expectedTotal:            cutil.GetPtr(4),
			expectInstance:           false,
			expectedTargetedInstance: true,
			verifyChildSpanner:       true,
		},
		{
			name:                     "success case when Tenant ID specified in query",
			reqOrgName:               ipOrg4,
			user:                     ipu,
			queryTenantID:            []string{tenant4.ID.String()},
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedCnt:              2,
			expectedTotal:            cutil.GetPtr(2),
			expectedTargetedInstance: true,
			expectedTenant:           cutil.GetPtr(tenant4.ID.String()),
		},
		{
			name:                     "success case when multiple Tenant ID's specified in query",
			reqOrgName:               ipOrg4,
			user:                     ipu,
			queryTenantID:            []string{tenant4.ID.String(), tenant5.ID.String()},
			expectedErr:              false,
			expectedStatus:           http.StatusOK,
			expectedCnt:              3,
			expectedTotal:            cutil.GetPtr(3),
			expectedTargetedInstance: true,
			expectedTenant:           cutil.GetPtr(tenant4.ID.String()),
		},
		{
			name:           "success case when Tenant IDs exceed the default TenantAccount page size",
			reqOrgName:     ipOrg4,
			user:           ipu,
			queryTenantID:  tenantIDsBeyondDefaultPage,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "returns nothing when tenant has no associated instance",
			reqOrgName:     ipOrg4,
			user:           ipu,
			queryTenantID:  []string{tenant6.ID.String()},
			expectedErr:    false,
			expectedStatus: http.StatusOK,
			expectedCnt:    0,
			expectedTotal:  cutil.GetPtr(0),
		},
		{
			name:           "failure case when invalid Tenant ID specified in query",
			reqOrgName:     ipOrg4,
			user:           ipu,
			queryTenantID:  []string{"invalid-tenant-id"},
			expectedErr:    true,
			expectedStatus: http.StatusBadRequest,
			expectedCnt:    0,
		},
		{
			name:           "failure case when Tenant ID specified in query does have an account with current org's Provider",
			reqOrgName:     ipOrg4,
			user:           ipu,
			queryTenantID:  []string{tenant2.ID.String()},
			expectedErr:    true,
			expectedStatus: http.StatusForbidden,
			expectedCnt:    0,
		},
		{
			name:             "success case when hasInstance is true in query",
			reqOrgName:       ipOrg1,
			user:             ipu,
			querySiteID:      cutil.GetPtr(site.ID.String()),
			queryHasInstance: cutil.GetPtr(true),
			expectedErr:      false,
			expectedStatus:   http.StatusOK,
			expectedCnt:      totalCount / 2,
			expectedTotal:    cutil.GetPtr(totalCount / 2),
		},
		{
			name:             "success case when hasInstance is false in query",
			reqOrgName:       ipOrg2,
			user:             ipu,
			querySiteID:      cutil.GetPtr(site2.ID.String()),
			queryHasInstance: cutil.GetPtr(false),
			expectedErr:      false,
			expectedStatus:   http.StatusOK,
			expectedCnt:      totalCount / 2,
			expectedTotal:    cutil.GetPtr(totalCount / 2),
		},
		{
			name:             "failure case when hasInstance is true but user is not a privileged Tenant",
			reqOrgName:       tnOrg1,
			user:             tnu,
			querySiteID:      cutil.GetPtr(site.ID.String()),
			queryHasInstance: cutil.GetPtr(true),
			expectedErr:      true,
			expectedStatus:   http.StatusForbidden,
		},
		{
			name:             "failure case when hasInstance is specified in query but siteId is not specified",
			reqOrgName:       ipOrg1,
			user:             ipu,
			queryHasInstance: cutil.GetPtr(true),
			expectedErr:      true,
			expectedStatus:   http.StatusBadRequest,
		},
		{
			name:             "failure case when privileged Tenant specifies hasInstance without siteId",
			reqOrgName:       tnOrg7,
			user:             tnu7,
			queryHasInstance: cutil.GetPtr(true),
			expectedErr:      true,
			expectedStatus:   http.StatusBadRequest,
		},
		{
			name:             "failure case when hasInstance is false but tenantId is specified",
			reqOrgName:       ipOrg1,
			user:             ipu,
			querySiteID:      cutil.GetPtr(site.ID.String()),
			queryHasInstance: cutil.GetPtr(false),
			queryTenantID:    []string{tenant7.ID.String()},
			expectedErr:      true,
			expectedStatus:   http.StatusBadRequest,
		},
		{
			name:                 "success case when isMissingOnSite is true in query",
			reqOrgName:           ipOrg4,
			user:                 ipu,
			querySiteID:          cutil.GetPtr(site3.ID.String()),
			queryIsMissingOnSite: cutil.GetPtr(true),
			expectedErr:          false,
			expectedStatus:       http.StatusOK,
			expectedCnt:          1,
			expectedTotal:        cutil.GetPtr(1),
		},
		{
			name:                 "success case when isMissingOnSite is false in query",
			reqOrgName:           ipOrg1,
			user:                 ipu,
			querySiteID:          cutil.GetPtr(site.ID.String()),
			queryIsMissingOnSite: cutil.GetPtr(false),
			expectedErr:          false,
			expectedStatus:       http.StatusOK,
			expectedCnt:          totalCount / 2,
			expectedTotal:        cutil.GetPtr(totalCount / 2),
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()

			q := url.Values{}
			if tc.querySiteID != nil {
				q.Add("siteId", *tc.querySiteID)
			}
			for _, id := range tc.queryID {
				q.Add("id", id)
			}
			for _, typeID := range tc.queryInstanceTypeID {
				q.Add("instanceTypeId", typeID)
			}
			for _, tenantID := range tc.queryTenantID {
				q.Add("tenantId", tenantID)
			}
			if tc.queryCapabilityType != nil {
				q.Add("capabilityType", *tc.queryCapabilityType)
			}
			for _, capName := range tc.queryCapabilityName {
				q.Add("capabilityName", capName)
			}
			for _, status := range tc.queryStatus {
				q.Add("status", status)
			}
			if tc.querySearch != nil {
				q.Add("query", *tc.querySearch)
			}
			if tc.queryHasInstanceType != nil {
				q.Add("hasInstanceType", strconv.FormatBool(*tc.queryHasInstanceType))
			}
			if tc.queryHasInstance != nil {
				q.Add("hasInstance", strconv.FormatBool(*tc.queryHasInstance))
			}
			if tc.queryIsMissingOnSite != nil {
				q.Add("isMissingOnSite", strconv.FormatBool(*tc.queryIsMissingOnSite))
			}
			if tc.queryIncludeMetadata != nil {
				q.Add("includeMetadata", strconv.FormatBool(*tc.queryIncludeMetadata))
			}
			if tc.queryIncludeRelations1 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations1)
			}
			if tc.queryIncludeRelations2 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations2)
			}
			if tc.queryIncludeRelations3 != nil {
				q.Add("includeRelation", *tc.queryIncludeRelations3)
			}
			if tc.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tc.pageNumber))
			}
			if tc.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tc.pageSize))
			}
			if tc.orderBy != nil {
				q.Set("orderBy", *tc.orderBy)
			}

			path := fmt.Sprintf("/v2/org/%s/nico/machine?%s", tc.reqOrgName, q.Encode())

			fmt.Println(path)

			req := httptest.NewRequest(http.MethodGet, path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)

			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tc.reqOrgName)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			gamh := GetAllMachineHandler{
				dbSession: dbSession,
				tc:        tempClient,
				cfg:       cfg,
			}
			err := gamh.Handle(ec)
			assert.Nil(t, err)
			if rec.Code != tc.expectedStatus {
				t.Errorf("response %v", rec.Body.String())
			}
			require.Equal(t, tc.expectedStatus, rec.Code)

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}

			if tc.expectedErr {
				return
			}

			resp := []model.APIMachine{}
			err = json.Unmarshal(rec.Body.Bytes(), &resp)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedCnt, len(resp))

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err = json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, pr.Total)
			}

			if tc.expectedFirstEntry != nil {
				assert.Equal(t, tc.expectedFirstEntry.ID, resp[0].ID)
			}

			if tc.queryIncludeRelations1 != nil || tc.queryIncludeRelations2 != nil || tc.queryIncludeRelations3 != nil {
				if tc.expectedInfrastructureProviderOrg != nil {
					assert.Equal(t, *tc.expectedInfrastructureProviderOrg, resp[0].InfrastructureProvider.Org)
				}
				if tc.expectedSiteName != nil {
					assert.Equal(t, *tc.expectedSiteName, resp[0].Site.Name)
				}
				if tc.expectedInstanceTypeName != nil {
					assert.Equal(t, *tc.expectedInstanceTypeName, resp[0].InstanceType.Name)
				}
			} else if len(resp) > 0 {
				assert.Nil(t, resp[0].InfrastructureProvider)
				assert.Nil(t, resp[0].Site)
			}

			for _, apim := range resp {
				assert.Equal(t, 2, len(apim.StatusHistory))
			}

			if tc.expectInstance {
				assert.NotNil(t, resp[0].InstanceID)
				assert.Equal(t, inss[0].ID.String(), *resp[0].InstanceID)
				assert.NotNil(t, resp[0].Instance)

				assert.NotNil(t, resp[1].InstanceID)
				assert.Equal(t, inss[1].ID.String(), *resp[1].InstanceID)
				assert.NotNil(t, resp[1].Instance)
			}

			if tc.expectedTenant != nil {
				assert.NotNil(t, resp[0].TenantID)
				assert.Equal(t, *tc.expectedTenant, *resp[0].TenantID)
				assert.NotNil(t, resp[0].Tenant)
			}

			if tc.expectedTargetedInstance {
				if len(resp) > 0 && resp[0].Instance != nil {
					assert.Equal(t, "", resp[0].Instance.InstanceTypeID)
				}
			}
		})
	}
}

func TestMachineHandler_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1}, tnRoles)
	tnu2 := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg2}, tnRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider")
	ip1 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg2, "infraProvider1")
	assert.NotNil(t, ip1)

	site := testMachineBuildSite(t, dbSession, ip, "testSite1", cdbm.SiteStatusRegistered)
	site2 := testMachineBuildSite(t, dbSession, ip1, "testSite2", cdbm.SiteStatusPending)
	site3 := testMachineBuildSite(t, dbSession, ip1, "testSite3", cdbm.SiteStatusRegistered)

	tenant := testMachineBuildTenant(t, dbSession, ipOrg1, "testTenant1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "testTenant2")
	_ = common.TestBuildTenantAccount(t, dbSession, ip, &tenant2.ID, tnOrg2, cdbm.TenantAccountStatusReady, tnu2)
	testMachineEnableTenantAccountTargetedInstanceCreation(t, dbSession, ip, tenant2.ID)

	instanceType1 := testMachineBuildInstanceType(t, dbSession, ip, site, "testInstanceType1")
	instanceType2 := testMachineBuildInstanceType(t, dbSession, ip, site, "testInstanceType2")
	instanceType3 := testMachineBuildInstanceType(t, dbSession, ip, site, "testInstanceType3")
	instanceType4 := testMachineBuildInstanceType(t, dbSession, ip, site, "testInstanceType4")
	instanceType5 := testMachineBuildInstanceType(t, dbSession, ip, site, "testInstanceType4")
	icap1 := common.TestCommonBuildMachineCapability(t, dbSession, nil, &instanceType5.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0Hz"), cutil.GetPtr("32GB"), nil, cutil.GetPtr(4), nil, nil)
	assert.NotNil(t, icap1)

	m := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType1.ID), cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReady)

	m1 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType2.ID), cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m1)

	m2 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType3.ID), cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m2)

	m3 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType3.ID), cutil.GetPtr("mcType"), false, true, cdbm.MachineStatusError)
	assert.NotNil(t, m3)

	m4 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType4.ID), cutil.GetPtr("mcType"), true, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m4)

	m5 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType4.ID), cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusError)
	assert.NotNil(t, m5)

	m6 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType2.ID), cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReset)
	assert.NotNil(t, m6)

	m7 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m7)

	m8 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType4.ID, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m8)

	m9 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, &instanceType4.ID, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m9)

	m10 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusReady)
	assert.NotNil(t, m10)

	m11 := testMachineBuildMachine(t, dbSession, ip.ID, site2.ID, nil, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusMaintenance)
	assert.NotNil(t, m11)

	m12 := testMachineBuildMachine(t, dbSession, ip.ID, site3.ID, nil, cutil.GetPtr("mcType"), false, false, cdbm.MachineStatusMaintenance)
	assert.NotNil(t, m12)

	mcap1 := common.TestCommonBuildMachineCapability(t, dbSession, &m9.ID, nil, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), nil, nil)
	assert.NotNil(t, mcap1)

	// build an instance
	vpc := testMachineBuildVpc(t, dbSession, ip, site, tenant, tnOrg1, "testVpc")

	allocation1 := testMachineBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation")
	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, allocation1, cdbm.AllocationResourceTypeInstanceType, instanceType1.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	assert.NotNil(t, alc1)

	allocation4 := testMachineBuildAllocation(t, dbSession, ip, tenant, site, "testAllocation4")
	_ = testInstanceSiteBuildAllocationContraints(t, dbSession, allocation4, cdbm.AllocationResourceTypeInstanceType, instanceType4.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)

	operatingSystem := testMachineBuildOperatingSystem(t, dbSession, "testOS", tenant.ID, tnu)
	isd := cdbm.NewInstanceDAO(dbSession)

	i1, err := isd.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     "testInst1",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType4.ID,
			VpcID:                    vpc.ID,
			MachineID:                &m4.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, i1)

	mOnlineRepairReady := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType2.ID), cutil.GetPtr("mcType"), true, false, cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, mOnlineRepairReady, instanceType2)
	iOnlineRepairReady, err := isd.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     "testOnlineRepairReady",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType2.ID,
			VpcID:                    vpc.ID,
			MachineID:                &mOnlineRepairReady.ID,
			Hostname:                 cutil.GetPtr("or-ready.example.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Status:                   cdbm.InstanceStatusReady,
			CreatedBy:                tnu.ID,
		},
	)
	require.NoError(t, err)

	mOnlineRepairMissing := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType2.ID), cutil.GetPtr("mcType"), true, true, cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, mOnlineRepairMissing, instanceType2)

	mOnlineRepairPending := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(instanceType2.ID), cutil.GetPtr("mcType"), true, false, cdbm.MachineStatusReady)
	_ = common.TestBuildMachineInstanceType(t, dbSession, mOnlineRepairPending, instanceType2)
	_, err = isd.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     "testOnlineRepairPending",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType2.ID,
			VpcID:                    vpc.ID,
			MachineID:                &mOnlineRepairPending.ID,
			Hostname:                 cutil.GetPtr("or-pending.example.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	require.NoError(t, err)

	buildOnlineRepairEnterRequest := func(allowAutoInstanceDeletion bool) *model.APIMachineUpdateRequest {
		return &model.APIMachineUpdateRequest{
			OnlineRepair: &model.APIMachineOnlineRepair{
				Enabled: cutil.GetPtr(true),
				Policy: &model.APIMachineOnlineRepairPolicy{
					AllowAutoInstanceDeletionOnFailure: cutil.GetPtr(allowAutoInstanceDeletion),
				},
				Acknowledgments: &model.APIMachineOnlineRepairAcknowledgments{
					AcceptDataCorruptionRisk:   cutil.GetPtr(true),
					AcceptRepairTeamAccess:     cutil.GetPtr(true),
					AcceptInstanceDeletionRisk: cutil.GetPtr(true),
				},
			},
			HealthIssue: &model.APIMachineHealthIssue{
				Category: model.HealthIssueStorage,
				Summary:  cutil.GetPtr("tenant summary"),
				Details:  cutil.GetPtr("tenant details"),
			},
		}
	}

	buildOnlineRepairExitRequest := func() *model.APIMachineUpdateRequest {
		return &model.APIMachineUpdateRequest{
			OnlineRepair: &model.APIMachineOnlineRepair{
				Enabled: cutil.GetPtr(false),
			},
		}
	}

	machineRepairResetReady := func(t *testing.T) {
		t.Helper()
		_, uerr := isd.Update(context.Background(), nil, cdbm.InstanceUpdateInput{
			InstanceID: iOnlineRepairReady.ID,
			InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{
				Status: cutil.GetPtr(cdbm.InstanceStatusReady),
				Labels: map[string]string{},
			},
		})
		require.NoError(t, uerr)
	}

	machineRepairSetRepairing := func(t *testing.T) {
		t.Helper()
		_, uerr := isd.Update(context.Background(), nil, cdbm.InstanceUpdateInput{
			InstanceID: iOnlineRepairReady.ID,
			InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{
				Status: cutil.GetPtr(cdbm.InstanceStatusRepairing),
				Labels: map[string]string{model.InstanceLabelOnlineRepairAllowAutoDeletion: "false"},
			},
		})
		require.NoError(t, uerr)
	}

	assertOnlineRepairLatestStatusDetail := func(t *testing.T, instanceID uuid.UUID, wantStatus, wantMessage string) {
		t.Helper()
		sdDAO := cdbm.NewStatusDetailDAO(dbSession)
		recent, rerr := sdDAO.GetRecentByEntityIDs(context.Background(), nil, []string{instanceID.String()}, 1)
		require.NoError(t, rerr)
		require.Len(t, recent, 1)
		assert.Equal(t, wantStatus, recent[0].Status)
		require.NotNil(t, recent[0].Message)
		assert.Equal(t, wantMessage, *recent[0].Message)
	}

	mit1 := common.TestBuildMachineInstanceType(t, dbSession, m, instanceType1)
	assert.NotNil(t, mit1)

	mit2 := common.TestBuildMachineInstanceType(t, dbSession, m1, instanceType2)
	assert.NotNil(t, mit2)

	mit3 := common.TestBuildMachineInstanceType(t, dbSession, m3, instanceType3)
	assert.NotNil(t, mit3)

	mit4 := common.TestBuildMachineInstanceType(t, dbSession, m4, instanceType4)
	assert.NotNil(t, mit4)

	mit5 := common.TestBuildMachineInstanceType(t, dbSession, m9, instanceType4)
	assert.NotNil(t, mit5)

	e := echo.New()
	cfg := common.GetTestConfig()

	tc := &tmocks.Client{}
	tsc := &tmocks.Client{}
	tsc1 := &tmocks.Client{}

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)
	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "SetMachineMaintenance", mock.Anything).Return(wrun, nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "AssociateMachinesWithInstanceType", mock.Anything).Return(wrun, nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "RemoveMachineInstanceTypeAssociation", mock.Anything).Return(wrun, nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "UpdateMachineMetadata", mock.Anything).Return(wrun, nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "CreateMachineHealthReport", mock.Anything).Return(wrun, nil)
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "DeleteMachineHealthReport", mock.Anything).Return(wrun, nil)

	// Mock timeout error
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "SetMachineMaintenance", mock.Anything).Return(wruntimeout, nil)
	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "AssociateMachinesWithInstanceType", mock.Anything).Return(wruntimeout, nil)
	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "RemoveMachineInstanceTypeAssociation", mock.Anything).Return(wruntimeout, nil)
	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "UpdateMachineMetadata", mock.Anything).Return(wruntimeout, nil)
	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "CreateMachineHealthReport", mock.Anything).Return(wruntimeout, nil)
	tsc1.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "DeleteMachineHealthReport", mock.Anything).Return(wruntimeout, nil)

	tsc1.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[site.ID.String()] = tsc
	scp.IDClientMap[site3.ID.String()] = tsc1

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	type args struct {
		reqData                     *model.APIMachineUpdateRequest
		reqOrg                      string
		reqUser                     *cdbm.User
		reqMachine                  *cdbm.Machine
		reqInstanceType             *cdbm.InstanceType
		reqMachineInstanceTypeCount *int
		respCode                    int
		reqOldInstanceType          *cdbm.InstanceType
		reqOldMachineInstanceType   *cdbm.MachineInstanceType
		reqClearInstanceType        bool
		reqLabels                   map[string]string
		beforeHandle                func(t *testing.T)
		verifyOnlineRepair          func(t *testing.T)
	}

	machineDAO := cdbm.NewMachineDAO(dbSession)

	tests := []struct {
		name               string
		fields             fields
		args               args
		verifyChildSpanner bool
	}{
		{
			name: "test Machine update API endpoint success, Machine is in Ready state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType3.ID.String()),
				},
				reqMachine:                  m1,
				reqInstanceType:             instanceType3,
				reqOrg:                      ipOrg1,
				reqUser:                     ipu,
				respCode:                    http.StatusOK,
				reqMachineInstanceTypeCount: cutil.GetPtr(1),
				reqOldMachineInstanceType:   mit2,
				reqOldInstanceType:          instanceType2,
			},
		},
		{
			name: "test Machine update API endpoint failure, setting maintenance mode and updating InstanceType at the same time is not allowed",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(true),
					InstanceTypeID:     cutil.GetPtr(instanceType3.ID.String()),
					MaintenanceMessage: cutil.GetPtr("test message"),
				},
				reqMachine: m10,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint failure, maintenance mode and instancetype clearing not allowed",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(true),
					ClearInstanceType:  cutil.GetPtr(true),
					MaintenanceMessage: cutil.GetPtr("test message"),
				},
				reqMachine: m10,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},

		{
			name: "test Machine update API endpoint success, machine maintenance mode is enabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(true),
					MaintenanceMessage: cutil.GetPtr("test message"),
				},
				reqMachine: m10,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
			},
		},
		{
			name: "test Machine update API endpoint success, machine maintenance mode is disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(false),
				},
				reqMachine: m10,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
			},
		},
		{
			name: "test Machine update API endpoint success, maintenance mode is set while Machine is being used by Instance",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(true),
					MaintenanceMessage: cutil.GetPtr("test message"),
				},
				reqMachine: m4,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
			},
		},
		{
			name: "test Machine update API endpoint fails, cannot set maintenance mode if Site is not registered",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(true),
					MaintenanceMessage: cutil.GetPtr("test message"),
				},
				reqMachine: m11,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint fails, cannot remove maintenance mode if Machine is not in maintenance",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(false),
				},
				reqMachine: m10,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint failure, machine is in reset state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType3.ID.String()),
				},
				reqMachine:      m6,
				reqInstanceType: instanceType3,
				reqOrg:          ipOrg1,
				reqUser:         ipu,
				respCode:        http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint fails, invalid instancetype",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(uuid.New().String()),
				},
				reqMachine:      m,
				reqInstanceType: nil,
				reqOrg:          ipOrg1,
				reqUser:         ipu,
				respCode:        http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint success with no association",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType2.ID.String()),
				},
				reqMachine:                  m2,
				reqInstanceType:             instanceType2,
				reqOrg:                      ipOrg1,
				reqUser:                     ipu,
				respCode:                    http.StatusOK,
				reqMachineInstanceTypeCount: cutil.GetPtr(1),
			},
		},
		{
			name: "test Machine update API endpoint fails with org provider doesn't match with machine",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType1.ID.String()),
				},
				reqMachine:      m1,
				reqInstanceType: instanceType1,
				reqOrg:          ipOrg2,
				reqUser:         ipu,
				respCode:        http.StatusForbidden,
			},
		},
		{
			name: "test Machine update API endpoint fails with existing allocation constraints for old instance type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType2.ID.String()),
				},
				reqMachine:      m,
				reqInstanceType: instanceType2,
				reqOrg:          ipOrg1,
				reqUser:         ipu,
				respCode:        http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint fails with existing allocation constraints for old instance type when clearing Instance Type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					ClearInstanceType: cutil.GetPtr(true),
				},
				reqMachine:      m,
				reqInstanceType: instanceType2,
				reqOrg:          ipOrg1,
				reqUser:         ipu,
				respCode:        http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint success when clearing Instance Type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					ClearInstanceType: cutil.GetPtr(true),
				},
				reqMachine:                m3,
				reqOrg:                    ipOrg1,
				reqUser:                   ipu,
				respCode:                  http.StatusOK,
				reqOldInstanceType:        instanceType3,
				reqOldMachineInstanceType: mit3,
				reqClearInstanceType:      true,
			},
		},
		{
			name: "test Machine update API endpoint error, both InstanceTypeID and ClearInstanceType are specified in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID:    cutil.GetPtr(instanceType3.ID.String()),
					ClearInstanceType: cutil.GetPtr(true),
				},
				reqMachine: m3,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint error, neither InstanceTypeID not ClearInstanceType are specified in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    &model.APIMachineUpdateRequest{},
				reqMachine: m3,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint error, Machine does not have Instance Type to clear",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					ClearInstanceType: cutil.GetPtr(true),
				},
				reqMachine: m7,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint error, Machine is being used by Instance",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType3.ID.String()),
				},
				reqMachine: m4,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint error, machine is in error state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType3.ID.String()),
				},
				reqMachine:                  m5,
				reqInstanceType:             instanceType3,
				reqOrg:                      ipOrg1,
				reqUser:                     ipu,
				respCode:                    http.StatusBadRequest,
				reqMachineInstanceTypeCount: cutil.GetPtr(1),
				reqOldMachineInstanceType:   mit2,
				reqOldInstanceType:          instanceType2,
			},
		},
		{
			name: "test Machine update API endpoint data inconsistency error, Machine doesn't have machine instancetype association",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					ClearInstanceType: cutil.GetPtr(true),
				},
				reqMachine: m8,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusInternalServerError,
			},
		},
		{
			name: "test Machine update API endpoint fails, machine capabilities doesn't match with requested instance type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					InstanceTypeID: cutil.GetPtr(instanceType5.ID.String()),
				},
				reqMachine:      m9,
				reqInstanceType: nil,
				reqOrg:          ipOrg1,
				reqUser:         ipu,
				respCode:        http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API endpoint failure, Temporal workflow returns simulated timeout error",
			fields: fields{
				dbSession: dbSession,
				tc:        tsc1,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					SetMaintenanceMode: cutil.GetPtr(true),
					MaintenanceMessage: cutil.GetPtr("test message"),
				},
				reqMachine: m12,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusInternalServerError,
			},
		},
		{
			name: "test Machine update API endpoint success, Machine labels are updated by Provider Admin",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					Labels: map[string]string{"test": "test"},
				},
				reqMachine: m1,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
				reqLabels:  map[string]string{"test": "test"},
			},
		},
		{
			name: "test Machine update API endpoint success, Machine labels are updated by Tenant Admin with TargetedInstanceCreation capability",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					Labels: map[string]string{"test-2": "test-2"},
				},
				reqMachine: m1,
				reqOrg:     tnOrg2,
				reqUser:    tnu2,
				respCode:   http.StatusOK,
				reqLabels:  map[string]string{"test-2": "test-2"},
			},
		},
		{
			name: "test Machine update API online repair failure, Machine is missing on Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairEnterRequest(false),
				reqMachine: mOnlineRepairMissing,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API online repair failure, Machine has no assigned Instance",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairEnterRequest(false),
				reqMachine: m7,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API online repair failure, Instance must be Ready to enter",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairEnterRequest(false),
				reqMachine: mOnlineRepairPending,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
			},
		},
		{
			name: "test Machine update API online repair failure, exit without Repairing status, marker label, or machine OnLineRepair health alert",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairExitRequest(),
				reqMachine: mOnlineRepairReady,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusBadRequest,
				beforeHandle: func(t *testing.T) {
					machineRepairResetReady(t)
				},
			},
		},
		{
			name: "test Machine update API online repair success, enter repair (Provider Admin)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairEnterRequest(false),
				reqMachine: mOnlineRepairReady,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
				beforeHandle: func(t *testing.T) {
					machineRepairResetReady(t)
				},
				verifyOnlineRepair: func(t *testing.T) {
					inst, gerr := isd.GetByID(context.Background(), nil, iOnlineRepairReady.ID, nil)
					require.NoError(t, gerr)
					assert.Equal(t, cdbm.InstanceStatusRepairing, inst.Status)
					assert.Equal(t, "false", inst.Labels[model.InstanceLabelOnlineRepairAllowAutoDeletion])
					assertOnlineRepairLatestStatusDetail(t, iOnlineRepairReady.ID, cdbm.InstanceStatusRepairing, "Instance is currently being repaired")
				},
			},
		},
		{
			name: "test Machine update API online repair success, enter repair with allowAutoInstanceDeletionOnFailure",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairEnterRequest(true),
				reqMachine: mOnlineRepairReady,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
				beforeHandle: func(t *testing.T) {
					machineRepairResetReady(t)
				},
				verifyOnlineRepair: func(t *testing.T) {
					inst, gerr := isd.GetByID(context.Background(), nil, iOnlineRepairReady.ID, nil)
					require.NoError(t, gerr)
					assert.Equal(t, cdbm.InstanceStatusRepairing, inst.Status)
					assert.Equal(t, "true", inst.Labels[model.InstanceLabelOnlineRepairAllowAutoDeletion])
					assertOnlineRepairLatestStatusDetail(t, iOnlineRepairReady.ID, cdbm.InstanceStatusRepairing, "Instance is currently being repaired")
				},
			},
		},
		{
			name: "test Machine update API online repair success, exit repair",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairExitRequest(),
				reqMachine: mOnlineRepairReady,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
				beforeHandle: func(t *testing.T) {
					machineRepairSetRepairing(t)
				},
				verifyOnlineRepair: func(t *testing.T) {
					inst, gerr := isd.GetByID(context.Background(), nil, iOnlineRepairReady.ID, nil)
					require.NoError(t, gerr)
					assert.Equal(t, cdbm.InstanceStatusReady, inst.Status)
					_, has := inst.Labels[model.InstanceLabelOnlineRepairAllowAutoDeletion]
					assert.False(t, has)
					assertOnlineRepairLatestStatusDetail(t, iOnlineRepairReady.ID, cdbm.InstanceStatusReady, "Instance repair has been completed, ready for use")
				},
			},
		},
		{
			name: "test Machine update API online repair success, exit when instance Ready but online repair marker label remains",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairExitRequest(),
				reqMachine: mOnlineRepairReady,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
				beforeHandle: func(t *testing.T) {
					t.Helper()
					_, uerr := isd.Update(context.Background(), nil, cdbm.InstanceUpdateInput{
						InstanceID: iOnlineRepairReady.ID,
						InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{
							Status: cutil.GetPtr(cdbm.InstanceStatusReady),
							Labels: map[string]string{model.InstanceLabelOnlineRepairAllowAutoDeletion: "false"},
						},
					})
					require.NoError(t, uerr)
				},
				verifyOnlineRepair: func(t *testing.T) {
					inst, gerr := isd.GetByID(context.Background(), nil, iOnlineRepairReady.ID, nil)
					require.NoError(t, gerr)
					assert.Equal(t, cdbm.InstanceStatusReady, inst.Status)
					_, has := inst.Labels[model.InstanceLabelOnlineRepairAllowAutoDeletion]
					assert.False(t, has)
					assertOnlineRepairLatestStatusDetail(t, iOnlineRepairReady.ID, cdbm.InstanceStatusReady, "Instance repair has been completed, ready for use")
				},
			},
		},
		{
			name: "test Machine update API online repair success, exit when Ready without marker but Machine health lists OnLineRepair alert",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairExitRequest(),
				reqMachine: mOnlineRepairReady,
				reqOrg:     ipOrg1,
				reqUser:    ipu,
				respCode:   http.StatusOK,
				beforeHandle: func(t *testing.T) {
					t.Helper()
					machineRepairResetReady(t)
					health := map[string]interface{}{
						"alerts": []map[string]interface{}{
							{
								"id":      model.MachineHealthAlertIDOnlineRepair,
								"message": `{}`,
							},
						},
					}
					_, uerr := machineDAO.Update(ctx, nil, cdbm.MachineUpdateInput{
						MachineID: mOnlineRepairReady.ID,
						Health:    health,
					})
					require.NoError(t, uerr)
				},
				verifyOnlineRepair: func(t *testing.T) {
					t.Helper()
					inst, gerr := isd.GetByID(context.Background(), nil, iOnlineRepairReady.ID, nil)
					require.NoError(t, gerr)
					assert.Equal(t, cdbm.InstanceStatusReady, inst.Status)
					_, has := inst.Labels[model.InstanceLabelOnlineRepairAllowAutoDeletion]
					assert.False(t, has)
					assertOnlineRepairLatestStatusDetail(t, iOnlineRepairReady.ID, cdbm.InstanceStatusReady, "Instance repair has been completed, ready for use")
					_, cerr := machineDAO.Clear(context.Background(), nil, cdbm.MachineClearInput{
						MachineID: mOnlineRepairReady.ID,
						Health:    true,
					})
					require.NoError(t, cerr)
				},
			},
		},
		{
			name: "test Machine update API online repair success, enter repair (privileged Tenant Admin)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData:    buildOnlineRepairEnterRequest(false),
				reqMachine: mOnlineRepairReady,
				reqOrg:     tnOrg2,
				reqUser:    tnu2,
				respCode:   http.StatusOK,
				beforeHandle: func(t *testing.T) {
					machineRepairResetReady(t)
				},
				verifyOnlineRepair: func(t *testing.T) {
					inst, gerr := isd.GetByID(context.Background(), nil, iOnlineRepairReady.ID, nil)
					require.NoError(t, gerr)
					assert.Equal(t, cdbm.InstanceStatusRepairing, inst.Status)
					assertOnlineRepairLatestStatusDetail(t, iOnlineRepairReady.ID, cdbm.InstanceStatusRepairing, "Instance is currently being repaired")
				},
			},
		},
		{
			name: "test Machine update API endpoint failure, Instance Type is being cleared by Tenant Admin with TargetedInstanceCreation capability",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIMachineUpdateRequest{
					ClearInstanceType: cutil.GetPtr(true),
				},
				reqMachine: m1,
				reqOrg:     tnOrg2,
				reqUser:    tnu2,
				respCode:   http.StatusForbidden,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			umh := UpdateMachineHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			jsonData, _ := json.Marshal(tt.args.reqData)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPatch, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/machine/%v", tt.args.reqOrg, tt.args.reqMachine.ID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqMachine.ID)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if tt.args.beforeHandle != nil {
				tt.args.beforeHandle(t)
			}

			err := umh.Handle(ec)
			require.NoError(t, err)

			if tt.args.respCode != rec.Code {
				t.Errorf("Response: %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := &model.APIMachine{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			if tt.args.reqMachine != nil {
				if tt.args.reqInstanceType != nil {
					assert.Equal(t, *rst.InstanceTypeID, *tt.args.reqData.InstanceTypeID)

					// Try to check that there are site calls that match the state of the DB after the update.
					// Is this enough, or should we adjust tests to attempt a machine and instance type that will only ever
					// be used for a test to confirm this?
					ttsc, _ := tt.fields.scp.GetClientByID(uuid.MustParse(rst.SiteID))
					ttscm := ttsc.(*tmocks.Client)
					for _, call := range ttscm.Calls {
						if call.Method == "ExecuteWorkflow" && call.Arguments[2] == "AssociateMachinesWithInstanceType" {
							siteReq := call.Arguments[3].(*corev1.AssociateMachinesWithInstanceTypeRequest)

							siteCallMachineID := siteReq.MachineIds[0]
							siteCallInstTypeID := uuid.MustParse(siteReq.InstanceTypeId)

							machine, err := machineDAO.GetByID(ctx, nil, siteCallMachineID, nil, false)

							assert.Nil(t, err, err)
							assert.Equal(t, siteCallInstTypeID, *machine.InstanceTypeID)
						}
					}
				}

				// Online repair updates Instance (and Site workflow); Machine row may not be touched, so Updated can match.
				if tt.args.verifyOnlineRepair == nil {
					assert.NotEqual(t, rst.Updated.String(), tt.args.reqMachine.Updated.String())
				}

				mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)

				// Check that new MachineInstanceType was created
				if tt.args.reqInstanceType != nil {
					emits, total, err := mitDAO.GetAll(context.Background(), nil, cdbm.MachineInstanceTypeFilterInput{
						MachineID:       cutil.GetPtr(tt.args.reqMachine.ID),
						InstanceTypeIDs: []uuid.UUID{tt.args.reqInstanceType.ID},
					}, cdbp.PageInput{}, nil)
					assert.Nil(t, err)
					if tt.args.reqMachineInstanceTypeCount != nil {
						assert.Equal(t, total, *tt.args.reqMachineInstanceTypeCount)
						assert.Equal(t, emits[0].InstanceTypeID.String(), tt.args.reqInstanceType.ID.String())
					}
				}

				// Check that old MachineInstanceType was deleted
				if tt.args.reqOldMachineInstanceType != nil {
					_, err := mitDAO.GetByID(context.Background(), nil, tt.args.reqOldMachineInstanceType.ID, []string{cdbm.InstanceTypeRelationName})
					assert.NotNil(t, err)
				}

				// If Instance Type was cleared then verify that Instance Type is nil
				if tt.args.reqClearInstanceType {
					assert.Nil(t, rst.InstanceTypeID)

					// Similar to updates, when we clear, we should confirm that there is a site call
					// that matches a machine that has been cleared in the DB.
					ttsc, _ := tt.fields.scp.GetClientByID(uuid.MustParse(rst.SiteID))
					ttscm := ttsc.(*tmocks.Client)
					for _, call := range ttscm.Calls {
						if call.Method == "ExecuteWorkflow" && call.Arguments[2] == "RemoveMachineInstanceTypeAssociation" {
							siteReq := call.Arguments[3].(*corev1.RemoveMachineInstanceTypeAssociationRequest)

							siteCallMachineID := siteReq.MachineId
							machine, err := machineDAO.GetByID(ctx, nil, siteCallMachineID, nil, false)
							assert.Nil(t, err, err)
							assert.Nil(t, machine.InstanceTypeID)
						}
					}

				}

				// Verify that maintenance mode is set
				if tt.args.reqData.SetMaintenanceMode != nil {
					if *tt.args.reqData.SetMaintenanceMode {
						assert.Equal(t, rst.Status, cdbm.MachineStatusMaintenance)
						// Verify that maintenance message is set
						if tt.args.reqData.MaintenanceMessage != nil {
							assert.Equal(t, *rst.MaintenanceMessage, *tt.args.reqData.MaintenanceMessage)
						}
					} else {
						assert.Equal(t, rst.Status, cdbm.MachineStatusInitializing)
						assert.Nil(t, rst.MaintenanceMessage)
					}
				}

				// Verify that Machine labels are updated
				if tt.args.reqLabels != nil {
					assert.Equal(t, rst.Labels, tt.args.reqLabels)
				}

				if tt.args.verifyOnlineRepair != nil {
					tt.args.verifyOnlineRepair(t)
				}
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestMachineHandler_GetStatusDetails(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)
	ipuv := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipViewerRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg2, "infra-provider-2")
	ip3 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg3, "infra-provider-3")

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	site2 := testMachineBuildSite(t, dbSession, ip2, "test-site-2", cdbm.SiteStatusRegistered)
	site3 := testMachineBuildSite(t, dbSession, ip3, "test-site-3", cdbm.SiteStatusRegistered)

	tenant := testMachineBuildTenant(t, dbSession, tnOrg1, "test-tenant-1")
	tenant2 := testMachineBuildTenant(t, dbSession, tnOrg2, "test-tenant-2")

	common.TestBuildTenantSite(t, dbSession, tenant, site, ipu)
	common.TestBuildTenantSite(t, dbSession, tenant2, site2, ipu)

	ist := testMachineBuildInstanceType(t, dbSession, ip, site, "instance-type-1")
	ist2 := testMachineBuildInstanceType(t, dbSession, ip, site2, "instance-type-2")
	ist3 := testMachineBuildInstanceType(t, dbSession, ip, site3, "instance-type-3")

	m := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ist.ID), nil, false, false, cdbm.MachineStatusInUse)
	m2 := testMachineBuildMachine(t, dbSession, ip2.ID, site2.ID, cutil.GetPtr(ist2.ID), nil, false, false, cdbm.MachineStatusInUse)
	m3 := testMachineBuildMachine(t, dbSession, ip3.ID, site3.ID, cutil.GetPtr(ist3.ID), nil, true, false, cdbm.MachineStatusInitializing)

	mc11 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0GHz"), cutil.GetPtr(2))
	assert.NotNil(t, mc11)
	mc12 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeMemory, "Corsair Venegeance LPX", cutil.GetPtr("16GB DDR4"), cutil.GetPtr(4))
	assert.NotNil(t, mc12)
	mi1 := testMachineBuildMachineInterface(t, dbSession, m.ID)
	assert.NotNil(t, mi1)

	// add status details objects
	totalCount := 30
	for i := 0; i < totalCount; i++ {
		if i%2 != 0 {
			testMachineBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusInitializing, nil)
			testMachineBuildStatusDetail(t, dbSession, m2.ID, cdbm.MachineStatusInitializing, nil)
			testMachineBuildStatusDetail(t, dbSession, m3.ID, cdbm.MachineStatusInitializing, nil)
		} else {
			testMachineBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusReady, nil)
			testMachineBuildStatusDetail(t, dbSession, m2.ID, cdbm.MachineStatusReady, nil)
			testMachineBuildStatusDetail(t, dbSession, m3.ID, cdbm.MachineStatusReady, nil)
		}
	}

	// init echo
	e := echo.New()

	// init handler
	handler := GetMachineStatusDetailsHandler{
		dbSession: dbSession,
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name         string
		reqMachineID string
		reqOrg       string
		reqUser      *cdbm.User
		respCode     int
	}{
		{
			name:         "failure user not found in request context",
			reqMachineID: m.ID,
			reqOrg:       ipOrg1,
			reqUser:      nil,
			respCode:     http.StatusInternalServerError,
		},
		{
			name:         "failure user not found in org",
			reqMachineID: m.ID,
			reqOrg:       "SomeOrg",
			reqUser:      ipu,
			respCode:     http.StatusForbidden,
		},
		{
			name:         "failure infrastructure provider not found for org",
			reqMachineID: m3.ID,
			reqOrg:       tnOrg1,
			reqUser:      tnu,
			respCode:     http.StatusBadRequest,
		},
		{
			name:         "failure machine id not found",
			reqMachineID: uuid.New().String(),
			reqOrg:       ipOrg1,
			reqUser:      ipu,
			respCode:     http.StatusNotFound,
		},
		{
			name:         "failure machine's infrastructure provider does not match org's infra provider",
			reqMachineID: m2.ID,
			reqOrg:       ipOrg1,
			reqUser:      ipu,
			respCode:     http.StatusBadRequest,
		},
		{
			name:         "success case for infra provider admin",
			reqMachineID: m.ID,
			reqOrg:       ipOrg1,
			reqUser:      ipu,
			respCode:     http.StatusOK,
		},
		{
			name:         "success case when user has Provider viewer role",
			reqMachineID: m.ID,
			reqOrg:       ipOrg1,
			reqUser:      ipuv,
			respCode:     http.StatusOK,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/machine/%v/status-history", tc.reqOrg, tc.reqMachineID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrg, tc.reqMachineID)
			ec.Set("user", tc.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			assert.NoError(t, handler.Handle(ec))
			assert.Equal(t, tc.respCode, rec.Code)

			// only check the rest if the response code is OK
			if rec.Code == http.StatusOK {
				resp := []model.APIStatusDetail{}
				assert.Nil(t, json.Unmarshal(rec.Body.Bytes(), &resp))
				assert.Equal(t, 20, len(resp)) // default page count is 20

				ph := rec.Header().Get(pagination.ResponseHeaderName)
				assert.NotEmpty(t, ph)

				pr := &pagination.PageResponse{}
				assert.NoError(t, json.Unmarshal([]byte(ph), pr))
				assert.Equal(t, totalCount, pr.Total)
			}
		})
	}
}

func TestMachineHandler_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipRoles)
	ipuv := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2, ipOrg3}, ipViewerRoles)

	tnOrg1 := "test-tn-org-1"
	tnOrg2 := "test-tn-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg1, tnOrg2}, tnRoles)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg2, "infra-provider-2")

	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)

	tenant := testMachineBuildTenant(t, dbSession, tnOrg1, "test-tenant-1")

	common.TestBuildTenantSite(t, dbSession, tenant, site, ipu)

	ist := testMachineBuildInstanceType(t, dbSession, ip, site, "instance-type-1")

	// First machine will have no instance, no associated instance type, and will have
	// been missing on site for an acceptable amount of time.
	// I.e., it satisfies all checks for deletion.
	m := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, nil, false, true, cdbm.MachineStatusError)

	sd := testMachineBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusInUse, cutil.GetPtr("Machine is in use"))
	_, err := dbSession.DB.Exec("UPDATE status_detail SET created = NOW() - INTERVAL '39 HOUR', updated = NOW() - INTERVAL '39 HOUR' WHERE id = ?", sd.ID.String())
	require.NoError(t, err)

	// Make m missing on site for more than 24 hours
	sd = testMachineBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusError, cutil.GetPtr("Machine is missing on Site"))
	_, err = dbSession.DB.Exec("UPDATE status_detail SET created = NOW() - INTERVAL '38 HOUR', updated = NOW() - INTERVAL '38 HOUR' WHERE id = ?", sd.ID.String())
	require.NoError(t, err)

	// M2 is missing on site, and it'll be missing long enough, but it'll have an instance associated.
	m2 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ist.ID), nil, false, true, cdbm.MachineStatusError)

	// Make m2 missing on site for more than 24 hours
	testMachineBuildStatusDetail(t, dbSession, m2.ID, cdbm.MachineStatusInUse, cutil.GetPtr("Machine is in use"))

	sd = testMachineBuildStatusDetail(t, dbSession, m2.ID, cdbm.MachineStatusError, cutil.GetPtr("Machine is missing on Site"))
	_, err = dbSession.DB.Exec("UPDATE status_detail SET created = NOW() - INTERVAL '26 HOUR' WHERE id = ?", sd.ID.String())
	require.NoError(t, err)

	// M3 is missing on site, and it'll be missing long enough, and it won't have an instance, but it'll have an instance-type associated.
	m3 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ist.ID), nil, true, true, cdbm.MachineStatusError)

	// Make m3 missing on site for more than 24 hours
	testMachineBuildStatusDetail(t, dbSession, m3.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is initializing"))

	sd = testMachineBuildStatusDetail(t, dbSession, m3.ID, cdbm.MachineStatusError, cutil.GetPtr("Machine is missing on Site"))
	_, err = dbSession.DB.Exec("UPDATE status_detail SET created = NOW() - INTERVAL '25 HOUR' WHERE id = ?", sd.ID.String())
	require.NoError(t, err)

	// M4 is missing on site, but it won't be missing long enough
	m4 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, nil, true, false, cdbm.MachineStatusError)

	// Make m4 missing on site for less than 24 hours
	sd = testMachineBuildStatusDetail(t, dbSession, m3.ID, cdbm.MachineStatusError, cutil.GetPtr("Machine is missing on Site"))
	_, err = dbSession.DB.Exec("UPDATE status_detail SET created = NOW() - INTERVAL '6 HOUR' WHERE id = ?", sd.ID.String())
	require.NoError(t, err)

	// M5 is not missing on site
	m5 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, true, false, cdbm.MachineStatusError)

	mc11 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0GHz"), cutil.GetPtr(2))
	assert.NotNil(t, mc11)
	mc12 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeMemory, "Corsair Venegeance LPX", cutil.GetPtr("16GB DDR4"), cutil.GetPtr(4))
	assert.NotNil(t, mc12)
	mi1 := testMachineBuildMachineInterface(t, dbSession, m.ID)
	assert.NotNil(t, mi1)

	mc21 := testMachineBuildMachineCapability(t, dbSession, &m2.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0GHz"), cutil.GetPtr(2))
	assert.NotNil(t, mc21)
	mi2 := testMachineBuildMachineInterface(t, dbSession, m2.ID)
	assert.NotNil(t, mi2)

	// Build Instances
	insDAO := cdbm.NewInstanceDAO(dbSession)

	vpc := testMachineBuildVpc(t, dbSession, ip, site, tenant, tnOrg1, "test-vpc-1")
	allocation := testMachineBuildAllocation(t, dbSession, ip, tenant, site, "test-allocation-1")
	_ = testInstanceSiteBuildAllocationContraints(t, dbSession, allocation, cdbm.AllocationResourceTypeInstanceType, ist.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	os := testMachineBuildOperatingSystem(t, dbSession, "test-os-1", tenant.ID, tnu)

	_, err = insDAO.Create(
		context.Background(), nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-1",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &ist.ID,
			VpcID:                    vpc.ID,
			MachineID:                &m2.ID,
			OperatingSystemID:        cutil.GetPtr(os.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("test-user-data"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			CreatedBy:                tnu.ID,
		},
	)
	assert.NoError(t, err)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		mID                string
		expectedStatus     int
		verifyChildSpanner bool
	}{
		{
			name:           "error when machine id not found",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            uuid.New().String(),
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "error when machine's infrastructure provider does not match org's infra provider",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            m5.ID,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "failure case for NOT having infra provider admin role",
			reqOrgName:         ipOrg1,
			user:               ipuv,
			mID:                m.ID,
			expectedStatus:     http.StatusForbidden,
			verifyChildSpanner: true,
		},
		{
			name:               "success case for infra provider",
			reqOrgName:         ipOrg1,
			user:               ipu,
			mID:                m.ID,
			expectedStatus:     http.StatusAccepted,
			verifyChildSpanner: true,
		},
		{
			name:           "failure case when Instance exists",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            m2.ID,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure case when associated with an instance type",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            m3.ID,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "failure case when not missing on site long enough",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            m4.ID,
			expectedStatus: http.StatusBadRequest,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.NotEqual(t, tc.name, "")
			// Setup echo server/context
			e := echo.New()
			req := httptest.NewRequest(http.MethodDelete, "/", nil)
			q := req.URL.Query()

			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			req.URL.RawQuery = q.Encode()
			rec := httptest.NewRecorder()
			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/machine/%v", tc.reqOrgName, tc.mID))
			names := []string{"orgName", "id"}
			ec.SetParamNames(names...)
			values := []string{tc.reqOrgName, tc.mID}
			ec.SetParamValues(values...)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			mh := DeleteMachineHandler{
				dbSession:  dbSession,
				tracerSpan: cutil.NewTracerSpan(),
			}
			err := mh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedStatus, rec.Code, tc.mID, rec.Body.String())

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

	forceTests := []struct {
		name                string
		query               string
		isAssigned          bool
		hasAttachedInstance bool
		siteStatus          string
		siteClientError     bool
		coreError           error
		wantCoreCall        bool
		wantStatus          int
		wantMessage         string
	}{
		{name: "true proxies the complete force-delete request", query: "/?force=true", wantCoreCall: true, wantStatus: http.StatusAccepted},
		{name: "older Core target not found completes compatibility cleanup", query: "/?force=true", coreError: status.Error(codes.NotFound, "Machine not found"), wantCoreCall: true, wantStatus: http.StatusAccepted},
		{name: "assigned Machine is rejected without cleanup", query: "/?force=true", isAssigned: true, wantStatus: http.StatusBadRequest, wantMessage: "Machine is currently in use by an Instance and cannot be force deleted"},
		{name: "attached Instance is rejected when assignment flag is stale", query: "/?force=true", hasAttachedInstance: true, wantStatus: http.StatusBadRequest, wantMessage: "Machine is currently in use by an Instance and cannot be force deleted"},
		{name: "Site must be Registered", query: "/?force=true", siteStatus: cdbm.SiteStatusPending, wantStatus: http.StatusBadRequest, wantMessage: "Site specified in request data is not in Registered state"},
		{name: "Site client lookup failure retains local records", query: "/?force=true", siteClientError: true, wantStatus: http.StatusInternalServerError, wantMessage: "Failed to retrieve workflow client for Site"},
		{name: "Core failed precondition retains local records", query: "/?force=true", coreError: status.Error(codes.FailedPrecondition, "Core Machine has an attached Instance"), wantCoreCall: true, wantStatus: http.StatusPreconditionFailed, wantMessage: "Core Machine has an attached Instance"},
		{name: "Core resource exhaustion retains local records", query: "/?force=true", coreError: status.Error(codes.ResourceExhausted, "Core admission capacity exhausted"), wantCoreCall: true, wantStatus: http.StatusTooManyRequests, wantMessage: "Core admission capacity exhausted"},
		{name: "Core unavailable retains local records", query: "/?force=true", coreError: status.Error(codes.Unavailable, "Core unavailable"), wantCoreCall: true, wantStatus: http.StatusServiceUnavailable, wantMessage: "Core unavailable"},
		{name: "Core proxy timeout retains local records", query: "/?force=true", coreError: tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil), wantCoreCall: true, wantStatus: http.StatusGatewayTimeout, wantMessage: "Core proxy request timed out"},
		{name: "invalid value is rejected before deletion", query: "/?force=definitely", wantStatus: http.StatusBadRequest, wantMessage: "Invalid force query parameter, expected a boolean value"},
	}
	for _, tc := range forceTests {
		t.Run(tc.name, func(t *testing.T) {
			fixture := common.NewTestSetupProviderMachineHandlerFixture(t, &corev1.AdminForceDeleteMachineResponse{})
			var siteClientPool common.SiteTemporalClientPool = fixture.SiteClientPool
			if tc.siteClientError {
				siteClientPool = failingSiteTemporalClientPool{err: fmt.Errorf("test Site client lookup failure")}
			}
			handler := NewDeleteMachineHandler(fixture.DBSession, siteClientPool)
			var linkedInstanceID uuid.UUID
			var linkedMachineInstanceTypeID uuid.UUID

			if tc.coreError != nil {
				tsc := fixture.SiteClientPool.IDClientMap[fixture.SiteID].(*tmocks.Client)
				tsc.ExpectedCalls = nil
				wrun := &tmocks.WorkflowRun{}
				wrun.On("Get", mock.Anything, mock.Anything).Return(tc.coreError)
				tsc.On(
					"ExecuteWorkflow",
					mock.Anything,
					mock.Anything,
					mock.Anything,
					mock.MatchedBy(func(req grpcproxy.Request) bool {
						*fixture.ProxiedReq = req
						return true
					}),
				).Return(wrun, nil)
			}

			if tc.query == "/?force=true" {
				machine, err := cdbm.NewMachineDAO(fixture.DBSession).GetByID(
					context.Background(),
					nil,
					fixture.MachineID,
					[]string{cdbm.InfrastructureProviderRelationName, cdbm.SiteRelationName, cdbm.InstanceTypeRelationName},
					false,
				)
				require.NoError(t, err)
				machineInstanceType := common.TestBuildMachineInstanceType(t, fixture.DBSession, machine, machine.InstanceType)
				linkedMachineInstanceTypeID = machineInstanceType.ID
				common.TestBuildMachineCapability(t, fixture.DBSession, &machine.ID, nil, cdbm.MachineCapabilityTypeCPU, "force-delete-cpu", nil, nil, nil, nil, nil, nil)
				_, err = cdbm.NewMachineInterfaceDAO(fixture.DBSession).Create(context.Background(), nil, cdbm.MachineInterfaceCreateInput{
					MachineID:             machine.ID,
					ControllerInterfaceID: cutil.GetPtr(uuid.New()),
					ControllerSegmentID:   cutil.GetPtr(uuid.New()),
					IsPrimary:             true,
					MacAddress:            cutil.GetPtr("00:00:00:00:00:01"),
					IpAddresses:           []string{},
				})
				require.NoError(t, err)

				if tc.siteStatus != "" {
					_, err = cdbm.NewSiteDAO(fixture.DBSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{
						SiteID: machine.SiteID,
						Status: &tc.siteStatus,
					})
					require.NoError(t, err)
				}

				if tc.isAssigned {
					isAssigned := true
					_, err = cdbm.NewMachineDAO(fixture.DBSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
						MachineID:  fixture.MachineID,
						IsAssigned: &isAssigned,
					})
					require.NoError(t, err)
				}

				if tc.hasAttachedInstance {
					user := fixture.User.(*cdbm.User)
					tenant := common.TestBuildTenant(t, fixture.DBSession, "force-delete-tenant", "force-delete-tenant-org", user)
					vpc := common.TestBuildVPC(t, fixture.DBSession, "force-delete-vpc", machine.InfrastructureProvider, tenant, machine.Site, nil, nil, nil, cdbm.VpcStatusReady, user)
					operatingSystem := common.TestBuildOperatingSystem(t, fixture.DBSession, "force-delete-os", tenant, cdbm.OperatingSystemStatusReady, user)
					instance := common.TestBuildInstance(t, fixture.DBSession, "force-delete-instance", tenant.ID, machine.InfrastructureProviderID, machine.SiteID, *machine.InstanceTypeID, vpc.ID, &machine.ID, operatingSystem.ID)
					linkedInstanceID = instance.ID
					common.TestBuildInterface(t, fixture.DBSession, instance.ID, nil, nil, true, nil, nil, nil, nil, user)
				}
			}

			rec := fixture.Request(t, handler.Handle, http.MethodDelete, tc.query, nil, "")
			require.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			if tc.wantCoreCall {
				require.Equal(t, corev1.Forge_AdminForceDeleteMachine_FullMethodName, fixture.ProxiedReq.FullMethod)
			} else {
				require.Empty(t, fixture.ProxiedReq.FullMethod)
			}
			if tc.wantStatus != http.StatusAccepted {
				require.Contains(t, rec.Body.String(), tc.wantMessage)
				if tc.query == "/?force=true" {
					_, err := cdbm.NewMachineDAO(fixture.DBSession).GetByID(context.Background(), nil, fixture.MachineID, nil, false)
					require.NoError(t, err)
					_, err = cdbm.NewMachineInstanceTypeDAO(fixture.DBSession).GetByID(context.Background(), nil, linkedMachineInstanceTypeID, nil)
					require.NoError(t, err)
					caps, _, err := cdbm.NewMachineCapabilityDAO(fixture.DBSession).GetAll(context.Background(), nil, []string{fixture.MachineID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
					require.NoError(t, err)
					require.NotEmpty(t, caps)
					machineInterfaces, _, err := cdbm.NewMachineInterfaceDAO(fixture.DBSession).GetAll(context.Background(), nil, cdbm.MachineInterfaceFilterInput{MachineIDs: []string{fixture.MachineID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
					require.NoError(t, err)
					require.NotEmpty(t, machineInterfaces)
				}
				if tc.hasAttachedInstance {
					_, err = cdbm.NewInstanceDAO(fixture.DBSession).GetByID(context.Background(), nil, linkedInstanceID, nil)
					require.NoError(t, err)
				}
				return
			}

			var coreReq corev1.AdminForceDeleteMachineRequest
			require.NoError(t, protojson.Unmarshal(fixture.ProxiedReq.RequestJSON, &coreReq))
			require.Equal(t, fixture.MachineID, coreReq.GetHostQuery())
			require.True(t, coreReq.GetDeleteInterfaces())
			require.True(t, coreReq.GetDeleteBmcInterfaces())
			require.True(t, coreReq.GetAllowDeleteWithInstanceType())
			require.False(t, coreReq.GetAllowDeleteWithInstance())
			require.JSONEq(t, `{"message":"Deletion request was accepted"}`, rec.Body.String())

			_, err := cdbm.NewMachineDAO(fixture.DBSession).GetByID(context.Background(), nil, fixture.MachineID, nil, false)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			_, err = cdbm.NewMachineInstanceTypeDAO(fixture.DBSession).GetByID(context.Background(), nil, linkedMachineInstanceTypeID, nil)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			caps, _, err := cdbm.NewMachineCapabilityDAO(fixture.DBSession).GetAll(context.Background(), nil, []string{fixture.MachineID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			require.NoError(t, err)
			require.Empty(t, caps)
			machineInterfaces, _, err := cdbm.NewMachineInterfaceDAO(fixture.DBSession).GetAll(context.Background(), nil, cdbm.MachineInterfaceFilterInput{MachineIDs: []string{fixture.MachineID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			require.NoError(t, err)
			require.Empty(t, machineInterfaces)
		})
	}

	t.Run("cleanup failure rolls back before compatibility retry", func(t *testing.T) {
		fixture := common.NewTestSetupProviderMachineHandlerFixture(t, &corev1.AdminForceDeleteMachineResponse{})
		handler := NewDeleteMachineHandler(fixture.DBSession, fixture.SiteClientPool)
		machine, err := cdbm.NewMachineDAO(fixture.DBSession).GetByID(
			context.Background(),
			nil,
			fixture.MachineID,
			[]string{cdbm.InfrastructureProviderRelationName, cdbm.SiteRelationName, cdbm.InstanceTypeRelationName},
			false,
		)
		require.NoError(t, err)
		machineInstanceType := common.TestBuildMachineInstanceType(t, fixture.DBSession, machine, machine.InstanceType)
		common.TestBuildMachineCapability(t, fixture.DBSession, &machine.ID, nil, cdbm.MachineCapabilityTypeCPU, "force-delete-rollback-cpu", nil, nil, nil, nil, nil, nil)
		_, err = cdbm.NewMachineInterfaceDAO(fixture.DBSession).Create(context.Background(), nil, cdbm.MachineInterfaceCreateInput{
			MachineID:             machine.ID,
			ControllerInterfaceID: cutil.GetPtr(uuid.New()),
			ControllerSegmentID:   cutil.GetPtr(uuid.New()),
			IsPrimary:             true,
			MacAddress:            cutil.GetPtr("00:00:00:00:00:02"),
			IpAddresses:           []string{},
		})
		require.NoError(t, err)

		const (
			addConstraintSQL  = "ALTER TABLE machine ADD CONSTRAINT machine_force_delete_deleted_null_test CHECK (deleted IS NULL)"
			dropConstraintSQL = "ALTER TABLE machine DROP CONSTRAINT machine_force_delete_deleted_null_test"
		)
		_, err = fixture.DBSession.DB.Exec(addConstraintSQL)
		require.NoError(t, err)
		constraintActive := true
		t.Cleanup(func() {
			if constraintActive {
				_, cleanupErr := fixture.DBSession.DB.Exec(dropConstraintSQL)
				require.NoError(t, cleanupErr)
			}
		})

		first := fixture.Request(t, handler.Handle, http.MethodDelete, "/?force=true", nil, "")
		require.Equal(t, http.StatusInternalServerError, first.Code, first.Body.String())
		require.Contains(t, first.Body.String(), "Failed to clean up force-deleted Machine")
		_, err = cdbm.NewMachineDAO(fixture.DBSession).GetByID(context.Background(), nil, fixture.MachineID, nil, false)
		require.NoError(t, err)
		_, err = cdbm.NewMachineInstanceTypeDAO(fixture.DBSession).GetByID(context.Background(), nil, machineInstanceType.ID, nil)
		require.NoError(t, err)
		caps, _, err := cdbm.NewMachineCapabilityDAO(fixture.DBSession).GetAll(context.Background(), nil, []string{fixture.MachineID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
		require.NoError(t, err)
		require.NotEmpty(t, caps)
		machineInterfaces, _, err := cdbm.NewMachineInterfaceDAO(fixture.DBSession).GetAll(context.Background(), nil, cdbm.MachineInterfaceFilterInput{MachineIDs: []string{fixture.MachineID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		require.NotEmpty(t, machineInterfaces)

		_, err = fixture.DBSession.DB.Exec(dropConstraintSQL)
		require.NoError(t, err)
		constraintActive = false
		tsc := fixture.SiteClientPool.IDClientMap[fixture.SiteID].(*tmocks.Client)
		tsc.ExpectedCalls = nil
		wrun := &tmocks.WorkflowRun{}
		wrun.On("Get", mock.Anything, mock.Anything).Return(status.Error(codes.NotFound, "Machine not found"))
		tsc.On(
			"ExecuteWorkflow",
			mock.Anything,
			mock.Anything,
			mock.Anything,
			mock.MatchedBy(func(req grpcproxy.Request) bool {
				*fixture.ProxiedReq = req
				return true
			}),
		).Return(wrun, nil)

		retry := fixture.Request(t, handler.Handle, http.MethodDelete, "/?force=true", nil, "")
		require.Equal(t, http.StatusAccepted, retry.Code, retry.Body.String())
		_, err = cdbm.NewMachineDAO(fixture.DBSession).GetByID(context.Background(), nil, fixture.MachineID, nil, false)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)
		_, err = cdbm.NewMachineInstanceTypeDAO(fixture.DBSession).GetByID(context.Background(), nil, machineInstanceType.ID, nil)
		require.ErrorIs(t, err, cdb.ErrDoesNotExist)
		caps, _, err = cdbm.NewMachineCapabilityDAO(fixture.DBSession).GetAll(context.Background(), nil, []string{fixture.MachineID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
		require.NoError(t, err)
		require.Empty(t, caps)
		machineInterfaces, _, err = cdbm.NewMachineInterfaceDAO(fixture.DBSession).GetAll(context.Background(), nil, cdbm.MachineInterfaceFilterInput{MachineIDs: []string{fixture.MachineID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		require.Empty(t, machineInterfaces)
	})
}

func TestGetDpuMachinesLegacyTemporalPayload(t *testing.T) {
	legacyDpuMachines := []*corev1.DpuMachine{{Machine: &corev1.Machine{Id: &corev1.MachineId{Id: uuid.NewString()}}}}
	dataConverter := tsdkConverter.GetDefaultDataConverter()
	payloads, err := dataConverter.ToPayloads(legacyDpuMachines)
	require.NoError(t, err)

	var currentResult corev1.DpuMachineList
	err = dataConverter.FromPayloads(payloads, &currentResult)
	require.ErrorIs(t, err, tsdkConverter.ErrUnableToDecode)

	var decodedLegacyResult []*corev1.DpuMachine
	require.NoError(t, dataConverter.FromPayloads(payloads, &decodedLegacyResult))
	require.Equal(t, legacyDpuMachines, decodedLegacyResult)
}

func TestMachineHandler_GetDpuMachines(t *testing.T) {
	ctx := context.Background()
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	// Org / user fixtures.
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1, ipOrg2}, ipRoles)
	ipuv := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg1}, ipViewerRoles)

	tnOrgPriv := "test-tn-org-priv"
	tnOrgRegular := "test-tn-org-regular"
	tnRoles := []string{authz.TenantAdminRole}
	tnuPriv := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrgPriv}, tnRoles)
	tnuRegular := testMachineBuildUser(t, dbSession, uuid.NewString(), []string{tnOrgRegular}, tnRoles)

	// Provider/site setup. `siteUnreg` is on `ip` but in Pending state so we
	// can drive the Site-not-Registered branch separately from auth checks.
	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infra-provider-1")
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg2, "infra-provider-2")
	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered)
	site2 := testMachineBuildSite(t, dbSession, ip2, "test-site-2", cdbm.SiteStatusRegistered)
	siteUnreg := testMachineBuildSite(t, dbSession, ip, "test-site-unreg", cdbm.SiteStatusPending)

	// Privileged tenant has TargetedInstanceCreation + a TenantAccount on
	// `ip`; regular tenant has neither, so it must hit a 403 even though it
	// has TENANT_ADMIN.
	tenantPriv := testMachineBuildTenant(t, dbSession, tnOrgPriv, "test-tenant-priv")
	_ = common.TestBuildTenantAccount(t, dbSession, ip, &tenantPriv.ID, tnOrgPriv, cdbm.TenantAccountStatusReady, tnuPriv)
	testMachineEnableTenantAccountTargetedInstanceCreation(t, dbSession, ip, tenantPriv.ID)
	_ = testMachineBuildTenant(t, dbSession, tnOrgRegular, "test-tenant-regular")

	ist := testMachineBuildInstanceType(t, dbSession, ip, site, "instance-type-1")

	// Host machine with two DPU-attached interfaces.
	mWithDpu := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ist.ID), nil, false, false, cdbm.MachineStatusInUse)
	// Host machine with no DPU-attached interfaces.
	mNoDpu := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr(ist.ID), nil, false, false, cdbm.MachineStatusInUse)
	// Machine on a different provider/site -> auth check should reject ipu.
	mOtherProvider := testMachineBuildMachine(t, dbSession, ip2.ID, site2.ID, cutil.GetPtr(ist.ID), nil, false, false, cdbm.MachineStatusInUse)
	// Machine on a Pending site -> Site-status precondition should reject.
	mUnregSite := testMachineBuildMachine(t, dbSession, ip.ID, siteUnreg.ID, cutil.GetPtr(ist.ID), nil, false, false, cdbm.MachineStatusInUse)

	// DPU machine fixtures referenced from `mWithDpu`'s interfaces.
	dpu1 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("DPU"), false, false, cdbm.MachineStatusReady)
	dpu2 := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("DPU"), false, false, cdbm.MachineStatusReady)

	miDAO := cdbm.NewMachineInterfaceDAO(dbSession)
	_, err := miDAO.Create(ctx, nil, cdbm.MachineInterfaceCreateInput{
		MachineID:             mWithDpu.ID,
		ControllerInterfaceID: cutil.GetPtr(uuid.New()),
		ControllerSegmentID:   cutil.GetPtr(uuid.New()),
		Hostname:              cutil.GetPtr("dpu-host-1.com"),
		IsPrimary:             true,
		MacAddress:            cutil.GetPtr("00:00:00:00:00:01"),
		IpAddresses:           []string{"192.168.1.1"},
		AttachedDpuMachineID:  &dpu1.ID,
	})
	require.NoError(t, err)
	_, err = miDAO.Create(ctx, nil, cdbm.MachineInterfaceCreateInput{
		MachineID:             mWithDpu.ID,
		ControllerInterfaceID: cutil.GetPtr(uuid.New()),
		ControllerSegmentID:   cutil.GetPtr(uuid.New()),
		Hostname:              cutil.GetPtr("dpu-host-2.com"),
		IsPrimary:             false,
		MacAddress:            cutil.GetPtr("00:00:00:00:00:02"),
		IpAddresses:           []string{"192.168.1.2"},
		AttachedDpuMachineID:  &dpu2.ID,
	})
	require.NoError(t, err)
	// Non-DPU interface for the non-DPU machine — exercises the no-DPU path.
	_, err = miDAO.Create(ctx, nil, cdbm.MachineInterfaceCreateInput{
		MachineID:             mNoDpu.ID,
		ControllerInterfaceID: cutil.GetPtr(uuid.New()),
		ControllerSegmentID:   cutil.GetPtr(uuid.New()),
		Hostname:              cutil.GetPtr("plain-host.com"),
		IsPrimary:             true,
		MacAddress:            cutil.GetPtr("00:00:00:00:00:03"),
		IpAddresses:           []string{"192.168.1.3"},
	})
	require.NoError(t, err)

	cfg := common.GetTestConfig()

	// Mock Temporal: success path returns two DPU machines for the workflow.
	dpuMachineList := &corev1.DpuMachineList{Machines: []*corev1.DpuMachine{
		{
			Machine: &corev1.Machine{
				Id:    &corev1.MachineId{Id: dpu1.ID},
				State: "READY",
			},
			DpuNetworkConfig: &corev1.ManagedHostNetworkConfigResponse{
				Asn:                          65001,
				VniDevice:                    "pf0hpf",
				ManagedHostConfigVersion:     "v1.0.0",
				UseAdminNetwork:              true,
				InstanceNetworkConfigVersion: "v1.0.0",
				RemoteId:                     "host-123",
				VpcIsolationBehavior:         corev1.VpcIsolationBehaviorType_VPC_ISOLATION_MUTUAL,
				StatefulAclsEnabled:          true,
				EnableDhcp:                   false,
				IsPrimaryDpu:                 true,
				DatacenterAsn:                65000,
			},
		},
		{
			Machine: &corev1.Machine{
				Id:    &corev1.MachineId{Id: dpu2.ID},
				State: "READY",
			},
			DpuNetworkConfig: &corev1.ManagedHostNetworkConfigResponse{
				Asn:                          65002,
				VniDevice:                    "pf0hpf",
				ManagedHostConfigVersion:     "v1.0.0",
				UseAdminNetwork:              false,
				InstanceNetworkConfigVersion: "v1.0.0",
				RemoteId:                     "host-456",
				VpcIsolationBehavior:         corev1.VpcIsolationBehaviorType_VPC_ISOLATION_MUTUAL,
				StatefulAclsEnabled:          true,
				EnableDhcp:                   false,
				IsPrimaryDpu:                 false,
				DatacenterAsn:                65000,
			},
		},
	}}

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return("test-workflow-id-dpu")
	wrun.Mock.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		result := args.Get(1).(*corev1.DpuMachineList)
		result.Machines = dpuMachineList.Machines
	}).Return(nil)

	tsc := &tmocks.Client{}
	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "GetDpuMachines", mock.Anything).Return(wrun, nil)

	wrunLegacy := &tmocks.WorkflowRun{}
	wrunLegacy.On("GetID").Return("test-workflow-id-dpu-legacy")
	wrunLegacy.Mock.On("Get", mock.Anything, mock.MatchedBy(func(result any) bool {
		_, ok := result.(*corev1.DpuMachineList)
		return ok
	})).Return(fmt.Errorf("payload item 0: %w: cannot unmarshal array into DpuMachineList", tsdkConverter.ErrUnableToDecode)).Once()
	wrunLegacy.Mock.On("Get", mock.Anything, mock.MatchedBy(func(result any) bool {
		_, ok := result.(*[]*corev1.DpuMachine)
		return ok
	})).Run(func(args mock.Arguments) {
		result := args.Get(1).(*[]*corev1.DpuMachine)
		*result = dpuMachineList.Machines
	}).Return(nil).Once()

	tscLegacy := &tmocks.Client{}
	tscLegacy.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "GetDpuMachines", mock.Anything).Return(wrunLegacy, nil)

	wrunErr := &tmocks.WorkflowRun{}
	wrunErr.On("GetID").Return("test-workflow-error-id")
	wrunErr.Mock.On("Get", mock.Anything, mock.Anything).Return(fmt.Errorf("workflow failed")).Once()
	tscErr := &tmocks.Client{}
	tscErr.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "GetDpuMachines", mock.Anything).Return(wrunErr, nil)

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("test-workflow-timeout-id")
	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil)).Once()
	tscTimeout := &tmocks.Client{}
	tscTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"), "GetDpuMachines", mock.Anything).Return(wrunTimeout, nil)
	tscTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	tcfg, _ := cfg.GetTemporalConfig()
	scpOK := sc.NewClientPool(tcfg)
	scpOK.IDClientMap[site.ID.String()] = tsc
	scpOK.IDClientMap[site2.ID.String()] = tsc
	scpOK.IDClientMap[siteUnreg.ID.String()] = tsc

	scpErr := sc.NewClientPool(tcfg)
	scpErr.IDClientMap[site.ID.String()] = tscErr

	scpLegacy := sc.NewClientPool(tcfg)
	scpLegacy.IDClientMap[site.ID.String()] = tscLegacy

	scpTimeout := sc.NewClientPool(tcfg)
	scpTimeout.IDClientMap[site.ID.String()] = tscTimeout

	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		reqOrgName         string
		user               *cdbm.User
		mID                string
		scp                *sc.ClientPool
		expectedStatus     int
		expectedDpuCount   int
		verifyChildSpanner bool
	}{
		{
			name:           "missing user in request context",
			reqOrgName:     ipOrg1,
			user:           nil,
			mID:            mWithDpu.ID,
			scp:            scpOK,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "user not member of org",
			reqOrgName:     "unknown-org",
			user:           ipu,
			mID:            mWithDpu.ID,
			scp:            scpOK,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "machine id not found",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            uuid.New().String(),
			scp:            scpOK,
			expectedStatus: http.StatusNotFound,
		},
		{
			name:           "site is not in registered state",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            mUnregSite.ID,
			scp:            scpOK,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "provider viewer is rejected (allowViewerRole=false)",
			reqOrgName:     ipOrg1,
			user:           ipuv,
			mID:            mWithDpu.ID,
			scp:            scpOK,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "non-privileged tenant is rejected (requirePrivilegedScope set)",
			reqOrgName:     tnOrgRegular,
			user:           tnuRegular,
			mID:            mWithDpu.ID,
			scp:            scpOK,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "provider admin from different org is not associated with machine's site",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            mOtherProvider.ID,
			scp:            scpOK,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:               "provider admin: machine has no attached DPU interfaces",
			reqOrgName:         ipOrg1,
			user:               ipu,
			mID:                mNoDpu.ID,
			scp:                scpOK,
			expectedStatus:     http.StatusOK,
			expectedDpuCount:   0,
			verifyChildSpanner: true,
		},
		{
			name:               "provider admin: full success with attached DPU machines",
			reqOrgName:         ipOrg1,
			user:               ipu,
			mID:                mWithDpu.ID,
			scp:                scpOK,
			expectedStatus:     http.StatusOK,
			expectedDpuCount:   2,
			verifyChildSpanner: true,
		},
		{
			name:               "provider admin: legacy workflow result remains supported during rollout",
			reqOrgName:         ipOrg1,
			user:               ipu,
			mID:                mWithDpu.ID,
			scp:                scpLegacy,
			expectedStatus:     http.StatusOK,
			expectedDpuCount:   2,
			verifyChildSpanner: true,
		},
		{
			name:               "privileged tenant with account on provider can access",
			reqOrgName:         tnOrgPriv,
			user:               tnuPriv,
			mID:                mWithDpu.ID,
			scp:                scpOK,
			expectedStatus:     http.StatusOK,
			expectedDpuCount:   2,
			verifyChildSpanner: true,
		},
		{
			name:           "workflow returns error",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            mWithDpu.ID,
			scp:            scpErr,
			expectedStatus: http.StatusInternalServerError,
		},
		{
			name:           "workflow times out",
			reqOrgName:     ipOrg1,
			user:           ipu,
			mID:            mWithDpu.ID,
			scp:            scpTimeout,
			expectedStatus: http.StatusInternalServerError,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			e := echo.New()
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/machine/%v/dpu", tc.reqOrgName, tc.mID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tc.reqOrgName, tc.mID)
			if tc.user != nil {
				ec.Set("user", tc.user)
			}

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			gadmh := GetAllDpuMachineHandler{
				dbSession:  dbSession,
				scp:        tc.scp,
				tracerSpan: cutil.NewTracerSpan(),
			}
			err := gadmh.Handle(ec)
			assert.Nil(t, err)
			assert.Equal(t, tc.expectedStatus, rec.Code, "%s: %s", tc.name, rec.Body.String())

			if rec.Code == http.StatusOK {
				var dpuMachines []interface{}
				err := json.Unmarshal(rec.Body.Bytes(), &dpuMachines)
				assert.Nil(t, err)
				assert.Equal(t, tc.expectedDpuCount, len(dpuMachines))
			}

			if tc.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}

	// Verify the Temporal mock expectations were actually exercised. In
	// particular this asserts the timeout path invoked TerminateWorkflow, so
	// regressions in timeout cleanup are caught instead of passing silently.
	tsc.AssertExpectations(t)
	wrun.AssertExpectations(t)
	tscLegacy.AssertExpectations(t)
	wrunLegacy.AssertExpectations(t)
	tscErr.AssertExpectations(t)
	wrunErr.AssertExpectations(t)
	tscTimeout.AssertExpectations(t)
	wrunTimeout.AssertExpectations(t)
}
