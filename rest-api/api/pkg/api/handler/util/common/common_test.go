// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"testing"
	"time"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cam "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

func testCommonInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for common tests
func testCommonSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create TenantAccount table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantAccount)(nil))
	assert.Nil(t, err)
	// create TenantSite table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil))
	assert.Nil(t, err)
	// create Security Group table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
	// create NVLink Logical Partition table before VPC, which references it
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Domain)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Subnet)(nil))
	assert.Nil(t, err)
	// create VpcPrefix table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.VpcPrefix)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.OperatingSystem)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create AllocationConstraint table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.AllocationConstraint)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InstanceType)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Machine)(nil))
	assert.Nil(t, err)
	// create MachineInstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineInstanceType)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Instance)(nil))
	assert.Nil(t, err)
	// create MachineInterface table before Interface, which depends on it
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineInterface)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Interface)(nil))
	assert.Nil(t, err)
	// create MachineCapability table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineCapability)(nil))
	assert.Nil(t, err)
	// create StatusDetail table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create SSH Key Group table (must be before SSHKeyAssociation which references it)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroup)(nil))
	assert.Nil(t, err)
	// create SSH Key table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKey)(nil))
	assert.Nil(t, err)
	// create SSH Key Association table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyAssociation)(nil))
	assert.Nil(t, err)
	// create SSH Key Group Site Association table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupSiteAssociation)(nil))
	assert.Nil(t, err)
	// create SSH Key Group Instance Association table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupInstanceAssociation)(nil))
	assert.Nil(t, err)
}

func testCommonBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)
	ip, err := ipDAO.Create(context.Background(), nil, cdbm.InfrastructureProviderCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Infrastructure Provider"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, ip)
	return ip
}

func testCommonBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
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

func testCommonBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, orgs []string, roles []string) *cdbm.User {
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

func testCommonBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, user *cdbm.User) *cdbm.Site {
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

func testCommonBuildVpc(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, site *cdbm.Site, org, name string, controllerVpcID *uuid.UUID) *cdbm.Vpc {
	vpc := &cdbm.Vpc{
		ID:                       uuid.New(),
		Name:                     name,
		Org:                      org,
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant.ID,
		SiteID:                   site.ID,
		ControllerVpcID:          controllerVpcID,
		Status:                   cdbm.VpcStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func testCommonBuildIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, routingType, prefix string, blockSize int, protocolVersion, status string, user *cdbm.User) *cdbm.IPBlock {
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
			Status:                   status,
			CreatedBy:                &user.ID,
		},
	)
	assert.Nil(t, err)
	return ipb
}

func testCommonBuildInstanceType(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, user *cdbm.User) *cdbm.InstanceType {
	itDAO := cdbm.NewInstanceTypeDAO(dbSession)
	it, err := itDAO.Create(context.Background(), nil, cdbm.InstanceTypeCreateInput{
		Name:                     name,
		DisplayName:              cutil.GetPtr(""),
		Description:              cutil.GetPtr(""),
		ControllerMachineType:    cutil.GetPtr("x86_64"),
		InfrastructureProviderID: ip.ID,
		SiteID:                   &site.ID,
		Status:                   cdbm.InstanceTypeStatusReady,
		CreatedBy:                user.ID,
	})
	assert.Nil(t, err)
	return it
}

func testCommonBuildMachineInstanceType(t *testing.T, dbSession *cdb.Session, machineID string, instanceTypeID uuid.UUID) *cdbm.MachineInstanceType {
	mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)
	mit, err := mitDAO.Create(context.Background(), nil, cdbm.MachineInstanceTypeCreateInput{
		MachineID:      machineID,
		InstanceTypeID: instanceTypeID,
	})
	assert.Nil(t, err)
	return mit
}

func testCommonBuildMachine(t *testing.T, dbSession *cdb.Session, infrastructureProviderID uuid.UUID, siteID uuid.UUID, instanceTypeID *uuid.UUID, controllerMachineID uuid.UUID, controllerMachineType *string, metadata *cdbm.SiteControllerMachine, defaultMacAddress *string, status string) *cdbm.Machine {
	mcDAO := cdbm.NewMachineDAO(dbSession)
	createInput := cdbm.MachineCreateInput{
		MachineID:                controllerMachineID.String(),
		InfrastructureProviderID: infrastructureProviderID,
		SiteID:                   siteID,
		InstanceTypeID:           instanceTypeID,
		ControllerMachineID:      controllerMachineID.String(),
		ControllerMachineType:    controllerMachineType,
		Vendor:                   cutil.GetPtr("test-vendor"),
		ProductName:              cutil.GetPtr("test-product-name"),
		SerialNumber:             cutil.GetPtr(uuid.NewString()),
		Metadata:                 metadata,
		DefaultMacAddress:        defaultMacAddress,
		Status:                   status,
	}
	mc, err := mcDAO.Create(context.Background(), nil, createInput)
	assert.Nil(t, err)
	return mc
}

func testCommonBuildDomain(t *testing.T, dbSession *cdb.Session, hostname, org string, userID *uuid.UUID) *cdbm.Domain {
	domain := &cdbm.Domain{
		ID:        uuid.New(),
		Hostname:  hostname,
		Org:       org,
		Status:    cdbm.DomainStatusPending,
		CreatedBy: *userID,
	}
	_, err := dbSession.DB.NewInsert().Model(domain).Exec(context.Background())
	assert.Nil(t, err)
	return domain
}

func TestGetInfrastructureProviderForOrg(t *testing.T) {
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	ctx := context.Background()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)
	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", "TestIPOrg", user)
	assert.NotNil(t, ip)

	tests := []struct {
		name        string
		orgName     string
		expectedErr bool
	}{
		{
			name:        "success when infrastructureProvider is in org",
			orgName:     "TestIPOrg",
			expectedErr: false,
		},
		{
			name:        "error when infrastructureProvider is not in org",
			orgName:     "someOrg",
			expectedErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ip, err := GetInfrastructureProviderForOrg(ctx, nil, dbSession, tc.orgName)
			assert.Equal(t, tc.expectedErr, err != nil)
			if !tc.expectedErr {
				assert.NotNil(t, ip)
			}
		})
	}

}

func TestGRPCStatusMessage(t *testing.T) {
	grpcInvalid := status.Error(codes.InvalidArgument, "model is required")
	plainErr := errors.New("plain error")

	tests := []struct {
		name string
		err  error
		want string
	}{
		{
			name: "nil error",
			err:  nil,
			want: "",
		},
		{
			name: "gRPC status message",
			err:  grpcInvalid,
			want: "model is required",
		},
		{
			name: "plain error",
			err:  plainErr,
			want: "plain error",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, GRPCStatusMessage(tt.err))
		})
	}
}

func TestUnwrapWorkflowError(t *testing.T) {
	plainErr := errors.New("plain")
	causeErr := errors.New("other error")
	grpcPerm := status.Error(codes.PermissionDenied, "forbidden")
	grpcInvalid := status.Error(codes.InvalidArgument, "Maximum Limit of Infiniband partitions had been reached")
	grpcResourceExhausted := status.Error(codes.ResourceExhausted, "VPC prefix capacity exhausted")
	wrappedResourceExhausted := swe.WrapErr(grpcResourceExhausted)

	tests := []struct {
		name                     string
		err                      error
		wantCode                 int
		wantErr                  error
		wantApplicationErrorType string
	}{
		{
			name:     "unwraps Temporal cause",
			err:      temporal.NewApplicationErrorWithCause("wrapper", "test-type", causeErr),
			wantCode: http.StatusInternalServerError,
			wantErr:  causeErr,
		},
		{
			name:     "maps gRPC permission denied",
			err:      temporal.NewApplicationErrorWithCause("wrapper", "test-type", grpcPerm),
			wantCode: http.StatusForbidden,
			wantErr:  grpcPerm,
		},
		{
			name:     "returns non-temporal error as is",
			err:      plainErr,
			wantCode: http.StatusInternalServerError,
			wantErr:  plainErr,
		},
		{
			name:     "maps gRPC invalid argument",
			err:      temporal.NewApplicationErrorWithCause("wrapper", "error", grpcInvalid),
			wantCode: http.StatusBadRequest,
			wantErr:  grpcInvalid,
		},
		{
			name:     "maps gRPC resource exhausted",
			err:      temporal.NewApplicationErrorWithCause("wrapper", "error", grpcResourceExhausted),
			wantCode: http.StatusTooManyRequests,
			wantErr:  grpcResourceExhausted,
		},
		{
			name:     "maps non-gRPC error with collected invalid argument (nvbugs 5778658)",
			err:      temporal.NewApplicationErrorWithCause("wrapper", swe.ErrTypeNICoInvalidArgument, causeErr),
			wantCode: http.StatusBadRequest,
			wantErr:  causeErr,
		},
		{
			name:                     "maps wrapped gRPC resource exhausted",
			err:                      wrappedResourceExhausted,
			wantCode:                 http.StatusTooManyRequests,
			wantErr:                  grpcResourceExhausted,
			wantApplicationErrorType: swe.ErrTypeNICoResourceExhausted,
		},
		{
			name:     "unwraps ApplicationError wrapped in generic error chain",
			err:      fmt.Errorf("workflow execution error: %w", fmt.Errorf("activity error: %w", temporal.NewApplicationErrorWithCause("wrapped", swe.ErrTypeNICoObjectNotFound, causeErr))),
			wantCode: http.StatusNotFound,
			wantErr:  causeErr,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.wantApplicationErrorType != "" {
				var applicationErr *temporal.ApplicationError
				require.ErrorAs(t, tt.err, &applicationErr)
				assert.Equal(t, tt.wantApplicationErrorType, applicationErr.Type())
				assert.True(t, applicationErr.NonRetryable())
			}

			code, gotErr := UnwrapWorkflowError(tt.err)
			assert.Equal(t, tt.wantCode, code)
			assert.Equal(t, tt.wantErr, gotErr)
		})
	}
}

func TestGetTenantForOrg(t *testing.T) {
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	ctx := context.Background()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, "test123GetTenForOrg", []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)
	tn := testCommonBuildTenant(t, dbSession, "testTenant", "testTnOrg", user)
	assert.NotNil(t, tn)

	tests := []struct {
		name        string
		orgName     string
		expectedErr bool
	}{
		{
			name:        "success when tenant is in org",
			orgName:     "testTnOrg",
			expectedErr: false,
		},
		{
			name:        "error when tenant is not in org",
			orgName:     "someOrg",
			expectedErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ip, err := GetTenantForOrg(ctx, nil, dbSession, tc.orgName)
			assert.Equal(t, tc.expectedErr, err != nil)
			if !tc.expectedErr {
				assert.NotNil(t, ip)
			}
		})
	}

}

func TestGetTenantFromTenantIDOrOrg(t *testing.T) {
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	ctx := context.Background()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, "TestGetTenantFromTenantIDOrOrg", []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)
	tn := testCommonBuildTenant(t, dbSession, "testTenant", "testTnOrg", user)
	assert.NotNil(t, tn)

	tests := []struct {
		name        string
		orgName     *string
		tenantID    *string
		expectedErr bool
	}{
		{
			name:        "error when both tenant id and org are nil",
			expectedErr: true,
		},
		{
			name:        "error when tenant id is invalid uuid",
			tenantID:    cutil.GetPtr("someuuid"),
			expectedErr: true,
		},
		{
			name:        "error when tenant id not found",
			tenantID:    cutil.GetPtr(uuid.New().String()),
			expectedErr: true,
		},
		{
			name:        "success when tenant id valid",
			tenantID:    cutil.GetPtr(tn.ID.String()),
			expectedErr: false,
		},
		{
			name:        "error when tenant not in org",
			orgName:     cutil.GetPtr("someOrg"),
			expectedErr: true,
		},
		{
			name:        "success when tenant in org",
			orgName:     cutil.GetPtr("testTnOrg"),
			expectedErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			tn, err := GetTenantFromTenantIDOrOrg(ctx, nil, dbSession, tc.tenantID, tc.orgName)
			assert.Equal(t, tc.expectedErr, err != nil)
			if !tc.expectedErr {
				assert.NotNil(t, tn)
			}
		})
	}
}

func TestGenerateAccountNumber(t *testing.T) {
	acctNum := GenerateAccountNumber()
	assert.True(t, len(acctNum) > 0)
}

