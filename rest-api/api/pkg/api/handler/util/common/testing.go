// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
)

var (
	testCfg *config.Config
)

// GetTestConfig returns a config for tests - each instance of
// config consumes file descriptors via viper and there is no
// easy way to clean the file descriptors during tests. hence
// using this to create once config instance for tests
func GetTestConfig() *config.Config {
	if testCfg == nil {
		testCfg = config.NewConfig()
	}
	return testCfg
}

// TestInitDB initializes the database
func TestInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// TestSetupSchema creates/resets the schema
func TestSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create TenantAccount table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create TenantSite table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil))
	assert.Nil(t, err)
	// create NVLinkLogicalPartition table (must be before VPC due to foreign key)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Domain)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create Allocation Constraint table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.AllocationConstraint)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Subnet)(nil))
	assert.Nil(t, err)
	// create VPC Prefix table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.VpcPrefix)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InstanceType)(nil))
	assert.Nil(t, err)
	// create Operating System table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.OperatingSystem)(nil))
	assert.Nil(t, err)
	// create Operating System Site Association table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.OperatingSystemSiteAssociation)(nil))
	assert.Nil(t, err)
	// create iPXE Template table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IpxeTemplate)(nil))
	assert.Nil(t, err)
	// create iPXE Template Site Association table (must be after IpxeTemplate due to foreign key)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IpxeTemplateSiteAssociation)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Machine)(nil))
	assert.Nil(t, err)
	// create SSH Key Group table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroup)(nil))
	assert.Nil(t, err)
	// create SSH Key Group Site Association table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupSiteAssociation)(nil))
	assert.Nil(t, err)
	// create SSH Key table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKey)(nil))
	assert.Nil(t, err)
	// create SSH Key Association table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyAssociation)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Instance)(nil))
	assert.Nil(t, err)
	// create SSH Key Group Instance Association table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupInstanceAssociation)(nil))
	assert.Nil(t, err)
	// create MachineInstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineInstanceType)(nil))
	assert.Nil(t, err)
	// create MachineCapability table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineCapability)(nil))
	assert.Nil(t, err)
	// create MachineInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineInterface)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Interface)(nil))
	assert.Nil(t, err)
	// create Fabric table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Fabric)(nil))
	assert.Nil(t, err)
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandPartition)(nil))
	assert.Nil(t, err)
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandInterface)(nil))
	assert.Nil(t, err)
	// create SpectrumXPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SpectrumXPartition)(nil))
	assert.Nil(t, err)
	// create SpectrumXAttachment table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SpectrumXAttachment)(nil))
	assert.Nil(t, err)
	// create NVLinkInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkInterface)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create VpcPeering table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.VpcPeering)(nil))
	assert.Nil(t, err)
	// create AuditEntry table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.AuditEntry)(nil))
	assert.Nil(t, err)
	// create DpuExtensionService table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.DpuExtensionService)(nil))
	assert.Nil(t, err)
	// create DpuExtensionServiceDeployment table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.DpuExtensionServiceDeployment)(nil))
	assert.Nil(t, err)

	// setup ipam table
	ipamStorage := cipam.NewBunStorage(dbSession.DB, nil)
	assert.Nil(t, ipamStorage.ApplyDbSchema())
	assert.Nil(t, ipamStorage.DeleteAllPrefixes(context.Background(), ""))

}

type TestSetupProviderMachineHandlerFixture struct {
	Org            string
	SiteID         string
	MachineID      string
	User           interface{}
	DBSession      *cdb.Session
	SiteClientPool *sc.ClientPool
	Config         *config.Config
	ProxiedReq     *grpcproxy.Request
}

