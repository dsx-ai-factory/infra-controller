// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

// ~~~~~ Test Helpers ~~~~~ //

func testStatsInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testStatsBuildUser(t *testing.T, dbSession *cdb.Session, orgs []string, roles []string) *cdbm.User {
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
	u, err := uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
		StarfleetID: cutil.GetPtr(uuid.NewString()),
		Email:       cutil.GetPtr("stats-test@test.com"),
		FirstName:   cutil.GetPtr("Stats"),
		LastName:    cutil.GetPtr("Tester"),
		OrgData:     OrgData,
	})
	assert.Nil(t, err)
	return u
}

func testStatsBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, org, name string) *cdbm.InfrastructureProvider {
	ip := &cdbm.InfrastructureProvider{
		ID:   uuid.New(),
		Name: name,
		Org:  org,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testStatsBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string) *cdbm.Site {
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
		Status:                      cdbm.SiteStatusRegistered,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testStatsBuildTenant(t *testing.T, dbSession *cdb.Session, org, name, displayName string) *cdbm.Tenant {
	tn := &cdbm.Tenant{
		ID:             uuid.New(),
		Name:           name,
		Org:            org,
		OrgDisplayName: &displayName,
	}
	_, err := dbSession.DB.NewInsert().Model(tn).Exec(context.Background())
	assert.Nil(t, err)
	return tn
}

func testStatsBuildInstanceType(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, name string) *cdbm.InstanceType {
	it := &cdbm.InstanceType{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		SiteID:                   cutil.GetPtr(site.ID),
		Status:                   cdbm.InstanceTypeStatusReady,
		CreatedBy:                uuid.New(),
		Version:                  "1.0",
	}
	_, err := dbSession.DB.NewInsert().Model(it).Exec(context.Background())
	assert.Nil(t, err)
	return it
}

func testStatsBuildMachine(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, itID *uuid.UUID, status string, isNetworkDegraded bool) *cdbm.Machine {
	mid := uuid.NewString()
	m := &cdbm.Machine{
		ID:                       mid,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		InstanceTypeID:           itID,
		ControllerMachineID:      mid,
		DefaultMacAddress:        cutil.GetPtr("00:1B:44:11:3A:B7"),
		IsAssigned:               itID != nil,
		IsNetworkDegraded:        isNetworkDegraded,
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func testStatsBuildMachineCapability(t *testing.T, dbSession *cdb.Session, machineID string, capType cdbm.MachineCapabilityType, name string, count int) *cdbm.MachineCapability {
	mc := &cdbm.MachineCapability{
		ID:        uuid.New(),
		MachineID: &machineID,
		Type:      capType,
		Name:      name,
		Count:     &count,
		Created:   cdb.GetCurTime(),
		Updated:   cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(mc).Exec(context.Background())
	assert.Nil(t, err)
	return mc
}

func testStatsBuildAllocation(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, site *cdbm.Site, name string) *cdbm.Allocation {
	al := &cdbm.Allocation{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site.ID,
		Status:                   cdbm.AllocationStatusRegistered,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(al).Exec(context.Background())
	assert.Nil(t, err)
	return al
}

func testStatsBuildAllocationConstraint(t *testing.T, dbSession *cdb.Session, alloc *cdbm.Allocation, instanceTypeID uuid.UUID, constraintValue int) *cdbm.AllocationConstraint {
	ac := &cdbm.AllocationConstraint{
		ID:              uuid.New(),
		AllocationID:    alloc.ID,
		ResourceType:    cdbm.AllocationResourceTypeInstanceType,
		ResourceTypeID:  instanceTypeID,
		ConstraintType:  cdbm.AllocationConstraintTypeReserved,
		ConstraintValue: constraintValue,
		CreatedBy:       uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(ac).Exec(context.Background())
	assert.Nil(t, err)
	return ac
}

func testStatsBuildInstance(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, tenant *cdbm.Tenant, vpc *cdbm.Vpc, itID *uuid.UUID, machineID *string, name, status string) *cdbm.Instance {
	inst := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     name,
		TenantID:                 tenant.ID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		InstanceTypeID:           itID,
		VpcID:                    vpc.ID,
		MachineID:                machineID,
		Status:                   status,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(inst).Exec(context.Background())
	assert.Nil(t, err)
	return inst
}

func testStatsBuildVpc(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, tenant *cdbm.Tenant, name string) *cdbm.Vpc {
	vpc := &cdbm.Vpc{
		ID:                       uuid.New(),
		Name:                     name,
		Org:                      tenant.Org,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		TenantID:                 tenant.ID,
		Status:                   cdbm.VpcStatusReady,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func testStatsSetupEchoContext(t *testing.T, org, siteID string, user *cdbm.User) (echo.Context, *httptest.ResponseRecorder) {
	ctx := context.Background()
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	e := echo.New()
	q := url.Values{}
	q.Add("siteId", siteID)

	req := httptest.NewRequest(http.MethodGet, "/?"+q.Encode(), nil)
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()

	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(org)
	ec.Set("user", user)
	ec.SetRequest(ec.Request().WithContext(ctx))

	return ec, rec
}

// ~~~~~ Test Data Setup ~~~~~ //
//
//
//   1 Infrastructure Provider (org: "stats-org")
//   1 Site
//   3 Tenants: alpha, beta, gamma
//   3 Instance Types: gpu-large, gpu-small, cpu-standard
//
//   Machines (29 assigned + 8 unassigned = 37 total):
//     gpu-large:    12 (1 Init, 5 Ready, 2 InUse, 1 Error, 1 Maint, 1 Unknown, 1 Decom)
//     gpu-small:    12 (1 Init, 4 Ready, 3 InUse, 1 Error, 1 Maint, 1 Unknown, 1 Reset)
//     cpu-standard:  5 (3 Ready, 1 InUse, 1 Error)
//     unassigned:    8 (2 Ready, 1 Error, 1 Maint, 1 Unknown, 1 Decom, 2 Reset)
//
//   GPU Capabilities:
//     gpu-large: "NVIDIA H100 SXM5 80GB" count=8 per machine, except 1 with count=4 (11×8 + 1×4 = 92)
//     gpu-small: "NVIDIA A100 SXM4 80GB" count=8 per machine, except 1 with count=4 (11×8 + 1×4 = 92)
//
//   Allocations (4 allocations, 7 constraints):
//     alpha: training-reserved (gpu-large:4, gpu-small:3), inference-ondemand (gpu-large:2)
//     beta:  simulation-pool (gpu-large:1, gpu-small:3)
//     gamma: general-compute (gpu-small:2, cpu:1)
//     Totals per IT: gpu-large=7, gpu-small=8, cpu=1
//
//   Instances (12 total — machines in use by tenants, status reflects machine state):
//     alpha: gpu-large×4 (2 InUse→Ready, 1 Error→Error, 1 Init→Provisioning)
//            gpu-small×2 (2 InUse→Ready)
//     beta:  gpu-large×1 (Maintenance→Ready)
//            gpu-small×3 (1 Error→Error, 1 Maintenance→Ready, 1 Init→Provisioning)
//     gamma: gpu-small×1 (InUse→Ready), cpu×1 (InUse→Ready)
//     Used per IT:  gpu-large=5, gpu-small=6, cpu=1
//
//   MaxAllocatable = max(0, ready - (allocated - used)):
//     gpu-large: max(0, 5-(7-5)) = 3
//     gpu-small: max(0, 4-(8-6)) = 2
//     cpu:       max(0, 3-(1-1)) = 3

func TestStatsHandlers(t *testing.T) {
	dbSession := testStatsInitDB(t)
	defer dbSession.Close()
	common.TestSetupSchema(t, dbSession)

	org := "stats-org"

	// Users
	providerUser := testStatsBuildUser(t, dbSession, []string{org}, []string{authz.ProviderAdminRole})
	tenantUser := testStatsBuildUser(t, dbSession, []string{org}, []string{authz.TenantAdminRole})

	// Infrastructure provider & site
	ip := testStatsBuildInfrastructureProvider(t, dbSession, org, "stats-provider")
	site := testStatsBuildSite(t, dbSession, ip, "stats-site")

	// Tenants
	tenantAlpha := testStatsBuildTenant(t, dbSession, "alpha-org", "alpha", "Alpha Corp")
	tenantBeta := testStatsBuildTenant(t, dbSession, "beta-org", "beta", "Beta Labs")
	tenantGamma := testStatsBuildTenant(t, dbSession, "gamma-org", "gamma", "Gamma Dev")

	// Instance Types
	itGPULarge := testStatsBuildInstanceType(t, dbSession, ip, site, "gpu-large")
	itGPUSmall := testStatsBuildInstanceType(t, dbSession, ip, site, "gpu-small")
	itCPU := testStatsBuildInstanceType(t, dbSession, ip, site, "cpu-standard")

	// ~~~~~ Machines ~~~~~ //

	// gpu-large: 12 machines (1 Init, 5 Ready, 2 InUse, 1 Error, 1 Maint, 1 Unknown, 1 Decom)
	gpuL := make([]*cdbm.Machine, 12)
	gpuL[0] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusInitializing, false)
	gpuL[1] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusReady, false)
	gpuL[2] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusReady, false)
	gpuL[3] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusReady, false)
	gpuL[4] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusReady, false)
	gpuL[5] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusReady, false)
	gpuL[6] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusInUse, false)
	gpuL[7] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusInUse, false)
	gpuL[8] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusError, false)
	gpuL[9] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusMaintenance, false)
	gpuL[10] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusUnknown, false)
	gpuL[11] = testStatsBuildMachine(t, dbSession, ip, site, &itGPULarge.ID, cdbm.MachineStatusDecommissioned, false)

	// gpu-small: 12 machines (1 Init, 4 Ready, 3 InUse, 1 Error, 1 Maint, 1 Unknown, 1 Reset)
	gpuS := make([]*cdbm.Machine, 12)
	gpuS[0] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusInitializing, false)
	gpuS[1] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusReady, false)
	gpuS[2] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusReady, false)
	gpuS[3] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusReady, false)
	gpuS[4] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusReady, false)
	gpuS[5] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusInUse, false)
	gpuS[6] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusInUse, false)
	gpuS[7] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusInUse, false)
	gpuS[8] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusError, false)
	gpuS[9] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusMaintenance, false)
	gpuS[10] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusUnknown, false)
	gpuS[11] = testStatsBuildMachine(t, dbSession, ip, site, &itGPUSmall.ID, cdbm.MachineStatusReset, false)

	// cpu-standard: 5 machines (3 Ready, 1 InUse, 1 Error)
	cpu := make([]*cdbm.Machine, 5)
	cpu[0] = testStatsBuildMachine(t, dbSession, ip, site, &itCPU.ID, cdbm.MachineStatusReady, false)
	cpu[1] = testStatsBuildMachine(t, dbSession, ip, site, &itCPU.ID, cdbm.MachineStatusReady, false)
	cpu[2] = testStatsBuildMachine(t, dbSession, ip, site, &itCPU.ID, cdbm.MachineStatusReady, false)
	cpu[3] = testStatsBuildMachine(t, dbSession, ip, site, &itCPU.ID, cdbm.MachineStatusInUse, false)
	cpu[4] = testStatsBuildMachine(t, dbSession, ip, site, &itCPU.ID, cdbm.MachineStatusError, false)

	// unassigned: 8 machines (2 Ready, 1 Error, 1 Maint, 1 Unknown, 1 Decom, 2 Reset)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusReady, false)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusReady, false)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusError, false)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusMaintenance, false)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusUnknown, false)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusDecommissioned, false)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusReset, false)
	testStatsBuildMachine(t, dbSession, ip, site, nil, cdbm.MachineStatusReset, false)

	// ~~~~~ GPU Capabilities ~~~~~ //

	for i, m := range gpuL {
		count := 8
		if i == 11 { // Decommissioned machine has only 4 GPUs (partially populated)
			count = 4
		}
		testStatsBuildMachineCapability(t, dbSession, m.ID, cdbm.MachineCapabilityTypeGPU, "NVIDIA H100 SXM5 80GB", count)
	}
	for i, m := range gpuS {
		count := 8
		if i == 11 { // Reset machine has only 4 GPUs
			count = 4
		}
		testStatsBuildMachineCapability(t, dbSession, m.ID, cdbm.MachineCapabilityTypeGPU, "NVIDIA A100 SXM4 80GB", count)
	}

	// ~~~~~ Allocations & Constraints ~~~~~ //

	allocTraining := testStatsBuildAllocation(t, dbSession, ip, tenantAlpha, site, "training-reserved")
	testStatsBuildAllocationConstraint(t, dbSession, allocTraining, itGPULarge.ID, 4)
	testStatsBuildAllocationConstraint(t, dbSession, allocTraining, itGPUSmall.ID, 3)

	allocInference := testStatsBuildAllocation(t, dbSession, ip, tenantAlpha, site, "inference-ondemand")
	testStatsBuildAllocationConstraint(t, dbSession, allocInference, itGPULarge.ID, 2)

	allocSimulation := testStatsBuildAllocation(t, dbSession, ip, tenantBeta, site, "simulation-pool")
	testStatsBuildAllocationConstraint(t, dbSession, allocSimulation, itGPULarge.ID, 1)
	testStatsBuildAllocationConstraint(t, dbSession, allocSimulation, itGPUSmall.ID, 3)

	allocGeneral := testStatsBuildAllocation(t, dbSession, ip, tenantGamma, site, "general-compute")
	testStatsBuildAllocationConstraint(t, dbSession, allocGeneral, itGPUSmall.ID, 2)
	testStatsBuildAllocationConstraint(t, dbSession, allocGeneral, itCPU.ID, 1)

	// ~~~~~ Instances ~~~~~ //

	vpcAlpha := testStatsBuildVpc(t, dbSession, ip, site, tenantAlpha, "vpc-alpha")
	vpcBeta := testStatsBuildVpc(t, dbSession, ip, site, tenantBeta, "vpc-beta")
	vpcGamma := testStatsBuildVpc(t, dbSession, ip, site, tenantGamma, "vpc-gamma")

	// alpha: 4 instances on gpu-large (2 InUse→Ready, 1 Error→Error, 1 Initializing→Provisioning)
	testStatsBuildInstance(t, dbSession, ip, site, tenantAlpha, vpcAlpha, &itGPULarge.ID, &gpuL[6].ID, "alpha-gpuL-1", cdbm.InstanceStatusReady)        // machine InUse
	testStatsBuildInstance(t, dbSession, ip, site, tenantAlpha, vpcAlpha, &itGPULarge.ID, &gpuL[7].ID, "alpha-gpuL-2", cdbm.InstanceStatusReady)        // machine InUse
	testStatsBuildInstance(t, dbSession, ip, site, tenantAlpha, vpcAlpha, &itGPULarge.ID, &gpuL[8].ID, "alpha-gpuL-3", cdbm.InstanceStatusError)        // machine Error
	testStatsBuildInstance(t, dbSession, ip, site, tenantAlpha, vpcAlpha, &itGPULarge.ID, &gpuL[0].ID, "alpha-gpuL-4", cdbm.InstanceStatusProvisioning) // machine Initializing

	// alpha: 2 instances on gpu-small (2 InUse→Ready)
	testStatsBuildInstance(t, dbSession, ip, site, tenantAlpha, vpcAlpha, &itGPUSmall.ID, &gpuS[5].ID, "alpha-gpuS-1", cdbm.InstanceStatusReady) // machine InUse
	testStatsBuildInstance(t, dbSession, ip, site, tenantAlpha, vpcAlpha, &itGPUSmall.ID, &gpuS[6].ID, "alpha-gpuS-2", cdbm.InstanceStatusReady) // machine InUse

	// beta: 1 instance on gpu-large (Maintenance→Ready)
	testStatsBuildInstance(t, dbSession, ip, site, tenantBeta, vpcBeta, &itGPULarge.ID, &gpuL[9].ID, "beta-gpuL-1", cdbm.InstanceStatusReady) // machine Maintenance

	// beta: 3 instances on gpu-small (1 Error→Error, 1 Maintenance→Ready, 1 Initializing→Provisioning)
	testStatsBuildInstance(t, dbSession, ip, site, tenantBeta, vpcBeta, &itGPUSmall.ID, &gpuS[8].ID, "beta-gpuS-1", cdbm.InstanceStatusError)        // machine Error
	testStatsBuildInstance(t, dbSession, ip, site, tenantBeta, vpcBeta, &itGPUSmall.ID, &gpuS[9].ID, "beta-gpuS-2", cdbm.InstanceStatusReady)        // machine Maintenance
	testStatsBuildInstance(t, dbSession, ip, site, tenantBeta, vpcBeta, &itGPUSmall.ID, &gpuS[0].ID, "beta-gpuS-3", cdbm.InstanceStatusProvisioning) // machine Initializing

	// gamma: 1 instance on gpu-small (InUse→Ready), 1 on cpu (InUse→Ready)
	testStatsBuildInstance(t, dbSession, ip, site, tenantGamma, vpcGamma, &itGPUSmall.ID, &gpuS[7].ID, "gamma-gpuS-1", cdbm.InstanceStatusReady) // machine InUse
	testStatsBuildInstance(t, dbSession, ip, site, tenantGamma, vpcGamma, &itCPU.ID, &cpu[3].ID, "gamma-cpu-1", cdbm.InstanceStatusReady)        // machine InUse

	cfg := common.GetTestConfig()

	// ~~~~~ Test: 3.1 Machine GPU Stats ~~~~~ //

	t.Run("GetMachineGPUStats", func(t *testing.T) {
		ec, rec := testStatsSetupEchoContext(t, org, site.ID.String(), providerUser)

		handler := NewGetMachineGPUStatsHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusOK, rec.Code)

		var result []model.APIMachineGPUStats
		err = json.Unmarshal(rec.Body.Bytes(), &result)
		require.Nil(t, err)

		assert.Equal(t, 2, len(result))

		gpuByName := make(map[string]model.APIMachineGPUStats)
		for _, g := range result {
			gpuByName[g.Name] = g
		}

		h100Stats := gpuByName["NVIDIA H100 SXM5 80GB"]
		assert.Equal(t, 92, h100Stats.GPUs) // 11×8 + 1×4
		assert.Equal(t, 12, h100Stats.Machines)

		a100Stats := gpuByName["NVIDIA A100 SXM4 80GB"]
		assert.Equal(t, 92, a100Stats.GPUs) // 11×8 + 1×4
		assert.Equal(t, 12, a100Stats.Machines)
	})

	// ~~~~~ Test: 3.2 Machine Instance Type Summary ~~~~~ //

	t.Run("GetMachineInstanceTypeSummary", func(t *testing.T) {
		ec, rec := testStatsSetupEchoContext(t, org, site.ID.String(), providerUser)

		handler := NewGetMachineInstanceTypeSummaryHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusOK, rec.Code)

		var result model.APIMachineInstanceTypeSummary
		err = json.Unmarshal(rec.Body.Bytes(), &result)
		require.Nil(t, err)

		// Assigned: 12 gpu-large + 12 gpu-small + 5 cpu = 29
		assert.Equal(t, 29, result.Assigned.Total)
		assert.Equal(t, 2, result.Assigned.Initializing) // 1 gpuL + 1 gpuS
		assert.Equal(t, 12, result.Assigned.Ready)       // 5 gpuL + 4 gpuS + 3 cpu
		assert.Equal(t, 6, result.Assigned.InUse)        // 2 gpuL + 3 gpuS + 1 cpu
		assert.Equal(t, 3, result.Assigned.Error)        // 1 gpuL + 1 gpuS + 1 cpu
		assert.Equal(t, 2, result.Assigned.Maintenance)  // 1 gpuL + 1 gpuS
		assert.Equal(t, 2, result.Assigned.Unknown)      // 1 gpuL + 1 gpuS
		// 29 > 2+12+6+3+2+2=27 because 1 Decom (gpuL) + 1 Reset (gpuS) = 2 untracked

		// Unassigned: 8 machines
		assert.Equal(t, 8, result.Unassigned.Total)
		assert.Equal(t, 0, result.Unassigned.Initializing)
		assert.Equal(t, 2, result.Unassigned.Ready)
		assert.Equal(t, 0, result.Unassigned.InUse)
		assert.Equal(t, 1, result.Unassigned.Error)
		assert.Equal(t, 1, result.Unassigned.Maintenance)
		assert.Equal(t, 1, result.Unassigned.Unknown)
		// 8 > 0+2+0+1+1+1=5 because 1 Decom + 2 Reset = 3 untracked
	})

	// ~~~~~ Test: 3.3 Machine Instance Type Stats (Detailed) ~~~~~ //

	t.Run("GetMachineInstanceTypeStats", func(t *testing.T) {
		ec, rec := testStatsSetupEchoContext(t, org, site.ID.String(), providerUser)

		handler := NewGetMachineInstanceTypeStatsHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusOK, rec.Code)

		var result []model.APIMachineInstanceTypeStats
		err = json.Unmarshal(rec.Body.Bytes(), &result)
		require.Nil(t, err)

		assert.Equal(t, 3, len(result))

		statsByName := make(map[string]model.APIMachineInstanceTypeStats)
		for _, s := range result {
			statsByName[s.Name] = s
		}

		// --- gpu-large ---
		gpuLStats := statsByName["gpu-large"]
		assert.Equal(t, itGPULarge.ID.String(), gpuLStats.ID)
		assert.Equal(t, "gpu-large", gpuLStats.Name)
		// assigned: 12 total (1 Init, 5 Ready, 2 InUse, 1 Error, 1 Maint, 1 Unknown, 1 Decom)
		assert.Equal(t, 12, gpuLStats.AssignedMachineStats.Total)
		assert.Equal(t, 1, gpuLStats.AssignedMachineStats.Initializing)
		assert.Equal(t, 5, gpuLStats.AssignedMachineStats.Ready)
		assert.Equal(t, 2, gpuLStats.AssignedMachineStats.InUse)
		assert.Equal(t, 1, gpuLStats.AssignedMachineStats.Error)
		assert.Equal(t, 1, gpuLStats.AssignedMachineStats.Maintenance)
		assert.Equal(t, 1, gpuLStats.AssignedMachineStats.Unknown)
		assert.Equal(t, 7, gpuLStats.Allocated)      // 4+2 alpha + 1 beta
		assert.Equal(t, 3, gpuLStats.MaxAllocatable) // max(0, 5-(7-5))
		// used: 5 machines (alpha: 2 InUse + 1 Error + 1 Init, beta: 1 Maint)
		assert.Equal(t, 5, gpuLStats.UsedMachineStats.Total)
		assert.Equal(t, 2, gpuLStats.UsedMachineStats.InUse)
		assert.Equal(t, 1, gpuLStats.UsedMachineStats.Error)
		assert.Equal(t, 1, gpuLStats.UsedMachineStats.Maintenance)
		assert.Equal(t, 1, gpuLStats.UsedMachineStats.Initializing)
		assert.Equal(t, 0, gpuLStats.UsedMachineStats.Unknown)
		assert.Equal(t, 2, len(gpuLStats.Tenants)) // alpha, beta

		gpuLTenantByName := make(map[string]model.APIMachineInstanceTypeTenantStats)
		for _, tn := range gpuLStats.Tenants {
			gpuLTenantByName[tn.Name] = tn
		}
		alphaGpuL := gpuLTenantByName["alpha-org"]
		assert.Equal(t, tenantAlpha.ID.String(), alphaGpuL.ID)
		assert.Equal(t, 6, alphaGpuL.Allocated) // training:4 + inference:2
		assert.Equal(t, 4, alphaGpuL.UsedMachineStats.Total)
		assert.Equal(t, 1, alphaGpuL.UsedMachineStats.Initializing)
		assert.Equal(t, 2, alphaGpuL.UsedMachineStats.InUse)
		assert.Equal(t, 1, alphaGpuL.UsedMachineStats.Error)
		assert.Equal(t, 2, len(alphaGpuL.Allocations))

		betaGpuL := gpuLTenantByName["beta-org"]
		assert.Equal(t, tenantBeta.ID.String(), betaGpuL.ID)
		assert.Equal(t, 1, betaGpuL.Allocated) // simulation:1
		assert.Equal(t, 1, betaGpuL.UsedMachineStats.Total)
		assert.Equal(t, 1, betaGpuL.UsedMachineStats.Maintenance)
		assert.Equal(t, 1, len(betaGpuL.Allocations))

		// --- gpu-small ---
		gpuSStats := statsByName["gpu-small"]
		assert.Equal(t, itGPUSmall.ID.String(), gpuSStats.ID)
		// assigned: 12 total (1 Init, 4 Ready, 3 InUse, 1 Error, 1 Maint, 1 Unknown, 1 Reset)
		assert.Equal(t, 12, gpuSStats.AssignedMachineStats.Total)
		assert.Equal(t, 1, gpuSStats.AssignedMachineStats.Initializing)
		assert.Equal(t, 4, gpuSStats.AssignedMachineStats.Ready)
		assert.Equal(t, 3, gpuSStats.AssignedMachineStats.InUse)
		assert.Equal(t, 1, gpuSStats.AssignedMachineStats.Error)
		assert.Equal(t, 1, gpuSStats.AssignedMachineStats.Maintenance)
		assert.Equal(t, 1, gpuSStats.AssignedMachineStats.Unknown)
		assert.Equal(t, 8, gpuSStats.Allocated)      // 3 alpha + 3 beta + 2 gamma
		assert.Equal(t, 2, gpuSStats.MaxAllocatable) // max(0, 4-(8-6))
		// used: 6 machines (alpha: 2 InUse, beta: 1 Error + 1 Maint + 1 Init, gamma: 1 InUse)
		assert.Equal(t, 6, gpuSStats.UsedMachineStats.Total)
		assert.Equal(t, 3, gpuSStats.UsedMachineStats.InUse)
		assert.Equal(t, 1, gpuSStats.UsedMachineStats.Error)
		assert.Equal(t, 1, gpuSStats.UsedMachineStats.Maintenance)
		assert.Equal(t, 1, gpuSStats.UsedMachineStats.Initializing)
		assert.Equal(t, 3, len(gpuSStats.Tenants)) // alpha, beta, gamma

		gpuSTenantByName := make(map[string]model.APIMachineInstanceTypeTenantStats)
		for _, tn := range gpuSStats.Tenants {
			gpuSTenantByName[tn.Name] = tn
		}
		alphaGpuS := gpuSTenantByName["alpha-org"]
		assert.Equal(t, 3, alphaGpuS.Allocated)
		assert.Equal(t, 2, alphaGpuS.UsedMachineStats.Total)
		assert.Equal(t, 2, alphaGpuS.UsedMachineStats.InUse)
		assert.Equal(t, 1, len(alphaGpuS.Allocations))

		betaGpuS := gpuSTenantByName["beta-org"]
		assert.Equal(t, 3, betaGpuS.Allocated)
		assert.Equal(t, 3, betaGpuS.UsedMachineStats.Total)
		assert.Equal(t, 1, betaGpuS.UsedMachineStats.Initializing)
		assert.Equal(t, 1, betaGpuS.UsedMachineStats.Error)
		assert.Equal(t, 1, betaGpuS.UsedMachineStats.Maintenance)
		assert.Equal(t, 1, len(betaGpuS.Allocations))

		gammaGpuS := gpuSTenantByName["gamma-org"]
		assert.Equal(t, 2, gammaGpuS.Allocated)
		assert.Equal(t, 1, gammaGpuS.UsedMachineStats.Total)
		assert.Equal(t, 1, gammaGpuS.UsedMachineStats.InUse)
		assert.Equal(t, 1, len(gammaGpuS.Allocations))

		// --- cpu-standard ---
		cpuStats := statsByName["cpu-standard"]
		assert.Equal(t, itCPU.ID.String(), cpuStats.ID)
		// assigned: 5 total (3 Ready, 1 InUse, 1 Error)
		assert.Equal(t, 5, cpuStats.AssignedMachineStats.Total)
		assert.Equal(t, 0, cpuStats.AssignedMachineStats.Initializing)
		assert.Equal(t, 3, cpuStats.AssignedMachineStats.Ready)
		assert.Equal(t, 1, cpuStats.AssignedMachineStats.InUse)
		assert.Equal(t, 1, cpuStats.AssignedMachineStats.Error)
		assert.Equal(t, 0, cpuStats.AssignedMachineStats.Maintenance)
		assert.Equal(t, 0, cpuStats.AssignedMachineStats.Unknown)
		assert.Equal(t, 1, cpuStats.Allocated)      // general-compute:1
		assert.Equal(t, 3, cpuStats.MaxAllocatable) // max(0, 3-(1-1))
		// used: 1 machine (gamma: 1 InUse)
		assert.Equal(t, 1, cpuStats.UsedMachineStats.Total)
		assert.Equal(t, 1, cpuStats.UsedMachineStats.InUse)
		assert.Equal(t, 0, cpuStats.UsedMachineStats.Error)
		assert.Equal(t, 0, cpuStats.UsedMachineStats.Maintenance)
		assert.Equal(t, 1, len(cpuStats.Tenants)) // gamma only
	})

	// ~~~~~ Test: 3.4 Tenant Instance Type Stats ~~~~~ //

	t.Run("GetTenantInstanceTypeStats", func(t *testing.T) {
		ec, rec := testStatsSetupEchoContext(t, org, site.ID.String(), providerUser)

		handler := NewGetTenantInstanceTypeStatsHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusOK, rec.Code)

		var result []model.APITenantInstanceTypeStats
		err = json.Unmarshal(rec.Body.Bytes(), &result)
		require.Nil(t, err)

		assert.Equal(t, 3, len(result))

		tenantByOrg := make(map[string]model.APITenantInstanceTypeStats)
		for _, ts := range result {
			tenantByOrg[ts.Org] = ts
		}

		// --- alpha ---
		alphaStats := tenantByOrg["alpha-org"]
		assert.Equal(t, tenantAlpha.ID.String(), alphaStats.ID)
		assert.Equal(t, "Alpha Corp", alphaStats.OrgDisplayName)
		assert.Equal(t, 2, len(alphaStats.InstanceTypes)) // gpu-large, gpu-small

		alphaByIT := make(map[string]model.APITenantInstanceTypeStatsEntry)
		for _, it := range alphaStats.InstanceTypes {
			alphaByIT[it.Name] = it
		}

		alphaGpuLEntry := alphaByIT["gpu-large"]
		assert.Equal(t, itGPULarge.ID.String(), alphaGpuLEntry.ID)
		assert.Equal(t, 6, alphaGpuLEntry.Allocated)      // 4+2
		assert.Equal(t, 3, alphaGpuLEntry.MaxAllocatable) // global gpu-large maxAlloc
		assert.Equal(t, 4, alphaGpuLEntry.UsedMachineStats.Total)
		assert.Equal(t, 1, alphaGpuLEntry.UsedMachineStats.Initializing)
		assert.Equal(t, 2, alphaGpuLEntry.UsedMachineStats.InUse)
		assert.Equal(t, 1, alphaGpuLEntry.UsedMachineStats.Error)
		assert.Equal(t, 0, alphaGpuLEntry.UsedMachineStats.Maintenance)
		assert.Equal(t, 2, len(alphaGpuLEntry.Allocations)) // training, inference

		alphaGpuSEntry := alphaByIT["gpu-small"]
		assert.Equal(t, 3, alphaGpuSEntry.Allocated)
		assert.Equal(t, 2, alphaGpuSEntry.MaxAllocatable) // global gpu-small maxAlloc
		assert.Equal(t, 2, alphaGpuSEntry.UsedMachineStats.Total)
		assert.Equal(t, 2, alphaGpuSEntry.UsedMachineStats.InUse)
		assert.Equal(t, 1, len(alphaGpuSEntry.Allocations))

		// --- beta ---
		betaStats := tenantByOrg["beta-org"]
		assert.Equal(t, tenantBeta.ID.String(), betaStats.ID)
		assert.Equal(t, "Beta Labs", betaStats.OrgDisplayName)
		assert.Equal(t, 2, len(betaStats.InstanceTypes)) // gpu-large, gpu-small

		betaByIT := make(map[string]model.APITenantInstanceTypeStatsEntry)
		for _, it := range betaStats.InstanceTypes {
			betaByIT[it.Name] = it
		}

		betaGpuLEntry := betaByIT["gpu-large"]
		assert.Equal(t, 1, betaGpuLEntry.Allocated)
		assert.Equal(t, 3, betaGpuLEntry.MaxAllocatable)
		assert.Equal(t, 1, betaGpuLEntry.UsedMachineStats.Total)
		assert.Equal(t, 0, betaGpuLEntry.UsedMachineStats.InUse)
		assert.Equal(t, 1, betaGpuLEntry.UsedMachineStats.Maintenance)
		assert.Equal(t, 1, len(betaGpuLEntry.Allocations))

		betaGpuSEntry := betaByIT["gpu-small"]
		assert.Equal(t, 3, betaGpuSEntry.Allocated)
		assert.Equal(t, 2, betaGpuSEntry.MaxAllocatable)
		assert.Equal(t, 3, betaGpuSEntry.UsedMachineStats.Total)
		assert.Equal(t, 1, betaGpuSEntry.UsedMachineStats.Initializing)
		assert.Equal(t, 1, betaGpuSEntry.UsedMachineStats.Error)
		assert.Equal(t, 1, betaGpuSEntry.UsedMachineStats.Maintenance)
		assert.Equal(t, 1, len(betaGpuSEntry.Allocations))

		// --- gamma ---
		gammaStats := tenantByOrg["gamma-org"]
		assert.Equal(t, tenantGamma.ID.String(), gammaStats.ID)
		assert.Equal(t, "Gamma Dev", gammaStats.OrgDisplayName)
		assert.Equal(t, 2, len(gammaStats.InstanceTypes)) // gpu-small, cpu

		gammaByIT := make(map[string]model.APITenantInstanceTypeStatsEntry)
		for _, it := range gammaStats.InstanceTypes {
			gammaByIT[it.Name] = it
		}

		gammaGpuSEntry := gammaByIT["gpu-small"]
		assert.Equal(t, 2, gammaGpuSEntry.Allocated)
		assert.Equal(t, 2, gammaGpuSEntry.MaxAllocatable)
		assert.Equal(t, 1, gammaGpuSEntry.UsedMachineStats.Total)
		assert.Equal(t, 1, gammaGpuSEntry.UsedMachineStats.InUse)
		assert.Equal(t, 1, len(gammaGpuSEntry.Allocations))

		gammaCPUEntry := gammaByIT["cpu-standard"]
		assert.Equal(t, 1, gammaCPUEntry.Allocated)
		assert.Equal(t, 3, gammaCPUEntry.MaxAllocatable) // global cpu maxAlloc
		assert.Equal(t, 1, gammaCPUEntry.UsedMachineStats.Total)
		assert.Equal(t, 1, gammaCPUEntry.UsedMachineStats.InUse)
		assert.Equal(t, 1, len(gammaCPUEntry.Allocations))
	})

	// ~~~~~ Test: Auth - Tenant user should be denied ~~~~~ //

	t.Run("GetMachineGPUStats_TenantUserDenied", func(t *testing.T) {
		ec, rec := testStatsSetupEchoContext(t, org, site.ID.String(), tenantUser)

		handler := NewGetMachineGPUStatsHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusForbidden, rec.Code)
	})

	// ~~~~~ Test: Missing siteId query param ~~~~~ //

	t.Run("GetMachineGPUStats_MissingSiteId", func(t *testing.T) {
		ctx := context.Background()
		ctx = common.TestCommonTraceProviderSetup(t, ctx)

		e := echo.New()
		req := httptest.NewRequest(http.MethodGet, "/", nil)
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
		rec := httptest.NewRecorder()

		ec := e.NewContext(req, rec)
		ec.SetParamNames("orgName")
		ec.SetParamValues(org)
		ec.Set("user", providerUser)
		ec.SetRequest(ec.Request().WithContext(ctx))

		handler := NewGetMachineGPUStatsHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})

	// ~~~~~ Test: Invalid siteId ~~~~~ //

	t.Run("GetMachineGPUStats_InvalidSiteId", func(t *testing.T) {
		ec, rec := testStatsSetupEchoContext(t, org, "not-a-uuid", providerUser)

		handler := NewGetMachineGPUStatsHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusBadRequest, rec.Code)
	})

	// ~~~~~ Test: Empty site (no machines) ~~~~~ //

	t.Run("GetMachineGPUStats_EmptySite", func(t *testing.T) {
		emptySite := testStatsBuildSite(t, dbSession, ip, "empty-site")
		ec, rec := testStatsSetupEchoContext(t, org, emptySite.ID.String(), providerUser)

		handler := NewGetMachineGPUStatsHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusOK, rec.Code)

		var result []model.APIMachineGPUStats
		err = json.Unmarshal(rec.Body.Bytes(), &result)
		require.Nil(t, err)
		assert.Equal(t, 0, len(result))
	})

	t.Run("GetMachineInstanceTypeSummary_EmptySite", func(t *testing.T) {
		emptySite := testStatsBuildSite(t, dbSession, ip, "empty-site-summary")
		ec, rec := testStatsSetupEchoContext(t, org, emptySite.ID.String(), providerUser)

		handler := NewGetMachineInstanceTypeSummaryHandler(dbSession, cfg)
		err := handler.Handle(ec)
		require.Nil(t, err)
		assert.Equal(t, http.StatusOK, rec.Code)

		var result model.APIMachineInstanceTypeSummary
		err = json.Unmarshal(rec.Body.Bytes(), &result)
		require.Nil(t, err)
		assert.Equal(t, 0, result.Assigned.Total)
		assert.Equal(t, 0, result.Unassigned.Total)
	})

}