func TestGetSiteFromIDString(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, "test123TestGetSite", []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", "TestIpOrg", user)
	assert.NotNil(t, ip)
	site := testCommonBuildSite(t, dbSession, ip, "testSite", user)
	assert.NotNil(t, site)

	tests := []struct {
		name      string
		siteid    string
		expectErr bool
	}{
		{
			name:      "success when Id exists",
			siteid:    site.ID.String(),
			expectErr: false,
		},
		{
			name:      "error when id is invalid uuid",
			siteid:    "baduuidstr",
			expectErr: true,
		},
		{
			name:      "error when id doesnt exist",
			siteid:    uuid.New().String(),
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			s, err := GetSiteFromIDString(ctx, nil, tc.siteid, dbSession)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, s)
			}
		})
	}
}

func TestAuthorizeProviderSiteForCore(t *testing.T) {
	ctx := context.Background()
	dbSession := TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	logger := zerolog.New(os.Stdout)

	org := "test-provider-org"
	user := TestBuildUser(t, dbSession, uuid.NewString(), org, []string{authz.ProviderAdminRole})
	assert.NotNil(t, user)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	assert.NotNil(t, ip)
	site := TestBuildSite(t, dbSession, ip, "Test Site", user)
	sDAO := cdbm.NewSiteDAO(dbSession)
	_, err := sDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)

	otherOrg := "other-provider-org"
	otherUser := TestBuildUser(t, dbSession, uuid.NewString(), otherOrg, []string{authz.ProviderAdminRole})
	assert.NotNil(t, otherUser)
	otherIP := TestBuildInfrastructureProvider(t, dbSession, "Other Infrastructure Provider", otherOrg, otherUser)
	assert.NotNil(t, otherIP)
	otherSite := TestBuildSite(t, dbSession, otherIP, "Other Site", otherUser)
	assert.NotNil(t, otherSite)

	tenantOrg := "tenant-org"
	tenantUser := TestBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{authz.TenantAdminRole})
	assert.NotNil(t, tenantUser)
	tenant := TestBuildTenant(t, dbSession, tenantOrg, "Tenant", tenantUser)
	assert.NotNil(t, tenant)
	tenantSite := TestBuildTenantSite(t, dbSession, tenant, site, user)
	assert.NotNil(t, tenantSite)

	scp := &stubSiteTemporalClientPool{client: &tmocks.Client{}}
	authInput := func(org string, user *cdbm.User, siteID string) AuthorizeProviderSiteForCoreInput {
		return AuthorizeProviderSiteForCoreInput{
			Ctx:       ctx,
			Logger:    logger,
			DBSession: dbSession,
			SCP:       scp,
			Org:       org,
			User:      user,
			SiteID:    siteID,
		}
	}

	t.Run("success", func(t *testing.T) {
		client, siteID, apiErr := AuthorizeProviderSiteForCore(authInput(org, user, site.ID.String()))
		require.Nil(t, apiErr)
		require.NotNil(t, client)
		assert.Equal(t, site.ID.String(), siteID)
	})

	t.Run("nil user", func(t *testing.T) {
		_, _, apiErr := AuthorizeProviderSiteForCore(authInput(org, nil, site.ID.String()))
		require.NotNil(t, apiErr)
		assert.Equal(t, http.StatusInternalServerError, apiErr.Code)
	})

	t.Run("user is not a provider admin", func(t *testing.T) {
		_, _, apiErr := AuthorizeProviderSiteForCore(authInput(tenantOrg, tenantUser, site.ID.String()))
		require.NotNil(t, apiErr)
		assert.Equal(t, http.StatusForbidden, apiErr.Code)
	})

	t.Run("site not found", func(t *testing.T) {
		missingSiteID := uuid.NewString()
		_, _, apiErr := AuthorizeProviderSiteForCore(authInput(org, user, missingSiteID))
		require.NotNil(t, apiErr)
		assert.Equal(t, http.StatusBadRequest, apiErr.Code)
		assert.Contains(t, apiErr.Message, missingSiteID)
	})

	t.Run("site belongs to another provider", func(t *testing.T) {
		_, _, apiErr := AuthorizeProviderSiteForCore(authInput(org, user, otherSite.ID.String()))
		require.NotNil(t, apiErr)
		assert.Equal(t, http.StatusForbidden, apiErr.Code)
	})
}

type stubSiteTemporalClientPool struct {
	client tclient.Client
}

func (s *stubSiteTemporalClientPool) GetClientByID(siteID uuid.UUID) (tclient.Client, error) {
	return s.client, nil
}

func TestGetIPBlockFromIDString(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", "TestIpOrg", user)
	assert.NotNil(t, ip)
	site := testCommonBuildSite(t, dbSession, ip, "testSite", user)
	assert.NotNil(t, site)
	tenant := testCommonBuildTenant(t, dbSession, "testTenant", ipOrg1, user)
	ipBlock := testCommonBuildIPBlock(t, dbSession, "testIPB", site, ip, &tenant.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, cdbm.IPBlockStatusReady, user)
	tests := []struct {
		name      string
		ipBlockID string
		expectErr bool
	}{
		{
			name:      "success when Id exists",
			ipBlockID: ipBlock.ID.String(),
			expectErr: false,
		},
		{
			name:      "error when id is invalid uuid",
			ipBlockID: "baduuidstr",
			expectErr: true,
		},
		{
			name:      "error when id doesnt exist",
			ipBlockID: uuid.New().String(),
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			filter := cdbm.IPBlockFilterInput{}
			filter.TenantAllocated(tenant.ID)
			s, err := GetIPBlockFromIDString(ctx, nil, tc.ipBlockID, filter, dbSession)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, s)
			}
		})
	}
}

func TestGetInstanceTypeFromIDString(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp1", "TestIpOrg1", user)
	assert.NotNil(t, ip)
	site := testCommonBuildSite(t, dbSession, ip, "testSite", user)
	assert.NotNil(t, site)
	instanceType := testCommonBuildInstanceType(t, dbSession, "it", site, ip, user)
	tests := []struct {
		name      string
		itID      string
		expectErr bool
	}{
		{
			name:      "success when Id exists",
			itID:      instanceType.ID.String(),
			expectErr: false,
		},
		{
			name:      "error when id is invalid uuid",
			itID:      "baduuidstr",
			expectErr: true,
		},
		{
			name:      "error when id doesnt exist",
			itID:      uuid.New().String(),
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			s, err := GetInstanceTypeFromIDString(ctx, nil, tc.itID, dbSession)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, s)
			}
		})
	}
}

func TestGetVpcFromIDString(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)
	site := testCommonBuildSite(t, dbSession, ip, "test", user)
	tenant1 := testCommonBuildTenant(t, dbSession, "t1", ipOrg1, user)
	vpc := testCommonBuildVpc(t, dbSession, ip, tenant1, site, ipOrg1, "testVPC", nil)

	tests := []struct {
		name      string
		id        string
		expectErr bool
	}{
		{
			name:      "success when Id exists",
			id:        vpc.ID.String(),
			expectErr: false,
		},
		{
			name:      "error when id is invalid uuid",
			id:        "baduuidstr",
			expectErr: true,
		},
		{
			name:      "error when id doesnt exist",
			id:        uuid.New().String(),
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			s, err := GetVpcFromIDString(ctx, nil, tc.id, nil, dbSession)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, s)
			}
		})
	}
}

func TestGetDomainFromIDString(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{authz.ProviderAdminRole}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	domain := testCommonBuildDomain(t, dbSession, "test.com", ipOrg1, &user.ID)
	tests := []struct {
		name      string
		id        string
		expectErr bool
	}{
		{
			name:      "success when Id exists",
			id:        domain.ID.String(),
			expectErr: false,
		},
		{
			name:      "error when id is invalid uuid",
			id:        "baduuidstr",
			expectErr: true,
		},
		{
			name:      "error when id doesnt exist",
			id:        uuid.New().String(),
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			s, err := GetDomainFromIDString(ctx, nil, tc.id, dbSession)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, s)
			}
		})
	}
}

func TestGetAllocationConstraintsForInstanceType(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site1)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	inst1 := testCommonBuildInstanceType(t, dbSession, "it", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	al1 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation", user)
	assert.NotNil(t, al1)

	alc1 := TestBuildAllocationConstraint(t, dbSession, al1, inst1, nil, 1, tnuser)
	assert.NotNil(t, alc1)

	tests := []struct {
		name         string
		tenantid     uuid.UUID
		instancetype *cdbm.InstanceType
		allocations  []cdbm.Allocation
		expectErr    bool
	}{
		{
			name:         "success when allocation exists",
			tenantid:     tenant1.ID,
			instancetype: inst1,
			allocations: []cdbm.Allocation{
				*al1,
			},
			expectErr: false,
		},
		{
			name:         "error when allocation doesnt exist",
			tenantid:     tenant1.ID,
			instancetype: inst1,
			allocations:  nil,
			expectErr:    true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			s, err := GetAllocationConstraintsForInstanceType(ctx, nil, dbSession, tc.tenantid, tc.instancetype, tc.allocations)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, s)
			}
		})
	}
}

func TestGetUnallocatedMachineForInstanceType(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	tx, _ := cdb.BeginTx(ctx, dbSession, &sql.TxOptions{})
	assert.NotNil(t, tx)
	defer tx.Rollback()

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site1)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	inst1 := testCommonBuildInstanceType(t, dbSession, "it", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	mcCount := 30
	for i := 0; i < mcCount; i++ {
		mcStatus := cdbm.MachineStatusReset
		if i > 20 {
			mcStatus = cdbm.MachineStatusReady
		}

		mc := testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, cutil.GetPtr(inst1.ID), uuid.New(), nil, nil, nil, mcStatus)
		assert.NotNil(t, mc)
		if i == 21 || i == 22 {
			_, err := cdbm.NewMachineDAO(dbSession).Update(ctx, nil, cdbm.MachineUpdateInput{
				MachineID: mc.ID,
				Labels:    map[string]string{"failure-domain": "fd-a"},
			})
			assert.NoError(t, err)
		}

		mit := testCommonBuildMachineInstanceType(t, dbSession, mc.ID, inst1.ID)
		assert.NotNil(t, mit)
	}

	tests := []struct {
		name         string
		instancetype *cdbm.InstanceType
		request      *cam.APIInstanceCreateRequest
		expectErr    bool
	}{
		{
			name:         "error when no Machine matches label selector",
			instancetype: inst1,
			request: &cam.APIInstanceCreateRequest{
				MachineLabelSelector: map[string]string{"failure-domain": "missing"},
			},
			expectErr: true,
		},
		{
			name:         "success when Machine matches all label selector",
			instancetype: inst1,
			request: &cam.APIInstanceCreateRequest{
				MachineLabelSelector: map[string]string{"failure-domain": "fd-a"},
			},
			expectErr: false,
		},
		{
			name:         "success when machine and machine instance type exists",
			instancetype: inst1,
			expectErr:    false,
		},
		{
			name:         "error when allocation does not exist",
			instancetype: nil,
			expectErr:    true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			s, err := GetUnallocatedMachineForInstanceType(ctx, zerolog.Nop(), tx, dbSession, tc.instancetype, tc.request)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, s)
				if tc.request != nil {
					assert.True(t, s.MatchesLabelSelector(tc.request.MachineLabelSelector))
				}
			}
		})
	}

	t.Run("rechecks labels after a concurrent update", func(t *testing.T) {
		concurrentInstanceType := testCommonBuildInstanceType(t, dbSession, "concurrent-label-update", site1, ip, tnuser)
		machine := testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, cutil.GetPtr(concurrentInstanceType.ID), uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
		_, err := cdbm.NewMachineDAO(dbSession).Update(ctx, nil, cdbm.MachineUpdateInput{
			MachineID: machine.ID,
			Labels:    map[string]string{"failure-domain": "fd-a"},
		})
		require.NoError(t, err)
		testCommonBuildMachineInstanceType(t, dbSession, machine.ID, concurrentInstanceType.ID)

		labelTx, err := cdb.BeginTx(ctx, dbSession, nil)
		require.NoError(t, err)
		_, err = cdbm.NewMachineDAO(dbSession).Update(ctx, labelTx, cdbm.MachineUpdateInput{
			MachineID: machine.ID,
			Labels:    map[string]string{"failure-domain": "fd-b"},
		})
		require.NoError(t, err)

		allocationTx, err := cdb.BeginTx(ctx, dbSession, nil)
		require.NoError(t, err)
		labelTxFinished := false
		defer func() {
			if !labelTxFinished {
				assert.NoError(t, labelTx.Rollback())
			}
			require.NoError(t, allocationTx.Rollback())
		}()
		var allocationBackendPID int
		err = allocationTx.GetBunTx().NewSelect().ColumnExpr("pg_backend_pid()").Scan(ctx, &allocationBackendPID)
		require.NoError(t, err)

		type allocationResult struct {
			machine *cdbm.Machine
			err     error
		}
		resultCh := make(chan allocationResult, 1)
		go func() {
			selected, selectionErr := GetUnallocatedMachineForInstanceType(
				ctx,
				zerolog.Nop(),
				allocationTx,
				dbSession,
				concurrentInstanceType,
				&cam.APIInstanceCreateRequest{MachineLabelSelector: map[string]string{"failure-domain": "fd-a"}},
			)
			resultCh <- allocationResult{machine: selected, err: selectionErr}
		}()

		require.Eventually(t, func() bool {
			var waitEventType string
			queryErr := dbSession.DB.QueryRowContext(ctx, `
				SELECT COALESCE(wait_event_type, '')
				FROM pg_catalog.pg_stat_activity
				WHERE pid = ?
			`, allocationBackendPID).Scan(&waitEventType)
			return queryErr == nil && waitEventType == "Lock"
		}, 5*time.Second, 10*time.Millisecond, "Machine selection did not wait for the concurrent label update")

		require.NoError(t, labelTx.Commit())
		labelTxFinished = true
		select {
		case result := <-resultCh:
			require.Nil(t, result.machine)
			require.Error(t, result.err)
		case <-time.After(5 * time.Second):
			t.Fatal("Machine selection did not resume after the concurrent label update committed")
		}
	})
}