func NewTestSetupProviderMachineHandlerFixture(t *testing.T, response proto.Message) TestSetupProviderMachineHandlerFixture {
	t.Helper()

	dbSession := TestInitDB(t)
	t.Cleanup(dbSession.Close)
	TestSetupSchema(t, dbSession)

	org := "test-org"
	user := TestBuildUser(t, dbSession, "test-starfleet-id", org, []string{authz.ProviderAdminRole})
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)
	it := TestBuildInstanceType(t, dbSession, "test-instance-type", cutil.GetPtr(site.ID), site, nil, user)
	machine := TestBuildMachine(t, dbSession, ip, site, &it.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)

	proxiedReq := &grpcproxy.Request{}
	wrun := &tmocks.WorkflowRun{}
	wrun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if response == nil {
			return
		}
		out := args.Get(1).(*grpcproxy.Response)
		respJSON, err := protojson.Marshal(response)
		require.NoError(t, err)
		out.ResponseJSON = respJSON
	}).Return(nil)

	tsc := &tmocks.Client{}
	tsc.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool {
			*proxiedReq = req
			return true
		}),
	).Return(wrun, nil)

	scp := sc.NewClientPool(nil)
	scp.IDClientMap[site.ID.String()] = tsc

	return TestSetupProviderMachineHandlerFixture{
		Org:            org,
		SiteID:         site.ID.String(),
		MachineID:      machine.ID,
		User:           user,
		DBSession:      dbSession,
		SiteClientPool: scp,
		Config:         GetTestConfig(),
		ProxiedReq:     proxiedReq,
	}
}

func (f TestSetupProviderMachineHandlerFixture) Request(t *testing.T, handler echo.HandlerFunc, method, target string, body any, source string) *httptest.ResponseRecorder {
	t.Helper()

	var reqBody string
	if body != nil {
		bodyBytes, err := json.Marshal(body)
		require.NoError(t, err)
		reqBody = string(bodyBytes)
	}

	e := echo.New()
	req := httptest.NewRequest(method, target, strings.NewReader(reqBody))
	if body != nil {
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	}
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	names := []string{"orgName", "id"}
	values := []string{f.Org, f.MachineID}
	if source != "" {
		names = append(names, "source")
		values = append(values, source)
	}
	ec.SetParamNames(names...)
	ec.SetParamValues(values...)
	ec.Set("user", f.User)

	require.NoError(t, handler(ec))
	return rec
}

// TestBuildInfrastructureProvider creates a test Infrastructure Provider
func TestBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)

	ip, err := ipDAO.Create(context.Background(), nil, cdbm.InfrastructureProviderCreateInput{
		Name:           name,
		DisplayName:    cutil.GetPtr("Test Infrastructure Provider"),
		Org:            org,
		OrgDisplayName: cutil.GetPtr(name),
		CreatedBy:      user.ID,
	})
	assert.Nil(t, err)

	return ip
}

// TestBuildTenant creates a test Tenant
func TestBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
	return TestBuildTenantWithDisplayName(t, dbSession, name, org, user, "Test Tenant")
}

func TestBuildTenantWithDisplayName(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User, displayName string) *cdbm.Tenant {
	tnDAO := cdbm.NewTenantDAO(dbSession)

	tn, err := tnDAO.Create(context.Background(), nil, cdbm.TenantCreateInput{
		Name:           name,
		DisplayName:    &displayName,
		Org:            org,
		OrgDisplayName: &displayName,
		CreatedBy:      user.ID,
	})
	assert.Nil(t, err)

	return tn
}