func TestGetSiteMachineCountStats(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	tx, _ := cdb.BeginTx(ctx, dbSession, &sql.TxOptions{})
	assert.NotNil(t, tx)
	defer tx.Rollback()

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	ipuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg1, ipuser)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", ipuser)
	assert.NotNil(t, site1)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	it1 := testCommonBuildInstanceType(t, dbSession, "instance-type", site1, ip, ipuser)
	assert.NotNil(t, it1)

	it2 := testCommonBuildInstanceType(t, dbSession, "instance-type-2", site1, ip, ipuser)
	assert.NotNil(t, it2)

	al1 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation", ipuser)
	assert.NotNil(t, al1)

	al2 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation-2", ipuser)
	assert.NotNil(t, al1)

	alc1 := TestBuildAllocationConstraint(t, dbSession, al1, it1, nil, 3, tnuser)
	assert.NotNil(t, alc1)

	alc2 := TestBuildAllocationConstraint(t, dbSession, al2, it2, nil, 2, tnuser)
	assert.NotNil(t, alc2)

	totalAllocationCount := alc1.ConstraintValue + alc2.ConstraintValue

	readyCount := 4
	inUseCount := 3
	errorCount := 6

	mcCount := readyCount + inUseCount + errorCount

	// 4 Machines in ready state, 3 have InstanceType, 1 does not
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it1.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it1.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it2.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, nil, uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)

	// 3 Machines in use state
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it1.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusInUse)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it1.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusInUse)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it2.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusInUse)

	// 6 Machines in error state, 3 have InstanceType, 3 do not
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it1.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusError)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it2.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusError)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, &it1.ID, uuid.New(), nil, nil, nil, cdbm.MachineStatusError)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, nil, uuid.New(), nil, nil, nil, cdbm.MachineStatusError)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, nil, uuid.New(), nil, nil, nil, cdbm.MachineStatusError)
	testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, nil, uuid.New(), nil, nil, nil, cdbm.MachineStatusError)

	tests := []struct {
		name                       string
		siteID                     uuid.UUID
		infrastructureProviderID   uuid.UUID
		wantTotalMachineCount      int
		wantMachineStatusStats     map[string]int
		wantMachineAllocationStats map[string]int
		logger                     zerolog.Logger
		expectErr                  bool
	}{
		{
			name:                     "site has expected breakdown of machine status",
			infrastructureProviderID: ip.ID,
			siteID:                   site1.ID,
			wantTotalMachineCount:    mcCount,
			wantMachineStatusStats: map[string]int{
				cdbm.MachineStatusReady: readyCount,
				cdbm.MachineStatusInUse: inUseCount,
				cdbm.MachineStatusError: errorCount,
			},
			wantMachineAllocationStats: map[string]int{
				cam.MachineStatsAllocatedInUse:    inUseCount,
				cam.MachineStatsAllocatedNotInUse: totalAllocationCount - inUseCount,
				cam.MachineStatsUnallocated:       mcCount - totalAllocationCount,
			},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			ms, err := GetSiteMachineCountStats(ctx, tx, dbSession, tc.logger, &tc.infrastructureProviderID, &tc.siteID)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.NotNil(t, ms)
			}

			assert.Equal(t, tc.wantTotalMachineCount, ms[tc.siteID].Total)

			for status := range tc.wantMachineStatusStats {
				assert.Equal(t, tc.wantMachineStatusStats[status], ms[tc.siteID].TotalByStatus[status])
				assert.Equal(t, tc.wantMachineStatusStats[status], ms[tc.siteID].TotalByStatusAndHealth[status]["healthy"])
			}

			assert.Equal(t, tc.wantMachineAllocationStats, ms[tc.siteID].TotalByAllocation)
		})
	}
}

func TestGetSiteGPUStats(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	ipOrg := "test-gpu-ip-org"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	ipuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "test-gpu-ip", ipOrg, ipuser)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "gpu-site-1", ipuser)
	site2 := testCommonBuildSite(t, dbSession, ip, "gpu-site-2", ipuser)

	mA := testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, nil, uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	mB := testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, nil, uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	mC := testCommonBuildMachine(t, dbSession, ip.ID, site2.ID, nil, uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)

	const h100 = "NVIDIA H100"
	const a100 = "NVIDIA A100"

	// site1: H100 on mA(8) and mB(8) -> 16 GPUs / 2 machines; A100 on mA(2) -> 2 GPUs / 1 machine
	TestBuildMachineCapability(t, dbSession, &mA.ID, nil, cdbm.MachineCapabilityTypeGPU, h100, nil, nil, nil, cutil.GetPtr(8), nil, nil)
	TestBuildMachineCapability(t, dbSession, &mA.ID, nil, cdbm.MachineCapabilityTypeGPU, a100, nil, nil, nil, cutil.GetPtr(2), nil, nil)
	TestBuildMachineCapability(t, dbSession, &mB.ID, nil, cdbm.MachineCapabilityTypeGPU, h100, nil, nil, nil, cutil.GetPtr(8), nil, nil)
	// site2: H100 on mC(4)
	TestBuildMachineCapability(t, dbSession, &mC.ID, nil, cdbm.MachineCapabilityTypeGPU, h100, nil, nil, nil, cutil.GetPtr(4), nil, nil)
	// non-GPU capability must be excluded from GPU stats
	TestBuildMachineCapability(t, dbSession, &mA.ID, nil, cdbm.MachineCapabilityTypeCPU, "Intel Xeon", nil, nil, nil, cutil.GetPtr(2), nil, nil)

	logger := zerolog.Nop()

	t.Run("provider-wide returns per-site stats sorted by name", func(t *testing.T) {
		stats, err := GetSiteGPUStats(ctx, nil, dbSession, logger, &ip.ID, nil)
		require.Nil(t, err)

		require.Len(t, stats[site1.ID], 2)
		assert.Equal(t, a100, stats[site1.ID][0].Name)
		assert.Equal(t, 2, stats[site1.ID][0].GPUs)
		assert.Equal(t, 1, stats[site1.ID][0].Machines)
		assert.Equal(t, h100, stats[site1.ID][1].Name)
		assert.Equal(t, 16, stats[site1.ID][1].GPUs)
		assert.Equal(t, 2, stats[site1.ID][1].Machines)

		require.Len(t, stats[site2.ID], 1)
		assert.Equal(t, h100, stats[site2.ID][0].Name)
		assert.Equal(t, 4, stats[site2.ID][0].GPUs)
		assert.Equal(t, 1, stats[site2.ID][0].Machines)
	})

	t.Run("site-scoped returns only the requested site", func(t *testing.T) {
		stats, err := GetSiteGPUStats(ctx, nil, dbSession, logger, &ip.ID, &site2.ID)
		require.Nil(t, err)

		assert.Len(t, stats, 1)
		require.Len(t, stats[site2.ID], 1)
		assert.Equal(t, 4, stats[site2.ID][0].GPUs)
	})

	t.Run("site with no GPUs yields no entry", func(t *testing.T) {
		emptySite := testCommonBuildSite(t, dbSession, ip, "gpu-site-empty", ipuser)
		stats, err := GetSiteGPUStats(ctx, nil, dbSession, logger, &ip.ID, &emptySite.ID)
		require.Nil(t, err)
		assert.Empty(t, stats[emptySite.ID])
	})
}

func TestGetAllocationIDsForTenantAtSite(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	testCommonSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site1)

	site2 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site2)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	inst1 := testCommonBuildInstanceType(t, dbSession, "it1", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	inst2 := testCommonBuildInstanceType(t, dbSession, "it2", site1, ip, tnuser)
	assert.NotNil(t, inst2)

	al1 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation", user)
	assert.NotNil(t, al1)

	al2 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation2", user)
	assert.NotNil(t, al2)

	tests := []struct {
		name      string
		tenantID  uuid.UUID
		siteID    uuid.UUID
		expectCnt int
		expectErr bool
	}{
		{
			name:      "success case non-zero allocation IDs",
			tenantID:  tenant1.ID,
			siteID:    site1.ID,
			expectCnt: 2,
			expectErr: false,
		},
		{
			name:      "success case with zero allocationIDs",
			tenantID:  tenant1.ID,
			siteID:    site2.ID,
			expectCnt: 0,
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			aIDs, err := GetAllocationIDsForTenantAtSite(ctx, nil, dbSession, ip.ID, tc.tenantID, tc.siteID)
			assert.Equal(t, tc.expectErr, err != nil)
			assert.Equal(t, tc.expectCnt, len(aIDs))
		})
	}
}

func TestGetTotalAllocationConstraintValueForInstanceType(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	testCommonSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site1)

	site2 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site2)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	inst1 := testCommonBuildInstanceType(t, dbSession, "it1", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	inst2 := testCommonBuildInstanceType(t, dbSession, "it2", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	al1 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation", user)
	assert.NotNil(t, al1)

	al2 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation2", user)
	assert.NotNil(t, al1)

	al3 := TestBuildAllocation(t, dbSession, site2, tenant1, "test-allocation3", user)
	assert.NotNil(t, al3)

	alc1 := TestBuildAllocationConstraint(t, dbSession, al1, inst1, nil, 3, tnuser)
	assert.NotNil(t, alc1)

	alc2 := TestBuildAllocationConstraint(t, dbSession, al2, inst1, nil, 7, tnuser)
	assert.NotNil(t, alc2)

	alc3 := TestBuildAllocationConstraint(t, dbSession, al3, inst2, nil, 1, tnuser)
	assert.NotNil(t, alc3)

	tests := []struct {
		name           string
		allocationIDs  []uuid.UUID
		constraintType *string
		instanceTypeID *uuid.UUID
		expectCnt      int
		expectErr      bool
	}{
		{
			name:           "success case with allocationIDs, constraintType nil",
			allocationIDs:  nil,
			constraintType: nil,
			instanceTypeID: &inst1.ID,
			expectCnt:      10,
			expectErr:      false,
		},
		{
			name:           "success case for specific instance type",
			allocationIDs:  []uuid.UUID{al1.ID, al2.ID},
			instanceTypeID: &inst1.ID,
			constraintType: cutil.GetPtr(cdbm.AllocationConstraintTypeReserved),
			expectCnt:      10,
			expectErr:      false,
		},
		{
			name:           "success case for specific instance type, another instancetype",
			instanceTypeID: &inst2.ID,
			constraintType: cutil.GetPtr(cdbm.AllocationConstraintTypeReserved),
			expectCnt:      1,
			expectErr:      false,
		},
		{
			name:           "success case with different constraint type",
			constraintType: cutil.GetPtr(cdbm.AllocationConstraintTypePreemptible),
			expectCnt:      0,
			expectErr:      false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			tot, err := GetTotalAllocationConstraintValueForInstanceType(ctx, nil, dbSession, tc.allocationIDs, tc.instanceTypeID, tc.constraintType)
			assert.Equal(t, tc.expectErr, err != nil)
			assert.Equal(t, tc.expectCnt, tot)
		})
	}
}

func TestGetCountOfInstanceTypeAllocationConstraint(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site1)

	site2 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site2)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	inst1 := testCommonBuildInstanceType(t, dbSession, "it1", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	inst2 := testCommonBuildInstanceType(t, dbSession, "it2", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	al1 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation", user)
	assert.NotNil(t, al1)

	al2 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation2", user)
	assert.NotNil(t, al1)

	al3 := TestBuildAllocation(t, dbSession, site2, tenant1, "test-allocation3", user)
	assert.NotNil(t, al3)

	alc1 := TestBuildAllocationConstraint(t, dbSession, al1, inst1, nil, 1, tnuser)
	assert.NotNil(t, alc1)

	alc2 := TestBuildAllocationConstraint(t, dbSession, al2, inst2, nil, 1, tnuser)
	assert.NotNil(t, alc2)

	tests := []struct {
		name           string
		site           *cdbm.Site
		instanceTypeID *uuid.UUID
		expectCnt      int
		expectErr      bool
	}{
		{
			name:      "success case with non-zero",
			site:      site1,
			expectCnt: 2,
			expectErr: false,
		},
		{
			name:           "success case for specific instance type",
			site:           site1,
			instanceTypeID: &inst1.ID,
			expectCnt:      1,
			expectErr:      false,
		},
		{
			name:      "success case with zero",
			site:      site2,
			expectCnt: 0,
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			acs, c, err := GetAllAllocationConstraintsForInstanceType(ctx, nil, dbSession, ip, tc.site, tenant1, tc.instanceTypeID)
			assert.Equal(t, tc.expectErr, err != nil)
			assert.Equal(t, tc.expectCnt, c)
			assert.Equal(t, tc.expectCnt, len(acs))
		})
	}
}

func TestGetAndValidateQueryRelations(t *testing.T) {
	q1 := url.Values{}
	q1.Add("includeRelation", cdbm.TenantRelationName)
	q1.Add("includeRelation", cdbm.InfrastructureProviderRelationName)

	q2 := url.Values{}
	q2.Add("includeRelation", cdbm.VpcRelationName)

	tests := []struct {
		name            string
		qParams         url.Values
		relatedEntities map[string]bool
		expectErr       bool
		expectKeyLen    int
	}{
		{
			name:            "success when query values and relation entities exists",
			qParams:         q1,
			relatedEntities: cdbm.TenantAccountRelatedEntities,
			expectErr:       false,
			expectKeyLen:    2,
		},
		{
			name:            "faile when query values and relation entities doesn't exist",
			qParams:         q2,
			relatedEntities: cdbm.TenantAccountRelatedEntities,
			expectErr:       true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			s, err := GetAndValidateQueryRelations(tc.qParams, tc.relatedEntities)
			assert.Equal(t, tc.expectErr, err != "")
			if s != nil {
				assert.Equal(t, tc.expectKeyLen, len(s))
			}
		})
	}
}

func TestGetInstanceTypeAllocationStats(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	testCommonSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrg3 := "test-tenant-org-3"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)
	tnu2 := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg2}, tnRoles)
	tnu3 := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg3}, tnRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg, ipu)

	site1 := testCommonBuildSite(t, dbSession, ip, "test-site-1", ipu)

	it1 := testCommonBuildInstanceType(t, dbSession, "instance-type-1", site1, ip, ipu)

	goodMachines := 30
	badMachines := 10
	allocatedMachines := 20

	m1s := []*cdbm.Machine{}
	for i := 0; i < goodMachines; i++ {
		m := TestBuildMachine(t, dbSession, ip, site1, &it1.ID, cutil.GetPtr("x86"), cdbm.MachineStatusReady)
		TestBuildMachineInstanceType(t, dbSession, m, it1)
		m1s = append(m1s, m)
	}

	// Add some bad machines
	for i := goodMachines; i < (goodMachines + badMachines); i++ {
		m := TestBuildMachine(t, dbSession, ip, site1, &it1.ID, cutil.GetPtr("x86"), cdbm.MachineStatusError)
		TestBuildMachineInstanceType(t, dbSession, m, it1)
		m1s = append(m1s, m)
	}

	site2 := testCommonBuildSite(t, dbSession, ip, "test-site-2", ipu)

	it2 := testCommonBuildInstanceType(t, dbSession, "instance-type-2", site2, ip, ipu)

	m2s := []*cdbm.Machine{}
	for i := 0; i < 20; i++ {
		m := TestBuildMachine(t, dbSession, ip, site2, &it2.ID, cutil.GetPtr("x86"), cdbm.MachineStatusReady)
		TestBuildMachineInstanceType(t, dbSession, m, it2)
		m2s = append(m2s, m)
	}

	tn1 := testCommonBuildTenant(t, dbSession, "tenant-1", tnOrg1, tnu1)
	tn2 := testCommonBuildTenant(t, dbSession, "tenant-2", tnOrg1, tnu2)
	tn3 := testCommonBuildTenant(t, dbSession, "tenant-2", tnOrg1, tnu3)

	TestBuildTenantSite(t, dbSession, tn1, site1, tnu1)
	TestBuildTenantSite(t, dbSession, tn2, site2, tnu2)
	TestBuildTenantSite(t, dbSession, tn3, site2, tnu3)

	vpc1 := testCommonBuildVpc(t, dbSession, ip, tn1, site1, tnOrg1, "test-vpc-1", cutil.GetPtr(uuid.New()))
	vpc2 := testCommonBuildVpc(t, dbSession, ip, tn2, site1, tnOrg2, "test-vpc-2", cutil.GetPtr(uuid.New()))
	vpc3 := testCommonBuildVpc(t, dbSession, ip, tn3, site1, tnOrg3, "test-vpc-3", cutil.GetPtr(uuid.New()))

	al1 := TestBuildAllocation(t, dbSession, site1, tn1, "test-allocation-1", ipu)
	alc1 := TestBuildAllocationConstraint(t, dbSession, al1, it1, nil, 15, ipu)

	al2 := TestBuildAllocation(t, dbSession, site1, tn2, "test-allocation-2", ipu)
	alc2 := TestBuildAllocationConstraint(t, dbSession, al2, it1, nil, 8, ipu)

	al3 := TestBuildAllocation(t, dbSession, site1, tn3, "test-allocation-3", ipu)
	alc3 := TestBuildAllocationConstraint(t, dbSession, al3, it1, nil, 17, ipu)

	os1 := TestBuildOperatingSystem(t, dbSession, "test-os-1", tn1, cdbm.OperatingSystemStatusReady, tnu1)
	os2 := TestBuildOperatingSystem(t, dbSession, "test-os-2", tn1, cdbm.OperatingSystemStatusReady, tnu1)

	it1inss := []cdbm.Instance{}
	tn1inss := []cdbm.Instance{}
	tn2inss := []cdbm.Instance{}
	tn3inss := []cdbm.Instance{}

	for i := 0; i < allocatedMachines; i++ {
		var ins *cdbm.Instance
		if i < 5 {
			ins = TestBuildInstance(t, dbSession, fmt.Sprintf("test-instance-%v", i), tn1.ID, ip.ID, site1.ID, it1.ID, vpc1.ID, &m1s[i].ID, os1.ID)
			tn1inss = append(tn1inss, *ins)
		} else if i < 8 {
			ins = TestBuildInstance(t, dbSession, fmt.Sprintf("test-instance-%v", i), tn2.ID, ip.ID, site1.ID, it1.ID, vpc2.ID, &m1s[i].ID, os2.ID)
			tn2inss = append(tn2inss, *ins)
		} else {
			ins = TestBuildInstance(t, dbSession, fmt.Sprintf("test-instance-%v", i), tn3.ID, ip.ID, site1.ID, it1.ID, vpc3.ID, &m1s[i].ID, os2.ID)
			tn3inss = append(tn3inss, *ins)
		}
		it1inss = append(it1inss, *ins)
	}

	tests := []struct {
		name        string
		tenantID    *uuid.UUID
		ipID        *uuid.UUID
		siteID      *uuid.UUID
		it          *cdbm.InstanceType
		tnas        []cdbm.Allocation
		instances   []cdbm.Instance
		expectStats *cam.APIInstanceTypeAllocationStats
		expectErr   bool
		logger      zerolog.Logger
	}{
		{
			name:      "success case with Instance Type with Allocation and active Instances, retrieved by Tenant, case 1",
			tenantID:  cutil.GetPtr(tn1.ID),
			instances: tn1inss,
			it:        it1,
			expectStats: &cam.APIInstanceTypeAllocationStats{
				Assigned:       len(m1s),
				Total:          alc1.ConstraintValue,
				Used:           len(tn1inss),
				Unused:         alc1.ConstraintValue - len(tn1inss),
				UnusedUsable:   alc1.ConstraintValue - len(tn1inss),
				MaxAllocatable: nil,
			},
			expectErr: false,
		},
		{
			name:      "success case with Instance Type with Allocation and active Instances, retrieved by Tenant, case 2",
			tenantID:  cutil.GetPtr(tn2.ID),
			instances: tn2inss,
			it:        it1,
			expectStats: &cam.APIInstanceTypeAllocationStats{
				Assigned:       len(m1s),
				Total:          alc2.ConstraintValue,
				Used:           len(tn2inss),
				Unused:         alc2.ConstraintValue - len(tn2inss),
				UnusedUsable:   alc2.ConstraintValue - len(tn2inss),
				MaxAllocatable: nil,
			},
			expectErr: false,
		},
		{
			name:      "failure case with Instance Type with Allocation and active Instances, retrieved by Tenant, case 3",
			tenantID:  cutil.GetPtr(tn3.ID),
			instances: tn3inss,
			it:        it1,
			expectStats: &cam.APIInstanceTypeAllocationStats{
				Assigned:       len(m1s),
				Total:          alc3.ConstraintValue,
				Used:           len(tn3inss),
				Unused:         alc3.ConstraintValue - len(tn3inss),
				UnusedUsable:   alc3.ConstraintValue - len(tn3inss),
				MaxAllocatable: nil,
			},
			expectErr: false,
		},
		{
			name:      "success case with Instance Type with Allocation and active Instances, retrieved by Provider",
			tenantID:  nil,
			instances: it1inss,
			it:        it1,
			expectStats: &cam.APIInstanceTypeAllocationStats{
				Assigned:       len(m1s),
				Total:          alc1.ConstraintValue + alc2.ConstraintValue + alc3.ConstraintValue,
				Used:           len(it1inss),
				Unused:         (alc1.ConstraintValue + alc2.ConstraintValue + alc3.ConstraintValue) - len(it1inss),
				UnusedUsable:   (alc1.ConstraintValue + alc2.ConstraintValue + alc3.ConstraintValue) - len(it1inss) - badMachines,
				MaxAllocatable: cutil.GetPtr(len(m1s) - (alc1.ConstraintValue + alc2.ConstraintValue + alc3.ConstraintValue)),
			},
			expectErr: false,
		},
		{
			name:      "success case with Instance Type without Allocation or active Instances, retrieved by Provider",
			tenantID:  nil,
			instances: []cdbm.Instance{},
			it:        it2,
			expectStats: &cam.APIInstanceTypeAllocationStats{
				Assigned:       len(m2s),
				Total:          0,
				Used:           0,
				Unused:         0,
				UnusedUsable:   0,
				MaxAllocatable: cutil.GetPtr(len(m2s)),
			},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			stats, err := GetInstanceTypeAllocationStats(ctx, dbSession, tc.logger, *tc.it, tc.tenantID)
			assert.Equal(t, tc.expectErr, err != nil)
			if stats != nil {
				assert.Equal(t, tc.expectStats, stats)
			}
		})
	}
}