func TestBuildTenantAccount(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, tenantOrg string, status string, user *cdbm.User) *cdbm.TenantAccount {
	taDAO := cdbm.NewTenantAccountDAO(dbSession)

	ta, err := taDAO.Create(context.Background(), nil, cdbm.TenantAccountCreateInput{
		AccountNumber:            GenerateAccountNumber(),
		TenantID:                 tenantID,
		TenantOrg:                tenantOrg,
		InfrastructureProviderID: ip.ID,
		Status:                   status,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)

	return ta
}

// TestBuildTenantAccountWithTargetedInstanceCreation creates a test TenantAccount
// with the TargetedInstanceCreation capability enabled in its config. Privilege is
// resolved from a Ready TenantAccount config (see TenantHasTargetedInstanceCreation),
// so tests that exercise privileged Tenant paths must enable it on the account rather
// than only on the legacy Tenant config.
func TestBuildTenantAccountWithTargetedInstanceCreation(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, tenantOrg string, status string, user *cdbm.User) *cdbm.TenantAccount {
	taDAO := cdbm.NewTenantAccountDAO(dbSession)

	ta, err := taDAO.Create(context.Background(), nil, cdbm.TenantAccountCreateInput{
		AccountNumber:            GenerateAccountNumber(),
		TenantID:                 tenantID,
		TenantOrg:                tenantOrg,
		InfrastructureProviderID: ip.ID,
		Status:                   status,
		Config:                   &cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
		CreatedBy:                user.ID,
	})
	require.NoError(t, err)

	return ta
}

// TestBuildSite creates a test Site
func TestBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
	stDAO := cdbm.NewSiteDAO(dbSession)

	st, err := stDAO.Create(context.Background(), nil, cdbm.SiteCreateInput{
		Name:                          name,
		DisplayName:                   cutil.GetPtr("Test Site"),
		Description:                   cutil.GetPtr("Test Site Description"),
		Org:                           ip.Org,
		InfrastructureProviderID:      ip.ID,
		SiteControllerVersion:         cutil.GetPtr("1.0.0"),
		SiteAgentVersion:              cutil.GetPtr("1.0.0"),
		RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration:   cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:             false,
		SerialConsoleHostname:         cutil.GetPtr("TestSshHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(30),
		SerialConsoleMaxSessionLength: cutil.GetPtr(60),
		Status:                        cdbm.SiteStatusPending,
		CreatedBy:                     user.ID,
	})
	assert.Nil(t, err)

	return st
}

// TestBuildTenantSite creates a test Tenant/Site association
func TestBuildTenantSite(t *testing.T, dbSession *cdb.Session, tn *cdbm.Tenant, st *cdbm.Site, user *cdbm.User) *cdbm.TenantSite {
	tsDAO := cdbm.NewTenantSiteDAO(dbSession)

	ts, err := tsDAO.Create(
		context.Background(),
		nil,
		cdbm.TenantSiteCreateInput{
			TenantID:  tn.ID,
			TenantOrg: tn.Org,
			SiteID:    st.ID,
			CreatedBy: user.ID,
		},
	)
	assert.Nil(t, err)

	return ts
}

// TestBuildUser creates a test User
func TestBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	u, err := uDAO.Create(
		context.Background(),
		nil,
		cdbm.UserCreateInput{
			AuxiliaryID: nil,
			StarfleetID: &starfleetID,
			Email:       cutil.GetPtr("jdoe@test.com"),
			FirstName:   cutil.GetPtr("John"),
			LastName:    cutil.GetPtr("Doe"),
			OrgData: cdbm.OrgData{
				org: cdbm.Org{
					ID:          123,
					Name:        org,
					DisplayName: org,
					OrgType:     "ENTERPRISE",
					Roles:       roles,
				},
			},
		},
	)
	assert.Nil(t, err)

	return u
}

// TestBuildAllocation creates a test Allocation
func TestBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
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

// TestBuildAllocationConstraint creates a test Allocation Constraint of Instance Type
func TestBuildAllocationConstraint(t *testing.T, dbSession *cdb.Session, al *cdbm.Allocation, it *cdbm.InstanceType, ipb *cdbm.IPBlock, constraintValue int, user *cdbm.User) *cdbm.AllocationConstraint {
	acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
	var ac *cdbm.AllocationConstraint
	var err error
	if it != nil {
		ac, err = acDAO.Create(context.Background(), nil, cdbm.AllocationConstraintCreateInput{
			AllocationID: al.ID, ResourceType: cdbm.AllocationResourceTypeInstanceType,
			ResourceTypeID: it.ID, ConstraintType: cdbm.AllocationConstraintTypeReserved,
			ConstraintValue: constraintValue, CreatedBy: user.ID,
		})
		assert.Nil(t, err)
	} else if ipb != nil {
		ac, err = acDAO.Create(context.Background(), nil, cdbm.AllocationConstraintCreateInput{
			AllocationID: al.ID, ResourceType: cdbm.AllocationResourceTypeIPBlock,
			ResourceTypeID: ipb.ID, ConstraintType: cdbm.AllocationConstraintTypeReserved,
			ConstraintValue: constraintValue, CreatedBy: user.ID,
		})
		assert.Nil(t, err)
	}
	return ac
}