func TestGetIsProviderRequest(t *testing.T) {
	ctx := context.Background()
	dbSession := TestInitDB(t)
	defer dbSession.Close()

	TestSetupSchema(t, dbSession)

	logger := zerolog.New(os.Stdout)

	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipViewerRoles := []string{authz.ProviderViewerRole}

	ipu := TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	tnu := TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	mOrg1 := "test-mixed-role-org-1"
	mOrg2 := "test-mixed-role-org-2"
	mOrg3 := "test-mixed-role-org-3"

	nOrg1 := "test-no-entity-org-1"
	nOrg2 := "test-no-entity-org-2"

	mRoles := []string{authz.ProviderAdminRole, authz.TenantAdminRole}

	mu1 := TestBuildUser(t, dbSession, uuid.NewString(), mOrg1, mRoles)
	mu2 := TestBuildUser(t, dbSession, uuid.NewString(), mOrg2, mRoles)
	mu3 := TestBuildUser(t, dbSession, uuid.NewString(), mOrg3, mRoles)

	nu1 := TestBuildUser(t, dbSession, uuid.NewString(), nOrg1, ipRoles)
	assert.NotNil(t, nu1)
	nu2 := TestBuildUser(t, dbSession, uuid.NewString(), nOrg2, tnRoles)
	assert.NotNil(t, nu2)

	mip1 := TestBuildInfrastructureProvider(t, dbSession, "Test Mixed Role Provider 1", mOrg1, mu1)
	mtn1 := TestBuildTenant(t, dbSession, "Test Mixed Role Tenant 1", mOrg1, mu1)

	mip2 := TestBuildInfrastructureProvider(t, dbSession, "Test Mixed Role Provider 1", mOrg2, mu2)
	mtn2 := TestBuildTenant(t, dbSession, "Test Mixed Role Tenant 2", mOrg3, mu3)

	type args struct {
		ctx            context.Context
		logger         zerolog.Logger
		dbSession      *cdb.Session
		org            string
		userOrgDetails *cdbm.Org
		providerRoles  []string
		tenantRoles    []string
		queryParams    url.Values
	}
	tests := []struct {
		name                  string
		args                  args
		wantIsProviderRequest bool
		wantProvider          *cdbm.InfrastructureProvider
		wantTenant            *cdbm.Tenant
		wantAPIError          *cutil.APIError
	}{
		{
			name: "success case with Provider admin role",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       ipOrg,
				userOrgDetails: &cdbm.Org{
					Name:  ipOrg,
					Roles: ipRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
				},
			},
			wantIsProviderRequest: true,
			wantProvider:          ip,
			wantTenant:            nil,
			wantAPIError:          nil,
		},
		{
			name: "success case with Provider viewer role",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       ipOrg,
				userOrgDetails: &cdbm.Org{
					Name:  ipOrg,
					Roles: ipViewerRoles,
				},
				providerRoles: ipViewerRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
				},
			},
			wantIsProviderRequest: true,
			wantProvider:          ip,
			wantTenant:            nil,
			wantAPIError:          nil,
		},
		{
			name: "failure case specifying both Provider and Tenant IDs in query params",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       ipOrg,
				userOrgDetails: &cdbm.Org{
					Name:  ipOrg,
					Roles: ipRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{uuid.NewString()},
					"tenantId":                 []string{uuid.NewString()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
		{
			name: "failure case with Provider admin specifying Tenant ID query",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       ipOrg,
				userOrgDetails: &cdbm.Org{
					Name:  ipOrg,
					Roles: ipRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"tenantId": []string{uuid.NewString()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusForbidden, "test message", nil),
		},
		{
			name: "success case with Tenant admin",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       tnOrg,
				userOrgDetails: &cdbm.Org{
					Name:  tnOrg,
					Roles: tnRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"tenantId": []string{tn.ID.String()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            tn,
			wantAPIError:          nil,
		},
		{
			name: "failure case with Tenant admin specifying Provider ID query",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       tnOrg,
				userOrgDetails: &cdbm.Org{
					Name:  tnOrg,
					Roles: tnRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{uuid.NewString()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusForbidden, "test message", nil),
		},
		{
			name: "success case with Provider/Tenant admin specifying Provider ID query",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg1,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg1,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{mip1.ID.String()},
				},
			},
			wantIsProviderRequest: true,
			wantProvider:          mip1,
			wantTenant:            mtn1,
			wantAPIError:          nil,
		},
		{
			name: "success case with Provider/Tenant admin specifying Provider ID query with no Tenant",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg2,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg2,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{mip2.ID.String()},
				},
			},
			wantIsProviderRequest: true,
			wantProvider:          mip2,
			wantTenant:            nil,
			wantAPIError:          nil,
		},
		{
			name: "failure case with Provider/Tenant admin specifying Tenant ID query with no Tenant",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg2,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg2,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"tenantId": []string{uuid.NewString()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
		{
			name: "success case with Provider/Tenant admin specifying Tenant ID query",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg1,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg1,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"tenantId": []string{mtn1.ID.String()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          mip1,
			wantTenant:            mtn1,
			wantAPIError:          nil,
		},
		{
			name: "failure case with Provider/Tenant admin specifying no query",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg1,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg1,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams:   url.Values{},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
		{
			name: "success case with Provider/Tenant admin specifying Tenant ID query with no Provider",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg3,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg3,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"tenantId": []string{mtn2.ID.String()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            mtn2,
			wantAPIError:          nil,
		},
		{
			name: "failure case with Provider/Tenant admin specifying Provider ID query with no Provider",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg3,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg3,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{uuid.NewString()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
		{
			name: "failure case with Provider/Tenant admin specifying Provider ID query for unassociated Provider",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg2,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg2,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"infrastructureProviderId": []string{ip.ID.String()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
		{
			name: "failure case with Provider/Tenant admin specifying unassociated Tenant ID in query",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       mOrg3,
				userOrgDetails: &cdbm.Org{
					Name:  mOrg3,
					Roles: mRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams: url.Values{
					"tenantId": []string{tn.ID.String()},
				},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
		{
			name: "failure case with Provider admin for without associated Provider",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       nOrg1,
				userOrgDetails: &cdbm.Org{
					Name:  nOrg1,
					Roles: ipRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams:   url.Values{},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
		{
			name: "failure case with Tenant admin for without associated Tenant",
			args: args{
				ctx:       ctx,
				logger:    logger,
				dbSession: dbSession,
				org:       nOrg2,
				userOrgDetails: &cdbm.Org{
					Name:  nOrg2,
					Roles: tnRoles,
				},
				providerRoles: ipRoles,
				tenantRoles:   tnRoles,
				queryParams:   url.Values{},
			},
			wantIsProviderRequest: false,
			wantProvider:          nil,
			wantTenant:            nil,
			wantAPIError:          cutil.NewAPIError(http.StatusBadRequest, "test message", nil),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gotIsProviderRequest, gotProvider, gotTenant, gotAPIError := GetIsProviderRequest(tt.args.ctx, tt.args.logger, tt.args.dbSession, tt.args.org,
				&cdbm.User{OrgData: cdbm.OrgData{tt.args.org: *tt.args.userOrgDetails}}, tt.args.providerRoles, tt.args.tenantRoles, tt.args.queryParams)

			assert.Equal(t, tt.wantIsProviderRequest, gotIsProviderRequest)

			if tt.wantProvider != nil {
				assert.Equal(t, tt.wantProvider.ID, gotProvider.ID)
			}

			if tt.wantTenant != nil {
				assert.Equal(t, tt.wantTenant.ID, gotTenant.ID)
			}

			if tt.wantAPIError != nil {
				assert.Equal(t, tt.wantAPIError.Code, gotAPIError.Code)
			}
		})
	}
}

func TestMatchInstanceTypeCapabilitiesForMachines(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	logger := zerolog.New(os.Stdout)

	tx, _ := cdb.BeginTx(ctx, dbSession, &sql.TxOptions{})
	assert.NotNil(t, tx)
	defer tx.Rollback()

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site1)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	inst1 := testCommonBuildInstanceType(t, dbSession, "it", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	icap1 := TestCommonBuildMachineCapability(t, dbSession, nil, &inst1.ID, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0Hz"), cutil.GetPtr("32GB"), nil, cutil.GetPtr(4), nil, nil)
	assert.NotNil(t, icap1)

	icap2 := TestCommonBuildMachineCapability(t, dbSession, nil, &inst1.ID, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), nil, nil)
	assert.NotNil(t, icap2)

	icap3 := TestCommonBuildMachineCapability(t, dbSession, nil, &inst1.ID, cdbm.MachineCapabilityTypeNetwork, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil)
	assert.NotNil(t, icap3)

	// An omitted DeviceType is a wildcard. This filter has only an SPX
	// candidate on the matching Machine below.
	icap4 := TestCommonBuildMachineCapability(t, dbSession, nil, &inst1.ID, cdbm.MachineCapabilityTypeNetwork, "ConnectX-8", nil, nil, nil, cutil.GetPtr(4), nil, nil)
	assert.NotNil(t, icap4)

	mc1 := testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, cutil.GetPtr(inst1.ID), uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	assert.NotNil(t, mc1)

	mcap2 := TestCommonBuildMachineCapability(t, dbSession, &mc1.ID, nil, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), nil, nil)
	assert.NotNil(t, mcap2)

	mcap1 := TestCommonBuildMachineCapability(t, dbSession, &mc1.ID, nil, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0Hz"), cutil.GetPtr("32GB"), nil, cutil.GetPtr(4), nil, nil)
	assert.NotNil(t, mcap1)

	mcap3 := TestCommonBuildMachineCapability(t, dbSession, &mc1.ID, nil, cdbm.MachineCapabilityTypeNetwork, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil)
	assert.NotNil(t, mcap3)

	// The same network description can identify generic, DPU, and SPX
	// capabilities. The DPU filter above must match its exact device type rather
	// than whichever same-name row happens to be loaded last.
	mcap4 := TestCommonBuildMachineCapability(t, dbSession, &mc1.ID, nil, cdbm.MachineCapabilityTypeNetwork, "MT28908 Family [ConnectX-7]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(1), nil, nil)
	assert.NotNil(t, mcap4)
	mcap5 := TestCommonBuildMachineCapability(t, dbSession, &mc1.ID, nil, cdbm.MachineCapabilityTypeNetwork, "MT28908 Family [ConnectX-7]", nil, nil, nil, cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSPX), nil)
	assert.NotNil(t, mcap5)
	mcap6 := TestCommonBuildMachineCapability(t, dbSession, &mc1.ID, nil, cdbm.MachineCapabilityTypeNetwork, "ConnectX-8", nil, nil, nil, cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeSPX), nil)
	assert.NotNil(t, mcap6)

	mc2 := testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, cutil.GetPtr(inst1.ID), uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	assert.NotNil(t, mc2)

	mcap21 := TestCommonBuildMachineCapability(t, dbSession, &mc2.ID, nil, cdbm.MachineCapabilityTypeCPU, "AMD Opteron Series x10", cutil.GetPtr("3.0Hz"), cutil.GetPtr("32GB"), nil, cutil.GetPtr(4), nil, nil)
	assert.NotNil(t, mcap21)

	// A requested Machine without capability rows must not disappear from the
	// candidate index when another requested Machine does have matching rows.
	mc3 := testCommonBuildMachine(t, dbSession, ip.ID, site1.ID, cutil.GetPtr(inst1.ID), uuid.New(), nil, nil, nil, cdbm.MachineStatusReady)
	assert.NotNil(t, mc3)

	tests := []struct {
		name                  string
		ctx                   context.Context
		logger                zerolog.Logger
		dbSession             *cdb.Session
		machineIDs            []string
		instanceTypeID        uuid.UUID
		expectMatch           bool
		expectMachineIDReturn bool
		expectMachineID       string
		expectErr             bool
	}{
		{
			name:                  "success when machine and instance type capabilties matches",
			dbSession:             dbSession,
			logger:                logger,
			instanceTypeID:        inst1.ID,
			machineIDs:            []string{mc1.ID},
			expectErr:             false,
			expectMatch:           true,
			expectMachineIDReturn: false,
		},
		{
			name:                  "success when machine and instance type capabilties doesn't match",
			dbSession:             dbSession,
			logger:                logger,
			instanceTypeID:        inst1.ID,
			machineIDs:            []string{mc2.ID},
			expectErr:             false,
			expectMatch:           false,
			expectMachineIDReturn: true,
			expectMachineID:       mc2.ID,
		},
		{
			name:                  "fails when one requested machine has no capabilities",
			dbSession:             dbSession,
			logger:                logger,
			instanceTypeID:        inst1.ID,
			machineIDs:            []string{mc1.ID, mc3.ID},
			expectErr:             false,
			expectMatch:           false,
			expectMachineIDReturn: true,
			expectMachineID:       mc3.ID,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			m, mid, err := MatchInstanceTypeCapabilitiesForMachines(ctx, tc.logger, tc.dbSession, tc.instanceTypeID, tc.machineIDs)
			assert.Equal(t, tc.expectErr, err != nil)
			if err == nil {
				assert.Equal(t, tc.expectMatch, m)
				if tc.expectMachineIDReturn {
					assert.Equal(t, tc.expectMachineID, *mid)
				}
			}
		})
	}
}

func TestMachineCapabilityMatchesFilter(t *testing.T) {
	dpu := cdbm.MachineCapabilityDeviceTypeDPU
	spx := cdbm.MachineCapabilityDeviceTypeSPX

	newPair := func() (*cdbm.MachineCapability, *cdbm.MachineCapability) {
		candidate := &cdbm.MachineCapability{
			Type:             cdbm.MachineCapabilityTypeNetwork,
			Name:             "ConnectX-8",
			Frequency:        cutil.GetPtr("3.0GHz"),
			Capacity:         cutil.GetPtr("400Gbps"),
			HardwareRevision: cutil.GetPtr("A1"),
			Cores:            cutil.GetPtr(8),
			Threads:          cutil.GetPtr(16),
			Vendor:           cutil.GetPtr("NVIDIA"),
			DeviceType:       &dpu,
			Count:            cutil.GetPtr(2),
		}
		filter := *candidate
		return candidate, &filter
	}

	tests := []struct {
		name      string
		configure func(candidate, filter *cdbm.MachineCapability)
		want      bool
	}{
		{
			name: "all populated fields match",
			want: true,
		},
		{
			name: "omitted optional filters are wildcards",
			configure: func(candidate, filter *cdbm.MachineCapability) {
				candidate.DeviceType = &spx
				filter.Frequency = nil
				filter.Capacity = nil
				filter.HardwareRevision = nil
				filter.Cores = nil
				filter.Threads = nil
				filter.Vendor = nil
				filter.DeviceType = nil
				filter.InactiveDevices = nil
				filter.Count = nil
			},
			want: true,
		},
		{
			name: "type differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Type = cdbm.MachineCapabilityTypeGPU
				candidate.DeviceType = nil
			},
		},
		{
			name: "name differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Name = "ConnectX-7"
			},
		},
		{
			name: "populated filter rejects a missing candidate field",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Frequency = nil
			},
		},
		{
			name: "frequency differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Frequency = cutil.GetPtr("2.0GHz")
			},
		},
		{
			name: "capacity differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Capacity = cutil.GetPtr("200Gbps")
			},
		},
		{
			name: "hardware revision differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.HardwareRevision = cutil.GetPtr("B1")
			},
		},
		{
			name: "cores differ",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Cores = cutil.GetPtr(4)
			},
		},
		{
			name: "threads differ",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Threads = cutil.GetPtr(8)
			},
		},
		{
			name: "vendor differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Vendor = cutil.GetPtr("Other")
			},
		},
		{
			name: "explicit device type differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.DeviceType = &spx
			},
		},
		{
			name: "inactive devices differ",
			configure: func(candidate, filter *cdbm.MachineCapability) {
				candidate.Type = cdbm.MachineCapabilityTypeInfiniBand
				filter.Type = cdbm.MachineCapabilityTypeInfiniBand
				candidate.DeviceType = nil
				filter.DeviceType = nil
				candidate.InactiveDevices = []int{1, 3}
				filter.InactiveDevices = []int{1, 2}
			},
		},
		{
			name: "count differs",
			configure: func(candidate, _ *cdbm.MachineCapability) {
				candidate.Count = cutil.GetPtr(4)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			candidate, filter := newPair()
			if tt.configure != nil {
				tt.configure(candidate, filter)
			}
			assert.Equal(t, tt.want, machineCapabilityMatchesFilter(candidate, filter))
		})
	}
}

func TestGetAllocationResourceTypeMaps(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()
	testCommonSetupSchema(t, dbSession)

	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"
	ipOrg3 := "test-ip-org-3"
	orgRoles := []string{"NICO_SERVICE_PROVIDER_ADMIN"}
	user := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg1, ipOrg2, ipOrg3}, orgRoles)

	ip := testCommonBuildInfrastructureProvider(t, dbSession, "TestIp", ipOrg1, user)
	assert.NotNil(t, ip)

	site1 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site1)

	site2 := testCommonBuildSite(t, dbSession, ip, "test", user)
	assert.NotNil(t, site2)

	tnOrg1 := "test-tenant-org-1"
	tnRoles := []string{"TENANT_ADMIN_1"}
	tnuser := testCommonBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg1}, tnRoles)

	tenant1 := testCommonBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, tnuser)
	assert.NotNil(t, tenant1)

	inst1 := testCommonBuildInstanceType(t, dbSession, "it1", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	inst2 := testCommonBuildInstanceType(t, dbSession, "it2", site1, ip, tnuser)
	assert.NotNil(t, inst1)

	ipBlock1 := testCommonBuildIPBlock(t, dbSession, "testIPB", site1, ip, &tenant1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, cdbm.IPBlockProtocolVersionV4, cdbm.IPBlockStatusReady, user)

	al1 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation", user)
	assert.NotNil(t, al1)

	al2 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation2", user)
	assert.NotNil(t, al1)

	al3 := TestBuildAllocation(t, dbSession, site2, tenant1, "test-allocation3", user)
	assert.NotNil(t, al3)

	al4 := TestBuildAllocation(t, dbSession, site1, tenant1, "test-allocation4", user)
	assert.NotNil(t, al4)

	alc1 := TestBuildAllocationConstraint(t, dbSession, al1, inst1, nil, 3, tnuser)
	assert.NotNil(t, alc1)

	alc2 := TestBuildAllocationConstraint(t, dbSession, al2, inst1, nil, 7, tnuser)
	assert.NotNil(t, alc2)

	alc3 := TestBuildAllocationConstraint(t, dbSession, al3, inst2, nil, 1, tnuser)
	assert.NotNil(t, alc3)

	alc4 := TestBuildAllocationConstraint(t, dbSession, al4, nil, ipBlock1, 24, tnuser)
	assert.NotNil(t, alc4)

	tests := []struct {
		name                    string
		allocationConstraints   []cdbm.AllocationConstraint
		expectInstanceTypeCount int
		expectIPBlockCount      int
		expectErr               bool
		logger                  zerolog.Logger
	}{
		{
			name:                    "success case with provided allocation constraints, returns instance type and ipblock map",
			allocationConstraints:   []cdbm.AllocationConstraint{*alc1, *alc4},
			expectInstanceTypeCount: 1,
			expectIPBlockCount:      1,
			expectErr:               false,
		},
		{
			name:                    "success case with provided allocation constraints, returns only instance type",
			allocationConstraints:   []cdbm.AllocationConstraint{*alc1},
			expectInstanceTypeCount: 1,
			expectIPBlockCount:      0,
			expectErr:               false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			instMap, ipBlockMap, err := GetAllocationResourceTypeMaps(ctx, tc.logger, dbSession, tc.allocationConstraints)
			assert.Equal(t, tc.expectErr, err != nil)
			assert.Equal(t, len(instMap), tc.expectInstanceTypeCount)
			assert.Equal(t, len(ipBlockMap), tc.expectIPBlockCount)
		})
	}
}

// TestUniqueChecker_Add_Basic tests the Add method
func TestUniqueChecker_Add_Basic(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	id1 := uuid.New()
	id2 := uuid.New()
	id3 := uuid.New()

	// Add first entry with unique value
	checker.Update(id1, "00:11:22:33:44:55")

	// Add second entry with different unique value
	checker.Update(id2, "AA:BB:CC:DD:EE:FF")

	// Add third entry with duplicate unique value
	checker.Update(id3, "00:11:22:33:44:55")

	// Should have 1 duplicate (values are stored in lowercase)
	duplicates := checker.GetDuplicates()
	assert.Len(t, duplicates, 1)
	assert.Contains(t, duplicates, "00:11:22:33:44:55")

	// Should detect duplicates
	assert.True(t, checker.HasDuplicates())
}

// TestUniqueChecker_Add_NoDuplicates tests that no duplicates are detected when all values are unique
func TestUniqueChecker_Add_NoDuplicates(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	id1 := uuid.New()
	id2 := uuid.New()
	id3 := uuid.New()

	// Add entries with all unique values
	checker.Update(id1, "00:11:22:33:44:55")
	checker.Update(id2, "AA:BB:CC:DD:EE:FF")
	checker.Update(id3, "FF:FF:FF:FF:FF:FF")

	// Should have no duplicates
	duplicates := checker.GetDuplicates()
	assert.Empty(t, duplicates)
	assert.False(t, checker.HasDuplicates())
}

// TestUniqueChecker_Update tests the Update method
func TestUniqueChecker_Update(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	id1 := uuid.New()
	id2 := uuid.New()

	// Initial add
	oldMac := "00:11:22:33:44:55"
	newMac := "AA:BB:CC:DD:EE:FF"

	checker.Update(id1, oldMac)
	checker.Update(id2, "11:22:33:44:55:66")

	// Initially no duplicates
	assert.False(t, checker.HasDuplicates())

	// Update id1 to a new unique value
	checker.Update(id1, newMac)

	// Old value should no longer be counted
	assert.False(t, checker.HasDuplicates())

	// Update id1 to same value as id2 - should create duplicate
	checker.Update(id1, "11:22:33:44:55:66")

	// Should now have duplicates
	duplicates := checker.GetDuplicates()
	assert.Len(t, duplicates, 1)
	assert.Contains(t, duplicates, "11:22:33:44:55:66")
	assert.True(t, checker.HasDuplicates())
}

// TestUniqueChecker_Update_NoChange tests updating with same value
func TestUniqueChecker_Update_NoChange(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	id1 := uuid.New()
	mac := "00:11:22:33:44:55"

	checker.Update(id1, mac)

	// Update with same value should be no-op
	checker.Update(id1, mac)

	// Should still have no duplicates
	assert.False(t, checker.HasDuplicates())
	assert.Empty(t, checker.GetDuplicates())
}

// TestUniqueChecker_Update_NewID tests updating a new ID that doesn't exist yet
func TestUniqueChecker_Update_NewID(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	id1 := uuid.New()
	id2 := uuid.New()

	checker.Update(id1, "00:11:22:33:44:55")

	// Update a new ID that hasn't been added yet
	checker.Update(id2, "AA:BB:CC:DD:EE:FF")

	// Should have no duplicates
	assert.False(t, checker.HasDuplicates())

	// Verify id2 was added (values are stored in lowercase)
	assert.Equal(t, "aa:bb:cc:dd:ee:ff", checker.idToUniqueValue[id2])
}

// TestUniqueChecker_GetDuplicates tests the GetDuplicates method with multiple duplicates
func TestUniqueChecker_GetDuplicates(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	id1 := uuid.New()
	id2 := uuid.New()
	id3 := uuid.New()
	id4 := uuid.New()
	id5 := uuid.New()

	// Add values where two unique values are duplicated
	checker.Update(id1, "MAC-A")
	checker.Update(id2, "MAC-B")
	checker.Update(id3, "MAC-A") // duplicate of id1
	checker.Update(id4, "MAC-C")
	checker.Update(id5, "MAC-A") // another duplicate of id1

	// Should have 1 duplicate value (MAC-A appears 3 times, stored as lowercase)
	duplicates := checker.GetDuplicates()
	assert.Len(t, duplicates, 1)
	assert.Contains(t, duplicates, "mac-a")
	assert.True(t, checker.HasDuplicates())
}

// TestUniqueChecker_GetDuplicates_MultipleDuplicates tests multiple different duplicate values
func TestUniqueChecker_GetDuplicates_MultipleDuplicates(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	// Create 6 IDs
	ids := make([]uuid.UUID, 6)
	for i := range ids {
		ids[i] = uuid.New()
	}

	// Add values where two different unique values are duplicated
	checker.Update(ids[0], "MAC-A")
	checker.Update(ids[1], "MAC-B")
	checker.Update(ids[2], "MAC-A") // duplicate MAC-A
	checker.Update(ids[3], "MAC-C")
	checker.Update(ids[4], "MAC-B") // duplicate MAC-B
	checker.Update(ids[5], "MAC-D")

	// Should have 2 duplicate values (stored as lowercase)
	duplicates := checker.GetDuplicates()
	assert.Len(t, duplicates, 2)
	assert.Contains(t, duplicates, "mac-a")
	assert.Contains(t, duplicates, "mac-b")
	assert.True(t, checker.HasDuplicates())
}