// TestBuildInstanceType creates a test Instance Type
func TestBuildInstanceType(t *testing.T, dbSession *cdb.Session, name string, infinityResourceTypeID *uuid.UUID, st *cdbm.Site, labels map[string]string, user *cdbm.User) *cdbm.InstanceType {
	itDAO := cdbm.NewInstanceTypeDAO(dbSession)

	it, err := itDAO.Create(context.Background(), nil, cdbm.InstanceTypeCreateInput{
		Name:                     name,
		Description:              cutil.GetPtr("Latest generation"),
		InfrastructureProviderID: st.InfrastructureProviderID,
		InfinityResourceTypeID:   infinityResourceTypeID,
		SiteID:                   &st.ID,
		Labels:                   labels,
		Status:                   cdbm.InstanceTypeStatusPending,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)

	return it
}

// TestBuildIPBlock creates a test IP Block
func TestBuildIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, tenantID *uuid.UUID, routingType, prefix string, blockSize int, protocolVersion string, user *cdbm.User) *cdbm.IPBlock {
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)

	ipb, err := ipbDAO.Create(
		context.Background(),
		nil,
		cdbm.IPBlockCreateInput{
			Name:                     name,
			SiteID:                   site.ID,
			InfrastructureProviderID: site.InfrastructureProviderID,
			TenantID:                 tenantID,
			RoutingType:              routingType,
			Prefix:                   prefix,
			PrefixLength:             blockSize,
			ProtocolVersion:          protocolVersion,
			Status:                   cdbm.IPBlockStatusPending,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)

	return ipb
}

// TestBuildMachine creates a test Machine
func TestBuildMachine(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, itID *uuid.UUID, controllerMachineType *string, status string) *cdbm.Machine {
	mDAO := cdbm.NewMachineDAO(dbSession)
	mID := uuid.NewString()
	createInput := cdbm.MachineCreateInput{
		MachineID:                mID,
		InfrastructureProviderID: ip.ID,
		SiteID:                   site.ID,
		InstanceTypeID:           itID,
		ControllerMachineID:      mID,
		ControllerMachineType:    controllerMachineType,
		Vendor:                   cutil.GetPtr("test-vendor"),
		ProductName:              cutil.GetPtr("test-product-name"),
		SerialNumber:             cutil.GetPtr(uuid.NewString()),
		Status:                   status,
	}
	m, err := mDAO.Create(context.Background(), nil, createInput)
	assert.Nil(t, err)

	return m
}

// TestBuildMachineInstanceType creates a test Machine Instance Type
func TestBuildMachineInstanceType(t *testing.T, dbSession *cdb.Session, m *cdbm.Machine, it *cdbm.InstanceType) *cdbm.MachineInstanceType {
	mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)

	mit, err := mitDAO.Create(context.Background(), nil, cdbm.MachineInstanceTypeCreateInput{
		MachineID:      m.ID,
		InstanceTypeID: it.ID,
	})
	assert.Nil(t, err)

	return mit
}

// TestBuildMachineCapability creates a test Machine Capability
func TestBuildMachineCapability(t *testing.T, dbSession *cdb.Session, mID *string, itID *uuid.UUID, capabilityType cdbm.MachineCapabilityType, name string, frequency *string, capacity *string, vendor *string, count *int, deviceType *cdbm.MachineCapabilityDeviceType, inactiveDevices []int) *cdbm.MachineCapability {
	mcDAO := cdbm.NewMachineCapabilityDAO(dbSession)

	mc, err := mcDAO.Create(context.Background(), nil, cdbm.MachineCapabilityCreateInput{
		MachineID:       mID,
		InstanceTypeID:  itID,
		Type:            capabilityType,
		Name:            name,
		Frequency:       frequency,
		Capacity:        capacity,
		Vendor:          vendor,
		Count:           count,
		DeviceType:      deviceType,
		InactiveDevices: inactiveDevices,
	})
	assert.Nil(t, err)

	return mc
}

// TestBuildVPC creates a test VPC
func TestBuildVPC(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, tn *cdbm.Tenant, st *cdbm.Site, cnvID *uuid.UUID, networkVirtualizationType *string, labels map[string]string, status string, user *cdbm.User) *cdbm.Vpc {
	vDAO := cdbm.NewVpcDAO(dbSession)

	if networkVirtualizationType == nil {
		networkVirtualizationType = cutil.GetPtr(cdbm.VpcEthernetVirtualizer)
	}

	input := cdbm.VpcCreateInput{
		Name:                      name,
		Description:               cutil.GetPtr("Test Vpc"),
		Org:                       st.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: networkVirtualizationType,
		ControllerVpcID:           cnvID,
		Labels:                    labels,
		Status:                    status,
		CreatedBy:                 *user,
	}

	vpc, err := vDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

// TestBuildSubnet creates a test subnet
func TestBuildSubnet(t *testing.T, dbSession *cdb.Session, name string, tn *cdbm.Tenant, vpc *cdbm.Vpc, cnsID *uuid.UUID, status string, user *cdbm.User) *cdbm.Subnet {
	subnetDAO := cdbm.NewSubnetDAO(dbSession)

	subnet, err := subnetDAO.Create(context.Background(), nil, cdbm.SubnetCreateInput{
		Name:                       name,
		Description:                cutil.GetPtr("Test Subnet"),
		Org:                        tn.Org,
		SiteID:                     vpc.SiteID,
		VpcID:                      vpc.ID,
		TenantID:                   tn.ID,
		ControllerNetworkSegmentID: cnsID,
		PrefixLength:               0,
		Status:                     status,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)

	return subnet
}

// TestBuildOperatingSystem creates a test os
func TestBuildOperatingSystem(t *testing.T, dbSession *cdb.Session, name string, tn *cdbm.Tenant, status string, user *cdbm.User) *cdbm.OperatingSystem {
	operatingSystem := &cdbm.OperatingSystem{
		ID:        uuid.New(),
		Name:      name,
		TenantID:  cutil.GetPtr(tn.ID),
		Status:    status,
		CreatedBy: user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

func TestBuildOperatingSystemSiteAssociation(t *testing.T, dbSession *cdb.Session, osID uuid.UUID, siteID uuid.UUID, version *string, status string, user *cdbm.User) *cdbm.OperatingSystemSiteAssociation {
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dbSession)

	ossa, err := ossaDAO.Create(context.Background(), nil, cdbm.OperatingSystemSiteAssociationCreateInput{
		OperatingSystemID: osID,
		SiteID:            siteID,
		Version:           version,
		Status:            status,
		CreatedBy:         user.ID,
	})
	assert.Nil(t, err)
	return ossa
}

// TestBuildInstance creates a test instance.
// It returns a persisted instance without any allocation linkage.
func TestBuildInstance(t *testing.T, dbSession *cdb.Session, name string, tnID uuid.UUID, ipID uuid.UUID, stID uuid.UUID, itID uuid.UUID, vpcID uuid.UUID, mID *string, osID uuid.UUID) *cdbm.Instance {
	ins := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     name,
		TenantID:                 tnID,
		InfrastructureProviderID: ipID,
		SiteID:                   stID,
		InstanceTypeID:           &itID,
		VpcID:                    vpcID,
		MachineID:                mID,
		OperatingSystemID:        &osID,
		Hostname:                 nil,
		UserData:                 nil,
		IpxeScript:               nil,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
		Status:                   cdbm.InstanceStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(ins).Exec(context.Background())
	assert.Nil(t, err)
	return ins
}

// TestBuildNetworkSecurityGroup creates a test security group
func TestBuildNetworkSecurityGroup(t *testing.T, dbSession *cdb.Session, name string, siteID, tenantID uuid.UUID, status string) *cdbm.NetworkSecurityGroup {
	sg := &cdbm.NetworkSecurityGroup{
		ID:       uuid.NewString(),
		Name:     name,
		SiteID:   siteID,
		TenantID: tenantID,
		Status:   status,
		Created:  cdb.GetCurTime(),
		Updated:  cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(sg).Exec(context.Background())
	assert.Nil(t, err)
	return sg
}

// TestCommonBuildMachineCapability creates a machine capability
func TestCommonBuildMachineCapability(t *testing.T, dbSession *cdb.Session, machineID *string, instanceTypeID *uuid.UUID, cptype cdbm.MachineCapabilityType, name string, freq *string, cap *string, vendor *string, count *int, deviceType *cdbm.MachineCapabilityDeviceType, info map[string]interface{}) *cdbm.MachineCapability {
	mcDAO := cdbm.NewMachineCapabilityDAO(dbSession)
	mc, err := mcDAO.Create(context.Background(), nil, cdbm.MachineCapabilityCreateInput{MachineID: machineID, InstanceTypeID: instanceTypeID, Type: cptype, Name: name, Frequency: freq, Capacity: cap, Vendor: vendor, Count: count, DeviceType: deviceType, Info: info})
	assert.Nil(t, err)
	return mc
}

// TestBuildStatusDetail creates a test status detail
func TestBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID string, status string, message *string) {
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	ssd, err := sdDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID, Status: status, Message: message})
	assert.Nil(t, err)
	assert.NotNil(t, ssd)
	assert.Equal(t, entityID, ssd.EntityID)
	assert.Equal(t, status, ssd.Status)
}

// TestBuildVpcPeering creates a test VPC peering between two VPCs
func TestBuildVpcPeering(t *testing.T, dbSession *cdb.Session, vpc1ID, vpc2ID uuid.UUID, siteID uuid.UUID, infrastructureProviderID *uuid.UUID, tenantID *uuid.UUID, isMultiTenant bool, createdByID uuid.UUID) *cdbm.VpcPeering {
	vpDAO := cdbm.NewVpcPeeringDAO(dbSession)
	vp, err := vpDAO.Create(context.Background(), nil, cdbm.VpcPeeringCreateInput{
		Vpc1ID:                   vpc1ID,
		Vpc2ID:                   vpc2ID,
		SiteID:                   siteID,
		InfrastructureProviderID: infrastructureProviderID,
		TenantID:                 tenantID,
		IsMultiTenant:            isMultiTenant,
		CreatedByID:              createdByID,
	})
	assert.Nil(t, err)
	return vp
}

// TestCommonTraceProviderSetup creates a test provider and spanner
func TestCommonTraceProviderSetup(t *testing.T, ctx context.Context) (trace.Tracer, trace.SpanContext, context.Context) {
	// OTEL spanner configuration
	provider := trace.NewNoopTracerProvider()
	otel.SetTextMapPropagator(propagation.TraceContext{})
	sc := trace.NewSpanContext(trace.SpanContextConfig{
		TraceID: trace.TraceID{0x01},
		SpanID:  trace.SpanID{0x01},
	})

	// Start echo test parent tracer/spanner
	ctx = trace.ContextWithRemoteSpanContext(ctx, sc)
	tracer := provider.Tracer(otelecho.TracerName)
	tracer.Start(ctx, "Test-Echo-Spanner")

	return tracer, sc, ctx
}

func TestBuildAuditEntry(t *testing.T, dbSession *cdb.Session, orgName string, userID *uuid.UUID, statusCode int) *cdbm.AuditEntry {
	aeDAO := cdbm.NewAuditEntryDAO(dbSession)
	aeo, err := aeDAO.Create(context.Background(), nil, cdbm.AuditEntryCreateInput{
		Endpoint: fmt.Sprintf("/v2/org/%s/nico/ep", orgName),
		QueryParams: url.Values{
			"test": []string{"1234"},
		},
		Method: "POST",
		Body: map[string]interface{}{
			"key1": "value1",
		},
		StatusCode: statusCode,
		ClientIP:   "12.123.43.112",
		UserID:     userID,
		OrgName:    orgName,
		ExtraData:  nil,
		Timestamp:  time.Now(),
		Duration:   200 * time.Millisecond,
	})
	assert.Nil(t, err)
	assert.NotNil(t, aeo)
	assert.Equal(t, orgName, aeo.OrgName)
	assert.Equal(t, userID, aeo.UserID)
	return aeo
}

func TestBuildVpcPrefixIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, routingType, prefix string, blockSize int, protocolVersion string, fullGrant bool, status string, user *cdbm.User) *cdbm.IPBlock {
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

func TestBuildVPCPrefix(t *testing.T, dbSession *cdb.Session, name string, st *cdbm.Site, tenant *cdbm.Tenant, vpcID uuid.UUID, ipv4BlockID *uuid.UUID, prefix *string, prefixLength *int, status string, user *cdbm.User) *cdbm.VpcPrefix {
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)

	vpcprefix, err := vpcPrefixDAO.Create(context.Background(), nil, cdbm.VpcPrefixCreateInput{Name: name, TenantOrg: st.Org, SiteID: st.ID, VpcID: vpcID, TenantID: tenant.ID, IpBlockID: ipv4BlockID, Prefix: *prefix, PrefixLength: *prefixLength, Status: status, CreatedBy: user.ID})
	assert.Nil(t, err)

	return vpcprefix
}