// TestUniqueChecker_BatchOperationExample demonstrates usage in a batch operation
func TestUniqueChecker_BatchOperationExample(t *testing.T) {
	// Simulate batch create operation with MAC addresses
	type MachineRequest struct {
		BmcMacAddress       string
		ChassisSerialNumber string
	}

	requests := []MachineRequest{
		{BmcMacAddress: "00:11:22:33:44:55", ChassisSerialNumber: "SN001"},
		{BmcMacAddress: "AA:BB:CC:DD:EE:FF", ChassisSerialNumber: "SN002"},
		{BmcMacAddress: "00:11:22:33:44:55", ChassisSerialNumber: "SN003"}, // Duplicate MAC
		{BmcMacAddress: "FF:FF:FF:FF:FF:FF", ChassisSerialNumber: "SN001"}, // Duplicate Serial
	}

	macChecker := NewUniqueChecker[int]()
	serialChecker := NewUniqueChecker[int]()

	// Add all requests to checkers
	for i, req := range requests {
		macChecker.Update(i, req.BmcMacAddress)
		serialChecker.Update(i, req.ChassisSerialNumber)
	}

	// Check for duplicates
	macDuplicates := macChecker.GetDuplicates()
	serialDuplicates := serialChecker.GetDuplicates()

	// Should have detected 1 MAC duplicate and 1 Serial duplicate (values are stored in lowercase)
	assert.Len(t, macDuplicates, 1)
	assert.Contains(t, macDuplicates, "00:11:22:33:44:55")

	assert.Len(t, serialDuplicates, 1)
	assert.Contains(t, serialDuplicates, "sn001")

	// Can build error messages
	var validationErrors []string
	for _, dup := range macDuplicates {
		validationErrors = append(validationErrors, fmt.Sprintf("duplicate BMC MAC address '%s'", dup))
	}
	for _, dup := range serialDuplicates {
		validationErrors = append(validationErrors, fmt.Sprintf("duplicate chassis serial number '%s'", dup))
	}

	assert.Len(t, validationErrors, 2)
}

// TestUniqueChecker_UpdateScenario demonstrates usage in a batch update operation
func TestUniqueChecker_UpdateScenario(t *testing.T) {
	// Simulate existing machines in database
	id1 := uuid.MustParse("00000000-0000-0000-0000-000000000001")
	id2 := uuid.MustParse("00000000-0000-0000-0000-000000000002")
	id3 := uuid.MustParse("00000000-0000-0000-0000-000000000003")

	existingMachines := map[uuid.UUID]string{
		id1: "MAC-001",
		id2: "MAC-002",
		id3: "MAC-003",
	}

	// Simulate update requests
	type UpdateRequest struct {
		ID            uuid.UUID
		NewBmcAddress string
	}

	requests := []UpdateRequest{
		{
			ID:            id1,
			NewBmcAddress: "MAC-NEW-1", // Change to unique value - OK
		},
		{
			ID:            id3,
			NewBmcAddress: "MAC-002", // Change to same value as id2 - DUPLICATE
		},
	}

	macChecker := NewUniqueChecker[uuid.UUID]()

	// First, populate checker with existing machines
	for machineID, mac := range existingMachines {
		macChecker.Update(machineID, mac)
	}

	// Initially no duplicates
	assert.False(t, macChecker.HasDuplicates())

	// Now process update requests
	for _, req := range requests {
		macChecker.Update(req.ID, req.NewBmcAddress)
	}

	// Should have detected 1 conflict (MAC-002 now used by both id2 and id3, stored as lowercase)
	duplicates := macChecker.GetDuplicates()
	assert.Len(t, duplicates, 1)
	assert.Contains(t, duplicates, "mac-002")
	assert.True(t, macChecker.HasDuplicates())
}

// TestUniqueChecker_ComplexScenario tests a complex scenario with adds and updates
func TestUniqueChecker_ComplexScenario(t *testing.T) {
	checker := NewUniqueChecker[uuid.UUID]()

	// Create some IDs
	id1 := uuid.New()
	id2 := uuid.New()
	id3 := uuid.New()
	id4 := uuid.New()

	// Initial state: 4 machines with unique MACs
	checker.Update(id1, "MAC-A")
	checker.Update(id2, "MAC-B")
	checker.Update(id3, "MAC-C")
	checker.Update(id4, "MAC-D")

	// No duplicates
	assert.False(t, checker.HasDuplicates())

	// Update id3 to use same MAC as id1
	checker.Update(id3, "MAC-A")

	// Should have 1 duplicate (stored as lowercase)
	duplicates := checker.GetDuplicates()
	assert.Len(t, duplicates, 1)
	assert.Contains(t, duplicates, "mac-a")

	// Update id3 back to unique value
	checker.Update(id3, "MAC-E")

	// No duplicates again
	assert.False(t, checker.HasDuplicates())

	// Update id1 and id2 to same value
	checker.Update(id1, "MAC-X")
	checker.Update(id2, "MAC-X")

	// Should have 1 duplicate (stored as lowercase)
	duplicates = checker.GetDuplicates()
	assert.Len(t, duplicates, 1)
	assert.Contains(t, duplicates, "mac-x")
}

func TestValidateKnownQueryParams(t *testing.T) {
	type rackRequest struct {
		SiteID string `query:"siteId"`
		Name   string `query:"name"`
	}
	type pageRequest struct {
		PageNumber string `query:"pageNumber"`
		PageSize   string `query:"pageSize"`
	}

	tests := []struct {
		name    string
		query   string
		structs []any
		wantErr bool
		errMsg  string
	}{
		{
			name:    "all params known",
			query:   "siteId=abc&name=rack1",
			structs: []any{rackRequest{}},
			wantErr: false,
		},
		{
			name:    "no params",
			query:   "",
			structs: []any{rackRequest{}},
			wantErr: false,
		},
		{
			name:    "unknown param",
			query:   "siteId=abc&foo=bar",
			structs: []any{rackRequest{}},
			wantErr: true,
			errMsg:  "Unknown query parameter specified in request: foo",
		},
		{
			name:    "all unknown",
			query:   "bogus=1",
			structs: []any{rackRequest{}},
			wantErr: true,
			errMsg:  "Unknown query parameter specified in request: bogus",
		},
		{
			name:    "subset of allowed",
			query:   "name=rack1",
			structs: []any{rackRequest{}},
			wantErr: false,
		},
		{
			name:    "multiple structs merges allowed keys",
			query:   "siteId=abc&pageNumber=1",
			structs: []any{rackRequest{}, pageRequest{}},
			wantErr: false,
		},
		{
			name:    "multiple structs still rejects unknown",
			query:   "siteId=abc&pageNumber=1&bogus=x",
			structs: []any{rackRequest{}, pageRequest{}},
			wantErr: true,
			errMsg:  "Unknown query parameter specified in request: bogus",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			queryParams, _ := url.ParseQuery(tt.query)
			err := ValidateKnownQueryParams(queryParams, tt.structs...)
			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.errMsg)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestQueryTagsFor(t *testing.T) {
	type withTags struct {
		A string `query:"alpha"`
		B string `query:"beta"`
		C string
	}
	type empty struct{}

	tags := QueryTagsFor(withTags{})
	assert.ElementsMatch(t, []string{"alpha", "beta"}, tags)

	// calling again should return cached result
	tags2 := QueryTagsFor(withTags{})
	assert.ElementsMatch(t, tags, tags2)

	// struct with no query tags
	assert.Empty(t, QueryTagsFor(empty{}))

	// pointer to struct
	tags3 := QueryTagsFor(&withTags{})
	assert.ElementsMatch(t, []string{"alpha", "beta"}, tags3)
}

// TestGetFlowUUIDPtr locks down the helper used to translate an optional API
// ID string into Flow's proto UUID wrapper. nil and empty must both round-
// trip to a nil wrapper so the proto request gets no UUID field and Flow
// treats the value as unset (e.g. falls back to default rule resolution for
// ruleId callers).
func TestGetFlowUUIDPtr(t *testing.T) {
	t.Run("nil pointer returns nil", func(t *testing.T) {
		assert.Nil(t, GetFlowUUIDPtr(nil))
	})
	t.Run("empty string returns nil", func(t *testing.T) {
		s := ""
		assert.Nil(t, GetFlowUUIDPtr(&s))
	})
	t.Run("non-empty string wraps into proto UUID", func(t *testing.T) {
		s := "550e8400-e29b-41d4-a716-446655440000"
		got := GetFlowUUIDPtr(&s)
		if assert.NotNil(t, got) {
			assert.Equal(t, s, got.GetId())
		}
	})
}

func TestEvaluateInfiniBandRequestAgainstMachineCaps(t *testing.T) {
	deviceType := cdbm.MachineCapabilityDeviceType("")
	machineIbCaps := []cdbm.MachineCapability{
		{
			Type:            cdbm.MachineCapabilityTypeInfiniBand,
			Name:            "MT28908 Family [ConnectX-6]",
			Vendor:          cutil.GetPtr("Mellanox Technologies"),
			Count:           cutil.GetPtr(3),
			DeviceType:      &deviceType,
			InactiveDevices: []int{1, 3},
		},
	}

	t.Run("builds validation errors from suggested device instances", func(t *testing.T) {
		req := cam.APIInstanceCreateRequest{
			InfiniBandInterfaces: []cam.APIInfiniBandInterfaceCreateOrUpdateRequest{
				{Device: "MT28908 Family [ConnectX-6]", DeviceInstance: 1, IsPhysical: true},
			},
		}
		match := req.ValidateInfiniBandRequestForMachineCapability(machineIbCaps)
		assert.False(t, match.Satisfied)
		assert.True(t, match.CountSatisfiable)

		selErr := &InfiniBandMachineSelectionError{SuggestedByDevice: match.SuggestedByDevice}
		errs := selErr.ValidationError()
		require.Len(t, errs, 1)
		assert.Contains(t, errs, "infiniBandInterfaces")
		errMsg := errs["infiniBandInterfaces"].Error()
		assert.Contains(t, errMsg, "requested device instances are not available on any Machine for this Instance Type")
		assert.Contains(t, errMsg, "Use deviceInstances: [0 2] for device: MT28908 Family [ConnectX-6]")
		assert.Equal(t, []int{0, 2}, match.SuggestedByDevice["MT28908 Family [ConnectX-6]"])
	})
}

func TestTenantHasTargetedInstanceCreation(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	org := "test-priv-org"
	user := testCommonBuildUser(t, dbSession, uuid.NewString(), []string{org}, []string{authz.ProviderAdminRole})
	ip := testCommonBuildInfrastructureProvider(t, dbSession, "Test Provider", org, user)
	ip2 := testCommonBuildInfrastructureProvider(t, dbSession, "Test Provider 2", org+"-2", user)

	// A Site under ip is required so effective capability resolution can report
	// a Ready TenantAccount's global default as an enabled Site. The coarse and
	// provider-scoped ceilings report privileged only when at least one Site
	// resolves to an enabled effective capability.
	ipSite := testCommonBuildSite(t, dbSession, ip, "Priv Site", user)

	tnDAO := cdbm.NewTenantDAO(dbSession)

	// Tenant with a Ready TenantAccount that enables TargetedInstanceCreation.
	enabledTenant, err := tnDAO.Create(ctx, nil, cdbm.TenantCreateInput{
		Name:      "enabled-tenant",
		Org:       org + "-enabled",
		CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	// Tenant whose only Ready TenantAccount leaves the capability disabled.
	disabledTenant, err := tnDAO.Create(ctx, nil, cdbm.TenantCreateInput{
		Name:      "disabled-tenant",
		Org:       org + "-disabled",
		CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	// Tenant with the capability enabled but only on a non-Ready TenantAccount.
	pendingTenant, err := tnDAO.Create(ctx, nil, cdbm.TenantCreateInput{
		Name:      "pending-tenant",
		Org:       org + "-pending",
		CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	taDAO := cdbm.NewTenantAccountDAO(dbSession)
	_, err = taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             uuid.NewString(),
		TenantID:                  &enabledTenant.ID,
		TenantOrg:                 enabledTenant.Org,
		InfrastructureProviderID:  ip.ID,
		InfrastructureProviderOrg: ip.Org,
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
		CreatedBy:                 user.ID,
	})
	assert.Nil(t, err)
	// A second Ready account (different provider) with the capability disabled
	// must not mask the enabled one.
	_, err = taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             uuid.NewString(),
		TenantID:                  &enabledTenant.ID,
		TenantOrg:                 enabledTenant.Org,
		InfrastructureProviderID:  ip2.ID,
		InfrastructureProviderOrg: ip2.Org,
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: false},
		CreatedBy:                 user.ID,
	})
	assert.Nil(t, err)

	_, err = taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             uuid.NewString(),
		TenantID:                  &disabledTenant.ID,
		TenantOrg:                 disabledTenant.Org,
		InfrastructureProviderID:  ip.ID,
		InfrastructureProviderOrg: ip.Org,
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: false},
		CreatedBy:                 user.ID,
	})
	assert.Nil(t, err)

	_, err = taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             uuid.NewString(),
		TenantID:                  &pendingTenant.ID,
		TenantOrg:                 pendingTenant.Org,
		InfrastructureProviderID:  ip.ID,
		InfrastructureProviderOrg: ip.Org,
		Status:                    cdbm.TenantAccountStatusPending,
		Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
		CreatedBy:                 user.ID,
	})
	assert.Nil(t, err)

	// A Ready account at a different Provider ensures the list helper cannot
	// treat any Ready account as sufficient for an override at ip.
	_, err = taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             uuid.NewString(),
		TenantID:                  &pendingTenant.ID,
		TenantOrg:                 pendingTenant.Org,
		InfrastructureProviderID:  ip2.ID,
		InfrastructureProviderOrg: ip2.Org,
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: false},
		CreatedBy:                 user.ID,
	})
	assert.Nil(t, err)

	cdbm.TestBuildTenantSite(t, dbSession, pendingTenant, ipSite, &cdbm.TenantSiteConfig{
		TargetedInstanceCreation: cutil.GetPtr(true),
	}, user)

	tests := []struct {
		name     string
		tenant   *cdbm.Tenant
		scope    *TenantPrivilegeScope
		expected bool
		wantErr  bool
	}{
		{name: "nil tenant", tenant: nil, scope: nil, expected: false},
		{name: "nil scope requires explicit scope", tenant: enabledTenant, scope: nil, expected: false, wantErr: true},
		{name: "enabled via Ready TenantAccount", tenant: enabledTenant, scope: &TenantPrivilegeScope{InfrastructureProviderID: &ip.ID}, expected: true},
		{name: "disabled TenantAccount config", tenant: disabledTenant, scope: &TenantPrivilegeScope{InfrastructureProviderID: &ip.ID}, expected: false},
		{name: "enabled only on non-Ready TenantAccount", tenant: pendingTenant, scope: &TenantPrivilegeScope{InfrastructureProviderID: &ip.ID}, expected: false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, gerr := TenantHasTargetedInstanceCreation(ctx, nil, dbSession, tc.tenant, tc.scope)
			if tc.wantErr {
				assert.Error(t, gerr)
			} else {
				assert.Nil(t, gerr)
			}
			assert.Equal(t, tc.expected, got)
		})
	}

	t.Run("Site override requires Ready TenantAccount at the Site Provider", func(t *testing.T) {
		got, gerr := TenantHasTargetedInstanceCreation(ctx, nil, dbSession, pendingTenant, &TenantPrivilegeScope{SiteID: &ipSite.ID})
		assert.Nil(t, gerr)
		assert.False(t, got)

		privilegedSiteIDs, gerr := GetPrivilegedAccessSiteIDsForTenant(ctx, nil, dbSession, pendingTenant)
		assert.Nil(t, gerr)
		assert.NotContains(t, privilegedSiteIDs, ipSite.ID)
	})

	effOrg := "test-eff-org"
	effUser := testCommonBuildUser(t, dbSession, uuid.NewString(), []string{effOrg}, []string{authz.ProviderAdminRole})
	effIP := testCommonBuildInfrastructureProvider(t, dbSession, "Test Provider", effOrg, effUser)
	site := testCommonBuildSite(t, dbSession, effIP, "Test Site", effUser)
	site2 := testCommonBuildSite(t, dbSession, effIP, "Test Site 2", effUser)
	site3 := testCommonBuildSite(t, dbSession, effIP, "Test Site 3", effUser)

	tenant, err := tnDAO.Create(ctx, nil, cdbm.TenantCreateInput{
		Name:      "tenant",
		Org:       effOrg + "-tenant",
		CreatedBy: effUser.ID,
	})
	assert.Nil(t, err)

	_, err = taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             uuid.NewString(),
		TenantID:                  &tenant.ID,
		TenantOrg:                 tenant.Org,
		InfrastructureProviderID:  effIP.ID,
		InfrastructureProviderOrg: effIP.Org,
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
		CreatedBy:                 effUser.ID,
	})
	assert.Nil(t, err)

	cdbm.TestBuildTenantSite(t, dbSession, tenant, site, nil, effUser)
	ts2 := cdbm.TestBuildTenantSite(t, dbSession, tenant, site2, &cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)}, effUser)
	cdbm.TestBuildTenantSite(t, dbSession, disabledTenant, site, nil, effUser)

	siteTests := []struct {
		name     string
		tenant   *cdbm.Tenant
		site     *cdbm.Site
		expected bool
	}{
		{name: "TenantSite without explicit override inherits enabled account default", tenant: tenant, site: site, expected: true},
		{name: "explicit override disables site", tenant: tenant, site: site2, expected: false},
		{name: "enabled account default when no TenantSite exists", tenant: tenant, site: site3, expected: true},
		{name: "TenantSite without explicit override inherits disabled account default", tenant: disabledTenant, site: site, expected: false},
	}

	for _, tc := range siteTests {
		t.Run(tc.name, func(t *testing.T) {
			got, gerr := TenantHasTargetedInstanceCreation(ctx, nil, dbSession, tc.tenant, &TenantPrivilegeScope{SiteID: &tc.site.ID})
			assert.Nil(t, gerr)
			assert.Equal(t, tc.expected, got)
		})
	}

	privilegedSiteIDs, gerr := GetPrivilegedAccessSiteIDsForTenant(ctx, nil, dbSession, tenant)
	assert.Nil(t, gerr)
	assert.ElementsMatch(t, []uuid.UUID{site.ID, site3.ID}, privilegedSiteIDs)

	_ = ts2

	// A Tenant whose Ready TenantAccount global default is disabled but which
	// has a per-site override enabling the capability must pass a Site-scoped
	// check for that Site. Provider-scoped checks still follow the account
	// global default and remain false.
	overrideTenant, err := tnDAO.Create(ctx, nil, cdbm.TenantCreateInput{
		Name:      "override-tenant",
		Org:       effOrg + "-override-tenant",
		CreatedBy: effUser.ID,
	})
	assert.Nil(t, err)

	_, err = taDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
		AccountNumber:             uuid.NewString(),
		TenantID:                  &overrideTenant.ID,
		TenantOrg:                 overrideTenant.Org,
		InfrastructureProviderID:  effIP.ID,
		InfrastructureProviderOrg: effIP.Org,
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: false},
		CreatedBy:                 effUser.ID,
	})
	assert.Nil(t, err)

	// Provider-scoped check is false before any enabling override exists.
	got, gerr := TenantHasTargetedInstanceCreation(ctx, nil, dbSession, overrideTenant, &TenantPrivilegeScope{InfrastructureProviderID: &effIP.ID})
	assert.Nil(t, gerr)
	assert.False(t, got)

	cdbm.TestBuildTenantSite(t, dbSession, overrideTenant, site, &cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)}, effUser)

	got, gerr = TenantHasTargetedInstanceCreation(ctx, nil, dbSession, overrideTenant, &TenantPrivilegeScope{SiteID: &site.ID})
	assert.Nil(t, gerr)
	assert.True(t, got)

	got, gerr = TenantHasTargetedInstanceCreation(ctx, nil, dbSession, overrideTenant, &TenantPrivilegeScope{InfrastructureProviderID: &effIP.ID})
	assert.Nil(t, gerr)
	assert.False(t, got)

	// Provider-scoped ceiling: enabled on ip but not on ip2.
	got, gerr = TenantHasTargetedInstanceCreation(ctx, nil, dbSession, enabledTenant, &TenantPrivilegeScope{InfrastructureProviderID: &ip.ID})
	assert.Nil(t, gerr)
	assert.True(t, got)

	got, gerr = TenantHasTargetedInstanceCreation(ctx, nil, dbSession, enabledTenant, &TenantPrivilegeScope{InfrastructureProviderID: &ip2.ID})
	assert.Nil(t, gerr)
	assert.False(t, got)

	t.Run("TenantSite without a Site relation returns an error", func(t *testing.T) {
		deletedSite := testCommonBuildSite(t, dbSession, effIP, "Deleted Site", effUser)
		cdbm.TestBuildTenantSite(t, dbSession, tenant, deletedSite, &cdbm.TenantSiteConfig{
			TargetedInstanceCreation: cutil.GetPtr(true),
		}, effUser)

		siteDAO := cdbm.NewSiteDAO(dbSession)
		derr := siteDAO.Delete(ctx, nil, deletedSite.ID)
		assert.Nil(t, derr)

		got, gerr := TenantHasTargetedInstanceCreation(ctx, nil, dbSession, tenant, &TenantPrivilegeScope{SiteID: &deletedSite.ID})
		assert.False(t, got)
		assert.EqualError(t, gerr, "failed to retrieve related Site for Tenant/Site association, DB error")
	})
}

func TestTenantHasLegacyTargetedInstanceCreation(t *testing.T) {
	ctx := context.Background()
	dbSession := testCommonInitDB(t)
	defer dbSession.Close()

	testCommonSetupSchema(t, dbSession)

	org := "test-legacy-capability-org"
	user := testCommonBuildUser(t, dbSession, uuid.NewString(), []string{org}, []string{authz.ProviderAdminRole})
	ip := testCommonBuildInfrastructureProvider(t, dbSession, "Legacy Provider", org, user)
	ip2 := testCommonBuildInfrastructureProvider(t, dbSession, "Legacy Provider 2", org+"-2", user)
	site := testCommonBuildSite(t, dbSession, ip, "Legacy Site", user)

	tenantDAO := cdbm.NewTenantDAO(dbSession)
	accountDAO := cdbm.NewTenantAccountDAO(dbSession)

	buildTenant := func(name string) *cdbm.Tenant {
		t.Helper()
		tenant, err := tenantDAO.Create(ctx, nil, cdbm.TenantCreateInput{
			Name:      name,
			Org:       org + "-" + name,
			CreatedBy: user.ID,
		})
		require.NoError(t, err)
		return tenant
	}
	buildAccount := func(tenant *cdbm.Tenant, provider *cdbm.InfrastructureProvider, status string, enabled bool) {
		t.Helper()
		_, err := accountDAO.Create(ctx, nil, cdbm.TenantAccountCreateInput{
			AccountNumber:             uuid.NewString(),
			TenantID:                  &tenant.ID,
			TenantOrg:                 tenant.Org,
			InfrastructureProviderID:  provider.ID,
			InfrastructureProviderOrg: provider.Org,
			Status:                    status,
			Config:                    &cdbm.TenantAccountConfig{TargetedInstanceCreation: enabled},
			CreatedBy:                 user.ID,
		})
		require.NoError(t, err)
	}

	allEnabledTenant := buildTenant("all-enabled")
	buildAccount(allEnabledTenant, ip, cdbm.TenantAccountStatusReady, true)
	buildAccount(allEnabledTenant, ip2, cdbm.TenantAccountStatusReady, true)

	accountDisabledTenant := buildTenant("account-disabled")
	buildAccount(accountDisabledTenant, ip, cdbm.TenantAccountStatusReady, true)
	buildAccount(accountDisabledTenant, ip2, cdbm.TenantAccountStatusReady, false)

	siteDisabledTenant := buildTenant("site-disabled")
	buildAccount(siteDisabledTenant, ip, cdbm.TenantAccountStatusReady, true)
	cdbm.TestBuildTenantSite(t, dbSession, siteDisabledTenant, site, &cdbm.TenantSiteConfig{
		TargetedInstanceCreation: cutil.GetPtr(false),
	}, user)

	pendingTenant := buildTenant("pending-only")
	buildAccount(pendingTenant, ip, cdbm.TenantAccountStatusPending, true)

	tests := []struct {
		name     string
		tenant   *cdbm.Tenant
		expected bool
	}{
		{name: "nil Tenant", tenant: nil, expected: false},
		{name: "all Ready TenantAccounts enabled", tenant: allEnabledTenant, expected: true},
		{name: "one Ready TenantAccount disabled", tenant: accountDisabledTenant, expected: false},
		{name: "TenantSite explicitly disabled", tenant: siteDisabledTenant, expected: false},
		{name: "no Ready TenantAccount", tenant: pendingTenant, expected: false},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := TenantHasLegacyTargetedInstanceCreation(ctx, nil, dbSession, tc.tenant)
			require.NoError(t, err)
			assert.Equal(t, tc.expected, got)
		})
	}
}