func TestBuildDpuExtensionService(t *testing.T, dbSession *cdb.Session, name string, serviceType string, tenant *cdbm.Tenant, site *cdbm.Site, version string, status string, user *cdbm.User) *cdbm.DpuExtensionService {
	desDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)

	des, err := desDAO.Create(context.Background(), nil, cdbm.DpuExtensionServiceCreateInput{
		Name:        name,
		Description: cutil.GetPtr("Test DPU Extension Service"),
		ServiceType: serviceType,
		SiteID:      site.ID,
		TenantID:    tenant.ID,
		Version:     cutil.GetPtr(version),
		VersionInfo: &cdbm.DpuExtensionServiceVersionInfo{
			Version:        version,
			Data:           "apiVersion: v1\nkind: Pod",
			HasCredentials: true,
			Created:        time.Now(),
		},
		ActiveVersions: []string{version},
		Status:         status,
		CreatedBy:      user.ID,
	})
	assert.Nil(t, err)

	return des
}

func TestBuildDpuExtensionServiceUpdateActiveVersions(t *testing.T, dbSession *cdb.Session, dpuExtensionService *cdbm.DpuExtensionService, versions []string) *cdbm.DpuExtensionService {
	desDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)

	des, err := desDAO.Update(context.Background(), nil, cdbm.DpuExtensionServiceUpdateInput{
		DpuExtensionServiceID: dpuExtensionService.ID,
		ActiveVersions:        versions,
	})
	assert.Nil(t, err)

	return des
}

func TestBuildDpuExtensionServiceDeployment(t *testing.T, dbSession *cdb.Session, dpuExtensionService *cdbm.DpuExtensionService, instanceID uuid.UUID, version string, status string, user *cdbm.User) *cdbm.DpuExtensionServiceDeployment {
	desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(dbSession)

	desd, err := desdDAO.Create(context.Background(), nil, cdbm.DpuExtensionServiceDeploymentCreateInput{
		DpuExtensionServiceID: dpuExtensionService.ID,
		SiteID:                dpuExtensionService.SiteID,
		TenantID:              dpuExtensionService.TenantID,
		InstanceID:            instanceID,
		Version:               version,
		Status:                status,
		CreatedBy:             user.ID,
	})
	assert.Nil(t, err)

	return desd
}

func TestBuildInterface(t *testing.T, dbSession *cdb.Session, instanceID uuid.UUID, subnetID *uuid.UUID, vpcPrefixID *uuid.UUID, isPhysical bool, device *string, deviceInstance *int, vfID *int, status *string, user *cdbm.User) *cdbm.Interface {
	iiDAO := cdbm.NewInterfaceDAO(dbSession)

	if status == nil {
		status = cutil.GetPtr(cdbm.InterfaceStatusPending)
	}

	ii, err := iiDAO.Create(context.Background(), nil, cdbm.InterfaceCreateInput{
		InstanceID:        instanceID,
		SubnetID:          subnetID,
		VpcPrefixID:       vpcPrefixID,
		IsPhysical:        isPhysical,
		Device:            device,
		DeviceInstance:    deviceInstance,
		VirtualFunctionID: vfID,
		Status:            *status,
		CreatedBy:         user.ID,
	})
	assert.Nil(t, err)
	return ii
}
