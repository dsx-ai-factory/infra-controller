// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdmu "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	sutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"
	oteltrace "go.opentelemetry.io/otel/trace"
	"go.temporal.io/api/enums/v1"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"gopkg.in/yaml.v3"
)

func assertDeletionAcceptedResponse(t *testing.T, body []byte) {
	t.Helper()

	var resp model.APIMessageResponse
	require.NoError(t, json.Unmarshal(body, &resp))
	assert.Equal(t, model.DeletionRequestAcceptedMessage, resp.Message)
}

type phoneHomeLocation string

const (
	phoneHomeAbsent                 phoneHomeLocation = "absent"
	phoneHomeAtRoot                 phoneHomeLocation = "root"
	phoneHomeInAutoinstallUserData  phoneHomeLocation = "autoinstall.user-data"
	phoneHomeAtRootAndInAutoinstall phoneHomeLocation = "root and autoinstall.user-data"
)

func phoneHomeLocationIn(userData map[string]interface{}) phoneHomeLocation {
	_, atRoot := userData[util.SitePhoneHomeName]
	inAutoinstall := false
	autoinstall, ok := userData["autoinstall"].(map[string]interface{})
	if ok {
		targetUserData, ok := autoinstall["user-data"].(map[string]interface{})
		if ok {
			_, inAutoinstall = targetUserData[util.SitePhoneHomeName]
		}
	}

	switch {
	case atRoot && inAutoinstall:
		return phoneHomeAtRootAndInAutoinstall
	case atRoot:
		return phoneHomeAtRoot
	case inAutoinstall:
		return phoneHomeInAutoinstallUserData
	default:
		return phoneHomeAbsent
	}
}

func expectedPhoneHomeLocation(userData map[string]interface{}) phoneHomeLocation {
	if _, ok := userData["autoinstall"]; ok {
		return phoneHomeInAutoinstallUserData
	}
	return phoneHomeAtRoot
}

func testInstanceInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// reset the tables needed for Allocation tests
func testInstanceSetupSchema(t *testing.T, dbSession *cdb.Session) {
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
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Domain)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create AllocationConstraint table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.AllocationConstraint)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create NVLink Logical Partition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Subnet)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Machine)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineInstanceType)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.OperatingSystem)(nil))
	assert.Nil(t, err)
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandPartition)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InstanceType)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Interface)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Instance)(nil))
	assert.Nil(t, err)
	// create SSHKeyGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroup)(nil))
	assert.Nil(t, err)
	// create SSHKeyGroupSiteAssociation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupSiteAssociation)(nil))
	assert.Nil(t, err)
	// create SSHKeyGroupInstanceAssociation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyGroupInstanceAssociation)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Interface)(nil))
	assert.Nil(t, err)
	// create InfiniBandInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandInterface)(nil))
	assert.Nil(t, err)
	// create NVLink Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkInterface)(nil))
	assert.Nil(t, err)
	// create NetworkSecurityGroup table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
}

func testInstanceSiteBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)

	ip, err := ipDAO.Create(context.Background(), nil, cdbm.InfrastructureProviderCreateInput{
		Name:        name,
		DisplayName: cutil.GetPtr("Test Infrastructure Provider"),
		Org:         org,
		CreatedBy:   user.ID,
	})
	assert.Nil(t, err)

	return ip
}

func testInstanceBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, status string, isInfinityEnabled bool, user *cdbm.User) *cdbm.Site {
	stDAO := cdbm.NewSiteDAO(dbSession)

	st, err := stDAO.Create(context.Background(), nil, cdbm.SiteCreateInput{
		Name:                          name,
		DisplayName:                   cutil.GetPtr("Test Site"),
		Description:                   cutil.GetPtr("Test Site Description"),
		Org:                           ip.Org,
		InfrastructureProviderID:      ip.ID,
		SiteControllerVersion:         cutil.GetPtr("V1-T1761856992374052"),
		SiteAgentVersion:              cutil.GetPtr("V1-T1761856992374052"),
		RegistrationToken:             cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration:   cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:             isInfinityEnabled,
		SerialConsoleHostname:         cutil.GetPtr("TestSshHostname"),
		IsSerialConsoleEnabled:        true,
		SerialConsoleIdleTimeout:      cutil.GetPtr(30),
		SerialConsoleMaxSessionLength: cutil.GetPtr(60),
		Status:                        status,
		CreatedBy:                     user.ID,
	})
	assert.Nil(t, err)

	return st
}

func testInstanceBuildTenant(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.Tenant {
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
func testInstanceBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, org string, roles []string) *cdbm.User {
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

func testInstanceSiteBuildAllocation(t *testing.T, dbSession *cdb.Session, st *cdbm.Site, tn *cdbm.Tenant, name string, user *cdbm.User) *cdbm.Allocation {
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

func testInstanceSiteBuildAllocationContraints(t *testing.T, dbSession *cdb.Session, al *cdbm.Allocation, rt string, rtID uuid.UUID, ct string, cv int, user *cdbm.User) *cdbm.AllocationConstraint {
	alctDAO := cdbm.NewAllocationConstraintDAO(dbSession)

	alct, err := alctDAO.Create(context.Background(), nil, cdbm.AllocationConstraintCreateInput{
		AllocationID: al.ID, ResourceType: rt, ResourceTypeID: rtID,
		ConstraintType: ct, ConstraintValue: cv, CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	return alct
}

func testInstanceBuildVPC(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, tn *cdbm.Tenant, st *cdbm.Site, cvID *uuid.UUID, labels map[string]string, networkVTtype *string, defaultNVLinkLogicalPartitionID *uuid.UUID, status string, user *cdbm.User) *cdbm.Vpc {
	vpcDAO := cdbm.NewVpcDAO(dbSession)

	input := cdbm.VpcCreateInput{
		Name:                      name,
		Description:               cutil.GetPtr("Test Vpc"),
		Org:                       st.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: networkVTtype,
		ControllerVpcID:           cvID,
		Labels:                    labels,
		NVLinkLogicalPartitionID:  defaultNVLinkLogicalPartitionID,
		Status:                    status,
		CreatedBy:                 *user,
	}

	vpc, err := vpcDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

func testInstanceBuildSubnet(t *testing.T, dbSession *cdb.Session, name string, tn *cdbm.Tenant, vpc *cdbm.Vpc, cnsID *uuid.UUID, status string, user *cdbm.User) *cdbm.Subnet {
	subnetDAO := cdbm.NewSubnetDAO(dbSession)
	ipv4Prefix := fmt.Sprintf("10.%d.0.0/24", (int(name[0])+len(name))%200+1)

	subnet, err := subnetDAO.Create(context.Background(), nil, cdbm.SubnetCreateInput{
		Name:                       name,
		Description:                cutil.GetPtr("Test Subnet"),
		Org:                        tn.Org,
		SiteID:                     vpc.SiteID,
		VpcID:                      vpc.ID,
		TenantID:                   tn.ID,
		ControllerNetworkSegmentID: cnsID,
		IPv4Prefix:                 &ipv4Prefix,
		PrefixLength:               24,
		Status:                     status,
		CreatedBy:                  user.ID,
	})
	assert.Nil(t, err)

	return subnet
}

func testInstanceBuildInstanceType(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, site *cdbm.Site, status string) *cdbm.InstanceType {
	instanceType := &cdbm.InstanceType{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		SiteID:                   cutil.GetPtr(site.ID),
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(instanceType).Exec(context.Background())
	assert.Nil(t, err)
	return instanceType
}

func testInstanceBuildOperatingSystem(t *testing.T, dbSession *cdb.Session, name string, tn *cdbm.Tenant, osType string, allowOverride bool, userData *string, phoneHomeEnabled bool, status string, user *cdbm.User) *cdbm.OperatingSystem {
	operatingSystem := &cdbm.OperatingSystem{
		ID:               uuid.New(),
		Name:             name,
		TenantID:         cutil.GetPtr(tn.ID),
		Type:             osType,
		AllowOverride:    allowOverride,
		PhoneHomeEnabled: phoneHomeEnabled,
		IsActive:         true,
		UserData:         userData,
		Status:           status,
		CreatedBy:        user.ID,
	}

	// If iPXE, the OS should have a script set.
	if osType == cdbm.OperatingSystemTypeIPXE {
		operatingSystem.IpxeScript = cutil.GetPtr(common.DefaultIpxeScript)
	}

	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

func testInstanceBuildOperatingSystemSiteAssociation(t *testing.T, dbSession *cdb.Session, siteID, osID uuid.UUID) *cdbm.OperatingSystemSiteAssociation {
	ossa := &cdbm.OperatingSystemSiteAssociation{
		ID:                uuid.New(),
		OperatingSystemID: osID,
		SiteID:            siteID,
		Version:           cutil.GetPtr("1234"),
		Status:            cdbm.OperatingSystemSiteAssociationStatusSynced,
		Created:           cdb.GetCurTime(),
		Updated:           cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(ossa).Exec(context.Background())
	assert.Nil(t, err)
	return ossa
}

func testInstanceBuildMachineInstanceType(t *testing.T, dbSession *cdb.Session, mc *cdbm.Machine, in *cdbm.InstanceType) *cdbm.MachineInstanceType {
	mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)

	mit, err := mitDAO.Create(context.Background(), nil, cdbm.MachineInstanceTypeCreateInput{
		MachineID:      mc.ID,
		InstanceTypeID: in.ID,
	})
	assert.Nil(t, err)

	mDAO := cdbm.NewMachineDAO(dbSession)
	_, _ = mDAO.Update(context.Background(), nil, cdbm.MachineUpdateInput{MachineID: mc.ID, InstanceTypeID: &in.ID})

	return mit
}

func testInstanceBuildMachine(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, isassigned *bool, controllerMachineType *string) *cdbm.Machine {
	return testInstanceBuildMachineWithID(t, dbSession, ip, site, isassigned, controllerMachineType, "fm"+uuid.NewString())
}

func testInstanceBuildMachineWithID(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, isassigned *bool, controllerMachineType *string, mid string) *cdbm.Machine {
	m := &cdbm.Machine{
		ID:                       mid,
		InfrastructureProviderID: ip,
		SiteID:                   site,
		ControllerMachineID:      mid,
		ControllerMachineType:    controllerMachineType,
		Metadata:                 nil,
		IsAssigned:               *isassigned,
		DefaultMacAddress:        cutil.GetPtr("00:1B:44:11:3A:B7"),
		Status:                   cdbm.MachineStatusReady,
	}
	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func testInstanceBuildMachineCapability(t *testing.T, dbSession *cdb.Session, mID *string, capabilityType cdbm.MachineCapabilityType, name string, capacity *string, count *int) *cdbm.MachineCapability {
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

func testInstanceBuildMachineInterface(t *testing.T, dbSession *cdb.Session, subID uuid.UUID, mID string) *cdbm.MachineInterface {
	mi := &cdbm.MachineInterface{
		ID:                    uuid.New(),
		MachineID:             mID,
		ControllerInterfaceID: cutil.GetPtr(uuid.New()),
		ControllerSegmentID:   cutil.GetPtr(uuid.New()),
		Hostname:              cutil.GetPtr("test.com"),
		IsPrimary:             true,
		SubnetID:              cutil.GetPtr(subID),
		MacAddress:            cutil.GetPtr("00:00:00:00:00:00"),
		IPAddresses:           []string{"192.168.0.1, 172.168.0.1"},
		Created:               cdb.GetCurTime(),
		Updated:               cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(mi).Exec(context.Background())
	assert.Nil(t, err)
	return mi
}

// testInstanceBuildInstance creates a persisted instance fixture without any
// allocation linkage. Tests that need allocation state must create it explicitly.
func testInstanceBuildInstance(t *testing.T, dbSession *cdb.Session, name string, tn uuid.UUID, ip uuid.UUID, st uuid.UUID, ist *uuid.UUID, vpc uuid.UUID, mc *string, os *uuid.UUID, ipxeScript *string, status string) *cdbm.Instance {
	insID := uuid.New()
	ins := &cdbm.Instance{
		ID:                       insID,
		Name:                     name,
		TenantID:                 tn,
		InfrastructureProviderID: ip,
		SiteID:                   st,
		InstanceTypeID:           ist,
		VpcID:                    vpc,
		MachineID:                mc,
		OperatingSystemID:        os,
		ControllerInstanceID:     &insID,
		Hostname:                 nil,
		UserData:                 nil,
		IpxeScript:               ipxeScript,
		AlwaysBootWithCustomIpxe: false,
		PhoneHomeEnabled:         false,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(ins).Exec(context.Background())
	assert.Nil(t, err)
	return ins
}

func testInstanceBuildVPCPrefix(t *testing.T, dbSession *cdb.Session, name string, tn *cdbm.Tenant, vpc *cdbm.Vpc, ipbID *uuid.UUID, prefix string, prefixLength int, status string, user *cdbm.User) *cdbm.VpcPrefix {
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)

	vpcPrefix, err := vpcPrefixDAO.Create(context.Background(), nil, cdbm.VpcPrefixCreateInput{
		Name:         name,
		TenantOrg:    tn.Org,
		SiteID:       vpc.SiteID,
		VpcID:        vpc.ID,
		TenantID:     tn.ID,
		IpBlockID:    ipbID,
		Prefix:       prefix,
		PrefixLength: prefixLength,
		Status:       status,
		CreatedBy:    user.ID,
	})
	assert.Nil(t, err)
	return vpcPrefix
}

func testInstanceBuildIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, routingType string, prefix string, prefixLength int, protocolVersion string, status string, user *cdbm.User) *cdbm.IPBlock {
	ipbDAO := cdbm.NewIPBlockDAO(dbSession)
	ipb, err := ipbDAO.Create(context.Background(), nil, cdbm.IPBlockCreateInput{
		Name:                     name,
		SiteID:                   site.ID,
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenantID,
		RoutingType:              routingType,
		Prefix:                   prefix,
		PrefixLength:             prefixLength,
		ProtocolVersion:          protocolVersion,
		Status:                   status,
		CreatedBy:                &user.ID,
	})
	assert.Nil(t, err)
	return ipb
}

func testInstanceBuildInterface(t *testing.T, dbSession *cdb.Session, instanceID uuid.UUID, subnetID *uuid.UUID, vpcPrefixID *uuid.UUID, device *string, deviceInstance *int, virtualFunctionID *int, isPhysical bool, status string, user *cdbm.User) *cdbm.Interface {
	interfaceDAO := cdbm.NewInterfaceDAO(dbSession)
	ifc, err := interfaceDAO.Create(context.Background(), nil, cdbm.InterfaceCreateInput{
		InstanceID:        instanceID,
		SubnetID:          subnetID,
		VpcPrefixID:       vpcPrefixID,
		Device:            device,
		DeviceInstance:    deviceInstance,
		VirtualFunctionID: virtualFunctionID,
		IsPhysical:        isPhysical,
		Status:            status,
		CreatedBy:         user.ID,
	})
	assert.Nil(t, err)
	return ifc
}

func testUpdateInstance(t *testing.T, dbSession *cdb.Session, ins *cdbm.Instance) *cdbm.Instance {
	_, err := dbSession.DB.NewUpdate().Where("id = ?", ins.ID).Model(ins).Exec(context.Background())
	assert.Nil(t, err)
	return ins
}

func testUpdateInterfaceWithIPs(t *testing.T, dbSession *cdb.Session, ifc *cdbm.Interface, ipAddresses []string) *cdbm.Interface {
	ifc.IPAddresses = ipAddresses
	_, err := dbSession.DB.NewUpdate().Where("id = ?", ifc.ID).Model(ifc).Exec(context.Background())
	assert.Nil(t, err)
	return ifc
}

type ethernetReconciliationExpectation struct {
	rowCount        int
	readyIDs        []uuid.UUID
	deletingIDs     []uuid.UUID
	pendingCount    int
	uniqueIPAddress *string
	usagePrefix     *cdbm.VpcPrefix
}

func testUpdateMachineToUnhealthy(t *testing.T, dbSession *cdb.Session, m *cdbm.Machine) *cdbm.Machine {
	m.Status = cdbm.MachineStatusError
	_, err := dbSession.DB.NewUpdate().Where("id = ?", m.ID).Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func testUpdateMachineToMissing(t *testing.T, dbSession *cdb.Session, m *cdbm.Machine) *cdbm.Machine {
	m.IsMissingOnSite = true
	_, err := dbSession.DB.NewUpdate().Where("id = ?", m.ID).Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

// testUpdateMachineStatusAndControllerState sets DB status and site-controller machine state (metadata) for provisioning tests.
func testUpdateMachineStatusAndControllerState(t *testing.T, dbSession *cdb.Session, m *cdbm.Machine, status string, controllerState string) *cdbm.Machine {
	m.Status = status
	if controllerState != "" {
		m.Metadata = &cdbm.SiteControllerMachine{Machine: &corev1.Machine{State: controllerState}}
	} else {
		m.Metadata = nil
	}
	_, err := dbSession.DB.NewUpdate().Where("id = ?", m.ID).Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func testInstanceBuildInstanceInterface(t *testing.T, dbSession *cdb.Session, in uuid.UUID, sub *uuid.UUID, vp *uuid.UUID, mi *uuid.UUID, status string) *cdbm.Interface {
	ifc := &cdbm.Interface{
		ID:                 uuid.New(),
		InstanceID:         in,
		SubnetID:           sub,
		VpcPrefixID:        vp,
		MachineInterfaceID: mi,
		Status:             status,
		Created:            cdb.GetCurTime(),
		Updated:            cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(ifc).Exec(context.Background())
	assert.Nil(t, err)
	return ifc
}

func testInstanceBuildInstanceNVLinkInterface(t *testing.T, dbSession *cdb.Session, siteID uuid.UUID, in uuid.UUID, nvLinkLogicalID uuid.UUID, nvLinkDomainID *uuid.UUID, device *string, deviceInstance int, status string) *cdbm.NVLinkInterface {
	nvlifc := &cdbm.NVLinkInterface{
		ID:                       uuid.New(),
		InstanceID:               in,
		SiteID:                   siteID,
		NVLinkLogicalPartitionID: nvLinkLogicalID,
		Device:                   device,
		DeviceInstance:           deviceInstance,
		NVLinkDomainID:           nvLinkDomainID,
		Status:                   status,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(nvlifc).Exec(context.Background())
	assert.Nil(t, err)
	return nvlifc
}

func testInstanceBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID uuid.UUID, status string) {
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	ssd, err := sdDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID.String(), Status: status, Message: nil})
	assert.Nil(t, err)
	assert.NotNil(t, ssd)
	assert.Equal(t, entityID.String(), ssd.EntityID)
	assert.Equal(t, status, ssd.Status)
}

func testInstanceBuildSSHKeyGroupInstanceAssociation(t *testing.T, dbSession *cdb.Session, skgID uuid.UUID, siteID uuid.UUID, instanceID uuid.UUID) *cdbm.SSHKeyGroupInstanceAssociation {
	skgia := &cdbm.SSHKeyGroupInstanceAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: skgID,
		SiteID:        siteID,
		InstanceID:    instanceID,
		Created:       cdb.GetCurTime(),
		Updated:       cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(skgia).Exec(context.Background())
	assert.Nil(t, err)
	return skgia
}

func testUpdateDESActiveVersions(t *testing.T, dbSession *cdb.Session, des *cdbm.DpuExtensionService) {
	desDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)
	_, err := desDAO.Update(context.Background(), nil, cdbm.DpuExtensionServiceUpdateInput{
		DpuExtensionServiceID: des.ID,
		ActiveVersions:        des.ActiveVersions,
	})
	assert.Nil(t, err)
}

func testInstanceBuildIBInterface(t *testing.T, dbSession *cdb.Session, instance *cdbm.Instance, site *cdbm.Site, ibPartition *cdbm.InfiniBandPartition, deviceInstance int, isPhysical bool, vfID *int, status *string, isMissingOnSite bool) *cdbm.InfiniBandInterface {
	if status == nil {
		status = cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady)
	}
	ibi := &cdbm.InfiniBandInterface{
		ID:                    uuid.New(),
		InstanceID:            instance.ID,
		SiteID:                site.ID,
		InfiniBandPartitionID: ibPartition.ID,
		Device:                "MT28908 Family [ConnectX-6]",
		Vendor:                cutil.GetPtr("Mellanox Technologies"),
		DeviceInstance:        deviceInstance,
		IsPhysical:            isPhysical,
		VirtualFunctionID:     vfID,
		Status:                *status,
		IsMissingOnSite:       isMissingOnSite,
	}
	_, err := dbSession.DB.NewInsert().Model(ibi).Exec(context.Background())
	assert.Nil(t, err)
	return ibi
}

func testUpdateOSIsActive(t *testing.T, dbSession *cdb.Session, ins *cdbm.OperatingSystem) *cdbm.OperatingSystem {
	_, err := dbSession.DB.NewUpdate().Where("id = ?", ins.ID).Model(ins).Exec(context.Background())
	assert.Nil(t, err)
	return ins
}

// assertInterfaceRoutingProfilePrefixes verifies proto routing profile prefix order.
func assertInterfaceRoutingProfilePrefixes(t *testing.T, actual *corev1.InstanceInterfaceRoutingProfile, expected []string) {
	t.Helper()
	require.NotNil(t, actual)
	require.Len(t, actual.AllowedAnycastPrefixes, len(expected))
	for i, prefix := range expected {
		assert.Equal(t, prefix, actual.AllowedAnycastPrefixes[i].Prefix)
	}
}

// assertInterfaceVpcSelection verifies that an Interface carries the expected
// Controller-managed VPC selection intent.
func assertInterfaceVpcSelection(t *testing.T, actual *corev1.InstanceInterfaceConfig, controllerVpcID uuid.UUID, familyMode cdbm.InterfaceVpcIPFamilyMode) {
	t.Helper()

	require.NotNil(t, actual)
	selection, ok := actual.NetworkDetails.(*corev1.InstanceInterfaceConfig_Vpc)
	require.True(t, ok)
	require.NotNil(t, selection.Vpc)
	require.NotNil(t, selection.Vpc.VpcId)
	assert.Equal(t, controllerVpcID.String(), selection.Vpc.VpcId.Value)

	var expectedFamilyMode corev1.InstanceInterfaceIpFamilyMode
	switch familyMode {
	case cdbm.InterfaceVpcIPFamilyModeIPv4Only:
		expectedFamilyMode = corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_IPV4_ONLY
	case cdbm.InterfaceVpcIPFamilyModeIPv6Only:
		expectedFamilyMode = corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_IPV6_ONLY
	case cdbm.InterfaceVpcIPFamilyModeDualStack:
		expectedFamilyMode = corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_DUAL_STACK
	default:
		require.FailNow(t, "unsupported VPC IP family mode", "mode: %s", familyMode)
	}
	assert.Equal(t, expectedFamilyMode, selection.Vpc.FamilyMode)
}

func TestBuildInstanceNetworkConfig(t *testing.T) {
	controllerVpcID := uuid.New()
	interfaceConfigs := []*corev1.InstanceInterfaceConfig{
		{
			NetworkSegmentId: &corev1.NetworkSegmentId{Value: uuid.NewString()},
		},
	}

	explicitNetwork := buildInstanceNetworkConfig(false, interfaceConfigs, nil)
	require.False(t, explicitNetwork.Auto)
	assert.Equal(t, interfaceConfigs, explicitNetwork.Interfaces)
	assert.Nil(t, explicitNetwork.AutoConfig)

	autoNetwork := buildInstanceNetworkConfig(true, interfaceConfigs, &controllerVpcID)
	require.True(t, autoNetwork.Auto)
	assert.Empty(t, autoNetwork.Interfaces)
	require.NotNil(t, autoNetwork.AutoConfig)
	require.NotNil(t, autoNetwork.AutoConfig.VpcId)
	assert.Equal(t, controllerVpcID.String(), autoNetwork.AutoConfig.VpcId.Value)
}

func TestCreateInstanceHandler_Handle(t *testing.T) {

	ctx := context.Background()

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg := "test-tenant-org-1"
	tnOrgRoles := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	tnOrg3 := "test-tenant-org-3"
	tnOrgRoles3 := []string{authz.TenantAdminRole}

	tnOrg4 := "test-tenant-org-4"
	tnOrgRoles4 := []string{authz.TenantAdminRole}

	tnOrg5 := "test-tenant-org-5"
	tnOrgRoles5 := []string{authz.TenantAdminRole}

	tnOrg6 := "test-tenant-org-6"
	tnOrgRoles6 := []string{authz.TenantAdminRole}

	tnOrg7 := "test-tenant-org-7"
	tnOrgRoles7 := []string{authz.TenantAdminRole}

	tnOrg8 := "test-tenant-org-8"
	tnOrgRoles8 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)

	stNotReady := testInstanceBuildSite(t, dbSession, ip, "test-site-not-ready", cdbm.SiteStatusPending, true, ipu)
	assert.NotNil(t, stNotReady)

	// Tenant 1
	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg, tnOrgRoles)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu1)
	// Privilege is resolved site-scoped. TenantSite associations without an
	// explicit override inherit tn1's Ready TenantAccount default.
	_ = common.TestBuildTenantAccountWithTargetedInstanceCreation(t, dbSession, ip, &tn1.ID, tnOrg, cdbm.TenantAccountStatusReady, tnu1)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	// InfiniBand Interface Support
	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	// Add InfiniBand capability to Instance Type
	common.TestBuildMachineCapability(t, dbSession, nil, &ist1.ID, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-6]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(3), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)

	// Add Network DPU capability to Instance Type
	common.TestBuildMachineCapability(t, dbSession, nil, &ist1.ID, cdbm.MachineCapabilityTypeNetwork, "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil)

	// Add NVLink GPU capability to Instance Type
	mcNvlType := common.TestBuildMachineCapability(t, dbSession, nil, &ist1.ID, cdbm.MachineCapabilityTypeGPU, "NVIDIA GB200", nil, nil, cutil.GetPtr("NVIDIA"), cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil)
	assert.NotNil(t, mcNvlType)

	istnoib := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-noib", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, istnoib)
	alnoib := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-noib", ipu)
	assert.NotNil(t, alnoib)
	alcnoib := testInstanceSiteBuildAllocationContraints(t, dbSession, alnoib, cdbm.AllocationResourceTypeInstanceType, istnoib.ID, cdbm.AllocationConstraintTypeReserved, 10, ipu)
	assert.NotNil(t, alcnoib)
	mcnoib := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcnoib)
	mcinstnoib := testInstanceBuildMachineInstanceType(t, dbSession, mcnoib, istnoib)
	assert.NotNil(t, mcinstnoib)

	// machine to instantiate by idbelonging to an instance type
	istbyid := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-byid", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, istbyid)

	albyid := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-byid", ipu)
	assert.NotNil(t, albyid)
	alcbyid := testInstanceSiteBuildAllocationContraints(t, dbSession, albyid, cdbm.AllocationResourceTypeInstanceType, istbyid.ID, cdbm.AllocationConstraintTypeReserved, 10, ipu)
	assert.NotNil(t, alcbyid)
	mcbyid := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcbyid)
	mcbyid.Labels = map[string]string{"failure-domain": "fd-a"}
	_, err := cdbm.NewMachineDAO(dbSession).Update(ctx, nil, cdbm.MachineUpdateInput{
		MachineID: mcbyid.ID,
		Labels:    mcbyid.Labels,
	})
	assert.NoError(t, err)

	// Add capability to machine
	common.TestBuildMachineCapability(t, dbSession, &mcbyid.ID, nil, cdbm.MachineCapabilityTypeGPU, "NVIDIA GB200", nil, nil, cutil.GetPtr("NVIDIA"), cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil)

	mcinstbyid := testInstanceBuildMachineInstanceType(t, dbSession, mcbyid, istbyid)
	assert.NotNil(t, mcinstbyid)

	// machine not belonging to an instance type
	mcnoinst := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcnoinst)
	// machine not belonging to site 1
	mcwrongsite := testInstanceBuildMachine(t, dbSession, ip.ID, st2.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcwrongsite)
	// machine already in-use/assigned
	mcassigned := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(true), nil)
	assert.NotNil(t, mcassigned)
	// unhealthy machines
	mcunhealthy := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcunhealthy)
	testUpdateMachineToUnhealthy(t, dbSession, mcunhealthy)
	mcunhealthy2 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcunhealthy2)
	testUpdateMachineToUnhealthy(t, dbSession, mcunhealthy2)
	mcmissing := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcmissing)
	testUpdateMachineToMissing(t, dbSession, mcmissing)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 9, ipu)
	assert.NotNil(t, alc1)

	// Use dedicated allocation inventory for VPC-selection coverage so the
	// request cannot consume capacity needed by unrelated table cases.
	istVpcSelection := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-vpc-selection", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, istVpcSelection)
	alcVpcSelection := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, istVpcSelection.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	assert.NotNil(t, alcVpcSelection)
	mcVpcSelection := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcVpcSelection)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mcVpcSelection, istVpcSelection))

	// Dedicated instance type for IP-exhaustion fixtures; must not consume ist1 allocation (limit 9).
	istExhaustFixture := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-exhaust-fixture", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, istExhaustFixture)
	alcExhaustFixture := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, istExhaustFixture.ID, cdbm.AllocationConstraintTypeReserved, 30, ipu)
	assert.NotNil(t, alcExhaustFixture)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	mc12 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc12)

	mc13 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc13)

	mc14 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc14)

	mc15 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc15)

	mc16 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc16)

	mc17 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc17)

	mc18 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc18)

	mc19 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc19)

	mc20 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc20)

	mcinst12 := testInstanceBuildMachineInstanceType(t, dbSession, mc12, ist1)
	assert.NotNil(t, mcinst12)

	mcinst13 := testInstanceBuildMachineInstanceType(t, dbSession, mc13, ist1)
	assert.NotNil(t, mcinst13)

	mcinst14 := testInstanceBuildMachineInstanceType(t, dbSession, mc14, ist1)
	assert.NotNil(t, mcinst14)

	mcinst15 := testInstanceBuildMachineInstanceType(t, dbSession, mc15, ist1)
	assert.NotNil(t, mcinst15)

	mcinst16 := testInstanceBuildMachineInstanceType(t, dbSession, mc16, ist1)
	assert.NotNil(t, mcinst16)

	mcinst17 := testInstanceBuildMachineInstanceType(t, dbSession, mc17, ist1)
	assert.NotNil(t, mcinst17)

	mcinst18 := testInstanceBuildMachineInstanceType(t, dbSession, mc18, ist1)
	assert.NotNil(t, mcinst18)

	mcinst19 := testInstanceBuildMachineInstanceType(t, dbSession, mc19, ist1)
	assert.NotNil(t, mcinst19)

	mcinst20 := testInstanceBuildMachineInstanceType(t, dbSession, mc20, ist1)
	assert.NotNil(t, mcinst20)

	addIst1InfiniBandMachineCapability := func(t *testing.T, machineID string) {
		common.TestBuildMachineCapability(t, dbSession, &machineID, nil, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-6]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(3), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)
	}
	for _, mc := range []*cdbm.Machine{mc1, mc12, mc13, mc14, mc15, mc16, mc17, mc18, mc19, mc20} {
		addIst1InfiniBandMachineCapability(t, mc.ID)
	}

	// Tenant 1
	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeIPXE, false, nil, true, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	os2 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-2", tn1, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os2)
	ossa1 := testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, st2.ID, os2.ID)
	assert.NotNil(t, ossa1)

	osPhoneHome := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-phonehome", tn1, cdbm.OperatingSystemTypeIPXE, true, cutil.GetPtr(""), true, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, osPhoneHome)

	// create a default NVLink Logical Partition
	nvllpDefault := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-default", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllpDefault)

	nvllpNotDefault := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-not-default", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllpNotDefault)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), cutil.GetPtr(nvllpDefault.ID), cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc2)

	vpcPending := testInstanceBuildVPC(t, dbSession, "test-vpc-3", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpcPending)

	vpcSiteReady := testInstanceBuildVPC(t, dbSession, "test-vpc-3", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpcSiteReady)

	vpcSiteNotReady := testInstanceBuildVPC(t, dbSession, "test-vpc-3", ip, tn1, stNotReady, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpcSiteNotReady)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc2, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet2)

	subnetReady := testInstanceBuildSubnet(t, dbSession, "test-subnet-3", tn1, vpcPending, nil, cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnetReady)

	subnetSiteReady := testInstanceBuildSubnet(t, dbSession, "test-subnet-4", tn1, vpcSiteReady, nil, cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnetSiteReady)

	subnetSiteNotReady := testInstanceBuildSubnet(t, dbSession, "test-subnet-4", tn1, vpcSiteNotReady, nil, cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnetSiteNotReady)

	subnetPending := testInstanceBuildSubnet(t, dbSession, "test-subnet-5", tn1, vpcSiteReady, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnetPending)

	subnetExhaustedIPv4 := "10.99.0.0/28"
	subnetExhausted, err := cdbm.NewSubnetDAO(dbSession).Create(context.Background(), nil, cdbm.SubnetCreateInput{
		Name:                       "test-subnet-exhausted",
		Description:                cutil.GetPtr("Test Subnet exhausted"),
		Org:                        tn1.Org,
		SiteID:                     vpc1.SiteID,
		VpcID:                      vpc1.ID,
		TenantID:                   tn1.ID,
		ControllerNetworkSegmentID: cutil.GetPtr(uuid.New()),
		IPv4Prefix:                 &subnetExhaustedIPv4,
		PrefixLength:               28,
		Status:                     cdbm.SubnetStatusReady,
		CreatedBy:                  tnu1.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, subnetExhausted)
	for i := 0; i < 14; i++ {
		exhaustInst := testInstanceBuildInstance(t, dbSession, fmt.Sprintf("exhaust-subnet-inst-%d", i), tn1.ID, ip.ID, st1.ID, &istExhaustFixture.ID, vpc1.ID, nil, &os1.ID, nil, cdbm.InstanceStatusReady)
		testInstanceBuildInstanceInterface(t, dbSession, exhaustInst.ID, &subnetExhausted.ID, nil, nil, cdbm.InterfaceStatusPending)
	}

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	desd1 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, desd1)

	desd2 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-2", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, desd2)

	desd3 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-3", model.DpuExtensionServiceTypeKubernetesPod, tn1, st2, "V1-T1761856992374052", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, desd3)

	// Build SSHKeyGroup 1
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation 1
	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa1)

	// Tenant 2
	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant-3", tnOrg2, tnu2)

	al2 := testInstanceSiteBuildAllocation(t, dbSession, st2, tn2, "test-allocation-2", ipu)
	assert.NotNil(t, al2)

	ist2 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-2", st2, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist2)

	alc2 := testInstanceSiteBuildAllocationContraints(t, dbSession, al2, cdbm.AllocationResourceTypeInstanceType, ist2.ID, cdbm.AllocationConstraintTypeReserved, 25, ipu)
	assert.NotNil(t, alc2)

	os3 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-3", tn2, cdbm.OperatingSystemTypeIPXE, false, nil, false, cdbm.OperatingSystemStatusReady, tnu2)
	assert.NotNil(t, os3)

	vpc3 := testInstanceBuildVPC(t, dbSession, "test-vpc-3", ip, tn2, st2, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu2)
	assert.NotNil(t, vpc3)

	vpc4 := testInstanceBuildVPC(t, dbSession, "test-vpc-4", ip, tn2, st2, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu2)
	assert.NotNil(t, vpc4)

	subnet3 := testInstanceBuildSubnet(t, dbSession, "test-subnet-3", tn2, vpc3, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu2)
	assert.NotNil(t, subnet3)

	subnet4 := testInstanceBuildSubnet(t, dbSession, "test-subnet-4", tn2, vpc4, nil, cdbm.SubnetStatusPending, tnu2)
	assert.NotNil(t, subnet4)

	// Fixtures for NSG-specific testing

	// Associate tenant 1 with site 2
	ts2t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2t1)

	// Associate tenant 2 with site 1
	ts1t2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg, tn2.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1t2)

	// NSG for tenant 1 on site 1
	nsgTenant1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site1)

	// NSG for tenant 1 on site 2
	nsgTenant1Site2 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-2", tn1, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site2)

	// NSG for tenant 2 on site 1
	nsgTenant2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-3", tn2, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant2Site1)

	// VPC prefix
	ipb2 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb2", st2, ip, &tn2.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu2)
	assert.NotNil(t, ipb2)
	vpcPrefix2 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-2", st2, tn2, vpc3.ID, &ipb2.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusError, tnu2)

	// Create 30 test Machines
	ms2 := []*cdbm.Machine{}
	for i := 0; i < 30; i++ {
		mc := testInstanceBuildMachine(t, dbSession, ip.ID, st2.ID, cutil.GetPtr(false), nil)
		ms2 = append(ms2, mc)

		testInstanceBuildMachineInstanceType(t, dbSession, mc, ist2)
		testInstanceBuildMachineInterface(t, dbSession, subnet3.ID, mc.ID)
	}

	// Create 25 test Instances
	insts2 := []*cdbm.Instance{}
	for i := 0; i < 25; i++ {
		inst := testInstanceBuildInstance(t, dbSession, fmt.Sprintf("test-instance-%d", i), tn2.ID, ip.ID, st2.ID, &ist2.ID, vpc3.ID, cutil.GetPtr(ms2[i].ID), &os3.ID, nil, cdbm.InstanceStatusReady)
		assert.NotNil(t, inst)
		insts2 = append(insts2, inst)
	}

	// Tenant 3
	st3 := testInstanceBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st3)

	tnu3 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-4", tnOrg3, tnOrgRoles3)
	tn3 := testInstanceBuildTenant(t, dbSession, "test-tenant-4", tnOrg3, tnu3)

	al3 := testInstanceSiteBuildAllocation(t, dbSession, st3, tn3, "test-allocation-3", ipu)
	assert.NotNil(t, al3)

	ist3 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-3", st3, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist3)

	alc4 := testInstanceSiteBuildAllocationContraints(t, dbSession, al3, cdbm.AllocationResourceTypeInstanceType, ist3.ID, cdbm.AllocationConstraintTypeReserved, 2, ipu)
	assert.NotNil(t, alc4)

	os4 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-4", tn3, cdbm.OperatingSystemTypeIPXE, false, nil, false, cdbm.OperatingSystemStatusReady, tnu3)
	assert.NotNil(t, os4)

	vpc5 := testInstanceBuildVPC(t, dbSession, "test-vpc-4", ip, tn3, st3, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu3)
	assert.NotNil(t, vpc5)

	vpc6 := testInstanceBuildVPC(t, dbSession, "test-vpc-5", ip, tn3, st3, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu3)
	assert.NotNil(t, vpc6)

	subnet5 := testInstanceBuildSubnet(t, dbSession, "test-subnet-3", tn3, vpc5, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu3)
	assert.NotNil(t, subnet5)

	subnet6 := testInstanceBuildSubnet(t, dbSession, "test-subnet-5", tn3, vpc6, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu3)
	assert.NotNil(t, subnet6)

	inst3 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn3.ID, ip.ID, st3.ID, &ist3.ID, vpc5.ID, nil, &os4.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst3)

	// Tenant 4
	tnu4 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-tenant-4", tnOrg4, tnOrgRoles4)

	// Tenant 5
	tnu5 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-5", tnOrg5, tnOrgRoles5)
	tn5 := testInstanceBuildTenant(t, dbSession, "test-tenant-5", tnOrg5, tnu5)

	// Build components for iPXE script test
	st4 := testInstanceBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, true, ipu)
	al5 := testInstanceSiteBuildAllocation(t, dbSession, st4, tn5, "test-allocation-5", ipu)
	ist5 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-5", st4, cdbm.InstanceStatusReady)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al5, cdbm.AllocationResourceTypeInstanceType, ist5.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)

	mc5 := testInstanceBuildMachine(t, dbSession, ip.ID, st4.ID, cutil.GetPtr(false), nil)
	mcinst5 := testInstanceBuildMachineInstanceType(t, dbSession, mc5, ist5)

	assert.NotNil(t, mcinst5)

	os5 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-5", tn5, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu5)
	testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, st4.ID, os5.ID)
	vpc7 := testInstanceBuildVPC(t, dbSession, "test-vpc-7", ip, tn5, st4, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu5)
	subnet7 := testInstanceBuildSubnet(t, dbSession, "test-subnet-7", tn5, vpc7, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu5)

	testInstanceBuildMachineInterface(t, dbSession, subnet7.ID, mc5.ID)
	inst4 := testInstanceBuildInstance(t, dbSession, "test-instance-9001", tn5.ID, ip.ID, st4.ID, &ist5.ID, vpc7.ID, nil, &os5.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst4)

	// Build components for multiple allocation constraint test

	// Tenant 6
	tnu6 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-6", tnOrg6, tnOrgRoles6)
	tn6 := testInstanceBuildTenant(t, dbSession, "test-tenant-6", tnOrg6, tnu6)

	st6 := testInstanceBuildSite(t, dbSession, ip, "test-site-6", cdbm.SiteStatusRegistered, false, ipu)
	al6 := testInstanceSiteBuildAllocation(t, dbSession, st6, tn6, "test-allocation-6", ipu)
	ist6 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-6", st6, cdbm.InstanceStatusReady)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al6, cdbm.AllocationResourceTypeInstanceType, ist6.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)

	// extra allocation constraint
	al7 := testInstanceSiteBuildAllocation(t, dbSession, st6, tn6, "test-allocation-7", ipu)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al7, cdbm.AllocationResourceTypeInstanceType, ist6.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)

	mc6 := testInstanceBuildMachine(t, dbSession, ip.ID, st6.ID, cutil.GetPtr(false), nil)
	mcinst6 := testInstanceBuildMachineInstanceType(t, dbSession, mc6, ist6)
	assert.NotNil(t, mcinst6)

	os6 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-6", tn6, cdbm.OperatingSystemTypeIPXE, false, nil, false, cdbm.OperatingSystemStatusReady, tnu6)
	vpc8 := testInstanceBuildVPC(t, dbSession, "test-vpc-8", ip, tn6, st6, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu6)
	subnet8 := testInstanceBuildSubnet(t, dbSession, "test-subnet-8", tn6, vpc8, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu6)
	testInstanceBuildMachineInterface(t, dbSession, subnet8.ID, mc6.ID)

	inst6 := testInstanceBuildInstance(t, dbSession, "test-instance-900100", tn6.ID, ip.ID, st6.ID, &ist6.ID, vpc8.ID, nil, &os6.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst6)

	// Tenant 7
	tnu7 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-7", tnOrg7, tnOrgRoles7)
	tn7 := testInstanceBuildTenant(t, dbSession, "test-tenant-7", tnOrg7, tnu7)

	st7 := testInstanceBuildSite(t, dbSession, ip, "test-site-7", cdbm.SiteStatusRegistered, false, ipu)
	al10 := testInstanceSiteBuildAllocation(t, dbSession, st7, tn7, "test-allocation-10", ipu)
	ist10 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-10", st7, cdbm.InstanceStatusReady)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al10, cdbm.AllocationResourceTypeInstanceType, ist10.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	mc10 := testInstanceBuildMachine(t, dbSession, ip.ID, st7.ID, cutil.GetPtr(false), nil)
	mcinst10 := testInstanceBuildMachineInstanceType(t, dbSession, mc10, ist10)
	assert.NotNil(t, mcinst10)
	os10 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-10", tn7, cdbm.OperatingSystemTypeIPXE, false, nil, false, cdbm.OperatingSystemStatusReady, tnu7)
	vpc10 := testInstanceBuildVPC(t, dbSession, "test-vpc-7", ip, tn7, st7, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu7)
	subnet10 := testInstanceBuildSubnet(t, dbSession, "test-subnet-10", tn7, vpc10, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu7)
	testInstanceBuildMachineInterface(t, dbSession, subnet10.ID, mc10.ID)
	// site 7b
	st7b := testInstanceBuildSite(t, dbSession, ip, "test-site-7b", cdbm.SiteStatusRegistered, false, ipu)
	al7b := testInstanceSiteBuildAllocation(t, dbSession, st7b, tn7, "test-allocation-7b", ipu)
	ist7b := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-7b", st7b, cdbm.InstanceStatusReady)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al7b, cdbm.AllocationResourceTypeInstanceType, ist7b.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	mc7b := testInstanceBuildMachine(t, dbSession, ip.ID, st7b.ID, cutil.GetPtr(false), nil)
	mcinst7b := testInstanceBuildMachineInstanceType(t, dbSession, mc7b, ist7b)
	assert.NotNil(t, mcinst7b)
	os7b := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-7b", tn7, cdbm.OperatingSystemTypeIPXE, false, nil, false, cdbm.OperatingSystemStatusReady, tnu7)
	vpc7b := testInstanceBuildVPC(t, dbSession, "test-vpc-7b", ip, tn7, st7b, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu7)
	subnet7b := testInstanceBuildSubnet(t, dbSession, "test-subnet-7b", tn7, vpc7b, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu7)
	testInstanceBuildMachineInterface(t, dbSession, subnet7b.ID, mc7b.ID)

	// Tenant8
	// OS Image Based Instance
	tnu8 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-os", tnOrg8, tnOrgRoles8)
	tn8 := testInstanceBuildTenant(t, dbSession, "test-tenant-os", tnOrg8, tnu8)

	st8 := testInstanceBuildSite(t, dbSession, ip, "test-site-8", cdbm.SiteStatusRegistered, false, ipu)
	al8 := testInstanceSiteBuildAllocation(t, dbSession, st8, tn8, "test-allocation-8", ipu)
	ist8 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-8", st8, cdbm.InstanceStatusReady)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al8, cdbm.AllocationResourceTypeInstanceType, ist8.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)

	// extra allocation constraint
	al9 := testInstanceSiteBuildAllocation(t, dbSession, st8, tn8, "test-allocation-9", ipu)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al9, cdbm.AllocationResourceTypeInstanceType, ist8.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)

	mc8 := testInstanceBuildMachine(t, dbSession, ip.ID, st8.ID, cutil.GetPtr(false), nil)
	mcinst8 := testInstanceBuildMachineInstanceType(t, dbSession, mc8, ist8)
	assert.NotNil(t, mcinst8)

	os11 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-11", tn8, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu8)
	ossa2 := testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, st8.ID, os11.ID)
	assert.NotNil(t, ossa2)
	vpc11 := testInstanceBuildVPC(t, dbSession, "test-vpc-11", ip, tn8, st8, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu8)
	subnet11 := testInstanceBuildSubnet(t, dbSession, "test-subnet-11", tn8, vpc11, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu8)
	testInstanceBuildMachineInterface(t, dbSession, subnet11.ID, mc8.ID)

	// Deactivated OS:
	os12 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-12", tn8, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu8)
	os12.IsActive = false
	testUpdateOSIsActive(t, dbSession, os12)
	ist9 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-9", st8, cdbm.InstanceStatusReady)
	testInstanceSiteBuildAllocationContraints(t, dbSession, al8, cdbm.AllocationResourceTypeInstanceType, ist9.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	mc9 := testInstanceBuildMachine(t, dbSession, ip.ID, st8.ID, cutil.GetPtr(false), nil)
	mcinst9 := testInstanceBuildMachineInstanceType(t, dbSession, mc9, ist9)
	assert.NotNil(t, mcinst9)
	testInstanceBuildMachineInterface(t, dbSession, subnet11.ID, mc9.ID)

	// FNN VPC
	vpc9 := testInstanceBuildVPC(t, dbSession, "test-vpc-9", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc9)
	vpc9Secondary := testInstanceBuildVPC(t, dbSession, "test-vpc-9-secondary", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc9Secondary)
	vpc9Site2 := testInstanceBuildVPC(t, dbSession, "test-vpc-9-site-2", ip, tn1, st2, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc9Site2)

	// VPC prefix
	ipb1 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, ipb1)
	ipb5 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.169.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, ipb5)
	ipbSite2 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-site2", st2, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.170.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, ipbSite2)
	vpcPrefix1 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-1", st1, tn1, vpc9.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	vpcPrefix3 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-3", st1, tn1, vpc1.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	vpcPrefix5 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-5", st1, tn1, vpc9.ID, &ipb5.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	vpcPrefix7 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-7", st1, tn1, vpc9.ID, &ipb5.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	vpcPrefixSite2 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-site2", st2, tn1, vpc9Site2.ID, &ipbSite2.ID, cutil.GetPtr("192.170.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefixSite2)
	ipbExhausted := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-exhausted", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.99.1.0", 28, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, ipbExhausted)
	vpcPrefixExhausted := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-exhausted", st1, tn1, vpc9.ID, &ipbExhausted.ID, cutil.GetPtr("10.99.1.0/28"), cutil.GetPtr(28), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefixExhausted)
	for i := 0; i < 8; i++ {
		exhaustInst := testInstanceBuildInstance(t, dbSession, fmt.Sprintf("exhaust-vpcprefix-inst-%d", i), tn1.ID, ip.ID, st1.ID, &istExhaustFixture.ID, vpc9.ID, nil, &os1.ID, nil, cdbm.InstanceStatusReady)
		testInstanceBuildInstanceInterface(t, dbSession, exhaustInst.ID, nil, &vpcPrefixExhausted.ID, nil, cdbm.InterfaceStatusPending)
	}

	subnetExhaustedUsageMap, err := cdbm.NewSubnetDAO(dbSession).GetPrefixUsage(context.Background(), nil, subnetExhausted)
	assert.Nil(t, err)
	subnetExhaustedUsage := subnetExhaustedUsageMap[subnetExhausted.ID]

	vpcPrefixExhaustedUsageMap, err := cdbm.NewVpcPrefixDAO(dbSession).GetPrefixUsage(context.Background(), nil, vpcPrefixExhausted)
	assert.Nil(t, err)
	vpcPrefixExhaustedUsage := vpcPrefixExhaustedUsageMap[vpcPrefixExhausted.ID]

	// NvLink Logical Partition
	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-2", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg, st1, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp2)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st1.ID.String()] = tsc
	scp.IDClientMap[st2.ID.String()] = tsc
	scp.IDClientMap[st3.ID.String()] = tsc
	scp.IDClientMap[st4.ID.String()] = tsc
	scp.IDClientMap[st6.ID.String()] = tsc
	scp.IDClientMap[st8.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInstance", mock.Anything).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInstanceV2", mock.Anything).Return(wrun, nil)

	// Mock per-Site client for st3
	tst3 := &tmocks.Client{}

	scp.IDClientMap[st7.ID.String()] = tst3

	// Mock timeout error
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tst3.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInstanceV2", mock.Anything).Return(wruntimeout, nil)

	tst3.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// Mock per-Site client for st4
	tst4 := &tmocks.Client{}

	scp.IDClientMap[st7b.ID.String()] = tst4

	// Mock other error
	wrun2 := &tmocks.WorkflowRun{}
	wrun2.On("GetID").Return("test-workflow-other-id")

	wrun2.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewApplicationErrorWithCause("test error", "test error type", errors.New("other error"), nil, nil))

	tst4.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"CreateInstanceV2", mock.Anything).Return(wrun2, nil)

	tst4.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	setupIbInactiveDevicesInstanceType := func(t *testing.T, allocName string, machineInactiveDevices []int) (*cdbm.InstanceType, *cdbm.Machine) {
		ist := testInstanceBuildInstanceType(t, dbSession, ip, "ist-ib-inactive-"+uuid.NewString(), st1, cdbm.InstanceStatusReady)
		al := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, allocName, ipu)
		testInstanceSiteBuildAllocationContraints(t, dbSession, al, cdbm.AllocationResourceTypeInstanceType, ist.ID, cdbm.AllocationConstraintTypeReserved, 10, ipu)
		common.TestBuildMachineCapability(t, dbSession, nil, &ist.ID, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-6]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(3), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), []int{1, 2})

		mc := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
		testInstanceBuildMachineInstanceType(t, dbSession, mc, ist)
		common.TestBuildMachineCapability(t, dbSession, &mc.ID, nil, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-6]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(3), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), machineInactiveDevices)
		testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc.ID)
		return ist, mc
	}

	ibInactiveDevicesBaseReq := func(name string) *model.APIInstanceCreateRequest {
		return &model.APIInstanceCreateRequest{
			Name:              name,
			TenantID:          tn1.ID.String(),
			VpcID:             vpc1.ID.String(),
			OperatingSystemID: cutil.GetPtr(os1.ID.String()),
			IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
			Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
				{
					SubnetID: cutil.GetPtr(subnet1.ID.String()),
				},
			},
			InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
				{
					InfiniBandPartitionID: ibp1.ID.String(),
					Device:                "MT28908 Family [ConnectX-6]",
					Vendor:                cutil.GetPtr("Mellanox Technologies"),
					DeviceInstance:        0,
					IsPhysical:            true,
				},
			},
		}
	}

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqData                      *model.APIInstanceCreateRequest
		reqOrg                       string
		reqUser                      *cdbm.User
		reqMachine                   *cdbm.Machine
		reqNVLinkLogicalPartitionIDs []string
		reqNVLinkMachineCapabilities *cdbm.MachineCapability
		respCode                     int
		respMessage                  string
		respUserDataContains         *string
		respUserData                 *string
		// prepareReq runs before the handler (e.g. insert a Machine and set req.MachineID) so cases stay self-contained.
		prepareReq func(t *testing.T, req *model.APIInstanceCreateRequest)
	}

	tests := []struct {
		name                     string
		fields                   fields
		args                     args
		expectedControllerVpcIDs map[string]uuid.UUID
		wantErr                  bool
		verifyChildSpanner       bool
	}{
		{
			name: "test Instance create API endpoint rejects power profile when DPS power management is disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:           "Test Instance rejected power profile",
					TenantID:       tn1.ID.String(),
					InstanceTypeID: cutil.GetPtr(ist1.ID.String()),
					VpcID:          vpc1.ID.String(),
					PowerProfile:   cutil.GetPtr("balanced"),
					UserData:       cutil.GetPtr(""),
					IpxeScript:     cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{SubnetID: cutil.GetPtr(subnet1.ID.String())},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusPreconditionFailed,
				respMessage: "Site does not have DPS power management enabled",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success with subnet interface and ssh key group iPXE script and Labels",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                   "Test Instance",
					Description:            cutil.GetPtr("Test Instance Description"),
					TenantID:               tn1.ID.String(),
					NetworkSecurityGroupID: cutil.GetPtr(nsgTenant1Site1.ID),
					InstanceTypeID:         cutil.GetPtr(ist1.ID.String()),
					VpcID:                  vpc1.ID.String(),
					OperatingSystemID:      cutil.GetPtr(os1.ID.String()),
					UserData:               nil,
					IpxeScript:             cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: desd1.ID.String(),
							Version:               "V1-T1761856992374052",
						},
						{
							DpuExtensionServiceID: desd2.ID.String(),
							Version:               "V1-T1761856992374052",
						},
					},
					SSHKeyGroupIDs: []string{skg1.ID.String()},
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{
							DeviceInstance:           0,
							NVLinkLogicalPartitionID: nvllpDefault.ID.String(),
						},
						{
							DeviceInstance:           1,
							NVLinkLogicalPartitionID: nvllpDefault.ID.String(),
						},
						{
							DeviceInstance:           2,
							NVLinkLogicalPartitionID: nvllpDefault.ID.String(),
						},
					},
					Labels: map[string]string{
						"GPUType": mcNvlType.Name,
					},
				},
				reqMachine:                   nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqNVLinkLogicalPartitionIDs: []string{nvllpDefault.ID.String()},
				reqNVLinkMachineCapabilities: mcNvlType,
				reqOrg:                       tnOrg,
				reqUser:                      tnu1,
				respCode:                     http.StatusCreated,
				respMessage:                  "",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint failure, machine DB status not Ready but controller Ready without allowUnhealthyMachine",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:        "Test Instance provable controller but DB not Ready",
					Description: cutil.GetPtr("Targeted machine: controller Ready prefix, DB status not Ready"),
					TenantID:    tn1.ID.String(),
					VpcID:       vpc1.ID.String(),
					UserData:    cutil.GetPtr(""),
					IpxeScript:  cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				prepareReq: func(t *testing.T, req *model.APIInstanceCreateRequest) {
					mc := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
					testInstanceBuildMachineInstanceType(t, dbSession, mc, ist1)
					testUpdateMachineStatusAndControllerState(t, dbSession, mc, cdbm.MachineStatusError, cdbm.MachineStatusReady)
					req.MachineID = cutil.GetPtr(mc.ID)
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "can be provisioned by setting `allowUnhealthyMachine` to true in request",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, allowUnhealthyMachine true, Maintenance and controller not provisionable",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:        "Test Instance maintenance controller not provisionable",
					Description: cutil.GetPtr("Maintenance status with non-Ready controller state"),
					TenantID:    tn1.ID.String(),
					VpcID:       vpc1.ID.String(),
					UserData:    cutil.GetPtr(""),
					IpxeScript:  cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					AllowUnhealthyMachine: cutil.GetPtr(true),
					PhoneHomeEnabled:      cutil.GetPtr(false),
				},
				prepareReq: func(t *testing.T, req *model.APIInstanceCreateRequest) {
					mc := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
					testInstanceBuildMachineInstanceType(t, dbSession, mc, ist1)
					testUpdateMachineStatusAndControllerState(t, dbSession, mc, cdbm.MachineStatusMaintenance, "Offline")
					req.MachineID = cutil.GetPtr(mc.ID)
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "has controller state: Offline that does not allow Instance creation even with `allowUnhealthyMachine` set to true",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, allowUnhealthyMachine true, Initializing and controller not provisionable",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:        "Test Instance initializing controller not provisionable",
					Description: cutil.GetPtr("Initializing status with non-Ready controller state"),
					TenantID:    tn1.ID.String(),
					VpcID:       vpc1.ID.String(),
					UserData:    cutil.GetPtr(""),
					IpxeScript:  cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					AllowUnhealthyMachine: cutil.GetPtr(true),
					PhoneHomeEnabled:      cutil.GetPtr(false),
				},
				prepareReq: func(t *testing.T, req *model.APIInstanceCreateRequest) {
					mc := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
					testInstanceBuildMachineInstanceType(t, dbSession, mc, ist1)
					testUpdateMachineStatusAndControllerState(t, dbSession, mc, cdbm.MachineStatusInitializing, "Offline")
					req.MachineID = cutil.GetPtr(mc.ID)
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "has status: Initializing that does not allow Instance creation even with `allowUnhealthyMachine` set to true",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, allowUnhealthyMachine true, Machine has Error status, but controller is Ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:        "test-instance-error-controller-ready",
					Description: cutil.GetPtr("Error status with Ready controller state"),
					TenantID:    tn1.ID.String(),
					VpcID:       vpc1.ID.String(),
					UserData:    cutil.GetPtr(""),
					IpxeScript:  cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					AllowUnhealthyMachine: cutil.GetPtr(true),
					PhoneHomeEnabled:      cutil.GetPtr(false),
				},
				prepareReq: func(t *testing.T, req *model.APIInstanceCreateRequest) {
					mc := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
					testInstanceBuildMachineInstanceType(t, dbSession, mc, ist1)
					testUpdateMachineStatusAndControllerState(t, dbSession, mc, cdbm.MachineStatusError, "Ready")
					req.MachineID = cutil.GetPtr(mc.ID)
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success with secondary VPC Prefix interface",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixInstanceSecondaryVpc",
					Description:       cutil.GetPtr("Test VPC Prefix Instance Description Secondary VPC"),
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					SecondaryVpcIDs:   []string{vpc1.ID.String()},
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqMachine: nil,
				reqOrg:     tnOrg,
				reqUser:    tnu1,
				respCode:   http.StatusCreated,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint preserves IPv6-only and dual-stack VPC selection intent",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcSelectionInstance",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(istVpcSelection.ID.String()),
					VpcID:             vpc9.ID.String(),
					SecondaryVpcIDs:   []string{vpc9Secondary.ID.String()},
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcID:      cutil.GetPtr(vpc9.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv6},
							IsPhysical: true,
						},
						{
							VpcID:      cutil.GetPtr(vpc9Secondary.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv4, model.IPFamilyIPv6},
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			expectedControllerVpcIDs: map[string]uuid.UUID{
				vpc9.ID.String():          *vpc9.ControllerVpcID,
				vpc9Secondary.ID.String(): *vpc9Secondary.ControllerVpcID,
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed when requested secondary VPCs do not match interface VPCs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixInstanceSecondaryVpcMismatch",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					SecondaryVpcIDs:   []string{vpc1.ID.String()},
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "One or more Interfaces in request data specify VPC Prefixes that do not belong to VPCs specified in `vpcId` or `secondaryVpcIds`",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed when an interface uses a VPC outside requested VPC IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixInstanceUnexpectedVpc",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: fmt.Sprintf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefix3.ID.String(), vpc1.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed when primary physical interface uses a VPC Prefix from another Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixPrimaryInterfaceWrongSite",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite2.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixSite2.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed when secondary interface uses a VPC Prefix from another Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixSecondaryInterfaceWrongSite",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite2.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixSite2.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success with VPC Prefix interface and Labels",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixInstance",
					Description:       cutil.GetPtr("Test VPC Prefix Instance Description"),
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
							InlineRoutingProfile: &model.APIInterfaceInlineRoutingProfile{
								AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
							},
						},
						{
							VpcPrefixID:       cutil.GetPtr(vpcPrefix5.ID.String()),
							IsPhysical:        false,
							Device:            cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance:    cutil.GetPtr(0),
							VirtualFunctionID: cutil.GetPtr(1),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix7.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine: nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:     tnOrg,
				reqUser:    tnu1,
				respCode:   http.StatusCreated,

				respMessage: "",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint success, custom ipxeScript is specified without OS along with phonehome enabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,

				cfg: cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:           "Test Instance Custom iPXE script",
					TenantID:       tn1.ID.String(),
					InstanceTypeID: cutil.GetPtr(ist1.ID.String()),
					VpcID:          vpc1.ID.String(),
					UserData:       cutil.GetPtr(cdmu.TestCommonCloudInit + "\n#comment-58b81c96-5e6a-11ef-82bb-233657c23006"),
					IpxeScript:     cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(true),
				},
				reqMachine:           nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:               tnOrg,
				reqUser:              tnu1,
				respCode:             http.StatusCreated,
				respMessage:          "",
				respUserDataContains: cutil.GetPtr("58b81c96-5e6a-11ef-82bb-233657c23006"),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, custom ipxeScript is specified without OS along with phonehome disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:           "Test Instance Custom iPXE script with no phonehome",
					TenantID:       tn1.ID.String(),
					InstanceTypeID: cutil.GetPtr(ist1.ID.String()),
					VpcID:          vpc1.ID.String(),
					UserData:       cutil.GetPtr(cdmu.TestCommonCloudInit + cdmu.TestCommonPhoneHomeSegment + "\nsome_key: some_value\n"),
					IpxeScript:     cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqMachine:   nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:       tnOrg,
				reqUser:      tnu1,
				respCode:     http.StatusCreated,
				respMessage:  "",
				respUserData: cutil.GetPtr(cdmu.TestCommonCloudInit + "some_key: some_value\n"),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, phone home enabled but empty user data, provided OS, ensure cloud-config is present",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance OS Empty User Data",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(osPhoneHome.ID.String()),
					UserData:          cutil.GetPtr(""),
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(true),
				},
				reqMachine:           nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:               tnOrg,
				reqUser:              tnu1,
				respCode:             http.StatusCreated,
				respMessage:          "",
				respUserDataContains: cutil.GetPtr(util.SiteCloudConfig),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, phone home enabled but nil user data, provided OS, ensure cloud-config is present",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance OS Nil User Data",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(osPhoneHome.ID.String()),
					UserData:          nil,
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(true),
				},
				reqMachine:           nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:               tnOrg,
				reqUser:              tnu1,
				respCode:             http.StatusCreated,
				respMessage:          "",
				respUserDataContains: cutil.GetPtr(util.SiteCloudConfig),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, minimum requirements",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:           "Test Instance with minimum requirements",
					TenantID:       tn1.ID.String(),
					InstanceTypeID: cutil.GetPtr(ist1.ID.String()),
					VpcID:          vpc1.ID.String(),
					UserData:       cutil.GetPtr(""),
					IpxeScript:     cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqMachine:   nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:       tnOrg,
				reqUser:      tnu1,
				respCode:     http.StatusCreated,
				respMessage:  "",
				respUserData: cutil.GetPtr(""),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, can't provide instance type id AND machine id",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:           "Test Instance for failure",
					TenantID:       tn1.ID.String(),
					InstanceTypeID: cutil.GetPtr(ist1.ID.String()),
					MachineID:      cutil.GetPtr(uuid.New().String()),
					VpcID:          vpc1.ID.String(),
					UserData:       cutil.GetPtr(""),
					IpxeScript:     cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "only one of `instanceTypeId` or `machineId` can be specified in request, not both",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, specify a machine ID belonging to an instance type but tenant is not authorized",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance with machine ID",
					TenantID:   tn2.ID.String(),
					MachineID:  cutil.GetPtr(uuid.New().String()),
					VpcID:      vpc3.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet3.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg2,
				reqUser:     tnu2,
				respCode:    http.StatusForbidden,
				respMessage: "Tenant does not have capability to create Instances using specific Machine ID",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint rejects Machine label selector when tenant is not authorized",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                 "Test Instance with unauthorized Machine label selector",
					TenantID:             tn2.ID.String(),
					InstanceTypeID:       cutil.GetPtr(ist2.ID.String()),
					MachineLabelSelector: map[string]string{"failure-domain": "fd-a"},
					VpcID:                vpc3.ID.String(),
					UserData:             cutil.GetPtr(""),
					IpxeScript:           cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{SubnetID: cutil.GetPtr(subnet3.ID.String())},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg2,
				reqUser:     tnu2,
				respCode:    http.StatusForbidden,
				respMessage: "Tenant does not have capability to create Instances using Machine label selector",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint rejects specified Machine that does not match label selector",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                 "Test Instance with mismatched Machine labels",
					TenantID:             tn1.ID.String(),
					MachineID:            cutil.GetPtr(mcbyid.ID),
					MachineLabelSelector: map[string]string{"failure-domain": "fd-b"},
					VpcID:                vpc2.ID.String(),
					UserData:             cutil.GetPtr(""),
					IpxeScript:           cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{SubnetID: cutil.GetPtr(subnet2.ID.String())},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Machine specified in request does not match machineLabelSelector",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, specify a machine ID belonging to an instance type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                 "Test Instance with machine ID",
					TenantID:             tn1.ID.String(),
					MachineID:            cutil.GetPtr(mcbyid.ID),
					MachineLabelSelector: map[string]string{"failure-domain": "fd-a"},
					VpcID:                vpc2.ID.String(),
					UserData:             cutil.GetPtr(""),
					IpxeScript:           cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet2.ID.String()),
						},
					},
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
							DeviceInstance:           0,
						},
						{
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
							DeviceInstance:           1,
						},
						{
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
							DeviceInstance:           2,
						},
						{
							NVLinkLogicalPartitionID: nvllp2.ID.String(),
							DeviceInstance:           3,
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqMachine:   mcbyid,
				reqOrg:       tnOrg,
				reqUser:      tnu1,
				respCode:     http.StatusCreated,
				respMessage:  "",
				respUserData: cutil.GetPtr(""),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint success, specify a machine ID NOT belonging to an instance type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance without machine ID",
					TenantID:   tn1.ID.String(),
					MachineID:  cutil.GetPtr(mcnoinst.ID),
					VpcID:      vpc1.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqMachine:   mcnoinst,
				reqOrg:       tnOrg,
				reqUser:      tnu1,
				respCode:     http.StatusCreated,
				respMessage:  "",
				respUserData: cutil.GetPtr(""),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, specify a machine ID not matching site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance with machine from wrong site",
					TenantID:   tn1.ID.String(),
					MachineID:  cutil.GetPtr(mcwrongsite.ID),
					VpcID:      vpc1.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Machine specified in request does not belong to Site",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, specify a non-existing DPU Extension Service ID for deployment",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance with non-existing DPU Extension Service ID for deployment",
					TenantID:   tn1.ID.String(),
					MachineID:  cutil.GetPtr(mcassigned.ID),
					VpcID:      vpc1.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: uuid.New().String(),
							Version:               "V1-T1761856992374052",
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "DPU Extension Service:",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, DPU Extension Service specified for deployment belongs to a different Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance with DPU Extension Service specified for deployment belongs to a different Site",
					TenantID:   tn1.ID.String(),
					MachineID:  cutil.GetPtr(mcassigned.ID),
					VpcID:      vpc1.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: desd3.ID.String(),
							Version:               "V1-T1761856992374052",
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusForbidden,
				respMessage: "does not belong to Site where Instance is being created",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, specify a machine ID already assigned",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance with assigned machine",
					TenantID:   tn1.ID.String(),
					MachineID:  cutil.GetPtr(mcassigned.ID),
					VpcID:      vpc1.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "is assigned to an Instance, cannot be used for new Instance",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, specify an unhealthy machine",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance with unhealthy machine",
					TenantID:   tn1.ID.String(),
					MachineID:  cutil.GetPtr(mcunhealthy.ID),
					VpcID:      vpc1.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "has status: Error that does not allow Instance creation",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, allowUnhealthyMachine true but machine not Ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                  "Test Instance with unhealthy machine and flag true",
					TenantID:              tn1.ID.String(),
					MachineID:             cutil.GetPtr(mcunhealthy2.ID),
					AllowUnhealthyMachine: cutil.GetPtr(true),
					VpcID:                 vpc1.ID.String(),
					UserData:              cutil.GetPtr(""),
					IpxeScript:            cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "has controller state:",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failure, specify a missing machine",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:       "Test Instance with missing machine",
					TenantID:   tn1.ID.String(),
					MachineID:  cutil.GetPtr(mcmissing.ID),
					VpcID:      vpc1.ID.String(),
					UserData:   cutil.GetPtr(""),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "is missing on site, cannot be used for new Instance",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint successfully, even more than one Allocation Constraint created and unused",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "test-instance-more-allocation",
					TenantID:          tn6.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist6.ID.String()),
					VpcID:             vpc8.ID.String(),
					OperatingSystemID: cutil.GetPtr(os6.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet8.ID.String()),
						},
					},
				},
				reqMachine:  mc6,
				reqOrg:      tnOrg6,
				reqUser:     tnu6,
				respCode:    http.StatusCreated,
				respMessage: "",
			},
			wantErr: false,
		},
		{
			name: "os ID specified but not found, should fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance Custom iPXE script 01",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					UserData:          cutil.GetPtr(cdmu.TestCommonCloudInit),
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					OperatingSystemID: cutil.GetPtr(uuid.NewString()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(true),
				},
				reqMachine:  mc14,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Could not find OperatingSystem with ID specified in request data",
			},
			wantErr: false,
		},
		{
			name: "os ID specified but invalid, should fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance Custom iPXE script 02",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					UserData:          cutil.GetPtr(cdmu.TestCommonCloudInit),
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					OperatingSystemID: cutil.GetPtr("not a real UUID"),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(true),
				},
				reqMachine:  mc15,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "must be a valid UUID",
			},
			wantErr: false,
		},
		{
			name: "os specified is from different site than VPC, should fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance Site Mismatch",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					UserData:          nil,
					IpxeScript:        nil,
					OperatingSystemID: cutil.GetPtr(os2.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqMachine:  mc15,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Creation of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, same InfiniBand interfaces specified multiple times in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					Description:       cutil.GetPtr("Test Instance Description"),
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					SSHKeyGroupIDs: []string{skg1.ID.String()},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							DeviceInstance:        1,
							IsPhysical:            true,
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusConflict,
				respMessage: "",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint failed, InfiniBand interfaces specified for non-IB Instance Type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance no-IB",
					Description:       cutil.GetPtr("Test Instance Description"),
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(istnoib.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					SSHKeyGroupIDs: []string{skg1.ID.String()},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "No Machines are available for specified Instance Type",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, either os or ipxeScript should specify",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance iPXE script",
					TenantID:          tn5.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist5.ID.String()),
					VpcID:             vpc7.ID.String(),
					OperatingSystemID: nil,
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet7.ID.String()),
						},
					},
				},
				reqMachine:  mc5,
				reqOrg:      tnOrg5,
				reqUser:     tnu5,
				respCode:    http.StatusBadRequest,
				respMessage: "either `operatingSystemId` or `ipxeScript` must be specified",
			},
			wantErr: false,
		},
		{
			name: "os type image, temporarily expect StatusBadRequest (Image based OS not allowed)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance os image type",
					TenantID:          tn8.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist8.ID.String()),
					VpcID:             vpc11.ID.String(),
					OperatingSystemID: cutil.GetPtr(os11.ID.String()),
					UserData:          cutil.GetPtr(cdmu.TestCommonCloudInit),
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet11.ID.String()),
						},
					},
				},
				reqMachine:  mc8,
				reqOrg:      tnOrg8,
				reqUser:     tnu8,
				respCode:    http.StatusBadRequest,
				respMessage: "Creation of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.",
			},
			wantErr: false,
		},
		{
			name: "deactivated os, should fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance deactivated os image type",
					TenantID:          tn8.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist9.ID.String()),
					VpcID:             vpc11.ID.String(),
					OperatingSystemID: cutil.GetPtr(os12.ID.String()),
					UserData:          cutil.GetPtr(cdmu.TestCommonCloudInit),
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet11.ID.String()),
						},
					},
				},
				reqMachine:  mc9,
				reqOrg:      tnOrg8,
				reqUser:     tnu8,
				respCode:    http.StatusBadRequest,
				respMessage: "",
			},
			wantErr: false,
		},
		{
			name: "error creating Instance due to name clash",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusConflict,
				respMessage: "id",
			},
			wantErr: false,
		},
		{
			name: "error creating Instance due to invalid Subnet",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance 2",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "at least one Interface must be specified",
			},
			wantErr: false,
		}, {
			name: "test Instance create API endpoint failed, Site is not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpcSiteNotReady.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnetSiteNotReady.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "The Site where this Instance is being created is not in Registered stat",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, VPC is not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpcPending.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnetReady.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "VPC specified in request data is not ready",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed with VPC Prefix, provided VPC isn't FNN",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixInstance",
					Description:       cutil.GetPtr("Test VPC Prefix Instance Description"),
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID: cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:  true,
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine:  mc12,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefix based interfaces", vpc1.ID),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, Subnet is not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpcSiteReady.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnetPending.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: fmt.Sprintf("Subnet: %v specified in request data is not in Ready state", subnetPending.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed when primary physical interface uses a prefix from a secondary VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixPrimaryInterfaceMustMatchVpc",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					SecondaryVpcIDs:   []string{vpc1.ID.String()},
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "The primary physical Interface for deviceInstance: 0 must use a VPC Prefix that belongs to VPC specified in `vpcId`",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint preserves prefix response for mixed selectors without device info",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVpcPrefixPrimaryInterfaceMustMatchVpcNoDeviceInfo",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					SecondaryVpcIDs:   []string{vpc1.ID.String()},
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID: cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:  true,
						},
						{
							VpcID:      cutil.GetPtr(vpc9.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv4},
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "The primary physical Interface must use a VPC Prefix that belongs to VPC specified in `vpcId`",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, VPC Prefix is not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "TestVPCPrefix2Instance",
					TenantID:          tn2.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist2.ID.String()),
					VpcID:             vpc3.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID: cutil.GetPtr(vpcPrefix2.ID.String()),
							IsPhysical:  true,
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg2,
				reqUser:     tnu2,
				respCode:    http.StatusBadRequest,
				respMessage: fmt.Sprintf("VPC Prefix: %v specified in request data is not in Ready state", vpcPrefix2.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, multiple physical interface set",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance 01",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID:   cutil.GetPtr(subnet1.ID.String()),
							IsPhysical: true,
						},
						{
							SubnetID:   cutil.GetPtr(subnet2.ID.String()),
							IsPhysical: true,
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "only one interface can be marked as physical",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, operating System specified in request is not owned by tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance 02",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os3.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "OperatingSystem specified in request is not owned by Tenant",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, operating System specified in request does not allow overriding user data",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance UserData",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          cutil.GetPtr(cdmu.TestCommonCloudInit),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Operating System specified in request does not allow overriding `userData`",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, always iPXE boot flag specified along with image based OS (Image based OS not allowed)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                     "Test Instance custom iPXE",
					TenantID:                 tn5.ID.String(),
					InstanceTypeID:           cutil.GetPtr(ist5.ID.String()),
					VpcID:                    vpc7.ID.String(),
					OperatingSystemID:        cutil.GetPtr(os5.ID.String()),
					AlwaysBootWithCustomIpxe: cutil.GetPtr(true),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet7.ID.String()),
						},
					},
				},
				reqMachine:  mc15,
				reqOrg:      tnOrg5,
				reqUser:     tnu5,
				respCode:    http.StatusBadRequest,
				respMessage: "Creation of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, custom iPXE specified along with image based OS (Image based OS not allowed)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance custom iPXE",
					TenantID:          tn5.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist5.ID.String()),
					VpcID:             vpc7.ID.String(),
					OperatingSystemID: cutil.GetPtr(os5.ID.String()),
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet7.ID.String()),
						},
					},
				},
				reqMachine:  mc5,
				reqOrg:      tnOrg5,
				reqUser:     tnu5,
				respCode:    http.StatusBadRequest,
				respMessage: "Creation of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, Number of Active Instance more than Allocation Constraint value",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "test-instance-26",
					TenantID:          tn2.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist2.ID.String()),
					VpcID:             vpc3.ID.String(),
					OperatingSystemID: cutil.GetPtr(os3.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet3.ID.String()),
						},
					},
				},
				reqMachine:  ms2[25],
				reqOrg:      tnOrg2,
				reqUser:     tnu2,
				respCode:    http.StatusForbidden,
				respMessage: "Tenant has reached the maximum number of Instances for Instance Type specified in request data",
			},
			wantErr: false,
		},

		{
			name: "test Instance create API endpoint failed, org does not have a Tenant associated ",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg4,
				reqUser:     tnu4,
				respCode:    http.StatusBadRequest,
				respMessage: "Org does not have a Tenant associated",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, Could not find InstanceType ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance with no name conflict",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(uuid.New().String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Could not find Instance Type with ID specified in request data",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, Could not find NSG ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                   uuid.NewString(),
					TenantID:               tn1.ID.String(),
					InstanceTypeID:         cutil.GetPtr(ist1.ID.String()),
					NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()),
					VpcID:                  vpc1.ID.String(),
					OperatingSystemID:      cutil.GetPtr(os1.ID.String()),
					UserData:               nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Could not find NetworkSecurityGroup with ID",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, NSG does not belong to Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                   uuid.NewString(),
					TenantID:               tn1.ID.String(),
					InstanceTypeID:         cutil.GetPtr(ist1.ID.String()),
					NetworkSecurityGroupID: &nsgTenant1Site2.ID,
					VpcID:                  vpc1.ID.String(),
					OperatingSystemID:      cutil.GetPtr(os1.ID.String()),
					UserData:               nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusForbidden,
				respMessage: "NetworkSecurityGroup with ID specified in request data does not belong to Site",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, NSG does not belong to Tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                   uuid.NewString(),
					TenantID:               tn1.ID.String(),
					InstanceTypeID:         cutil.GetPtr(ist1.ID.String()),
					NetworkSecurityGroupID: &nsgTenant2Site1.ID,
					VpcID:                  vpc1.ID.String(),
					OperatingSystemID:      cutil.GetPtr(os1.ID.String()),
					UserData:               nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
				},
				reqMachine:  mc1,
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusForbidden,
				respMessage: "NetworkSecurityGroup with ID specified in request data does not belong to Tenant",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, Invalid Tenant ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:           "Test Instance",
					TenantID:       "not-a-valid-uuid",
					InstanceTypeID: cutil.GetPtr(ist1.ID.String()),
					VpcID:          vpc1.ID.String(),
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Error validating Instance creation request data",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, does not have any un-allocated Machine for InstanceType",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					TenantID:          tn3.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist3.ID.String()),
					VpcID:             vpc5.ID.String(),
					OperatingSystemID: cutil.GetPtr(os4.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet5.ID.String()),
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg3,
				reqUser:     tnu3,
				respCode:    http.StatusBadRequest,
				respMessage: "No Machines are available for specified Instance Type",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed, subnet id in request does not match with VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					TenantID:          tn3.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist3.ID.String()),
					VpcID:             vpc5.ID.String(),
					OperatingSystemID: cutil.GetPtr(os4.ID.String()),
					UserData:          nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet6.ID.String()),
						},
					},
				},
				reqMachine:  nil,
				reqOrg:      tnOrg3,
				reqUser:     tnu3,
				respCode:    http.StatusBadRequest,
				respMessage: fmt.Sprintf("Subnet: %v specified in request does not match with VPC", subnet6.ID.String()),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint fails with sync workflow timeout ",
			fields: fields{
				dbSession: dbSession,
				tc:        tst3,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					Description:       cutil.GetPtr("Test Instance Description"),
					TenantID:          tn7.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist10.ID.String()),
					VpcID:             vpc10.ID.String(),
					OperatingSystemID: cutil.GetPtr(os10.ID.String()),
					UserData:          nil,
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet10.ID.String()),
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine:  mc10,
				reqOrg:      tnOrg7,
				reqUser:     tnu7,
				respCode:    http.StatusInternalServerError,
				respMessage: "",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint fails with workflow error and properly reports unwrapped error",
			fields: fields{
				dbSession: dbSession,
				tc:        tst4,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance",
					Description:       cutil.GetPtr("Test Instance Description"),
					TenantID:          tn7.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist7b.ID.String()),
					VpcID:             vpc7b.ID.String(),
					OperatingSystemID: cutil.GetPtr(os7b.ID.String()),
					UserData:          nil,
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet7b.ID.String()),
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine:  mc7b,
				reqOrg:      tnOrg7,
				reqUser:     tnu7,
				respCode:    http.StatusInternalServerError,
				respMessage: "Failed to execute sync workflow to create Instance on Site: other error",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint fails with infiniband interface required isphysical is true",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                   "Test Instance infiniband",
					Description:            cutil.GetPtr("Test Instance Description"),
					TenantID:               tn1.ID.String(),
					NetworkSecurityGroupID: cutil.GetPtr(nsgTenant1Site1.ID),
					InstanceTypeID:         cutil.GetPtr(ist1.ID.String()),
					VpcID:                  vpc1.ID.String(),
					OperatingSystemID:      cutil.GetPtr(os1.ID.String()),
					UserData:               nil,
					IpxeScript:             cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					SSHKeyGroupIDs: []string{skg1.ID.String()},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            false,
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine:  nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "must be set to true. Virtual functions are currently not supported for InfiniBand interfaces",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint fails with infiniband interface device instance is greater than the number of device instances in the InfiniBand Instance Type's Machine Capabilities",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:                   "Test Instance infiniband device",
					Description:            cutil.GetPtr("Test Instance Description"),
					TenantID:               tn1.ID.String(),
					NetworkSecurityGroupID: cutil.GetPtr(nsgTenant1Site1.ID),
					InstanceTypeID:         cutil.GetPtr(ist1.ID.String()),
					VpcID:                  vpc1.ID.String(),
					OperatingSystemID:      cutil.GetPtr(os1.ID.String()),
					UserData:               nil,
					IpxeScript:             cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnet1.ID.String()),
						},
					},
					SSHKeyGroupIDs: []string{skg1.ID.String()},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        4,
							IsPhysical:            true,
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine:  nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Requested InfiniBand device instances are not available on any Machine for this Instance Type",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint fails with interface duplicate device and instance is in the Instance Type's Machine Capabilities",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance duplicate device and instance",
					Description:       cutil.GetPtr("Test Instance duplicate device and instance Description"),
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					UserData:          nil,
					IpxeScript:        nil,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix1.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix5.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
					Labels: map[string]string{
						"GPUType": "H100",
					},
				},
				reqMachine:  nil, // We randomize machines.  Any instance type with multiple valid machines can't be tested for a known machine.
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Duplicate Interface configuration specified for Device MT42822 BlueField-2 integrated ConnectX-6 Dx network controller",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			// vpc1 is an ETHERNET_VIRTUALIZER VPC; `auto: true` is only
			// valid for instances in a Flat VPC. The model validator
			// admits the empty-interfaces + auto pair, so this exercises
			// the handler-side cross-check that fetches the VPC and
			// rejects the mismatch before the workflow is invoked.
			name: "test Instance create API endpoint rejects auto=true on a non-Flat VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Auto on ETV VPC",
					Description:       cutil.GetPtr("auto=true must be rejected outside Flat VPCs"),
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					AutoNetwork:       true,
					Interfaces:        []model.APIInterfaceCreateOrUpdateRequest{},
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "`autoNetwork` is only supported when the VPC has `networkVirtualizationType` set to `FLAT`",
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance create API endpoint failed when subnet IP addresses are exhausted",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance subnet exhausted",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc1.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID: cutil.GetPtr(subnetExhausted.ID.String()),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMessage: fmt.Sprintf(
					"Subnet %v does not have enough IP addresses: %d of %d IP addresses remain available, but the %d interface(s) in this request require %d IP address(es)",
					subnetExhausted.ID, subnetExhaustedUsage.AvailableIPs-subnetExhaustedUsage.AcquiredIPs, subnetExhaustedUsage.AvailableIPs, 1, 1,
				),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint failed when VPC Prefix IP addresses are exhausted",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceCreateRequest{
					Name:              "Test Instance vpc prefix exhausted",
					TenantID:          tn1.ID.String(),
					InstanceTypeID:    cutil.GetPtr(ist1.ID.String()),
					VpcID:             vpc9.ID.String(),
					OperatingSystemID: cutil.GetPtr(os1.ID.String()),
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixExhausted.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusBadRequest,
				respMessage: fmt.Sprintf(
					"VPC Prefix %v does not have enough IP addresses: %d of %d IP addresses remain available, but the %d interface(s) in this request require %d IP addresses",
					vpcPrefixExhausted.ID, vpcPrefixExhaustedUsage.AvailableIPs-vpcPrefixExhaustedUsage.AcquiredIPs, vpcPrefixExhaustedUsage.AvailableIPs, 1, 2,
				),
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint selects machine when Instance Type and Machine InactiveDevices differ but request is satisfied",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: ibInactiveDevicesBaseReq("Test Instance IB inactive devices mismatch"),
				prepareReq: func(t *testing.T, req *model.APIInstanceCreateRequest) {
					ist, _ := setupIbInactiveDevicesInstanceType(t, "test-allocation-ib-inactive-mismatch", []int{1, 3})
					req.InstanceTypeID = cutil.GetPtr(ist.ID.String())
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint returns suggested device instances when requested InfiniBand device instance is inactive on Machine",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: func() *model.APIInstanceCreateRequest {
					req := ibInactiveDevicesBaseReq("Test Instance IB inactive devices suggest")
					req.InfiniBandInterfaces[0].DeviceInstance = 1
					return req
				}(),
				prepareReq: func(t *testing.T, req *model.APIInstanceCreateRequest) {
					ist, _ := setupIbInactiveDevicesInstanceType(t, "test-allocation-ib-inactive-suggest", []int{1, 3})
					req.InstanceTypeID = cutil.GetPtr(ist.ID.String())
				},
				reqOrg:      tnOrg,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: "Requested InfiniBand device instances are not available on any Machine for this Instance Type",
			},
			wantErr: false,
		},
		{
			name: "test Instance create API endpoint selects machine when InfiniBand InactiveDevices match on Machine",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqData: ibInactiveDevicesBaseReq("Test Instance IB inactive devices match"),
				prepareReq: func(t *testing.T, req *model.APIInstanceCreateRequest) {
					ist, _ := setupIbInactiveDevicesInstanceType(t, "test-allocation-ib-inactive-match", []int{1, 2})
					req.InstanceTypeID = cutil.GetPtr(ist.ID.String())
				},
				reqOrg:   tnOrg,
				reqUser:  tnu1,
				respCode: http.StatusCreated,
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := CreateInstanceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       scp,
				cfg:       tt.fields.cfg,
			}

			if tt.args.prepareReq != nil {
				tt.args.prepareReq(t, tt.args.reqData)
			}

			jsonData, _ := json.Marshal(tt.args.reqData)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.reqOrg)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("CreateInstanceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("CreateInstanceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respMessage != "" {
				assert.Contains(t, rec.Body.String(), tt.args.respMessage)
			}
			if tt.args.respCode != http.StatusCreated {
				return
			}
			rst := &model.APIInstance{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, rst.Name, tt.args.reqData.Name)
			assert.Equal(t, rst.NetworkSecurityGroupID, tt.args.reqData.NetworkSecurityGroupID)
			assert.Equal(t, rst.Description, tt.args.reqData.Description)
			assert.Equal(t, rst.Status, cdbm.InstanceStatusPending)
			if tt.args.reqMachine != nil {
				assert.Equal(t, rst.MachineID, cutil.GetPtr(tt.args.reqMachine.ID))
			}
			assert.Equal(t, len(rst.StatusHistory), 1)
			if len(tt.args.reqData.Interfaces) > 0 {
				require.Len(t, rst.Interfaces, len(tt.args.reqData.Interfaces))
				hasInlineRoutingProfile := false
				for i := range tt.args.reqData.Interfaces {
					if tt.args.reqData.Interfaces[i].SubnetID != nil {
						assert.Equal(t, tt.args.reqData.Interfaces[i].SubnetID, rst.Interfaces[i].SubnetID)
					}

					if tt.args.reqData.Interfaces[i].VpcPrefixID != nil {
						assert.Equal(t, tt.args.reqData.Interfaces[i].VpcPrefixID, rst.Interfaces[i].VpcPrefixID)
					}

					if tt.args.reqData.Interfaces[i].VpcID != nil {
						assert.Equal(t, tt.args.reqData.Interfaces[i].VpcID, rst.Interfaces[i].VpcID)
						assert.Equal(t, tt.args.reqData.Interfaces[i].IPFamilies, rst.Interfaces[i].IPFamilies)
						assert.Nil(t, rst.Interfaces[i].VpcPrefixID)
					}

					if tt.args.reqData.Interfaces[i].IPAddress != nil {
						assert.Equal(t, tt.args.reqData.Interfaces[i].IPAddress, rst.Interfaces[i].RequestedIpAddress)
					}

					assert.Equal(t, tt.args.reqData.Interfaces[i].Device, rst.Interfaces[i].Device)
					assert.Equal(t, tt.args.reqData.Interfaces[i].DeviceInstance, rst.Interfaces[i].DeviceInstance)
					assert.Equal(t, tt.args.reqData.Interfaces[i].VirtualFunctionID, rst.Interfaces[i].VirtualFunctionID)
					if tt.args.reqData.Interfaces[i].InlineRoutingProfile != nil {
						hasInlineRoutingProfile = true
						require.NotNil(t, rst.Interfaces[i].InlineRoutingProfile)
						assert.Equal(t, tt.args.reqData.Interfaces[i].InlineRoutingProfile.AllowedAnycastPrefixes, rst.Interfaces[i].InlineRoutingProfile.AllowedAnycastPrefixes)
					}

					// Handle the fact that single-interface instance get normalized to have
					// PF for the first interface.
					if len(tt.args.reqData.Interfaces) > 1 {
						assert.Equal(t, tt.args.reqData.Interfaces[i].IsPhysical, rst.Interfaces[i].IsPhysical)
					}
				}

				if hasInlineRoutingProfile || len(tt.expectedControllerVpcIDs) > 0 {
					ifcDAO := cdbm.NewInterfaceDAO(dbSession)
					dbIfcs, _, ierr := ifcDAO.GetAll(ec.Request().Context(), nil,
						cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{uuid.MustParse(rst.ID)}},
						cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}},
						nil)
					require.NoError(t, ierr)
					require.Len(t, dbIfcs, len(tt.args.reqData.Interfaces))
					for i := range tt.args.reqData.Interfaces {
						if tt.args.reqData.Interfaces[i].InlineRoutingProfile != nil {
							require.NotNil(t, dbIfcs[i].InlineRoutingProfile)
							assert.Equal(t, tt.args.reqData.Interfaces[i].InlineRoutingProfile.AllowedAnycastPrefixes, dbIfcs[i].InlineRoutingProfile.AllowedAnycastPrefixes)
						}
						if tt.args.reqData.Interfaces[i].VpcID != nil {
							require.NotNil(t, dbIfcs[i].VpcID)
							assert.Equal(t, *tt.args.reqData.Interfaces[i].VpcID, dbIfcs[i].VpcID.String())
							require.NotNil(t, dbIfcs[i].VpcIPFamilyMode)
							assert.Equal(t, tt.args.reqData.Interfaces[i].VpcIPFamilyMode(), *dbIfcs[i].VpcIPFamilyMode)
							assert.Nil(t, dbIfcs[i].VpcPrefixID)
						}
					}
				}
			}

			if len(tsc.Calls) > 0 && len(tsc.Calls[len(tsc.Calls)-1].Arguments) > 3 {
				req := tsc.Calls[len(tsc.Calls)-1].Arguments[3].(*corev1.InstanceAllocationRequest)

				// Check that the list of IDs match in size and order.
				if len(tt.args.reqData.SSHKeyGroupIDs) > 0 {
					assert.Equal(t, req.Config.Tenant.TenantKeysetIds, tt.args.reqData.SSHKeyGroupIDs)
				} else {
					assert.Empty(t, req.Config.Tenant.TenantKeysetIds)
				}

				// Check that if user did not send Instance Type ID, it is not sent to Core
				if tt.args.reqData.InstanceTypeID == nil {
					assert.Nil(t, req.InstanceTypeId)
				} else {
					assert.Equal(t, *tt.args.reqData.InstanceTypeID, *req.InstanceTypeId)
				}

				// Check that the allow unhealthy machine flag is set correctly
				if tt.args.reqData.AllowUnhealthyMachine != nil && *tt.args.reqData.AllowUnhealthyMachine {
					assert.True(t, req.AllowUnhealthyMachine, fmt.Sprintf("%v", req))
				} else {
					assert.False(t, req.AllowUnhealthyMachine, fmt.Sprintf("%v", req))
				}

				for i, reqIfc := range tt.args.reqData.Interfaces {
					if reqIfc.InlineRoutingProfile != nil {
						assertInterfaceRoutingProfilePrefixes(t, req.Config.Network.Interfaces[i].RoutingProfile, reqIfc.InlineRoutingProfile.AllowedAnycastPrefixes)
					}
					if reqIfc.VpcID != nil {
						expectedControllerVpcID, ok := tt.expectedControllerVpcIDs[*reqIfc.VpcID]
						require.True(t, ok)
						assertInterfaceVpcSelection(t, req.Config.Network.Interfaces[i], expectedControllerVpcID, reqIfc.VpcIPFamilyMode())
					}
				}
			}

			if len(tt.args.reqData.InfiniBandInterfaces) > 0 {
				assert.Equal(t, len(tt.args.reqData.InfiniBandInterfaces), len(rst.InfiniBandInterfaces))
				assert.Equal(t, rst.InfiniBandInterfaces[0].InfiniBandPartitonID, tt.args.reqData.InfiniBandInterfaces[0].InfiniBandPartitionID)
				assert.Equal(t, rst.InfiniBandInterfaces[0].Device, tt.args.reqData.InfiniBandInterfaces[0].Device)
				assert.Equal(t, rst.InfiniBandInterfaces[0].DeviceInstance, tt.args.reqData.InfiniBandInterfaces[0].DeviceInstance)
				assert.Equal(t, rst.InfiniBandInterfaces[0].IsPhysical, tt.args.reqData.InfiniBandInterfaces[0].IsPhysical)
				assert.Equal(t, rst.InfiniBandInterfaces[0].VirtualFunctionID, tt.args.reqData.InfiniBandInterfaces[0].VirtualFunctionID)
			}

			if len(tt.args.reqData.NVLinkInterfaces) > 0 {
				assert.Equal(t, len(tt.args.reqData.NVLinkInterfaces), len(rst.NVLinkInterfaces))
				for i, nvlifc := range rst.NVLinkInterfaces {
					assert.Equal(t, tt.args.reqData.NVLinkInterfaces[i].DeviceInstance, nvlifc.DeviceInstance)
					expectedPartitionID := tt.args.reqData.NVLinkInterfaces[i].NVLinkLogicalPartitionID
					assert.Equal(t, expectedPartitionID, nvlifc.NVLinkLogicalPartitionID,
						"NVLink interface for DeviceInstance %d: expected partition %s, got %s",
						nvlifc.DeviceInstance, expectedPartitionID, nvlifc.NVLinkLogicalPartitionID)
				}
			}

			if tt.args.reqData.IpxeScript != nil {
				assert.Equal(t, rst.IpxeScript, tt.args.reqData.IpxeScript)
			}

			if tt.args.reqData.OperatingSystemID == nil {
				assert.Equal(t, rst.IpxeScript, tt.args.reqData.IpxeScript)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}

			if len(tt.args.reqData.SSHKeyGroupIDs) > 0 {
				assert.Equal(t, len(rst.SSHKeyGroups), len(tt.args.reqData.SSHKeyGroupIDs))
			}

			if tt.args.reqData.Labels != nil {
				assert.Equal(t, len(rst.Labels), len(tt.args.reqData.Labels))
			}

			assert.ElementsMatch(t, tt.args.reqData.SecondaryVpcIDs, rst.SecondaryVpcIDs)

			if tt.args.respUserData != nil {
				assert.Equal(t, *tt.args.respUserData, *rst.UserData)
			}

			if tt.args.respUserDataContains != nil {
				assert.Contains(t, *rst.UserData, *tt.args.respUserDataContains)
			}

			instUserData := map[string]interface{}{}
			if tt.args.reqData.PhoneHomeEnabled != nil {
				assert.Equal(t, rst.PhoneHomeEnabled, *tt.args.reqData.PhoneHomeEnabled)
				// Verify Phone home
				err := yaml.Unmarshal([]byte(*rst.UserData), &instUserData)
				assert.Equal(t, err, nil)
				if *tt.args.reqData.PhoneHomeEnabled {
					assert.Equal(t, expectedPhoneHomeLocation(instUserData), phoneHomeLocationIn(instUserData))
					if tt.args.reqData.OperatingSystemID != nil {
						lines := strings.Split(*rst.UserData, "\n")
						// ensure first line is always #cloud-config
						assert.Equal(t, util.SiteCloudConfig, lines[0])
						assert.NotEqual(t, util.SiteCloudConfig, lines[1])
					}
				} else {
					assert.Equal(t, phoneHomeAbsent, phoneHomeLocationIn(instUserData))
				}
			} else {
				if tt.args.reqData.OperatingSystemID != nil {
					// Verify OS has phone home enabled and insance inheritated from it.
					osDAO := cdbm.NewOperatingSystemDAO(dbSession)
					osID, _ := uuid.Parse(*tt.args.reqData.OperatingSystemID)
					dos, terr := osDAO.GetByID(context.Background(), nil, osID, nil)
					assert.Nil(t, terr)
					if dos.PhoneHomeEnabled {
						// Verify Phone home
						err := yaml.Unmarshal([]byte(*rst.UserData), &instUserData)
						assert.Equal(t, err, nil)
						assert.Equal(t, expectedPhoneHomeLocation(instUserData), phoneHomeLocationIn(instUserData))
					}
				}
			}
		})
	}

}

func resetInstanceStatus(t *testing.T, dbSession *cdb.Session, instanceID uuid.UUID, status string) {
	instanceDAO := cdbm.NewInstanceDAO(dbSession)
	_, err := instanceDAO.Update(context.Background(), nil, cdbm.InstanceUpdateInput{
		InstanceID:                instanceID,
		InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{Status: cutil.GetPtr(status)},
	})
	if err != nil {
		t.Fatalf("error updating instance status: %v", err)
	}
	assert.Nil(t, err)
}

func TestUpdateInstanceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, false, ipu)
	assert.NotNil(t, st2)

	st3 := testInstanceBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, false, ipu)
	assert.NotNil(t, st3)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg1, tnOrgRoles1)
	tn2 := testInstanceBuildTenant(t, dbSession, "test-tenant-3", tnOrg1, tnu2)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	al2 := testInstanceSiteBuildAllocation(t, dbSession, st2, tn2, "test-allocation-2", ipu)
	assert.NotNil(t, al2)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	ist2 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-2", st2, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist2)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 6, ipu)
	assert.NotNil(t, alc1)

	alc2 := testInstanceSiteBuildAllocationContraints(t, dbSession, al2, cdbm.AllocationResourceTypeInstanceType, ist2.ID, cdbm.AllocationConstraintTypeReserved, 6, ipu)
	assert.NotNil(t, alc2)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)

	mc3 := testInstanceBuildMachine(t, dbSession, ip.ID, st2.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc3)

	mc4 := testInstanceBuildMachine(t, dbSession, ip.ID, st2.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc4)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc3, ist2)
	assert.NotNil(t, mcinst2)

	mcinst4 := testInstanceBuildMachineInstanceType(t, dbSession, mc4, ist1)
	assert.NotNil(t, mcinst4)

	// Build SSHKeyGroup 1
	skg1 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg1, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("12345"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation 1
	skgsa1 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1122"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa1)

	// Build SSHKeyGroup 2
	skg2 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-2", tnOrg1, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("123457"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg2)

	// Build SSHKeyGroupSiteAssociation 2
	skgsa2 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg2.ID, st1.ID, cutil.GetPtr("3344"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa2)

	// Build SSHKeyGroup 3
	skg3 := testBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-3", tnOrg1, cutil.GetPtr("test"), tn1.ID, cutil.GetPtr("123458"), cdbm.SSHKeyGroupStatusSynced, tnu1.ID)
	assert.NotNil(t, skg3)

	// Build SSHKeyGroupSiteAssociation 3
	skgsa3 := testBuildSSHKeyGroupSiteAssociation(t, dbSession, skg3.ID, st2.ID, cutil.GetPtr("5566"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu1.ID)
	assert.NotNil(t, skgsa3)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, cutil.GetPtr(cdmu.TestCommonCloudInit), false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	// IPXE type with user-data override allowed.
	os2 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-2", tn1, cdbm.OperatingSystemTypeIPXE, true, cutil.GetPtr(cdmu.TestCommonCloudInit), true, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os2)

	// IPXE type with user-data override NOT allowed.
	os3 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-3", tn1, cdbm.OperatingSystemTypeIPXE, false, cutil.GetPtr(cdmu.TestCommonCloudInit), false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os3)

	// IPXE type with user-data override allowed and empty user-data
	osPhoneHome := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-phonehome", tn1, cdbm.OperatingSystemTypeIPXE, true, cutil.GetPtr(""), true, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, osPhoneHome)

	// Same as os3 but deactivated:
	os3off := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-3-deactivated", tn1, cdbm.OperatingSystemTypeIPXE, false, cutil.GetPtr(cdmu.TestCommonCloudInit), false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os3off)
	os3off.IsActive = false
	testUpdateOSIsActive(t, dbSession, os3off)

	// IPXE type with user-data override allowed.
	os4 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-4", tn2, cdbm.OperatingSystemTypeIPXE, true, cutil.GetPtr(cdmu.TestCommonCloudInit), true, cdbm.OperatingSystemStatusReady, tnu2)
	assert.NotNil(t, os4)

	// OS Image type with user-data override allowed.
	os5 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-5", tn2, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu2)
	assert.NotNil(t, os5)
	testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, st2.ID, os5.ID)

	// OS Image type with site association
	os6 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-6", tn2, cdbm.OperatingSystemTypeImage, true, nil, false, cdbm.OperatingSystemStatusReady, tnu2)
	assert.NotNil(t, os6)
	ossa1 := testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, st2.ID, os6.ID)
	assert.NotNil(t, ossa1)

	// OS Image type with site association that doesn't match any instances
	os7 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-7", tn2, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu2)
	assert.NotNil(t, os7)
	ossa2 := testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, st3.ID, os7.ID)
	assert.NotNil(t, ossa2)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	vpc3 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn2, st2, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu2)
	assert.NotNil(t, vpc3)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	subnet3 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn2, vpc3, nil, cdbm.SubnetStatusPending, tnu2)
	assert.NotNil(t, subnet3)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)
	existingPowerProfile := "balanced"
	_, updatePowerProfileErr := dbSession.DB.Exec("UPDATE instance SET power_profile = ? WHERE id = ?", existingPowerProfile, inst1.ID)
	require.NoError(t, updatePowerProfileErr)
	inst1.PowerProfile = &existingPowerProfile

	inst2 := testInstanceBuildInstance(t, dbSession, "test-instance-name-updated", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc2.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst2)

	inst3 := testInstanceBuildInstance(t, dbSession, "test-instance-3", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusTerminating)
	assert.NotNil(t, inst3)

	instConfiguring := testInstanceBuildInstance(t, dbSession, "test-instance-configuring", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusConfiguring)
	assert.NotNil(t, instConfiguring)

	// Instance with iPXE OS type and user-data allowed
	inst4 := testInstanceBuildInstance(t, dbSession, "test-instance-4", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst4)

	// Instance with iPXE OS type and user-data NOT allowed
	inst5 := testInstanceBuildInstance(t, dbSession, "test-instance-5", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os3.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst5)

	inst6 := testInstanceBuildInstance(t, dbSession, "test-instance-6", tn2.ID, ip.ID, st2.ID, &ist2.ID, vpc3.ID, cutil.GetPtr(mc3.ID), &os4.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst6)

	inst7 := testInstanceBuildInstance(t, dbSession, "test-instance-7", tn2.ID, ip.ID, st2.ID, &ist2.ID, vpc3.ID, cutil.GetPtr(mc3.ID), &os5.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst7)

	inst8 := testInstanceBuildInstance(t, dbSession, "test-instance-8", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst8)

	skgia8 := testInstanceBuildSSHKeyGroupInstanceAssociation(t, dbSession, skg1.ID, st1.ID, inst8.ID)
	assert.NotNil(t, skgia8)

	inst9 := testInstanceBuildInstance(t, dbSession, "test-instance-9", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst9)

	inst10 := testInstanceBuildInstance(t, dbSession, "test-instance-10", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst10)

	instsub1 := testInstanceBuildInstanceInterface(t, dbSession, inst1.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusReady)
	assert.NotNil(t, instsub1)

	// Instance update with FNN VPC

	ist4 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-4", st3, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist4)

	mc5 := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc5)

	mcinst5 := testInstanceBuildMachineInstanceType(t, dbSession, mc5, ist4)
	assert.NotNil(t, mcinst5)

	al3 := testInstanceSiteBuildAllocation(t, dbSession, st3, tn2, "test-allocation-3", ipu)
	assert.NotNil(t, al3)

	alc3 := testInstanceSiteBuildAllocationContraints(t, dbSession, al3, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	assert.NotNil(t, alc3)

	alc4 := testInstanceSiteBuildAllocationContraints(t, dbSession, al3, cdbm.AllocationResourceTypeIPBlock, ist4.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	assert.NotNil(t, alc4)

	vpc4 := testInstanceBuildVPC(t, dbSession, "test-vpc-4", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc4)

	controllerVpcSelectionID := uuid.New()
	vpcSelection := testInstanceBuildVPC(t, dbSession, "test-vpc-selection-update", ip, tn1, st3, &controllerVpcSelectionID, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpcSelection)
	controllerVpcSelectionSecondaryID := uuid.New()
	vpcSelectionSecondary := testInstanceBuildVPC(t, dbSession, "test-vpc-selection-update-secondary", ip, tn1, st3, &controllerVpcSelectionSecondaryID, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpcSelectionSecondary)

	mcVpcSelectionUpdate := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcVpcSelectionUpdate)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mcVpcSelectionUpdate, ist4))
	instVpcSelectionUpdate := testInstanceBuildInstance(t, dbSession, "test-instance-vpc-selection-update", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpcSelection.ID, cutil.GetPtr(mcVpcSelectionUpdate.ID), &os2.ID, os2.IpxeScript, cdbm.InstanceStatusReady)
	assert.NotNil(t, instVpcSelectionUpdate)

	mcVpcSelectionPreserve := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcVpcSelectionPreserve)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mcVpcSelectionPreserve, ist4))
	instVpcSelectionPreserve := testInstanceBuildInstance(t, dbSession, "test-instance-vpc-selection-preserve", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpcSelection.ID, cutil.GetPtr(mcVpcSelectionPreserve.ID), &os2.ID, os2.IpxeScript, cdbm.InstanceStatusReady)
	assert.NotNil(t, instVpcSelectionPreserve)
	vpcSelectionIPBlock := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-vpc-selection-update", st3, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.181.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, vpcSelectionIPBlock)
	vpcSelectionPrefix := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-selection-update", st3, tn1, vpcSelection.ID, &vpcSelectionIPBlock.ID, cutil.GetPtr("192.181.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcSelectionPrefix)
	_, selectorErr := cdbm.NewInterfaceDAO(dbSession).Create(context.Background(), nil, cdbm.InterfaceCreateInput{
		InstanceID:      instVpcSelectionPreserve.ID,
		VpcID:           &vpcSelection.ID,
		VpcIPFamilyMode: cutil.GetPtr(cdbm.InterfaceVpcIPFamilyModeIPv4Only),
		VpcPrefixID:     &vpcSelectionPrefix.ID,
		IsPhysical:      true,
		Status:          cdbm.InterfaceStatusReady,
		CreatedBy:       tnu1.ID,
	})
	require.NoError(t, selectorErr)

	// VPC prefix
	ipb1 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb2", st3, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.168.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu2)
	assert.NotNil(t, ipb1)
	vpcPrefix1 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-1", st3, tn1, vpc4.ID, &ipb1.ID, cutil.GetPtr("192.168.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefix1)
	vpc4Site2 := testInstanceBuildVPC(t, dbSession, "test-vpc-4-site-2", ip, tn1, st2, nil, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc4Site2)
	ipbSite2 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-site2-update", st2, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.173.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, ipbSite2)
	vpcPrefixSite2 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-site2-update", st2, tn1, vpc4Site2.ID, &ipbSite2.ID, cutil.GetPtr("192.173.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefixSite2)
	vpc4Site3Secondary := testInstanceBuildVPC(t, dbSession, "test-vpc-4-site-3-secondary", ip, tn1, st3, nil, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc4Site3Secondary)
	ipbSite3Secondary := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb-site3-secondary-update", st3, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.174.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
	assert.NotNil(t, ipbSite3Secondary)
	vpcPrefixSite3Secondary := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-site3-secondary-update", st3, tn1, vpc4Site3Secondary.ID, &ipbSite3Secondary.ID, cutil.GetPtr("192.174.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefixSite3Secondary)

	ipb2 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb2", st3, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.172.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu2)
	assert.NotNil(t, ipb2)
	vpcPrefix2 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-2", st3, tn1, vpc4.ID, &ipb2.ID, cutil.GetPtr("192.172.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefix2)

	// Use for updating the instance
	ipb3 := common.TestBuildVpcPrefixIPBlock(t, dbSession, "testipb3", st3, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "192.152.0.0", 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu2)
	assert.NotNil(t, ipb3)
	vpcPrefix3 := common.TestBuildVPCPrefix(t, dbSession, "test-vpcprefix-3", st3, tn1, vpc4.ID, &ipb3.ID, cutil.GetPtr("192.152.0.0/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
	assert.NotNil(t, vpcPrefix3)

	inst11 := testInstanceBuildInstance(t, dbSession, "test-instance-11", tn1.ID, ip.ID, st3.ID, &ist1.ID, vpc4.ID, cutil.GetPtr(mc1.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst11)

	inst12 := testInstanceBuildInstance(t, dbSession, "test-instance-12", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)

	instifc1 := testInstanceBuildInstanceInterface(t, dbSession, inst12.ID, nil, &vpcPrefix1.ID, nil, cdbm.InterfaceStatusReady)
	assert.NotNil(t, instifc1)

	instifc2 := testInstanceBuildInstanceInterface(t, dbSession, inst12.ID, nil, &vpcPrefix2.ID, nil, cdbm.InterfaceStatusReady)
	assert.NotNil(t, instifc2)

	mc7 := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc7)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc7, ist4))
	mc8 := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc8)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc8, ist4))
	mc9 := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc9)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc9, ist4))

	buildNsgPropagationMultiVpcPair := func(primaryName, secondaryName, primaryPrefixName, secondaryPrefixName, primaryCIDR, secondaryCIDR string) (*cdbm.Vpc, *cdbm.Vpc, *cdbm.VpcPrefix, *cdbm.VpcPrefix) {
		primary := testInstanceBuildVPC(t, dbSession, primaryName, ip, tn1, st3, nil, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
		secondary := testInstanceBuildVPC(t, dbSession, secondaryName, ip, tn1, st3, nil, nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
		primaryIPB := common.TestBuildVpcPrefixIPBlock(t, dbSession, primaryPrefixName+"-ipb", st3, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, primaryCIDR, 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
		secondaryIPB := common.TestBuildVpcPrefixIPBlock(t, dbSession, secondaryPrefixName+"-ipb", st3, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, secondaryCIDR, 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
		primaryPrefix := common.TestBuildVPCPrefix(t, dbSession, primaryPrefixName, st3, tn1, primary.ID, &primaryIPB.ID, cutil.GetPtr(primaryCIDR+"/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
		secondaryPrefix := common.TestBuildVPCPrefix(t, dbSession, secondaryPrefixName, st3, tn1, secondary.ID, &secondaryIPB.ID, cutil.GetPtr(secondaryCIDR+"/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
		return primary, secondary, primaryPrefix, secondaryPrefix
	}

	vpcPrimaryFull, vpcSecondaryFull, vpcPrefixPrimaryFull, vpcPrefixSecondaryFull := buildNsgPropagationMultiVpcPair("test-update-vpc-primary-full", "test-update-vpc-secondary-full", "test-update-vpcprefix-full-primary", "test-update-vpcprefix-full-secondary", "192.175.0.0", "192.176.0.0")
	vpcPrimaryNone, vpcSecondaryNone, vpcPrefixPrimaryNone, vpcPrefixSecondaryNone := buildNsgPropagationMultiVpcPair("test-update-vpc-primary-none", "test-update-vpc-secondary-none", "test-update-vpcprefix-none-primary", "test-update-vpcprefix-none-secondary", "192.177.0.0", "192.178.0.0")
	vpcPrimaryPartial, vpcSecondaryPartial, vpcPrefixPrimaryPartial, vpcPrefixSecondaryPartial := buildNsgPropagationMultiVpcPair("test-update-vpc-primary-partial", "test-update-vpc-secondary-partial", "test-update-vpcprefix-partial-primary", "test-update-vpcprefix-partial-secondary", "192.179.0.0", "192.180.0.0")

	instUpdateFull := testInstanceBuildInstance(t, dbSession, "test-instance-update-vpc-full", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpcPrimaryFull.ID, cutil.GetPtr(mc7.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instUpdateFull)
	instUpdateNone := testInstanceBuildInstance(t, dbSession, "test-instance-update-vpc-none", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpcPrimaryNone.ID, cutil.GetPtr(mc8.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instUpdateNone)
	instUpdateRebootFull := testInstanceBuildInstance(t, dbSession, "test-instance-update-vpc-reboot-full", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpcPrimaryFull.ID, cutil.GetPtr(mc7.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instUpdateRebootFull)
	instUpdatePartial := testInstanceBuildInstance(t, dbSession, "test-instance-update-vpc-partial", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpcPrimaryPartial.ID, cutil.GetPtr(mc9.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instUpdatePartial)

	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instUpdateFull.ID, nil, &vpcPrefixPrimaryFull.ID, nil, cdbm.InterfaceStatusReady))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instUpdateNone.ID, nil, &vpcPrefixPrimaryNone.ID, nil, cdbm.InterfaceStatusReady))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instUpdateRebootFull.ID, nil, &vpcPrefixPrimaryFull.ID, nil, cdbm.InterfaceStatusReady))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instUpdatePartial.ID, nil, &vpcPrefixPrimaryPartial.ID, nil, cdbm.InterfaceStatusReady))

	nsgTenant1Site3 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-4", tn1, st3, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site3)

	setVpcProp := func(vpc *cdbm.Vpc, related []string, unprop []string, status corev1.NetworkSecurityGroupPropagationStatus) {
		vpc.NetworkSecurityGroupID = &nsgTenant1Site3.ID
		vpc.NetworkSecurityGroupPropagationDetails = &cdbm.NetworkSecurityGroupPropagationDetails{
			NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{
				Id:                      vpc.ID.String(),
				Status:                  status,
				RelatedInstanceIds:      related,
				UnpropagatedInstanceIds: unprop,
			},
		}
		testUpdateVPC(t, dbSession, vpc)
	}

	setVpcProp(vpcPrimaryFull, []string{instUpdateFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcSecondaryFull, []string{instUpdateFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcPrimaryFull, []string{instUpdateRebootFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcSecondaryFull, []string{instUpdateRebootFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcPrimaryNone, []string{instUpdateNone.ID.String()}, []string{instUpdateNone.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)
	setVpcProp(vpcSecondaryNone, []string{instUpdateNone.ID.String()}, []string{instUpdateNone.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)
	setVpcProp(vpcPrimaryPartial, []string{instUpdatePartial.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcSecondaryPartial, []string{instUpdatePartial.ID.String()}, []string{instUpdatePartial.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)

	// Add Network DPU capability to Instance Type
	common.TestBuildMachineCapability(t, dbSession, nil, &ist4.ID, cdbm.MachineCapabilityTypeNetwork, "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(2), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil)

	issue4908Device := cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller")
	issue4908DeviceInstance := cutil.GetPtr(0)
	issue4908VFID := cutil.GetPtr(1)

	issue4908AddVFMachine := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, issue4908AddVFMachine, ist4))
	issue4908AddVFInstance := testInstanceBuildInstance(t, dbSession, "issue-4908-add-vf", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(issue4908AddVFMachine.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	issue4908AddVFPF := testInstanceBuildInterface(t, dbSession, issue4908AddVFInstance.ID, nil, &vpcPrefix1.ID, issue4908Device, issue4908DeviceInstance, nil, true, cdbm.InterfaceStatusReady, tnu1)
	testUpdateInterfaceWithIPs(t, dbSession, issue4908AddVFPF, []string{"192.168.0.1"})

	issue4908RemoveMachine := testInstanceBuildMachine(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, issue4908RemoveMachine, ist4))
	issue4908RemoveInstance := testInstanceBuildInstance(t, dbSession, "issue-4908-remove-vf", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(issue4908RemoveMachine.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	issue4908RemovePF := testInstanceBuildInterface(t, dbSession, issue4908RemoveInstance.ID, nil, &vpcPrefix1.ID, issue4908Device, issue4908DeviceInstance, nil, true, cdbm.InterfaceStatusReady, tnu1)
	testUpdateInterfaceWithIPs(t, dbSession, issue4908RemovePF, []string{"192.168.0.3"})
	issue4908RemoveVF := testInstanceBuildInterface(t, dbSession, issue4908RemoveInstance.ID, nil, &vpcPrefixSite3Secondary.ID, issue4908Device, issue4908DeviceInstance, issue4908VFID, false, cdbm.InterfaceStatusReady, tnu1)
	testUpdateInterfaceWithIPs(t, dbSession, issue4908RemoveVF, []string{"192.174.0.1"})

	inst13 := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-update", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)

	// Add NVLink GPU capability to Machine
	common.TestBuildMachineCapability(t, dbSession, &mc5.ID, nil, cdbm.MachineCapabilityTypeGPU, "NVIDIA GB200", nil, nil, cutil.GetPtr("NVIDIA"), cutil.GetPtr(4), cutil.GetPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil)

	nvllp1 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-1", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, st3, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp1)

	nvllp2 := testBuildNVLinkLogicalPartition(t, dbSession, "test-nvllp-2", cutil.GetPtr("Test NVLink Logical Partition"), tnOrg1, st3, tn1, cutil.GetPtr(cdbm.NVLinkLogicalPartitionStatusReady), false)
	assert.NotNil(t, nvllp2)

	instnvlifc1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instnvlifc1)

	instnvlifc2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instnvlifc2)

	instnvlifc3 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instnvlifc3)

	instnvlifc4 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 3, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instnvlifc4)

	// Dedicated instances for four-NVLink → two-NVLink subset updates (avoid mutating inst13 used by later tests).
	instSubsetFourToTwoA := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-four-to-two-a", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instSubsetFourToTwoA)
	instSubsetANvl1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoA.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetANvl1)
	instSubsetANvl2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoA.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetANvl2)
	instSubsetANvl3 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoA.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetANvl3)
	instSubsetANvl4 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoA.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 3, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetANvl4)

	instSubsetFourToTwoB := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-four-to-two-b", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instSubsetFourToTwoB)
	instSubsetBNvl1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoB.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetBNvl1)
	instSubsetBNvl2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoB.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetBNvl2)
	instSubsetBNvl3 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoB.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetBNvl3)
	instSubsetBNvl4 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instSubsetFourToTwoB.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 3, cdbm.NVLinkInterfaceStatusReady)
	assert.NotNil(t, instSubsetBNvl4)

	// Instances with four Pending NVLink interfaces — isolate stale vs fresh `updated` timestamp behavior across subtests.
	inst13PendingStale := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-pending-stale", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst13PendingStale)

	inst13psNvl1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingStale.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13psNvl1)
	inst13psNvl2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingStale.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13psNvl2)
	inst13psNvl3 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingStale.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13psNvl3)
	inst13psNvl4 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingStale.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 3, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13psNvl4)

	inst13PendingFresh := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-pending-fresh", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst13PendingFresh)

	inst13pfNvl1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingFresh.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13pfNvl1)
	inst13pfNvl2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingFresh.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13pfNvl2)
	inst13pfNvl3 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingFresh.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13pfNvl3)
	inst13pfNvl4 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, inst13PendingFresh.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 3, cdbm.NVLinkInterfaceStatusPending)
	assert.NotNil(t, inst13pfNvl4)

	// Instance with four Deleting NVLink rows (devices 0–3 across nvllp1/nvllp2); same multiset re-request must re-issue new Pending rows.
	instFourDeletingNVLink := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-four-all-deleting", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instFourDeletingNVLink)

	inst4DelNvl1 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instFourDeletingNVLink.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusDeleting)
	inst4DelNvl2 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instFourDeletingNVLink.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 1, cdbm.NVLinkInterfaceStatusDeleting)
	inst4DelNvl3 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instFourDeletingNVLink.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 2, cdbm.NVLinkInterfaceStatusDeleting)
	inst4DelNvl4 := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instFourDeletingNVLink.ID, nvllp2.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 3, cdbm.NVLinkInterfaceStatusDeleting)
	assert.NotNil(t, inst4DelNvl1)
	assert.NotNil(t, inst4DelNvl2)
	assert.NotNil(t, inst4DelNvl3)
	assert.NotNil(t, inst4DelNvl4)

	// Dedicated instances for NVLink Error → re-issue (within grace vs stale Updated).
	instNvlinkErrorGrace := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-error-grace", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instNvlinkErrorGrace)
	nvlinkErrGraceIfc := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instNvlinkErrorGrace.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusError)
	assert.NotNil(t, nvlinkErrGraceIfc)

	instNvlinkErrorStale := testInstanceBuildInstance(t, dbSession, "test-instance-nvlink-error-stale", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc5.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instNvlinkErrorStale)
	nvlinkErrStaleIfc := testInstanceBuildInstanceNVLinkInterface(t, dbSession, st3.ID, instNvlinkErrorStale.ID, nvllp1.ID, cutil.GetPtr(uuid.New()), cutil.GetPtr("NVIDIA GB200"), 0, cdbm.NVLinkInterfaceStatusError)
	assert.NotNil(t, nvlinkErrStaleIfc)

	mc6 := testInstanceBuildMachine(t, dbSession, ip.ID, st2.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc6)

	mcinst6 := testInstanceBuildMachineInstanceType(t, dbSession, mc6, ist2)
	assert.NotNil(t, mcinst6)

	inst14 := testInstanceBuildInstance(t, dbSession, "test-instance-14", tn2.ID, ip.ID, st2.ID, &ist2.ID, vpc2.ID, cutil.GetPtr(mc6.ID), &os4.ID, nil, cdbm.InstanceStatusError)
	assert.NotNil(t, inst14)

	insDAO := cdbm.NewInstanceDAO(dbSession)
	_, err := insDAO.Update(context.Background(), nil, cdbm.InstanceUpdateInput{
		InstanceID:                inst14.ID,
		InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{IsMissingOnSite: cutil.GetPtr(true)},
	})
	assert.NoError(t, err)

	// Fixtures for NSG-specific testing

	// Associate tenant 1 with site 2
	ts2t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2t1)

	// Associate tenant 2 with site 1 and site 3
	ts1t2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn2.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1t2)
	ts3t2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn2.ID, st3.ID, tnu1.ID)
	assert.NotNil(t, ts3t2)
	ts3t1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, st3.ID, tnu1.ID)
	assert.NotNil(t, ts3t1)

	// NSG for tenant 1 on site 1
	nsgTenant1Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site1)

	// NSG for tenant 1 on site 2
	nsgTenant1Site2 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-2", tn1, st2, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant1Site2)

	// NSG for tenant 2 on site 1
	nsgTenant2Site1 := testBuildNetworkSecurityGroup(t, dbSession, "test-nsg-3", tn2, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsgTenant2Site1)

	// InfiniBand Interface Support
	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp1)

	ibi1 := testInstanceBuildIBInterface(t, dbSession, inst1, st1, ibp1, 0, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibi1)

	// Extra InfiniBand Partitions for updating instance with InfiniBand Interfaces

	// Add InfiniBand capability to Instance Type
	common.TestBuildMachineCapability(t, dbSession, nil, &ist1.ID, cdbm.MachineCapabilityTypeInfiniBand, "MT28908 Family [ConnectX-6]", nil, nil, cutil.GetPtr("Mellanox Technologies"), cutil.GetPtr(5), cutil.GetPtr(cdbm.MachineCapabilityDeviceType("")), nil)

	ibp2 := testBuildIBPartition(t, dbSession, "test-ibp-2", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp2)

	ibp3 := testBuildIBPartition(t, dbSession, "test-ibp-2", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp3)

	ibp4 := testBuildIBPartition(t, dbSession, "test-ibp-2", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp4)

	ibp6 := testBuildIBPartition(t, dbSession, "test-ibp-ibdup-4slot", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp6)

	// Extra InfiniBand Partitions for updating instance with InfiniBand Interfaces
	ibp5 := testBuildIBPartition(t, dbSession, "test-ibp-2", tnOrg1, st2, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp5)

	// Instance on st2 under tn1 using ist2 (which has no InfiniBand
	// MachineCapability), paired with ibp5 (also on st2) so the partition
	// site check passes and the handler reaches the capability-check
	// branch.
	instNoIB := testInstanceBuildInstance(t, dbSession, "test-instance-no-ib", tn1.ID, ip.ID, st2.ID, &ist2.ID, vpc4Site2.ID, cutil.GetPtr(mc3.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instNoIB)

	// Instance with four READY InfiniBand interfaces — distinct (partition ID, device, device instance) per row;
	// used for READY multi-interface no-op tests without sharing state with IB replace tests on inst1.
	mcIbDup := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcIbDup)

	instIbReadyDup := testInstanceBuildInstance(t, dbSession, "test-instance-ib-ready-four", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mcIbDup.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instIbReadyDup)

	ibiIbDup0 := testInstanceBuildIBInterface(t, dbSession, instIbReadyDup, st1, ibp2, 0, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibiIbDup0)

	ibiIbDup1 := testInstanceBuildIBInterface(t, dbSession, instIbReadyDup, st1, ibp3, 1, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibiIbDup1)

	ibiIbDup2 := testInstanceBuildIBInterface(t, dbSession, instIbReadyDup, st1, ibp4, 2, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibiIbDup2)

	ibiIbDup3 := testInstanceBuildIBInterface(t, dbSession, instIbReadyDup, st1, ibp6, 3, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibiIbDup3)

	// Instance with one READY InfiniBand — used only for partition-in-map-key behavior (same device/instance, different partition is not a no-op).
	mcIbPartitionSwap := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcIbPartitionSwap)

	instIbPartitionSwap := testInstanceBuildInstance(t, dbSession, "test-instance-ib-partition-key", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mcIbPartitionSwap.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instIbPartitionSwap)

	ibiIbPartitionSwap := testInstanceBuildIBInterface(t, dbSession, instIbPartitionSwap, st1, ibp1, 0, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibiIbPartitionSwap)

	// Instances with InfiniBand interface in Error — re-issue paths: Error within InfiniBandInterfaceStatusSyncGraceWindow (explicit branch) vs stale Updated (grace else branch).
	mcIbIfcErrGrace := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcIbIfcErrGrace)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mcIbIfcErrGrace, ist1))
	instIbIfcErrorGrace := testInstanceBuildInstance(t, dbSession, "test-instance-ib-ifc-error-grace", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mcIbIfcErrGrace.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instIbIfcErrorGrace)
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instIbIfcErrorGrace.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusReady))
	ibiIbErrorGrace := testInstanceBuildIBInterface(t, dbSession, instIbIfcErrorGrace, st1, ibp2, 0, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusError), false)
	assert.NotNil(t, ibiIbErrorGrace)

	mcIbIfcErrStale := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mcIbIfcErrStale)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mcIbIfcErrStale, ist1))
	instIbIfcErrorStale := testInstanceBuildInstance(t, dbSession, "test-instance-ib-ifc-error-stale", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mcIbIfcErrStale.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instIbIfcErrorStale)
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instIbIfcErrorStale.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusReady))
	ibiIbErrorStale := testInstanceBuildIBInterface(t, dbSession, instIbIfcErrorStale, st1, ibp2, 0, true, nil, cutil.GetPtr(cdbm.InfiniBandInterfaceStatusError), false)
	assert.NotNil(t, ibiIbErrorStale)

	// Instance for DPU Extension Service Deployment update
	mc15 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc15)

	inst15 := testInstanceBuildInstance(t, dbSession, "test-instance-des-preserve", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc15.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst15)

	des1 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "1.0.0", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)

	// Add a second version to des1
	des1.ActiveVersions = append(des1.ActiveVersions, "2.0.0")
	testUpdateDESActiveVersions(t, dbSession, des1)

	des2 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-2", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "1.5.0", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des2)

	des3 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-3", model.DpuExtensionServiceTypeKubernetesPod, tn2, st2, "1.0.0", cdbm.DpuExtensionServiceStatusReady, tnu2)
	assert.NotNil(t, des3)

	des4 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-4", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "1.0.0", cdbm.DpuExtensionServiceStatusReady, tnu2)
	assert.NotNil(t, des4)

	desd1 := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des1, inst15.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu1)
	assert.NotNil(t, desd1)

	desd2 := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des2, inst15.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusTerminating, tnu1)
	assert.NotNil(t, desd2)

	// Instance to test preservation of existing DPU Extension Service Deployments when omitted from request
	mc16 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc16)

	inst16 := testInstanceBuildInstance(t, dbSession, "test-instance-des-update", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc16.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst16)

	desd16 := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des1, inst16.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu1)
	assert.NotNil(t, desd16)

	// Instance to test creation of new DPU Extension Service Deployments when empty array is provided in request
	mc17 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc17)

	inst17 := testInstanceBuildInstance(t, dbSession, "test-instance-des-update", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc17.ID), &os2.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst17)

	desd17 := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des1, inst17.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu1)
	assert.NotNil(t, desd17)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls to site(s)
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st1.ID.String()] = tsc
	scp.IDClientMap[st3.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID, bool, bool) error"), mock.AnythingOfType("uuid.UUID"), true, mock.AnythingOfType("bool")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"RebootInstanceV2", mock.Anything).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateInstance", mock.Anything).Return(wrun, nil)

	// Mock per-Site client for st3
	tst3 := &tmocks.Client{}

	scp.IDClientMap[st2.ID.String()] = tst3

	// Mock timeout error
	wruntimeout := &tmocks.WorkflowRun{}
	wruntimeout.On("GetID").Return("test-workflow-timeout-id")

	wruntimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tst3.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"RebootInstanceV2", mock.Anything).Return(wruntimeout, nil)

	tst3.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"UpdateInstance", mock.Anything).Return(wruntimeout, nil)

	tst3.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	ifcDAO := cdbm.NewInterfaceDAO(dbSession)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	type args struct {
		reqData                  *model.APIInstanceUpdateRequest
		reqOrg                   string
		reqUser                  *cdbm.User
		reqInstance              string
		cleanInstanceToStatus    string
		respNoOfInterfaces       *int
		respNoOfNVLinkInterfaces *int
		ethInterfacesToDelete    []cdbm.Interface
		ibInterfaceToDelete      []cdbm.InfiniBandInterface
		// Expect these InfiniBand interface rows to stay Ready with no Pending rows created on the Instance
		// when the request matches on (partition ID, device, device instance) — READY no-op IB update path.
		expectInfiniBandInterfacesRemainReady []uuid.UUID
		nvlinkInterfacesToDelete              []cdbm.NVLinkInterface
		respCode                              int
		respUserDataContains                  *string
		respUserData                          *string
		respMessage                           *string
		expectedDesdIDs                       []string
		expectedSecondaryVpcIDs               []string
		expectedNetworkSecurityGroupInherited *bool
		expectedPropagationDetailedStatus     *string
		expectedPropagationStatus             *string
		expectedSitePowerProfile              *string
		// When true, only assert len(siteReq.Config.Nvlink.GpuConfigs) matches the request (e.g. NVLink no-op where workflow uses DB order).
		nvLinkGpuConfigsVerifyCountOnly bool
		// When non-nil, expected len(siteReq.Config.Nvlink.GpuConfigs) for verifySiteControllerRequest (default: len(reqData.NVLinkInterfaces)).
		expectSiteNVLinkGpuConfigCount *int
		// When true with nvlinkInterfacesToDelete, still assert those rows are Deleting but skip Pending-row count/order checks.
		nvLinkSkipPendingDBAssertions bool
		// Optional hook after building the echo context and before Handle (e.g. adjust DB timestamps for time-sensitive branches).
		beforeHandle           func(t *testing.T)
		ethernetReconciliation *ethernetReconciliationExpectation
	}

	tests := []struct {
		name                        string
		fields                      fields
		args                        args
		wantErr                     bool
		expectedInterfaceVpcID      *uuid.UUID
		expectedControllerVpcIDs    map[string]uuid.UUID
		expectedResolvedVpcPrefixID *uuid.UUID
		verifySiteControllerRequest bool
		verifyChildSpanner          bool
	}{
		{
			name: "test Instance update preserves persisted power profile when omitted",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("Test Instance preserved power profile"),
					IpxeScript: os2.IpxeScript,
				},
				reqInstance:              inst1.ID.String(),
				cleanInstanceToStatus:    inst1.Status,
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
				expectedSitePowerProfile: &existingPowerProfile,
			},
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update marks the Instance configuring for a SpectrumX-only update",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					// The iPXE-based Operating System rejects an unset ipxeScript, so carry it
					// through to keep this case independent of the surrounding table order.
					IpxeScript: os2.IpxeScript,
					SpectrumXAttachments: []model.APISpectrumXAttachmentCreateOrUpdateRequest{
						{
							SpectrumXPartitionID: uuid.NewString(),
							Device:               "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC",
							DeviceInstance:       cutil.GetPtr(0),
							AttachmentType:       model.SpectrumXAttachmentTypePhysical,
						},
					},
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			verifySiteControllerRequest: true,
		},
		{
			name: "test Instance update rejects power profile when DPS power management is disabled",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					PowerProfile: cutil.GetPtr("performance"),
				},
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusPreconditionFailed,
				respMessage: cutil.GetPtr("Site does not have DPS power management enabled"),
			},
		},
		{
			name: "test Instance update API endpoint success with InfiniBand Interfaces no-op when request matches READY rows on partition, device and device instance",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance IB no-op"),
					Description: cutil.GetPtr("Test Instance Description"),
					IpxeScript:  os2.IpxeScript,
					Labels: map[string]string{
						"new_key": "new_value",
					},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp1.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:                           inst1.ID.String(),
				cleanInstanceToStatus:                 inst1.Status,
				reqOrg:                                tnOrg1,
				reqUser:                               tnu1,
				respCode:                              http.StatusOK,
				expectInfiniBandInterfacesRemainReady: []uuid.UUID{ibi1.ID},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with four InfiniBand Interfaces no-op when request matches READY rows on partition, device and device instance",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("Test Instance IB no-op four"),
					IpxeScript: os2.IpxeScript,
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp2.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
						{
							InfiniBandPartitionID: ibp3.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        1,
							IsPhysical:            true,
						},
						{
							InfiniBandPartitionID: ibp4.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        2,
							IsPhysical:            true,
						},
						{
							InfiniBandPartitionID: ibp6.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        3,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:                           instIbReadyDup.ID.String(),
				cleanInstanceToStatus:                 instIbReadyDup.Status,
				reqOrg:                                tnOrg1,
				reqUser:                               tnu1,
				respCode:                              http.StatusOK,
				expectInfiniBandInterfacesRemainReady: []uuid.UUID{ibiIbDup0.ID, ibiIbDup1.ID, ibiIbDup2.ID, ibiIbDup3.ID},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with InfiniBand Interface when partition differs on same device instance (not a no-op; keyed by partition+device+instance)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance IB partition swap same slot"),
					Description: cutil.GetPtr("Test Instance Description"),
					IpxeScript:  os2.IpxeScript,
					Labels: map[string]string{
						"new_key": "new_value",
					},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp2.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:           instIbPartitionSwap.ID.String(),
				cleanInstanceToStatus: instIbPartitionSwap.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				ibInterfaceToDelete:   []cdbm.InfiniBandInterface{*ibiIbPartitionSwap},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update re-issues InfiniBand when most recent row is Error and Updated within sync grace window",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("Test Instance IB error re-issue grace"),
					IpxeScript: os2.IpxeScript,
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp2.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:           instIbIfcErrorGrace.ID.String(),
				cleanInstanceToStatus: instIbIfcErrorGrace.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				ibInterfaceToDelete:   []cdbm.InfiniBandInterface{*ibiIbErrorGrace},
				beforeHandle: func(t *testing.T) {
					recent := time.Now().UTC().Add(-45 * time.Second)
					_, err := dbSession.DB.Exec(
						"UPDATE infiniband_interface SET updated = ? WHERE id = ?",
						recent,
						ibiIbErrorGrace.ID,
					)
					require.NoError(t, err)
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update re-issues InfiniBand when most recent row is Error but Updated older than sync grace window",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("Test Instance IB error re-issue stale"),
					IpxeScript: os2.IpxeScript,
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp2.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:           instIbIfcErrorStale.ID.String(),
				cleanInstanceToStatus: instIbIfcErrorStale.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				ibInterfaceToDelete:   []cdbm.InfiniBandInterface{*ibiIbErrorStale},
				beforeHandle: func(t *testing.T) {
					stale := time.Now().UTC().Add(-2 * time.Minute)
					_, err := dbSession.DB.Exec(
						"UPDATE infiniband_interface SET updated = ? WHERE id = ?",
						stale,
						ibiIbErrorStale.ID,
					)
					require.NoError(t, err)
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with InfiniBand Interfaces",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance"),
					Description: cutil.GetPtr("Test Instance Description"),
					IpxeScript:  os2.IpxeScript,
					Labels: map[string]string{
						"new_key": "new_value",
					},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp2.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
						{
							InfiniBandPartitionID: ibp3.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        1,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				ibInterfaceToDelete:   []cdbm.InfiniBandInterface{*ibi1},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint failure due to InfiniBand Partition Site Mismatch",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Failure IB"),
					Description: cutil.GetPtr("Test Instance Description"),
					IpxeScript:  os2.IpxeScript,
					Labels: map[string]string{
						"new_key": "new_value",
					},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp5.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:           inst2.ID.String(),
				cleanInstanceToStatus: inst2.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint failure with InfiniBand Interfaces as InstanceType doesn't have InfiniBand Capability",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Failure IB No Cap"),
					Description: cutil.GetPtr("Test Instance Description"),
					IpxeScript:  os2.IpxeScript,
					Labels: map[string]string{
						"new_key": "new_value",
					},
					InfiniBandInterfaces: []model.APIInfiniBandInterfaceCreateOrUpdateRequest{
						{
							InfiniBandPartitionID: ibp5.ID.String(),
							Device:                "MT28908 Family [ConnectX-6]",
							Vendor:                cutil.GetPtr("Mellanox Technologies"),
							DeviceInstance:        0,
							IsPhysical:            true,
						},
					},
				},
				reqInstance:           instNoIB.ID.String(),
				cleanInstanceToStatus: instNoIB.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success even when Instance already in configuring state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("test-instance-configuring"),
					IpxeScript: os2.IpxeScript,
				},
				reqInstance:           instConfiguring.ID.String(),
				cleanInstanceToStatus: instConfiguring.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success preserves DPU Extension Service Deployments when omitted from request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("test-instance-des-preserve-renamed"),
					IpxeScript: os2.IpxeScript,
				},
				reqInstance:           inst16.ID.String(),
				cleanInstanceToStatus: inst16.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				expectedDesdIDs:       []string{desd16.ID.String()},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success clears DPU Extension Service Deployments when explicitly empty in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:                           cutil.GetPtr("test-instance-des-cleared"),
					IpxeScript:                     os2.IpxeScript,
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{},
				},
				reqInstance:           inst17.ID.String(),
				cleanInstanceToStatus: inst17.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				expectedDesdIDs:       []string{},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with DPU Extension Service Deployments",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("test-instance-des-update"),
					Description: cutil.GetPtr("Test Instance updated description"),
					IpxeScript:  os2.IpxeScript,
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: des1.ID.String(),
							Version:               "2.0.0",
						},
						{
							DpuExtensionServiceID: des2.ID.String(),
							Version:               "1.5.0",
						},
					},
				},
				reqInstance:           inst15.ID.String(),
				cleanInstanceToStatus: inst15.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint failure with DPU Extension Service from wrong tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("test-instance-des-wrong-tenant"),
					IpxeScript: os2.IpxeScript,
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: des3.ID.String(),
							Version:               "1.0.0",
						},
					},
				},
				reqInstance:           inst15.ID.String(),
				cleanInstanceToStatus: inst15.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure with invalid DPU Extension Service version",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("test-instance-des-invalid-version"),
					IpxeScript: os2.IpxeScript,
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: des1.ID.String(),
							Version:               "99.0.0",
						},
					},
				},
				reqInstance:           inst15.ID.String(),
				cleanInstanceToStatus: inst15.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure with invalid DPU Extension Service ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("test-instance-des-invalid-id"),
					IpxeScript: os2.IpxeScript,
					DpuExtensionServiceDeployments: []model.APIDpuExtensionServiceDeploymentRequest{
						{
							DpuExtensionServiceID: uuid.New().String(),
							Version:               "1.0.0",
						},
					},
				},
				reqInstance:           inst15.ID.String(),
				cleanInstanceToStatus: inst15.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance update SSH keygroup from nothing to something success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:           cutil.GetPtr("Test Instance"),
					Description:    cutil.GetPtr("Test Instance Description"),
					IpxeScript:     os2.IpxeScript,
					SSHKeyGroupIDs: []string{skg1.ID.String()},
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance update with NSG for - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					NetworkSecurityGroupID: &nsgTenant1Site1.ID,
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance update with NSG for wrong site - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					NetworkSecurityGroupID: &nsgTenant1Site2.ID,
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusForbidden,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance update with NSG for wrong Tenant - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					NetworkSecurityGroupID: &nsgTenant2Site1.ID,
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusForbidden,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance update with NSG not found - fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					NetworkSecurityGroupID: cutil.GetPtr(uuid.NewString()),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance update clear NSG - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:                   cutil.GetPtr("Test Instance"),
					Description:            cutil.GetPtr("Test Instance Description"),
					NetworkSecurityGroupID: cutil.GetPtr(""),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance update SSH Key Group replace existing SSH Key Groups success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:           cutil.GetPtr("Test Instance 8"),
					Description:    cutil.GetPtr("Test Instance Description"),
					IpxeScript:     os2.IpxeScript,
					SSHKeyGroupIDs: []string{skg1.ID.String()},
				},
				reqInstance:           inst8.ID.String(),
				cleanInstanceToStatus: inst8.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update SSH Key Group no existing SSH Key Groups success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:           cutil.GetPtr("Test Instance 9"),
					Description:    cutil.GetPtr("Test Instance Description"),
					IpxeScript:     os2.IpxeScript,
					SSHKeyGroupIDs: []string{skg2.ID.String()},
				},
				reqInstance:           inst9.ID.String(),
				cleanInstanceToStatus: inst9.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update SSH Key Group remove existing SSH Key Groups success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:           cutil.GetPtr("Test Instance 10"),
					Description:    cutil.GetPtr("Test Instance Description"),
					IpxeScript:     os2.IpxeScript,
					SSHKeyGroupIDs: []string{},
				},
				reqInstance:           inst10.ID.String(),
				cleanInstanceToStatus: inst10.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update SSH keygroup for wrong site fail",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:           cutil.GetPtr("Test Instance"),
					Description:    cutil.GetPtr("Test Instance Description"),
					IpxeScript:     os2.IpxeScript,
					SSHKeyGroupIDs: []string{skg3.ID.String()},
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance update API endpoint fails due to name uniqueness",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("test-instance-name-updated"),
					IpxeScript: os2.IpxeScript,
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusConflict,
			},
			wantErr: false,
		},
		{
			name: "test Instance update success with same name",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name: cutil.GetPtr("test-instance-1"),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success with reboot trigger",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					TriggerReboot:        cutil.GetPtr(true),
					RebootWithCustomIpxe: cutil.GetPtr(true),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success with OS change and user-data update trigger",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					UserData:   cutil.GetPtr(cdmu.TestCommonCloudInit + "\n#comment-2a69bc94-5e76-11ef-90ac-3f5706c2f872"),
				},
				reqInstance:           inst4.ID.String(),
				cleanInstanceToStatus: inst4.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				respUserDataContains:  cutil.GetPtr("2a69bc94-5e76-11ef-90ac-3f5706c2f872"),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success with OS change, custom user-data, phone-home disabled, update trigger",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript:       cutil.GetPtr(common.DefaultIpxeScript),
					UserData:         cutil.GetPtr(cdmu.TestCommonCloudInit + cdmu.TestCommonPhoneHomeSegment + "\nsome_key: some_value\n"),
					PhoneHomeEnabled: cutil.GetPtr(false),
				},
				reqInstance:           inst4.ID.String(),
				cleanInstanceToStatus: inst4.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
				respUserData:          cutil.GetPtr(cdmu.TestCommonCloudInit + "some_key: some_value\n"),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success with OS change and NO user-data update trigger",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
				},
				reqInstance:           inst4.ID.String(),
				cleanInstanceToStatus: inst4.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success with OS change and NO user-data update trigger for OS instance without user-data allowed",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
				},
				reqInstance:           inst5.ID.String(),
				cleanInstanceToStatus: inst5.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure with OS change and user-data update trigger for OS instance without user-data allowed",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					UserData:   cutil.GetPtr(cdmu.TestCommonCloudInit),
				},
				reqInstance:           inst5.ID.String(),
				cleanInstanceToStatus: inst5.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure with OS change of bogus OS ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript:        cutil.GetPtr(common.DefaultIpxeScript),
					UserData:          cutil.GetPtr(cdmu.TestCommonCloudInit),
					OperatingSystemID: cutil.GetPtr(uuid.NewString()),
				},
				reqInstance:           inst5.ID.String(),
				cleanInstanceToStatus: inst5.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint successful change of OS",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					OperatingSystemID: cutil.GetPtr(os3.ID.String()),
				},
				reqInstance:           inst5.ID.String(),
				cleanInstanceToStatus: inst5.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint successful change of OS, user-data is nil, phone-home enabled, ensure cloud-config is present",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					OperatingSystemID: cutil.GetPtr(osPhoneHome.ID.String()),
					UserData:          nil,
					PhoneHomeEnabled:  cutil.GetPtr(true),
				},
				reqInstance:           inst5.ID.String(),
				cleanInstanceToStatus: inst5.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint successful change of OS, empty user-data, phone-home enabled, ensure cloud-config is present",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					OperatingSystemID: cutil.GetPtr(osPhoneHome.ID.String()),
					UserData:          cutil.GetPtr(""),
					PhoneHomeEnabled:  cutil.GetPtr(true),
				},
				reqInstance:           inst5.ID.String(),
				cleanInstanceToStatus: inst5.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure for change to deactivated OS",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					OperatingSystemID: cutil.GetPtr(os3off.ID.String()),
				},
				reqInstance:           inst5.ID.String(),
				cleanInstanceToStatus: inst5.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure when terminating",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
					UserData:   cutil.GetPtr(cdmu.TestCommonCloudInit),
				},
				reqInstance:           inst3.ID.String(),
				cleanInstanceToStatus: inst3.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusConflict,
			},
			wantErr: false,
		}, {
			name: "test Instance update API endpoint success with applyUpdatesOnReboot trigger",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					TriggerReboot:        cutil.GetPtr(true),
					RebootWithCustomIpxe: cutil.GetPtr(true),
					ApplyUpdatesOnReboot: cutil.GetPtr(true),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusOK,
			},
			wantErr: false,
		}, {
			name: "test Instance update API endpoint failure with reboot trigger, Instance is terminating",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					TriggerReboot: cutil.GetPtr(true),
				},
				reqInstance:           inst3.ID.String(),
				cleanInstanceToStatus: inst3.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusConflict,
			},
			wantErr: false,
		}, {
			name: "test Instance update API endpoint failure with reboot trigger, can't specify rebootWithCustomIpxe without triggerReboot",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					RebootWithCustomIpxe: cutil.GetPtr(true),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		}, {
			name: "test Instance update API endpoint failure with reboot trigger, can't specify applyUpdatesOnReboot without triggerReboot",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					ApplyUpdatesOnReboot: cutil.GetPtr(true),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		}, {
			name: "test Instance update API endpoint failure with reboot trigger, can't specify applyUpdatesOnReboot when triggerReboot set false",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					TriggerReboot:        cutil.GetPtr(false),
					ApplyUpdatesOnReboot: cutil.GetPtr(true),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				respCode:              http.StatusBadRequest,
			},
			wantErr: false,
		}, {
			name: "test Instance update API endpoint failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name: cutil.GetPtr("Test Instance"),
				},
				reqInstance:           inst1.ID.String(),
				cleanInstanceToStatus: inst1.Status,
				reqOrg:                ipOrg,
				reqUser:               ipu,
				respCode:              http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure, invalid Instance ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name: cutil.GetPtr("Test Instance"),
				},
				reqInstance: "",
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint fails with reboot workflow timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tst3,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					TriggerReboot:        cutil.GetPtr(true),
					RebootWithCustomIpxe: cutil.GetPtr(true),
				},
				reqInstance:           inst6.ID.String(),
				cleanInstanceToStatus: inst6.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu2,
				respCode:              http.StatusInternalServerError,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint fails with update workflow timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tst3,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("test-instance-1"),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
				},
				reqInstance:           inst6.ID.String(),
				cleanInstanceToStatus: inst6.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu2,
				respCode:              http.StatusInternalServerError,
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint with OS image base change (temporarily expect StatusBadRequest - Image based OS not allowed)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					OperatingSystemID: cutil.GetPtr(os5.ID.String()),
					UserData:          cutil.GetPtr(cdmu.TestCommonCloudInit + cdmu.TestCommonPhoneHomeSegment + "\nsome_key: some_value\n"),
					PhoneHomeEnabled:  cutil.GetPtr(true),
				},
				reqInstance:           inst7.ID.String(),
				cleanInstanceToStatus: inst7.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu2,
				respCode:              http.StatusBadRequest,
				respMessage:           cutil.GetPtr("Update of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled."),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failure with OS change to Image based OS (wrong site; now fails earlier with Image not allowed)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{

					OperatingSystemID: cutil.GetPtr(os7.ID.String()),
				},
				reqInstance:           inst7.ID.String(),
				cleanInstanceToStatus: inst7.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu2,
				respCode:              http.StatusBadRequest,
				respMessage:           cutil.GetPtr("Update of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled."),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint with OS change to Image based OS from correct site (temporarily expect StatusBadRequest)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					OperatingSystemID: cutil.GetPtr(os6.ID.String()),
				},
				reqInstance:           inst7.ID.String(),
				cleanInstanceToStatus: inst7.Status,
				reqOrg:                tnOrg1,
				reqUser:               tnu2,
				respCode:              http.StatusBadRequest,
				respMessage:           cutil.GetPtr("Update of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled."),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint preserves requested IPv6-only and dual-stack VPC selection intent",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					SecondaryVpcIDs: []string{vpcSelectionSecondary.ID.String()},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcID:      cutil.GetPtr(vpcSelection.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv6},
							IsPhysical: true,
						},
						{
							VpcID:      cutil.GetPtr(vpcSelectionSecondary.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv4, model.IPFamilyIPv6},
						},
					},
				},
				reqInstance:        instVpcSelectionUpdate.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusOK,
				respNoOfInterfaces: cutil.GetPtr(2),
			},
			expectedControllerVpcIDs: map[string]uuid.UUID{
				vpcSelection.ID.String():          controllerVpcSelectionID,
				vpcSelectionSecondary.ID.String(): controllerVpcSelectionSecondaryID,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
		},
		{
			name: "test Instance update API endpoint preserves existing VPC selection during unrelated update",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name: cutil.GetPtr("test-instance-vpc-selection-preserved"),
				},
				reqInstance: instVpcSelectionPreserve.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			expectedInterfaceVpcID: &vpcSelection.ID,
			expectedControllerVpcIDs: map[string]uuid.UUID{
				vpcSelection.ID.String(): controllerVpcSelectionID,
			},
			expectedResolvedVpcPrefixID: &vpcSelectionPrefix.ID,
			wantErr:                     false,
			verifySiteControllerRequest: true,
		},
		{
			name: "test Instance update API endpoint failure with interface update when Instance Type doesn't have Network Capabilities with DPU device type",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Update Failure"),
					Description: cutil.GetPtr("Test Instance Description Interface Update Failure"),
					IpxeScript:  os2.IpxeScript,
					Labels: map[string]string{
						"instance_update_interface_update": "true",
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqInstance:        inst11.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusBadRequest,
				respMessage:        cutil.GetPtr("Device and Device Instance cannot be specified if Instance Type or Machine doesn't have Network Capability with DPU device type"),
				respNoOfInterfaces: cutil.GetPtr(1),
				ethInterfacesToDelete: []cdbm.Interface{
					*instifc1,
					*instifc2,
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test UpdateInstance adding VF reuses unchanged PF issue 4908",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{ //nolint:exhaustruct // This case leaves unrelated response assertions unset.
				reqData: &model.APIInstanceUpdateRequest{ //nolint:exhaustruct // The request changes only Ethernet fields.
					Name:       cutil.GetPtr("Issue 4908 Add VF"),
					IpxeScript: os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpc4Site3Secondary.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID:             nil,
							VpcPrefixID:          cutil.GetPtr(vpcPrefix1.ID.String()),
							IPAddress:            nil,
							InlineRoutingProfile: nil,
							Device:               issue4908Device,
							DeviceInstance:       issue4908DeviceInstance,
							VirtualFunctionID:    nil,
							IsPhysical:           true,
						},
						{
							SubnetID:             nil,
							VpcPrefixID:          cutil.GetPtr(vpcPrefixSite3Secondary.ID.String()),
							IPAddress:            nil,
							InlineRoutingProfile: nil,
							Device:               issue4908Device,
							DeviceInstance:       issue4908DeviceInstance,
							VirtualFunctionID:    issue4908VFID,
							IsPhysical:           false,
						},
					},
				},
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				reqInstance:           issue4908AddVFInstance.ID.String(),
				cleanInstanceToStatus: issue4908AddVFInstance.Status,
				respCode:              http.StatusOK,
				ethernetReconciliation: &ethernetReconciliationExpectation{
					rowCount:        2,
					readyIDs:        []uuid.UUID{issue4908AddVFPF.ID},
					deletingIDs:     []uuid.UUID{},
					pendingCount:    1,
					uniqueIPAddress: cutil.GetPtr("192.168.0.1"),
					usagePrefix:     vpcPrefix1,
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test UpdateInstance marks only omitted Ethernet interface deleting issue 4908",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{ //nolint:exhaustruct // This case leaves unrelated response assertions unset.
				reqData: &model.APIInstanceUpdateRequest{ //nolint:exhaustruct // The request changes only Ethernet fields.
					Name:       cutil.GetPtr("Issue 4908 Remove VF"),
					IpxeScript: os2.IpxeScript,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							SubnetID:             nil,
							VpcPrefixID:          cutil.GetPtr(vpcPrefix1.ID.String()),
							IPAddress:            nil,
							InlineRoutingProfile: nil,
							Device:               issue4908Device,
							DeviceInstance:       issue4908DeviceInstance,
							VirtualFunctionID:    nil,
							IsPhysical:           true,
						},
					},
				},
				reqOrg:                tnOrg1,
				reqUser:               tnu1,
				reqInstance:           issue4908RemoveInstance.ID.String(),
				cleanInstanceToStatus: issue4908RemoveInstance.Status,
				respCode:              http.StatusOK,
				ethernetReconciliation: &ethernetReconciliationExpectation{
					rowCount:        2,
					readyIDs:        []uuid.UUID{issue4908RemovePF.ID},
					deletingIDs:     []uuid.UUID{issue4908RemoveVF.ID},
					pendingCount:    0,
					uniqueIPAddress: nil,
					usagePrefix:     nil,
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with interface update",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Update Success"),
					Description: cutil.GetPtr("Test Instance Description Interface Update Success"),
					IpxeScript:  os2.IpxeScript,
					Labels: map[string]string{
						"instance_update_interface_update": "true",
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
							InlineRoutingProfile: &model.APIInterfaceInlineRoutingProfile{
								AllowedAnycastPrefixes: []string{"192.0.2.0/24", "2001:db8::/64"},
							},
						},
					},
				},
				reqInstance:        inst12.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusOK,
				respNoOfInterfaces: cutil.GetPtr(1),
				ethInterfacesToDelete: []cdbm.Interface{
					*instifc1,
					*instifc2,
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with secondary interface on a different allowed VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Update Success Multi VPC"),
					Description: cutil.GetPtr("Test Instance Description Interface Update Success Multi VPC"),
					IpxeScript:  os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpc4Site3Secondary.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite3Secondary.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqInstance:        inst12.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusOK,
				respNoOfInterfaces: cutil.GetPtr(2),
				ethInterfacesToDelete: []cdbm.Interface{
					*instifc1,
					*instifc2,
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint inherited nsg propagation full state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpcSecondaryFull.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixPrimaryFull.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSecondaryFull.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqInstance:                           instUpdateFull.ID.String(),
				reqOrg:                                tnOrg1,
				reqUser:                               tnu1,
				respCode:                              http.StatusOK,
				respNoOfInterfaces:                    cutil.GetPtr(2),
				expectedSecondaryVpcIDs:               []string{vpcSecondaryFull.ID.String()},
				expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
				expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusFull),
				expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronized),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint inherited nsg propagation none state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpcSecondaryNone.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixPrimaryNone.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSecondaryNone.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqInstance:                           instUpdateNone.ID.String(),
				reqOrg:                                tnOrg1,
				reqUser:                               tnu1,
				respCode:                              http.StatusOK,
				respNoOfInterfaces:                    cutil.GetPtr(2),
				expectedSecondaryVpcIDs:               []string{vpcSecondaryNone.ID.String()},
				expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
				expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusNone),
				expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronizing),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint inherited nsg propagation partial state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpcSecondaryPartial.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixPrimaryPartial.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSecondaryPartial.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqInstance:                           instUpdatePartial.ID.String(),
				reqOrg:                                tnOrg1,
				reqUser:                               tnu1,
				respCode:                              http.StatusOK,
				respNoOfInterfaces:                    cutil.GetPtr(2),
				expectedSecondaryVpcIDs:               []string{vpcSecondaryPartial.ID.String()},
				expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
				expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusPartial),
				expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronizing),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success with reboot trigger and inherited nsg propagation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					TriggerReboot:        cutil.GetPtr(true),
					RebootWithCustomIpxe: cutil.GetPtr(true),
				},
				reqInstance:                           instUpdateRebootFull.ID.String(),
				cleanInstanceToStatus:                 instUpdateRebootFull.Status,
				reqOrg:                                tnOrg1,
				reqUser:                               tnu1,
				respCode:                              http.StatusOK,
				expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
				expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusFull),
				expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronized),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failed when requested secondary VPCs do not match interface VPCs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Update Secondary VPC Mismatch"),
					Description: cutil.GetPtr("Test Instance Description Interface Update Secondary VPC Mismatch"),
					IpxeScript:  os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpc4Site3Secondary.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqInstance: inst12.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: cutil.GetPtr("One or more Interfaces in request data specify VPC Prefixes that do not belong to VPCs specified in `vpcId` or `secondaryVpcIds`"),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failed when an interface uses a VPC outside requested VPC IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Update Unexpected VPC"),
					Description: cutil.GetPtr("Test Instance Description Interface Update Unexpected VPC"),
					IpxeScript:  os2.IpxeScript,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite3Secondary.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqInstance: inst12.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: cutil.GetPtr(fmt.Sprintf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefixSite3Secondary.ID.String(), vpc4Site3Secondary.ID.String())),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failed when primary physical interface uses a prefix from a secondary VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Update Primary Must Match VPC"),
					Description: cutil.GetPtr("Test Instance Description Interface Update Primary Must Match VPC"),
					IpxeScript:  os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpc4Site3Secondary.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite3Secondary.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqInstance:        inst12.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusBadRequest,
				respMessage:        cutil.GetPtr("The physical Interface for deviceInstance: 0 must use a VPC Prefix that belongs to VPC specified in `vpcId`"),
				respNoOfInterfaces: cutil.GetPtr(1),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint preserves prefix response for mixed selectors without device info",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Update Primary Must Match VPC No Device Info"),
					Description: cutil.GetPtr("Test Instance Description Interface Update Primary Must Match VPC No Device Info"),
					IpxeScript:  os2.IpxeScript,
					SecondaryVpcIDs: []string{
						vpc4Site3Secondary.ID.String(),
						vpcSelection.ID.String(),
					},
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID: cutil.GetPtr(vpcPrefixSite3Secondary.ID.String()),
							IsPhysical:  true,
						},
						{
							VpcID:      cutil.GetPtr(vpcSelection.ID.String()),
							IPFamilies: []model.IPFamily{model.IPFamilyIPv4},
						},
					},
				},
				reqInstance:        inst12.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusBadRequest,
				respMessage:        cutil.GetPtr("The physical Interface must use a VPC Prefix that belongs to VPC specified in `vpcId`"),
				respNoOfInterfaces: cutil.GetPtr(1),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failed when primary interface uses a VPC Prefix from another Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Wrong Site Primary"),
					Description: cutil.GetPtr("Test Instance Description Interface Wrong Site Primary"),
					IpxeScript:  os2.IpxeScript,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite2.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
					},
				},
				reqInstance:        inst12.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusBadRequest,
				respMessage:        cutil.GetPtr(fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixSite2.ID.String())),
				respNoOfInterfaces: cutil.GetPtr(1),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint failed when secondary interface uses a VPC Prefix from another Site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:        cutil.GetPtr("Test Instance Interface Wrong Site Secondary"),
					Description: cutil.GetPtr("Test Instance Description Interface Wrong Site Secondary"),
					IpxeScript:  os2.IpxeScript,
					Interfaces: []model.APIInterfaceCreateOrUpdateRequest{
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefix3.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(0),
						},
						{
							VpcPrefixID:    cutil.GetPtr(vpcPrefixSite2.ID.String()),
							IsPhysical:     true,
							Device:         cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							DeviceInstance: cutil.GetPtr(1),
						},
					},
				},
				reqInstance:        inst12.ID.String(),
				reqOrg:             tnOrg1,
				reqUser:            tnu1,
				respCode:           http.StatusBadRequest,
				respMessage:        cutil.GetPtr(fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixSite2.ID.String())),
				respNoOfInterfaces: cutil.GetPtr(1),
			},
			wantErr: false,
		},
		{
			name: "test Instance update API endpoint success when NVLink interfaces unchanged (no-op, multiset order)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					// Same four bindings as DB; order differs from DB creation order — handler no-op, workflow still sends all GPUs.
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 3},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 0},
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 2},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 1},
					},
				},
				reqInstance:                     inst13.ID.String(),
				reqOrg:                          tnOrg1,
				reqUser:                         tnu1,
				respCode:                        http.StatusOK,
				nvLinkGpuConfigsVerifyCountOnly: true,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success when NVLink pending rows older than 90s grace — re-issued (stale Updated timestamps)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 3},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 0},
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 2},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 1},
					},
				},
				reqInstance:                     inst13PendingStale.ID.String(),
				reqOrg:                          tnOrg1,
				reqUser:                         tnu1,
				respCode:                        http.StatusOK,
				nvLinkGpuConfigsVerifyCountOnly: true,
				beforeHandle: func(t *testing.T) {
					stale := time.Now().UTC().Add(-2 * time.Minute)
					_, err := dbSession.DB.Exec(
						"UPDATE nvlink_interface SET updated = ? WHERE instance_id = ?",
						stale,
						inst13PendingStale.ID,
					)
					require.NoError(t, err)
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success when NVLink interfaces pending and updated within 90s — multiset no-op (no DB re-issue)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 3},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 0},
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 2},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 1},
					},
				},
				reqInstance:                     inst13PendingFresh.ID.String(),
				reqOrg:                          tnOrg1,
				reqUser:                         tnu1,
				respCode:                        http.StatusOK,
				nvLinkGpuConfigsVerifyCountOnly: true,
				beforeHandle: func(t *testing.T) {
					// Ensure timestamps are within NVLinkInterfaceStatusSyncGraceWindow but not "just now" only.
					recent := time.Now().UTC().Add(-45 * time.Second)
					_, err := dbSession.DB.Exec(
						"UPDATE nvlink_interface SET updated = ? WHERE instance_id = ?",
						recent,
						inst13PendingFresh.ID,
					)
					require.NoError(t, err)
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update re-issues NVLink when most recent row is Error and Updated within sync grace window",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("Test Instance NVLink error re-issue grace"),
					IpxeScript: os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 0},
					},
				},
				reqInstance:              instNvlinkErrorGrace.ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
				respNoOfNVLinkInterfaces: cutil.GetPtr(1),
				nvlinkInterfacesToDelete: []cdbm.NVLinkInterface{*nvlinkErrGraceIfc},
				beforeHandle: func(t *testing.T) {
					recent := time.Now().UTC().Add(-45 * time.Second)
					_, err := dbSession.DB.Exec(
						"UPDATE nvlink_interface SET updated = ? WHERE id = ?",
						recent,
						nvlinkErrGraceIfc.ID,
					)
					require.NoError(t, err)
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update re-issues NVLink when most recent row is Error but Updated older than sync grace window",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("Test Instance NVLink error re-issue stale"),
					IpxeScript: os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 0},
					},
				},
				reqInstance:              instNvlinkErrorStale.ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
				respNoOfNVLinkInterfaces: cutil.GetPtr(1),
				nvlinkInterfacesToDelete: []cdbm.NVLinkInterface{*nvlinkErrStaleIfc},
				beforeHandle: func(t *testing.T) {
					stale := time.Now().UTC().Add(-2 * time.Minute)
					_, err := dbSession.DB.Exec(
						"UPDATE nvlink_interface SET updated = ? WHERE id = ?",
						stale,
						nvlinkErrStaleIfc.ID,
					)
					require.NoError(t, err)
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			// Reflects handler today: multiset match on all-Deleting NVLink rows is treated as a no-op — no Pending rows and no GpuConfigs (all rows stay Deleting).
			name: "test Instance update NVLink all-Deleting multiset match no-ops (no Pending GpuConfigs)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					// Same four bindings as DB (nvllp1: 0,1 and nvllp2: 2,3); order shuffled. All rows Deleting → handler no-ops NVLink subset.
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 3},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 0},
						{NVLinkLogicalPartitionID: nvllp2.ID.String(), DeviceInstance: 2},
						{NVLinkLogicalPartitionID: nvllp1.ID.String(), DeviceInstance: 1},
					},
				},
				reqInstance:                    instFourDeletingNVLink.ID.String(),
				reqOrg:                         tnOrg1,
				reqUser:                        tnu1,
				respCode:                       http.StatusOK,
				respNoOfNVLinkInterfaces:       cutil.GetPtr(4),
				nvLinkSkipPendingDBAssertions:  true,
				expectSiteNVLinkGpuConfigCount: cutil.GetPtr(0),
				nvlinkInterfacesToDelete: []cdbm.NVLinkInterface{
					*inst4DelNvl1, *inst4DelNvl2, *inst4DelNvl3, *inst4DelNvl4,
				},
				beforeHandle: func(t *testing.T) {
					recent := time.Now().UTC().Add(-45 * time.Second)
					_, err := dbSession.DB.Exec(
						"UPDATE nvlink_interface SET updated = ? WHERE instance_id = ?",
						recent,
						instFourDeletingNVLink.ID,
					)
					require.NoError(t, err)
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			// Reflects handler today: request keys matching Ready rows no-op NVLink churn; unchanged Ready rows remain and Site Controller still receives all active GpuConfigs from DB order.
			name: "test Instance update API endpoint success when Instance has four NVLink interfaces and update requests two (nvllp1 devices 0 and 1)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
							DeviceInstance:           0,
						},
						{
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
							DeviceInstance:           1,
						},
					},
				},
				reqInstance:                     instSubsetFourToTwoA.ID.String(),
				reqOrg:                          tnOrg1,
				reqUser:                         tnu1,
				respCode:                        http.StatusOK,
				respNoOfNVLinkInterfaces:        cutil.GetPtr(2),
				expectSiteNVLinkGpuConfigCount:  cutil.GetPtr(4),
				nvLinkGpuConfigsVerifyCountOnly: true,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			// Reflects handler today — same multiset no-op semantics as nvllp1 subset case above.
			name: "test Instance update API endpoint success when Instance has four NVLink interfaces and update requests two (nvllp2 devices 2 and 3)",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{
							NVLinkLogicalPartitionID: nvllp2.ID.String(),
							DeviceInstance:           2,
						},
						{
							NVLinkLogicalPartitionID: nvllp2.ID.String(),
							DeviceInstance:           3,
						},
					},
				},
				reqInstance:                     instSubsetFourToTwoB.ID.String(),
				reqOrg:                          tnOrg1,
				reqUser:                         tnu1,
				respCode:                        http.StatusOK,
				respNoOfNVLinkInterfaces:        cutil.GetPtr(2),
				expectSiteNVLinkGpuConfigCount:  cutil.GetPtr(4),
				nvLinkGpuConfigsVerifyCountOnly: true,
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with NVLink interface update with different NVLink Logical Partition IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript: os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{
						{
							NVLinkLogicalPartitionID: nvllp2.ID.String(),
							DeviceInstance:           0,
						},
						{
							NVLinkLogicalPartitionID: nvllp2.ID.String(),
							DeviceInstance:           1,
						},
						{
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
							DeviceInstance:           2,
						},
						{
							NVLinkLogicalPartitionID: nvllp1.ID.String(),
							DeviceInstance:           3,
						},
					},
				},
				reqInstance:              inst13.ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
				respNoOfNVLinkInterfaces: cutil.GetPtr(4),
				nvlinkInterfacesToDelete: []cdbm.NVLinkInterface{
					*instnvlifc1,
					*instnvlifc2,
					*instnvlifc3,
					*instnvlifc4,
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint success with NVLink interface update - delete existing NVLink interfaces",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					IpxeScript:       os2.IpxeScript,
					NVLinkInterfaces: []model.APINVLinkInterfaceCreateOrUpdateRequest{},
				},
				reqInstance:              inst13.ID.String(),
				reqOrg:                   tnOrg1,
				reqUser:                  tnu1,
				respCode:                 http.StatusOK,
				respNoOfNVLinkInterfaces: cutil.GetPtr(4),
				nvlinkInterfacesToDelete: []cdbm.NVLinkInterface{
					*instnvlifc1,
					*instnvlifc2,
					*instnvlifc3,
					*instnvlifc4,
				},
			},
			wantErr:                     false,
			verifySiteControllerRequest: true,
			verifyChildSpanner:          true,
		},
		{
			name: "test Instance update API endpoint failure, Instance is missing on site",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					Name:       cutil.GetPtr("Test Instance"),
					IpxeScript: cutil.GetPtr(common.DefaultIpxeScript),
				},
				reqInstance: inst14.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusConflict,
				respMessage: cutil.GetPtr("Instance is missing on site and cannot be updated"),
			},
			wantErr: false,
		},
		{
			// inst1 lives on vpc1 (ETHERNET_VIRTUALIZER). Toggling
			// `auto: true` on an instance whose VPC isn't Flat must be
			// rejected at the handler layer (the model validator only
			// checks the `auto`/`interfaces` exclusivity; it can't see
			// the VPC type).
			name: "test Instance update API endpoint rejects auto=true on a non-Flat VPC",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceUpdateRequest{
					AutoNetwork: cutil.GetPtr(true),
				},
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
				respMessage: cutil.GetPtr("`autoNetwork: true` is only supported when the Instance's VPC has `networkVirtualizationType` set to `FLAT`"),
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			uih := UpdateInstanceHandler{
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
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/instance/%v", tt.args.reqOrg, tt.args.reqInstance))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqInstance)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if tt.args.beforeHandle != nil {
				tt.args.beforeHandle(t)
			}

			if err := uih.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("UpdateInstanceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("UpdateInstanceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respMessage != nil {
				assert.Contains(t, rec.Body.String(), *tt.args.respMessage)
			}
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := &model.APIInstance{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			if tt.args.reqData.Name != nil {
				assert.Equal(t, rst.Name, *tt.args.reqData.Name)
			}

			if tt.args.reqData.Description != nil {
				assert.Equal(t, *rst.Description, *tt.args.reqData.Description)
			}

			if tt.args.reqData.NetworkSecurityGroupID != nil {
				if *tt.args.reqData.NetworkSecurityGroupID == "" {
					assert.Nil(t, rst.NetworkSecurityGroupID, "Failed to clear instance NSG ID")
				} else {
					assert.Equal(t, *tt.args.reqData.NetworkSecurityGroupID, *rst.NetworkSecurityGroupID)
				}
			}

			if tt.args.respUserData != nil {
				assert.Equal(t, *tt.args.respUserData, *rst.UserData)

				if tt.args.reqData.PhoneHomeEnabled != nil && *tt.args.reqData.PhoneHomeEnabled && tt.args.reqData.OperatingSystemID != nil {
					lines := strings.Split(*rst.UserData, "\n")
					assert.Equal(t, util.SiteCloudConfig, lines[0])
					assert.NotEqual(t, util.SiteCloudConfig, lines[1])
				}
			}

			if tt.args.respUserDataContains != nil {
				assert.Contains(t, *rst.UserData, *tt.args.respUserDataContains)
			}

			if tt.args.reqData.SSHKeyGroupIDs != nil {
				assert.Equal(t, len(tt.args.reqData.SSHKeyGroupIDs), len(rst.SSHKeyGroups))

				// Now that we know the sets are the same length,
				// let's make sure they have the same things.

				keygroups := map[string]bool{}
				for _, skgID := range tt.args.reqData.SSHKeyGroupIDs {
					keygroups[skgID] = true
				}

				for _, kgSummary := range rst.SSHKeyGroups {
					assert.Equal(t, true, keygroups[kgSummary.ID])
				}
			}

			if tt.args.reqData.Labels != nil {
				assert.NotNil(t, rst.Labels)
				assert.Equal(t, len(tt.args.reqData.Labels), len(rst.Labels))

				if len(tt.args.reqData.Labels) > 0 {
					for k := range tt.args.reqData.Labels {

						// Make sure we don't get tricked by
						// the zero-value at the key in the result map.
						resVal, resFound := rst.Labels[k]
						assert.True(t, resFound)

						assert.Equal(t, tt.args.reqData.Labels[k], resVal)
					}
				}
			}

			if tt.args.reqData.TriggerReboot != nil {
				assert.Equal(t, rst.Status, cdbm.InstancePowerStatusRebooting)
			}

			if tt.args.expectedDesdIDs != nil {
				assert.Equal(t, len(tt.args.expectedDesdIDs), len(rst.DpuExtensionServiceDeployments))

				foundDesdIDs := map[string]bool{}
				for _, d := range rst.DpuExtensionServiceDeployments {
					foundDesdIDs[d.ID] = true
				}

				for _, expectedID := range tt.args.expectedDesdIDs {
					assert.True(t, foundDesdIDs[expectedID], "expected deployment %s to be present in response", expectedID)
				}
			}

			reqIns, _ := insDAO.GetByID(ec.Request().Context(), nil, uuid.MustParse(tt.args.reqInstance), nil)

			if len(tt.expectedControllerVpcIDs) > 0 && len(tt.args.reqData.Interfaces) > 0 {
				require.Len(t, rst.Interfaces, len(tt.args.reqData.Interfaces))
				persistedIfcs, _, ierr := ifcDAO.GetAll(ec.Request().Context(), nil,
					cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{reqIns.ID}},
					cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}}, nil)
				require.NoError(t, ierr)
				require.Len(t, persistedIfcs, len(tt.args.reqData.Interfaces))
				for i, reqIfc := range tt.args.reqData.Interfaces {
					require.NotNil(t, reqIfc.VpcID)
					assert.Equal(t, reqIfc.VpcID, rst.Interfaces[i].VpcID)
					assert.Equal(t, reqIfc.IPFamilies, rst.Interfaces[i].IPFamilies)
					assert.Nil(t, rst.Interfaces[i].VpcPrefixID)
					require.NotNil(t, persistedIfcs[i].VpcID)
					assert.Equal(t, *reqIfc.VpcID, persistedIfcs[i].VpcID.String())
					require.NotNil(t, persistedIfcs[i].VpcIPFamilyMode)
					assert.Equal(t, reqIfc.VpcIPFamilyMode(), *persistedIfcs[i].VpcIPFamilyMode)
					assert.Nil(t, persistedIfcs[i].VpcPrefixID)
				}
			}

			if tt.expectedInterfaceVpcID != nil {
				require.NotEmpty(t, rst.Interfaces)
				responseIfc := rst.Interfaces[0]
				require.NotNil(t, responseIfc.VpcID)
				assert.Equal(t, tt.expectedInterfaceVpcID.String(), *responseIfc.VpcID)
				assert.Equal(t, []model.IPFamily{model.IPFamilyIPv4}, responseIfc.IPFamilies)
				assert.Nil(t, responseIfc.VpcPrefixID)
				assert.Nil(t, responseIfc.VpcPrefix)

				persistedIfcs, _, ierr := ifcDAO.GetAll(ec.Request().Context(), nil,
					cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{reqIns.ID}},
					cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}}, nil)
				require.NoError(t, ierr)
				require.Len(t, persistedIfcs, 1)
				require.NotNil(t, persistedIfcs[0].VpcID)
				assert.Equal(t, *tt.expectedInterfaceVpcID, *persistedIfcs[0].VpcID)
				require.NotNil(t, persistedIfcs[0].VpcIPFamilyMode)
				assert.Equal(t, cdbm.InterfaceVpcIPFamilyModeIPv4Only, *persistedIfcs[0].VpcIPFamilyMode)
				if tt.expectedResolvedVpcPrefixID == nil {
					assert.Nil(t, persistedIfcs[0].VpcPrefixID)
				} else {
					require.NotNil(t, persistedIfcs[0].VpcPrefixID)
					assert.Equal(t, *tt.expectedResolvedVpcPrefixID, *persistedIfcs[0].VpcPrefixID)
				}
			}

			ttsc, _ := tt.fields.scp.GetClientByID(reqIns.SiteID)
			ttscm := ttsc.(*tmocks.Client)

			if len(tt.args.ethInterfacesToDelete) > 0 && len(tt.args.reqData.Interfaces) > 0 {
				// Make sure the Interfaces are deleting
				ifcDAO := cdbm.NewInterfaceDAO(tt.fields.dbSession)
				for _, ifc := range tt.args.ethInterfacesToDelete {
					ifc, _ := ifcDAO.GetByID(ec.Request().Context(), nil, ifc.ID, nil)
					assert.Equal(t, ifc.Status, cdbm.InterfaceStatusDeleting)
				}

				// Make sure the Interfaces are pending
				// It should be in order of the request received
				ifcs, _, _ := ifcDAO.GetAll(ec.Request().Context(), nil,
					cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{reqIns.ID},
						Statuses: []string{cdbm.InterfaceStatusPending}},
					cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}},
					[]string{cdbm.SubnetRelationName})
				assert.Equal(t, len(ifcs), len(tt.args.reqData.Interfaces))
				for i, _ := range ifcs {
					if ifcs[i].SubnetID != nil && tt.args.reqData.Interfaces[i].SubnetID != nil {
						assert.Equal(t, ifcs[i].SubnetID.String(), *tt.args.reqData.Interfaces[i].SubnetID)
					}

					if ifcs[i].VpcPrefixID != nil && tt.args.reqData.Interfaces[i].VpcPrefixID != nil {
						assert.Equal(t, ifcs[i].VpcPrefixID.String(), *tt.args.reqData.Interfaces[i].VpcPrefixID)
					}

					assert.Equal(t, ifcs[i].IsPhysical, tt.args.reqData.Interfaces[i].IsPhysical)

					if ifcs[i].Device != nil && tt.args.reqData.Interfaces[i].Device != nil {
						assert.Equal(t, ifcs[i].Device, tt.args.reqData.Interfaces[i].Device)
					}

					if ifcs[i].DeviceInstance != nil && tt.args.reqData.Interfaces[i].DeviceInstance != nil {
						assert.Equal(t, ifcs[i].DeviceInstance, tt.args.reqData.Interfaces[i].DeviceInstance)
					}

					if ifcs[i].VirtualFunctionID != nil && tt.args.reqData.Interfaces[i].VirtualFunctionID != nil {
						assert.Equal(t, ifcs[i].VirtualFunctionID, tt.args.reqData.Interfaces[i].VirtualFunctionID)
					}

					if tt.args.reqData.Interfaces[i].IPAddress != nil {
						assert.Equal(t, tt.args.reqData.Interfaces[i].IPAddress, ifcs[i].RequestedIpAddress)
					}

					if tt.args.reqData.Interfaces[i].InlineRoutingProfile != nil {
						require.NotNil(t, ifcs[i].InlineRoutingProfile)
						assert.Equal(t, tt.args.reqData.Interfaces[i].InlineRoutingProfile.AllowedAnycastPrefixes, ifcs[i].InlineRoutingProfile.AllowedAnycastPrefixes)
					}

					assert.Equal(t, cdbm.InterfaceStatusPending, ifcs[i].Status)
				}
			}

			if tt.args.reqData.SecondaryVpcIDs != nil {
				assert.ElementsMatch(t, tt.args.reqData.SecondaryVpcIDs, rst.SecondaryVpcIDs)
			}

			if expected := tt.args.ethernetReconciliation; expected != nil {
				reconciledIfcs, _, reconciliationErr := ifcDAO.GetAll(
					ctx,
					nil,
					cdbm.InterfaceFilterInput{
						InstanceIDs:    []uuid.UUID{reqIns.ID},
						SubnetID:       nil,
						VpcPrefixID:    nil,
						Device:         nil,
						DeviceInstance: nil,
						IsPhysical:     nil,
						Statuses:       nil,
						IPAddresses:    nil,
					},
					cdbp.PageInput{Offset: nil, Limit: cutil.GetPtr(cdbp.TotalLimit), OrderBy: nil},
					nil,
				)
				require.NoError(t, reconciliationErr)
				assert.Len(t, reconciledIfcs, expected.rowCount)
				assert.Len(t, rst.Interfaces, expected.rowCount)

				pendingCount := 0
				deletingCount := 0
				rowsWithExpectedIP := 0

				for _, ifc := range reconciledIfcs {
					switch ifc.Status {
					case cdbm.InterfaceStatusPending:
						pendingCount++
					case cdbm.InterfaceStatusDeleting:
						deletingCount++
					}

					for _, ipAddress := range ifc.IPAddresses {
						if expected.uniqueIPAddress != nil && ipAddress == *expected.uniqueIPAddress {
							rowsWithExpectedIP++
						}
					}
				}

				assert.Equal(t, expected.pendingCount, pendingCount)
				assert.Equal(t, len(expected.deletingIDs), deletingCount)

				if expected.uniqueIPAddress != nil {
					assert.Equal(t, 1, rowsWithExpectedIP)
				}

				for _, interfaceID := range expected.readyIDs {
					ifc, getErr := ifcDAO.GetByID(ctx, nil, interfaceID, nil)
					require.NoError(t, getErr)
					assert.Equal(t, cdbm.InterfaceStatusReady, ifc.Status)
				}

				for _, interfaceID := range expected.deletingIDs {
					ifc, getErr := ifcDAO.GetByID(ctx, nil, interfaceID, nil)
					require.NoError(t, getErr)
					assert.Equal(t, cdbm.InterfaceStatusDeleting, ifc.Status)
				}

				if expected.usagePrefix != nil {
					usageByID, usageErr := cdbm.NewVpcPrefixDAO(tt.fields.dbSession).GetPrefixUsage(ctx, nil, expected.usagePrefix)
					require.NoError(t, usageErr)

					usage := usageByID[expected.usagePrefix.ID]
					require.NotNil(t, usage)
					assert.LessOrEqual(t, usage.AcquiredIPs+uint64(2), usage.AvailableIPs)
				}
			}

			if tt.args.expectedNetworkSecurityGroupInherited != nil {
				assert.Equal(t, *tt.args.expectedNetworkSecurityGroupInherited, rst.NetworkSecurityGroupInherited)
			}

			if tt.args.expectedPropagationDetailedStatus != nil {
				require.NotNil(t, rst.NetworkSecurityGroupPropagationDetails)
				assert.Equal(t, *tt.args.expectedPropagationDetailedStatus, rst.NetworkSecurityGroupPropagationDetails.DetailedStatus)
			}

			if tt.args.expectedPropagationStatus != nil {
				require.NotNil(t, rst.NetworkSecurityGroupPropagationDetails)
				assert.Equal(t, *tt.args.expectedPropagationStatus, rst.NetworkSecurityGroupPropagationDetails.Status)
			}

			if len(tt.args.ibInterfaceToDelete) > 0 && len(tt.args.reqData.InfiniBandInterfaces) > 0 {
				// Make sure the InfiniBand Interfaces are deleting
				ibiDAO := cdbm.NewInfiniBandInterfaceDAO(tt.fields.dbSession)
				for _, ibifc := range tt.args.ibInterfaceToDelete {
					ibifc, _ := ibiDAO.GetByID(ec.Request().Context(), nil, ibifc.ID, nil)
					assert.Equal(t, ibifc.Status, cdbm.InfiniBandInterfaceStatusDeleting)

					if len(rst.InfiniBandInterfaces) > 0 {
						for _, rstIbifc := range rst.InfiniBandInterfaces {
							if rstIbifc.ID == ibifc.ID.String() {
								assert.Equal(t, cdbm.InfiniBandInterfaceStatusDeleting, rstIbifc.Status)
								break
							}
						}
					}
				}

				// Make sure the InfiniBand Interfaces are pending
				// It should be in order of the request received
				// Get all InfiniBand Interfaces for the Instance in creation and assert they are in order and status is pending
				ibifcs, _, _ := ibiDAO.GetAll(ec.Request().Context(), nil,
					cdbm.InfiniBandInterfaceFilterInput{InstanceIDs: []uuid.UUID{reqIns.ID},
						Statuses: []string{cdbm.InfiniBandInterfaceStatusPending}},
					cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InfiniBandInterfaceOrderByCreated, Order: cdbp.OrderAscending}},
					[]string{cdbm.InfiniBandPartitionRelationName})
				assert.Equal(t, len(ibifcs), len(tt.args.reqData.InfiniBandInterfaces))
				for i, _ := range ibifcs {
					assert.Equal(t, tt.args.reqData.InfiniBandInterfaces[i].InfiniBandPartitionID, ibifcs[i].InfiniBandPartitionID.String())
					assert.Equal(t, cdbm.InfiniBandInterfaceStatusPending, ibifcs[i].Status)
				}
			}

			if len(tt.args.expectInfiniBandInterfacesRemainReady) > 0 {
				ibiDAO := cdbm.NewInfiniBandInterfaceDAO(tt.fields.dbSession)

				pendingIb, _, ierr := ibiDAO.GetAll(ec.Request().Context(), nil,
					cdbm.InfiniBandInterfaceFilterInput{InstanceIDs: []uuid.UUID{reqIns.ID},
						Statuses: []string{cdbm.InfiniBandInterfaceStatusPending}},
					cdbp.PageInput{}, nil)
				require.NoError(t, ierr)
				assert.Len(t, pendingIb, 0, "READY InfiniBand no-op must not insert Pending rows when partition+device+deviceInstance matches existing READY interfaces")

				for _, wantID := range tt.args.expectInfiniBandInterfacesRemainReady {
					ibRow, ierr := ibiDAO.GetByID(ec.Request().Context(), nil, wantID, nil)
					require.NoError(t, ierr)
					assert.Equal(t, cdbm.InfiniBandInterfaceStatusReady, ibRow.Status,
						"InfiniBand interface %s must stay Ready after no-op request (matching partition/device/instance)", wantID)
				}

				gotReadyInAPI := map[string]bool{}
				for _, apiIb := range rst.InfiniBandInterfaces {
					if apiIb.Status == cdbm.InfiniBandInterfaceStatusReady {
						gotReadyInAPI[apiIb.ID] = true
					}
				}
				for _, wantID := range tt.args.expectInfiniBandInterfacesRemainReady {
					require.True(t, gotReadyInAPI[wantID.String()],
						"expected READY InfiniBand interface %s in update response matching DB row", wantID)
				}
			}

			if len(tt.args.nvlinkInterfacesToDelete) > 0 && tt.args.reqData.NVLinkInterfaces != nil {
				// Make sure the NVLink Interfaces are deleting
				nvlIfcDAO := cdbm.NewNVLinkInterfaceDAO(tt.fields.dbSession)
				for _, nvlifc := range tt.args.nvlinkInterfacesToDelete {
					got, _ := nvlIfcDAO.GetByID(ec.Request().Context(), nil, nvlifc.ID, nil)
					assert.Equal(t, cdbm.NVLinkInterfaceStatusDeleting, got.Status)
				}

				if len(tt.args.reqData.NVLinkInterfaces) > 0 && !tt.args.nvLinkSkipPendingDBAssertions {
					// Make sure the NVLink Interfaces are pending
					// It should be in order of the request received
					nvlifcs, _, _ := nvlIfcDAO.GetAll(ec.Request().Context(), nil,
						cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{reqIns.ID},
							Statuses: []string{cdbm.NVLinkInterfaceStatusPending}},
						cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.NVLinkInterfaceOrderByCreated, Order: cdbp.OrderAscending}},
						[]string{cdbm.NVLinkLogicalPartitionRelationName})
					require.Equal(t, len(tt.args.reqData.NVLinkInterfaces), len(nvlifcs))
					for i := range nvlifcs {
						assert.Equal(t, tt.args.reqData.NVLinkInterfaces[i].NVLinkLogicalPartitionID, nvlifcs[i].NVLinkLogicalPartitionID.String())
						assert.Equal(t, cdbm.NVLinkInterfaceStatusPending, nvlifcs[i].Status)
					}
				}
			}

			if tt.verifySiteControllerRequest {
				// Collect the last matching ExecuteWorkflow call for this instance
				// (multiple tests may trigger calls for the same instance; verify only against the current test's call)
				var siteReq *corev1.InstanceConfigUpdateRequest
				for _, call := range ttscm.Calls {
					if call.Method == "ExecuteWorkflow" && call.Arguments[2] == "UpdateInstance" {
						req := call.Arguments[3].(*corev1.InstanceConfigUpdateRequest)
						if req.InstanceId.Value == tt.args.reqInstance {
							siteReq = req
						}
					}
				}
				require.NotNil(t, siteReq, "expected UpdateInstance workflow request for Instance %s", tt.args.reqInstance)
				if siteReq != nil {
					if tt.args.expectedSitePowerProfile != nil {
						require.NotNil(t, siteReq.Config.PowerProfile)
						assert.Equal(t, *tt.args.expectedSitePowerProfile, *siteReq.Config.PowerProfile)
					}

					// Verify the number of interfaces in the request as pending status
					// which is the number of interfaces in the request
					var reqInsIfcs []cdbm.Interface
					if tt.args.respNoOfInterfaces != nil {
						reqInsIfcs, _, _ = ifcDAO.GetAll(ec.Request().Context(), nil, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{reqIns.ID}, Statuses: []string{cdbm.InterfaceStatusPending}}, cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}}, nil)
					} else {
						reqInsIfcs, _, _ = ifcDAO.GetAll(ec.Request().Context(), nil, cdbm.InterfaceFilterInput{
							InstanceIDs:    []uuid.UUID{reqIns.ID},
							SubnetID:       nil,
							VpcPrefixID:    nil,
							Device:         nil,
							DeviceInstance: nil,
							IsPhysical:     nil,
							Statuses: []string{
								cdbm.InterfaceStatusPending,
								cdbm.InterfaceStatusProvisioning,
								cdbm.InterfaceStatusReady,
								cdbm.InterfaceStatusError,
							},
							IPAddresses: nil,
						}, cdbp.PageInput{Offset: nil, Limit: nil, OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}}, nil)
					}

					assert.Equal(t, len(reqInsIfcs), len(siteReq.Config.Network.Interfaces))

					for i, siteIfc := range siteReq.Config.Network.Interfaces {
						assert.NotNil(t, siteIfc, "encountered nil interface entry")

						if siteIfc == nil || (siteIfc.NetworkSegmentId == nil && siteIfc.NetworkDetails == nil) {
							assert.Fail(t, "encountered nil interface/segment entry for Site Controller request")
						}

						// Subnet case if we have both NetworkSegmentId and NetworkDetails
						if siteIfc.NetworkSegmentId != nil && siteIfc.NetworkDetails != nil {
							ifcNd, ok := siteIfc.NetworkDetails.(*corev1.InstanceInterfaceConfig_SegmentId)
							assert.True(t, ok)
							assert.Equal(t, ifcNd.SegmentId, siteIfc.NetworkSegmentId)

							//Make sure order is same as the request received
							assert.Equal(t, siteIfc.NetworkSegmentId.Value, reqInsIfcs[i].SubnetID.String())
						}

						// Prefix-backed cases have NetworkDetails without a segment ID.
						if siteIfc.NetworkDetails != nil && siteIfc.NetworkSegmentId == nil {
							switch networkDetails := siteIfc.NetworkDetails.(type) {
							case *corev1.InstanceInterfaceConfig_VpcPrefixId:
								require.NotNil(t, reqInsIfcs[i].VpcPrefixID)
								assert.Equal(t, networkDetails.VpcPrefixId.Value, reqInsIfcs[i].VpcPrefixID.String())
							case *corev1.InstanceInterfaceConfig_Vpc:
								require.NotNil(t, reqInsIfcs[i].VpcID)
								require.NotNil(t, reqInsIfcs[i].VpcIPFamilyMode)
								interfaceVpcID := reqInsIfcs[i].VpcID.String()
								if i < len(tt.args.reqData.Interfaces) && tt.args.reqData.Interfaces[i].VpcID != nil {
									interfaceVpcID = *tt.args.reqData.Interfaces[i].VpcID
								}
								expectedControllerVpcID, ok := tt.expectedControllerVpcIDs[interfaceVpcID]
								require.True(t, ok)
								assertInterfaceVpcSelection(t, siteIfc, expectedControllerVpcID, *reqInsIfcs[i].VpcIPFamilyMode)
							default:
								assert.Failf(t, "unexpected Interface network details", "%T", networkDetails)
							}
						}

						// Check if Device and DeviceInstance are present
						if reqInsIfcs[i].Device != nil && reqInsIfcs[i].DeviceInstance != nil {
							assert.Equal(t, siteIfc.Device, reqInsIfcs[i].Device)
							assert.Equal(t, siteIfc.DeviceInstance, uint32(*reqInsIfcs[i].DeviceInstance))
						}

						// Check if VirtualFunctionId is present
						if reqInsIfcs[i].VirtualFunctionID != nil {
							assert.Equal(t, uint32(*reqInsIfcs[i].VirtualFunctionID), siteIfc.GetVirtualFunctionId())
						}

						if reqInsIfcs[i].RequestedIpAddress != nil {
							assert.Equal(t, siteIfc.IpAddress, reqInsIfcs[i].RequestedIpAddress)
						}

						if tt.args.reqData.Interfaces != nil && i < len(tt.args.reqData.Interfaces) && tt.args.reqData.Interfaces[i].InlineRoutingProfile != nil {
							assertInterfaceRoutingProfilePrefixes(t, siteIfc.RoutingProfile, tt.args.reqData.Interfaces[i].InlineRoutingProfile.AllowedAnycastPrefixes)
						}
					}

					// Verify the InfiniBand Interfaces are in the Site Controller request
					if len(tt.args.reqData.InfiniBandInterfaces) > 0 {
						assert.Equal(t, len(siteReq.Config.Infiniband.IbInterfaces), len(tt.args.reqData.InfiniBandInterfaces))

						// Make sure order to should be same as the request received
						for i := range siteReq.Config.Infiniband.IbInterfaces {
							assert.Equal(t, siteReq.Config.Infiniband.IbInterfaces[i].IbPartitionId.Value, tt.args.reqData.InfiniBandInterfaces[i].InfiniBandPartitionID)
						}
					}

					// Verify the NVLink Interfaces are in the Site Controller request
					if len(tt.args.reqData.NVLinkInterfaces) > 0 {
						expNVL := len(tt.args.reqData.NVLinkInterfaces)
						if tt.args.expectSiteNVLinkGpuConfigCount != nil {
							expNVL = *tt.args.expectSiteNVLinkGpuConfigCount
						}
						assert.Equal(t, expNVL, len(siteReq.Config.Nvlink.GpuConfigs))

						if !tt.args.nvLinkGpuConfigsVerifyCountOnly {
							// Make sure order to should be same as the request received
							for i := range siteReq.Config.Nvlink.GpuConfigs {
								assert.Equal(t, siteReq.Config.Nvlink.GpuConfigs[i].LogicalPartitionId.Value, tt.args.reqData.NVLinkInterfaces[i].NVLinkLogicalPartitionID)
								assert.Equal(t, siteReq.Config.Nvlink.GpuConfigs[i].DeviceInstance, uint32(tt.args.reqData.NVLinkInterfaces[i].DeviceInstance))
							}
						}
					}

					// Verify the SpectrumX Attachments are in the Site Controller request
					if len(tt.args.reqData.SpectrumXAttachments) > 0 {
						require.NotNil(t, siteReq.Config.Spxconfig)
						assert.Equal(t, len(tt.args.reqData.SpectrumXAttachments), len(siteReq.Config.Spxconfig.SpxAttachments))

						// Make sure order to should be same as the request received
						for i := range siteReq.Config.Spxconfig.SpxAttachments {
							assert.Equal(t, siteReq.Config.Spxconfig.SpxAttachments[i].SpxPartitionId.Value, tt.args.reqData.SpectrumXAttachments[i].SpectrumXPartitionID)
							assert.Equal(t, siteReq.Config.Spxconfig.SpxAttachments[i].Device, tt.args.reqData.SpectrumXAttachments[i].Device)
						}
					}

					// Verify the DPU Extension Service Deployments are in the Site Controller request
					if len(tt.args.reqData.DpuExtensionServiceDeployments) > 0 {
						assert.Equal(t, len(tt.args.reqData.DpuExtensionServiceDeployments), len(siteReq.Config.DpuExtensionServices.ServiceConfigs), siteReq.Config.DpuExtensionServices.ServiceConfigs)

						// Make sure order to should be same as the request received
						for i := range siteReq.Config.DpuExtensionServices.ServiceConfigs {
							assert.Equal(t, siteReq.Config.DpuExtensionServices.ServiceConfigs[i].ServiceId, tt.args.reqData.DpuExtensionServiceDeployments[i].DpuExtensionServiceID)
						}
					}

					if tt.args.reqData.SSHKeyGroupIDs != nil {
						// Verify the length of the set of SKGs for the instance match
						// the length of the set sent to NICo
						assert.Equal(t, len(rst.SSHKeyGroups), len(siteReq.Config.Tenant.TenantKeysetIds))

						// Build a map for some lookups
						keygroups := map[string]bool{}
						for _, skg := range rst.SSHKeyGroups {
							keygroups[skg.ID] = true
						}

						// Check that each skgID sent to NICo matches one in
						// the current set for the Instance.
						for _, kgSummary := range siteReq.Config.Tenant.TenantKeysetIds {
							assert.Equal(t, true, keygroups[kgSummary], "%s not found in %+v", kgSummary, keygroups)
						}
					}
				}
			}

			assert.NotNil(t, rst.SerialConsoleURL)
			assert.NotEqual(t, rst.Updated.String(), inst1.Updated.String())

			// Verify Instance status is configuring if any of the interfaces are being updated
			if tt.args.reqData.NVLinkInterfaces != nil || tt.args.reqData.Interfaces != nil || tt.args.reqData.InfiniBandInterfaces != nil ||
				tt.args.reqData.SpectrumXAttachments != nil {
				assert.Equal(t, rst.Status, cdbm.InstanceStatusConfiguring)
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}

			if tt.args.cleanInstanceToStatus != "" && uuid.MustParse(tt.args.reqInstance) != uuid.Nil {
				defer resetInstanceStatus(t, tt.fields.dbSession, uuid.MustParse(tt.args.reqInstance), tt.args.cleanInstanceToStatus)
			}
		})
	}

}

func TestGetInstanceHandler_Handle(t *testing.T) {
	ctx := context.Background()

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqInstance   *cdbm.Instance
		reqInstanceID string
		reqOrg        string
		reqUser       *cdbm.User
		reqMachine    *cdbm.Machine
		respCode      int
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)

	nsg1 := testBuildNetworkSecurityGroup(t, dbSession, "network-security-group-1-for-the-win", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg1)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	mc2 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc2)

	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist1)
	assert.NotNil(t, mcinst2)

	mc3 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc3)

	mcinst3 := testInstanceBuildMachineInstanceType(t, dbSession, mc3, ist1)
	assert.NotNil(t, mcinst3)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	// Set an NSG on this VPC
	vpc2.NetworkSecurityGroupID = &nsg1.ID
	testUpdateVPC(t, dbSession, vpc2)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)
	subnet3 := testInstanceBuildSubnet(t, dbSession, "test-subnet-3", tn1, vpc2, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet3)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	// Attach an NSG to this instance
	inst1.NetworkSecurityGroupID = cutil.GetPtr(nsg1.ID)
	testUpdateInstance(t, dbSession, inst1)

	// InfiniBand Interface Support
	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp1)

	ibi1 := testInstanceBuildIBInterface(t, dbSession, inst1, st1, ibp1, 0, false, cutil.GetPtr(1), cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibi1)

	instsub1 := testInstanceBuildInstanceInterface(t, dbSession, inst1.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusPending)
	assert.NotNil(t, instsub1)

	// DPU Extension Service Deployment Support
	des1 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "1.0.0", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)

	desd1 := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des1, inst1.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu1)
	assert.NotNil(t, desd1)

	inst2 := testInstanceBuildInstance(t, dbSession, "test-instance-3", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc2.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst2)

	inst2.ControllerInstanceID = cutil.GetPtr(uuid.New())

	inst3 := testInstanceBuildInstance(t, dbSession, "test-instance-4", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc2.ID, cutil.GetPtr(mc3.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst3)
	inst3WithIfc := testInstanceBuildInstance(t, dbSession, "test-instance-4-with-interface", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc2.ID, cutil.GetPtr(mc2.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst3WithIfc)
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, inst3WithIfc.ID, &subnet3.ID, nil, nil, cdbm.InterfaceStatusPending))

	mc4 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc4)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc4, ist1))
	mc5 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc5)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc5, ist1))
	mc6 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc6)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc6, ist1))

	buildMultiVpcPair := func(primaryName, secondaryName, primaryPrefixName, secondaryPrefixName, primaryCIDR, secondaryCIDR string) (*cdbm.Vpc, *cdbm.Vpc, *cdbm.VpcPrefix, *cdbm.VpcPrefix) {
		primary := testInstanceBuildVPC(t, dbSession, primaryName, ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
		secondary := testInstanceBuildVPC(t, dbSession, secondaryName, ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
		primaryIPB := common.TestBuildVpcPrefixIPBlock(t, dbSession, primaryPrefixName+"-ipb", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, primaryCIDR[:len(primaryCIDR)-3], 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
		secondaryIPB := common.TestBuildVpcPrefixIPBlock(t, dbSession, secondaryPrefixName+"-ipb", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, secondaryCIDR[:len(secondaryCIDR)-3], 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
		primaryVP := common.TestBuildVPCPrefix(t, dbSession, primaryPrefixName, st1, tn1, primary.ID, &primaryIPB.ID, cutil.GetPtr(primaryCIDR), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
		secondaryVP := common.TestBuildVPCPrefix(t, dbSession, secondaryPrefixName, st1, tn1, secondary.ID, &secondaryIPB.ID, cutil.GetPtr(secondaryCIDR), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
		return primary, secondary, primaryVP, secondaryVP
	}

	vpcPrimaryFull, vpcSecondaryFull, vpcPrefixPrimaryFull, vpcPrefixSecondaryFull := buildMultiVpcPair("test-get-vpc-primary-full", "test-get-vpc-secondary-full", "test-get-vpcprefix-full-primary", "test-get-vpcprefix-full-secondary", "192.186.0.0/24", "192.189.0.0/24")
	vpcPrimaryNone, vpcSecondaryNone, vpcPrefixPrimaryNone, vpcPrefixSecondaryNone := buildMultiVpcPair("test-get-vpc-primary-none", "test-get-vpc-secondary-none", "test-get-vpcprefix-none-primary", "test-get-vpcprefix-none-secondary", "192.187.0.0/24", "192.190.0.0/24")
	vpcPrimaryPartial, vpcSecondaryPartial, vpcPrefixPrimaryPartial, vpcPrefixSecondaryPartial := buildMultiVpcPair("test-get-vpc-primary-partial", "test-get-vpc-secondary-partial", "test-get-vpcprefix-partial-primary", "test-get-vpcprefix-partial-secondary", "192.188.0.0/24", "192.191.0.0/24")

	instFull := testInstanceBuildInstance(t, dbSession, "test-instance-get-vpc-full", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpcPrimaryFull.ID, cutil.GetPtr(mc4.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instFull)
	instNone := testInstanceBuildInstance(t, dbSession, "test-instance-get-vpc-none", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpcPrimaryNone.ID, cutil.GetPtr(mc5.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instNone)
	instPartial := testInstanceBuildInstance(t, dbSession, "test-instance-get-vpc-partial", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpcPrimaryPartial.ID, cutil.GetPtr(mc6.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instPartial)

	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instFull.ID, nil, &vpcPrefixPrimaryFull.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instFull.ID, nil, &vpcPrefixSecondaryFull.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instNone.ID, nil, &vpcPrefixPrimaryNone.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instNone.ID, nil, &vpcPrefixSecondaryNone.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instPartial.ID, nil, &vpcPrefixPrimaryPartial.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instPartial.ID, nil, &vpcPrefixSecondaryPartial.ID, nil, cdbm.InterfaceStatusPending))

	setVpcProp := func(vpc *cdbm.Vpc, related []string, unprop []string, status corev1.NetworkSecurityGroupPropagationStatus) {
		vpc.NetworkSecurityGroupID = &nsg1.ID
		vpc.NetworkSecurityGroupPropagationDetails = &cdbm.NetworkSecurityGroupPropagationDetails{
			NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{
				Id:                      vpc.ID.String(),
				Status:                  status,
				RelatedInstanceIds:      related,
				UnpropagatedInstanceIds: unprop,
			},
		}
		testUpdateVPC(t, dbSession, vpc)
	}

	setVpcProp(vpcPrimaryFull, []string{instFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcSecondaryFull, []string{instFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcPrimaryNone, []string{instNone.ID.String()}, []string{instNone.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)
	setVpcProp(vpcSecondaryNone, []string{instNone.ID.String()}, []string{instNone.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)
	setVpcProp(vpcPrimaryPartial, []string{instPartial.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcSecondaryPartial, []string{instPartial.ID.String()}, []string{instPartial.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool

		expectSerialConsoleURL *string

		queryIncludeRelationTenant               *string
		queryIncludeRelationSite                 *string
		queryIncludeRelationVpc                  *string
		queryIncludeRelationInstanceType         *string
		queryIncludeRelationProvider             *string
		queryIncludeRelationAllocation           *string
		queryIncludeRelationMachine              *string
		queryIncludeRelationOperatingSystem      *string
		queryIncludeRelationNetworkSecurityGroup *string

		expectedTenantOrg                       *string
		expectedSiteName                        *string
		expectedVpcName                         *string
		expectedInstanceTypeName                *string
		expectedInfrastructureProviderOrg       *string
		expectedAllocationName                  *string
		expectedMachineControllerID             *string
		expectedOperatingSystemName             *string
		expectedNetworkSecurityGroupName        *string
		expectedIBInterfaceID                   *string
		expectedDpuExtensionServiceDeploymentID *string
		expectedNetworkSecurityGroupInherited   *bool
		expectedSecondaryVpcIDs                 []string
		expectedPropagationDetailedStatus       *string
		expectedPropagationStatus               *string
		expectSubnet                            bool
		verifyChildSpanner                      bool
	}{
		{
			name: "test Instance get API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqMachine:    mc1,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectedIBInterfaceID:                   cutil.GetPtr(ibi1.ID.String()),
			expectedDpuExtensionServiceDeploymentID: cutil.GetPtr(desd1.ID.String()),
			expectedNetworkSecurityGroupInherited:   cutil.GetPtr(false),
			expectSubnet:                            true,
			wantErr:                                 false,
		},
		{
			name: "test Instance get API endpoint with no nsg and no inherited nsg - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst2,
				reqInstanceID: inst2.ID.String(),
				reqMachine:    mc2,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectedNetworkSecurityGroupInherited: cutil.GetPtr(false), // Because the VPC has no NSG
			wantErr:                               false,
		},
		{
			name: "test Instance get API endpoint with inherited nsg - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst3,
				reqInstanceID: inst3.ID.String(),
				reqMachine:    mc3,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectedNetworkSecurityGroupInherited: cutil.GetPtr(false),
			wantErr:                               false,
		},
		{
			name: "test Instance get API endpoint with inherited nsg and interface-derived VPC context - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst3WithIfc,
				reqInstanceID: inst3WithIfc.ID.String(),
				reqMachine:    mc2,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
			expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusNone),
			expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronizing),
			wantErr:                               false,
		},
		{
			name: "test Instance get API endpoint with inherited nsg propagated across all associated vpcs - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   instFull,
				reqInstanceID: instFull.ID.String(),
				reqMachine:    mc4,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
			expectedSecondaryVpcIDs:               []string{vpcSecondaryFull.ID.String()},
			expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusFull),
			expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronized),
			wantErr:                               false,
		},
		{
			name: "test Instance get API endpoint with inherited nsg unpropagated across all associated vpcs - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   instNone,
				reqInstanceID: instNone.ID.String(),
				reqMachine:    mc5,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
			expectedSecondaryVpcIDs:               []string{vpcSecondaryNone.ID.String()},
			expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusNone),
			expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronizing),
			wantErr:                               false,
		},
		{
			name: "test Instance get API endpoint with inherited nsg partially propagated across associated vpcs - success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   instPartial,
				reqInstanceID: instPartial.ID.String(),
				reqMachine:    mc6,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectedNetworkSecurityGroupInherited: cutil.GetPtr(true),
			expectedSecondaryVpcIDs:               []string{vpcSecondaryPartial.ID.String()},
			expectedPropagationDetailedStatus:     cutil.GetPtr(model.APINetworkSecurityGroupPropagationDetailedStatusPartial),
			expectedPropagationStatus:             cutil.GetPtr(model.APINetworkSecurityGroupPropagationStatusSynchronizing),
			wantErr:                               false,
		},
		{
			name: "test Instance get API endpoint failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqMachine:    mc1,
				reqOrg:        ipOrg,
				reqUser:       ipu,
				respCode:      http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance get API endpoint failure, invalid Instance ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: "",
				reqMachine:    mc1,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance get API endpoint failure, Instance ID in request not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: uuid.New().String(),
				reqMachine:    mc1,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test Instance get API endpoint failure, Instance not belong to current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqMachine:    mc1,
				reqOrg:        tnOrg2,
				reqUser:       tnu2,
				respCode:      http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance get API endpoint success include all relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqMachine:    mc1,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			wantErr:            false,
			verifyChildSpanner: true,
			expectSubnet:       true,

			queryIncludeRelationTenant:               cutil.GetPtr(cdbm.TenantRelationName),
			queryIncludeRelationSite:                 cutil.GetPtr(cdbm.SiteRelationName),
			queryIncludeRelationVpc:                  cutil.GetPtr(cdbm.VpcRelationName),
			queryIncludeRelationInstanceType:         cutil.GetPtr(cdbm.InstanceTypeRelationName),
			queryIncludeRelationProvider:             cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelationMachine:              cutil.GetPtr(cdbm.MachineRelationName),
			queryIncludeRelationOperatingSystem:      cutil.GetPtr(cdbm.OperatingSystemRelationName),
			queryIncludeRelationNetworkSecurityGroup: cutil.GetPtr(cdbm.NetworkSecurityGroupRelationName),

			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedSiteName:                  cutil.GetPtr(st1.Name),
			expectedTenantOrg:                 cutil.GetPtr(tn1.Org),
			expectedInstanceTypeName:          cutil.GetPtr(ist1.Name),
			expectedVpcName:                   cutil.GetPtr(vpc1.Name),
			expectedMachineControllerID:       cutil.GetPtr(mc1.ControllerMachineID),
			expectedOperatingSystemName:       cutil.GetPtr(os1.Name),
			expectedNetworkSecurityGroupName:  cutil.GetPtr(nsg1.Name),
		},
		{
			name: "test Instance get API endpoint with ssh url",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:   inst1,
				reqInstanceID: inst1.ID.String(),
				reqMachine:    mc1,
				reqOrg:        tnOrg1,
				reqUser:       tnu1,
				respCode:      http.StatusOK,
			},
			expectSerialConsoleURL: cutil.GetPtr(fmt.Sprintf("ssh://%s@%s", inst1.ControllerInstanceID.String(), *st1.SerialConsoleHostname)),
			wantErr:                false,
			expectSubnet:           true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := GetInstanceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			if tt.queryIncludeRelationTenant != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationTenant)
			}
			if tt.queryIncludeRelationSite != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationSite)
			}
			if tt.queryIncludeRelationVpc != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationVpc)
			}
			if tt.queryIncludeRelationInstanceType != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationInstanceType)
			}
			if tt.queryIncludeRelationProvider != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationProvider)
			}
			if tt.queryIncludeRelationMachine != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationMachine)
			}
			if tt.queryIncludeRelationOperatingSystem != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationOperatingSystem)
			}
			if tt.queryIncludeRelationNetworkSecurityGroup != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationNetworkSecurityGroup)
			}
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/instance/%v", tt.args.reqOrg, tt.args.reqInstanceID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqInstanceID)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetInstanceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetInstanceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := &model.APIInstance{}

			serr := json.Unmarshal(rec.Body.Bytes(), rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, rst.Name, tt.args.reqInstance.Name)
			assert.Equal(t, rst.MachineID, cutil.GetPtr(tt.args.reqMachine.ID))
			assert.Equal(t, rst.Status, cdbm.InstanceStatusReady)

			if tt.expectSubnet {
				assert.Equal(t, *rst.Interfaces[0].SubnetID, instsub1.SubnetID.String())
				assert.NotNil(t, rst.Interfaces[0].Subnet)
			}

			if tt.expectSerialConsoleURL != nil {
				assert.Equal(t, *tt.expectSerialConsoleURL, *rst.SerialConsoleURL)
			}

			if tt.queryIncludeRelationTenant != nil {
				assert.Equal(t, *tt.expectedTenantOrg, rst.Tenant.Org)
			}

			if tt.queryIncludeRelationSite != nil {
				assert.Equal(t, *tt.expectedSiteName, rst.Site.Name)
			}

			if tt.queryIncludeRelationVpc != nil {
				assert.Equal(t, *tt.expectedVpcName, rst.Vpc.Name)
			}

			if tt.queryIncludeRelationInstanceType != nil {
				assert.Equal(t, *tt.expectedInstanceTypeName, rst.InstanceType.Name)
			}

			if tt.queryIncludeRelationProvider != nil {
				assert.Equal(t, *tt.expectedInfrastructureProviderOrg, rst.InfrastructureProvider.Org)
			}

			if tt.queryIncludeRelationMachine != nil {
				assert.Equal(t, *tt.expectedMachineControllerID, rst.Machine.ControllerMachineID)
			}

			if tt.queryIncludeRelationOperatingSystem != nil {
				assert.Equal(t, *tt.expectedOperatingSystemName, rst.OperatingSystem.Name)
			}

			if tt.queryIncludeRelationNetworkSecurityGroup != nil {
				assert.Equal(t, *tt.expectedNetworkSecurityGroupName, rst.NetworkSecurityGroup.Name)
			}

			if tt.expectedNetworkSecurityGroupInherited != nil {
				assert.Equal(t, *tt.expectedNetworkSecurityGroupInherited, rst.NetworkSecurityGroupInherited)
			}

			if tt.expectedSecondaryVpcIDs != nil {
				assert.ElementsMatch(t, tt.expectedSecondaryVpcIDs, rst.SecondaryVpcIDs)
			}

			if tt.expectedNetworkSecurityGroupInherited != nil && *tt.expectedNetworkSecurityGroupInherited {
				if tt.expectedPropagationDetailedStatus != nil {
					require.NotNil(t, rst.NetworkSecurityGroupPropagationDetails)
					assert.Equal(t, *tt.expectedPropagationDetailedStatus, rst.NetworkSecurityGroupPropagationDetails.DetailedStatus)
				}

				if tt.expectedPropagationStatus != nil {
					assert.Equal(t, *tt.expectedPropagationStatus, rst.NetworkSecurityGroupPropagationDetails.Status)
				}
			} else {
				assert.Nil(t, rst.NetworkSecurityGroupPropagationDetails)
			}

			if tt.expectedIBInterfaceID != nil {
				assert.Equal(t, *tt.expectedIBInterfaceID, rst.InfiniBandInterfaces[0].ID)
				assert.NotNil(t, rst.InfiniBandInterfaces[0].InfiniBandPartition)
				assert.NotEqual(t, rst.InfiniBandInterfaces[0].InfiniBandPartition.SiteID, "")
			}

			if tt.expectedDpuExtensionServiceDeploymentID != nil {
				assert.Greater(t, len(rst.DpuExtensionServiceDeployments), 0)
				assert.Equal(t, *tt.expectedDpuExtensionServiceDeploymentID, rst.DpuExtensionServiceDeployments[0].ID)
				assert.NotNil(t, rst.DpuExtensionServiceDeployments[0].DpuExtensionService)
				assert.NotEqual(t, rst.DpuExtensionServiceDeployments[0].Version, "")
				assert.NotEqual(t, rst.DpuExtensionServiceDeployments[0].Status, "")
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestGetAllInstanceHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}
	type args struct {
		reqOrg                      string
		reqUser                     *cdbm.User
		reqInstance                 *cdbm.Instance
		reqInfrastructureProviderID string
		reqSiteIDs                  []string
		respCode                    int
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)
	st2 := testInstanceBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st2)
	st3 := testInstanceBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st3)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := common.TestBuildTenantWithDisplayName(t, dbSession, "test-tenant", tnOrg1, tnu1, "Test Tenant 1")

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)
	al2 := testInstanceSiteBuildAllocation(t, dbSession, st2, tn1, "test-allocation-2", ipu)
	assert.NotNil(t, al2)
	al3 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-3", ipu)
	assert.NotNil(t, al3)
	al4 := testInstanceSiteBuildAllocation(t, dbSession, st3, tn1, "test-allocation-4", ipu)
	assert.NotNil(t, al4)

	ts1 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, st1.ID, tnu1.ID)
	assert.NotNil(t, ts1)
	ts2 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, st2.ID, tnu1.ID)
	assert.NotNil(t, ts2)
	ts3 := testBuildTenantSiteAssociation(t, dbSession, tnOrg1, tn1.ID, st3.ID, tnu1.ID)
	assert.NotNil(t, ts3)

	// NSG
	nsg1 := testBuildNetworkSecurityGroup(t, dbSession, "nsg1", tn1, st1, cdbm.NetworkSecurityGroupStatusReady)
	assert.NotNil(t, nsg1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)
	ist2 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-2", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist2)
	ist3 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-3", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist3)
	ist4 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-4", st3, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist4)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)
	alc2 := testInstanceSiteBuildAllocationContraints(t, dbSession, al3, cdbm.AllocationResourceTypeInstanceType, ist2.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc2)
	alc3 := testInstanceSiteBuildAllocationContraints(t, dbSession, al4, cdbm.AllocationResourceTypeInstanceType, ist4.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc3)

	mc1 := testInstanceBuildMachineWithID(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil, "machine-1")
	assert.NotNil(t, mc1)
	mc2 := testInstanceBuildMachineWithID(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil, "machine-2")
	assert.NotNil(t, mc2)
	mc3 := testInstanceBuildMachineWithID(t, dbSession, ip.ID, st3.ID, cutil.GetPtr(false), nil, "machine-3")
	assert.NotNil(t, mc3)
	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)
	mcinst2 := testInstanceBuildMachineInstanceType(t, dbSession, mc2, ist2)
	assert.NotNil(t, mcinst2)
	mcinst3 := testInstanceBuildMachineInstanceType(t, dbSession, mc3, ist4)
	assert.NotNil(t, mcinst3)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	os2 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-2", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os2)

	os3 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-3", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os3)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	// Set an NSG for this VPC
	vpc1.NetworkSecurityGroupID = &nsg1.ID
	testUpdateVPC(t, dbSession, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	vpc3 := testInstanceBuildVPC(t, dbSession, "test-vpc-3", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc3)

	vpc4 := testInstanceBuildVPC(t, dbSession, "test-vpc-4", ip, tn1, st3, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc4)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	subnet3 := testInstanceBuildSubnet(t, dbSession, "test-subnet-3", tn1, vpc4, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet3)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	instarr := []*cdbm.Instance{}
	instsubarr := []*cdbm.Interface{}
	for i := 11; i <= 35; i++ {
		inst := testInstanceBuildInstance(t, dbSession, fmt.Sprintf("test-instance-%d", i), tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
		assert.NotNil(t, inst)

		instsub := testInstanceBuildInstanceInterface(t, dbSession, inst.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusPending)
		assert.NotNil(t, instsub)
		instarr = append(instarr, inst)
		instsubarr = append(instsubarr, instsub)

		common.TestBuildStatusDetail(t, dbSession, inst.ID.String(), cdbm.InstanceStatusPending, cutil.GetPtr("request received, pending processing"))
		common.TestBuildStatusDetail(t, dbSession, inst.ID.String(), cdbm.InstanceStatusProvisioning, cutil.GetPtr("Instance is being provisioned on Site"))
	}
	inst1 := instarr[0]
	instsub1 := instsubarr[0]

	// Apply the NSG to this instance
	inst1.NetworkSecurityGroupID = cutil.GetPtr(nsg1.ID)
	testUpdateInstance(t, dbSession, inst1)

	// InfiniBand Interface Support
	ibp1 := testBuildIBPartition(t, dbSession, "test-ibp-1", tnOrg1, st1, tn1, cutil.GetPtr(uuid.New()), cutil.GetPtr(cdbm.InfiniBandPartitionStatusReady), false)
	assert.NotNil(t, ibp1)

	ibi1 := testInstanceBuildIBInterface(t, dbSession, inst1, st1, ibp1, 0, false, cutil.GetPtr(1), cutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady), false)
	assert.NotNil(t, ibi1)

	// DPU Extension Service Deployment Support
	des1 := common.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-1", model.DpuExtensionServiceTypeKubernetesPod, tn1, st1, "1.0.0", cdbm.DpuExtensionServiceStatusReady, tnu1)
	assert.NotNil(t, des1)

	desd1 := common.TestBuildDpuExtensionServiceDeployment(t, dbSession, des1, inst1.ID, "1.0.0", cdbm.DpuExtensionServiceDeploymentStatusRunning, tnu1)
	assert.NotNil(t, desd1)

	inst2 := testInstanceBuildInstance(t, dbSession, "test-instance-vpc", tn1.ID, ip.ID, st1.ID, &ist2.ID, vpc3.ID, cutil.GetPtr(mc2.ID), &os2.ID, cutil.GetPtr("test-ipxe-script"), cdbm.InstanceStatusReady)
	assert.NotNil(t, inst2)

	instsub2 := testInstanceBuildInstanceInterface(t, dbSession, inst2.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusPending)
	assert.NotNil(t, instsub2)

	common.TestBuildStatusDetail(t, dbSession, inst2.ID.String(), cdbm.InstanceStatusPending, cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, inst2.ID.String(), cdbm.InstanceStatusProvisioning, cutil.GetPtr("Instance is being provisioned on Site"))

	inst4 := testInstanceBuildInstance(t, dbSession, "test-instance-no-site-id", tn1.ID, ip.ID, st3.ID, &ist4.ID, vpc4.ID, cutil.GetPtr(mc3.ID), &os2.ID, cutil.GetPtr("test-ipxe-script"), cdbm.InstanceStatusReady)
	assert.NotNil(t, inst4)

	mc4 := testInstanceBuildMachineWithID(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil, "machine-getall-4")
	assert.NotNil(t, mc4)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc4, ist2))
	mc5 := testInstanceBuildMachineWithID(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil, "machine-getall-5")
	assert.NotNil(t, mc5)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc5, ist2))
	mc6 := testInstanceBuildMachineWithID(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil, "machine-getall-6")
	assert.NotNil(t, mc6)
	assert.NotNil(t, testInstanceBuildMachineInstanceType(t, dbSession, mc6, ist2))

	buildMultiVpcPair := func(primaryName, secondaryName, primaryPrefixName, secondaryPrefixName, primaryCIDR, secondaryCIDR string) (*cdbm.Vpc, *cdbm.Vpc, *cdbm.VpcPrefix, *cdbm.VpcPrefix) {
		primary := testInstanceBuildVPC(t, dbSession, primaryName, ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
		secondary := testInstanceBuildVPC(t, dbSession, secondaryName, ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcFNN), nil, cdbm.VpcStatusReady, tnu1)
		primaryIPB := common.TestBuildVpcPrefixIPBlock(t, dbSession, primaryPrefixName+"-ipb", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, primaryCIDR, 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
		secondaryIPB := common.TestBuildVpcPrefixIPBlock(t, dbSession, secondaryPrefixName+"-ipb", st1, ip, &tn1.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, secondaryCIDR, 24, cdbm.IPBlockProtocolVersionV4, false, cdbm.IPBlockStatusReady, tnu1)
		primaryPrefix := common.TestBuildVPCPrefix(t, dbSession, primaryPrefixName, st1, tn1, primary.ID, &primaryIPB.ID, cutil.GetPtr(primaryCIDR+"/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
		secondaryPrefix := common.TestBuildVPCPrefix(t, dbSession, secondaryPrefixName, st1, tn1, secondary.ID, &secondaryIPB.ID, cutil.GetPtr(secondaryCIDR+"/24"), cutil.GetPtr(24), cdbm.VpcPrefixStatusReady, tnu1)
		return primary, secondary, primaryPrefix, secondaryPrefix
	}

	vpcPrimaryFull, vpcSecondaryFull, vpcPrefixPrimaryFull, vpcPrefixSecondaryFull := buildMultiVpcPair("test-getall-vpc-primary-full", "test-getall-vpc-secondary-full", "test-getall-vpcprefix-full-primary", "test-getall-vpcprefix-full-secondary", "192.183.0.0", "192.186.0.0")
	vpcPrimaryNone, vpcSecondaryNone, vpcPrefixPrimaryNone, vpcPrefixSecondaryNone := buildMultiVpcPair("test-getall-vpc-primary-none", "test-getall-vpc-secondary-none", "test-getall-vpcprefix-none-primary", "test-getall-vpcprefix-none-secondary", "192.184.0.0", "192.187.0.0")
	vpcPrimaryPartial, vpcSecondaryPartial, vpcPrefixPrimaryPartial, vpcPrefixSecondaryPartial := buildMultiVpcPair("test-getall-vpc-primary-partial", "test-getall-vpc-secondary-partial", "test-getall-vpcprefix-partial-primary", "test-getall-vpcprefix-partial-secondary", "192.185.0.0", "192.188.0.0")

	instFull := testInstanceBuildInstance(t, dbSession, "test-instance-vpc-full", tn1.ID, ip.ID, st1.ID, &ist2.ID, vpcPrimaryFull.ID, cutil.GetPtr(mc4.ID), &os2.ID, cutil.GetPtr("test-ipxe-script"), cdbm.InstanceStatusReady)
	assert.NotNil(t, instFull)
	instNone := testInstanceBuildInstance(t, dbSession, "test-instance-vpc-none", tn1.ID, ip.ID, st1.ID, &ist2.ID, vpcPrimaryNone.ID, cutil.GetPtr(mc5.ID), &os2.ID, cutil.GetPtr("test-ipxe-script"), cdbm.InstanceStatusReady)
	assert.NotNil(t, instNone)
	instPartial := testInstanceBuildInstance(t, dbSession, "test-instance-vpc-partial", tn1.ID, ip.ID, st1.ID, &ist2.ID, vpcPrimaryPartial.ID, cutil.GetPtr(mc6.ID), &os2.ID, cutil.GetPtr("test-ipxe-script"), cdbm.InstanceStatusReady)
	assert.NotNil(t, instPartial)
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instFull.ID, nil, &vpcPrefixPrimaryFull.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instFull.ID, nil, &vpcPrefixSecondaryFull.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instNone.ID, nil, &vpcPrefixPrimaryNone.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instNone.ID, nil, &vpcPrefixSecondaryNone.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instPartial.ID, nil, &vpcPrefixPrimaryPartial.ID, nil, cdbm.InterfaceStatusPending))
	assert.NotNil(t, testInstanceBuildInstanceInterface(t, dbSession, instPartial.ID, nil, &vpcPrefixSecondaryPartial.ID, nil, cdbm.InterfaceStatusPending))
	common.TestBuildStatusDetail(t, dbSession, instFull.ID.String(), cdbm.InstanceStatusPending, cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, instFull.ID.String(), cdbm.InstanceStatusProvisioning, cutil.GetPtr("Instance is being provisioned on Site"))
	common.TestBuildStatusDetail(t, dbSession, instNone.ID.String(), cdbm.InstanceStatusPending, cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, instNone.ID.String(), cdbm.InstanceStatusProvisioning, cutil.GetPtr("Instance is being provisioned on Site"))
	common.TestBuildStatusDetail(t, dbSession, instPartial.ID.String(), cdbm.InstanceStatusPending, cutil.GetPtr("request received, pending processing"))
	common.TestBuildStatusDetail(t, dbSession, instPartial.ID.String(), cdbm.InstanceStatusProvisioning, cutil.GetPtr("Instance is being provisioned on Site"))

	setVpcProp := func(vpc *cdbm.Vpc, related []string, unprop []string, status corev1.NetworkSecurityGroupPropagationStatus) {
		vpc.NetworkSecurityGroupID = &nsg1.ID
		vpc.NetworkSecurityGroupPropagationDetails = &cdbm.NetworkSecurityGroupPropagationDetails{
			NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{
				Id:                      vpc.ID.String(),
				Status:                  status,
				RelatedInstanceIds:      related,
				UnpropagatedInstanceIds: unprop,
			},
		}
		testUpdateVPC(t, dbSession, vpc)
	}

	setVpcProp(vpcPrimaryFull, []string{instFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcSecondaryFull, []string{instFull.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcPrimaryNone, []string{instNone.ID.String()}, []string{instNone.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)
	setVpcProp(vpcSecondaryNone, []string{instNone.ID.String()}, []string{instNone.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)
	setVpcProp(vpcPrimaryPartial, []string{instPartial.ID.String()}, []string{}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL)
	setVpcProp(vpcSecondaryPartial, []string{instPartial.ID.String()}, []string{instPartial.ID.String()}, corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_NONE)

	// Setup instances with specific IP addresses for IP filtering tests
	// Use instances from the array so they're both on st1 and will be on the same page
	testUpdateInterfaceWithIPs(t, dbSession, instsubarr[0], []string{"192.168.1.100", "192.168.1.101"})
	testUpdateInterfaceWithIPs(t, dbSession, instsubarr[1], []string{"192.168.2.200"})

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool

		queryIncludeRelationTenant               *string
		queryIncludeRelationSite                 *string
		queryIncludeRelationVpc                  *string
		queryIncludeRelationInstanceType         *string
		queryIncludeRelationProvider             *string
		queryIncludeRelationOperatingSystem      *string
		queryIncludeRelationMachine              *string
		queryIncludeRelationNetworkSecurityGroup *string

		pageNumber *int
		pageSize   *int
		orderBy    *string

		filter                                      cdbm.InstanceFilterInput
		ipAddresses                                 []string
		expectedFirstEntryName                      string
		expectedInfrastructureProviderOrg           *string
		expectedCount                               int
		expectedTotal                               int
		expectedSiteName                            *string
		expectedTenantOrg                           *string
		expectedInstanceTypeName                    *string
		expectedNetworkSecurityGroupName            *string
		expectedVpcName                             *string
		expectedMachineControllerID                 *string
		expectedOperatingSystemName                 *string
		expectedIBInterfaceID                       *string
		expectedDpuExtensionServiceDeploymentID     *string
		expectedMachineIDOverride                   *string
		expectedAnyNetworkSecurityGroupInherited    bool
		expectedAnyNetworkSecurityGroupNotInherited bool
		expectedNetworkSecurityGroupInheritedByName map[string]bool
		expectedSecondaryVpcIDsByName               map[string][]string
		expectedPropagationDetailsByName            map[string]struct {
			DetailedStatus string
			Status         string
		}
		expectSubnet       bool
		verifyChildSpanner bool
	}{
		{
			name: "test Instance getall API endpoint success with infrastructure provider ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			wantErr:                                  false,
			orderBy:                                  cutil.GetPtr("NAME_ASC"),
			expectedCount:                            20,
			expectedTotal:                            29,
			expectedFirstEntryName:                   instarr[0].Name,
			verifyChildSpanner:                       true,
			expectedIBInterfaceID:                    cutil.GetPtr(ibi1.ID.String()),
			expectedDpuExtensionServiceDeploymentID:  cutil.GetPtr(desd1.ID.String()),
			expectSubnet:                             true,
			expectedAnyNetworkSecurityGroupInherited: true,
		},
		{
			name: "test Instance getall API endpoint success without infrastructure provider ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr:                false,
			orderBy:                cutil.GetPtr("NAME_ASC"),
			expectedCount:          20,
			expectedTotal:          29,
			expectedFirstEntryName: instarr[0].Name,
		},
		{
			name: "test Instance getall API endpoint success without Site ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr:       false,
			orderBy:       cutil.GetPtr("NAME_ASC"),
			expectedCount: 20,
			expectedTotal: 30,
		},
		{
			name: "test Instance getall API endpoint failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      ipOrg,
				reqUser:                     ipu,
				respCode:                    http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall API endpoint failure, invalid Site ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{""},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall API endpoint failure, non-existent Site ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{uuid.New().String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall API endpoint failure, non-existent Infrastructure Provider ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: uuid.New().String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance getall with paging and first page",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			pageNumber:             cutil.GetPtr(1),
			pageSize:               cutil.GetPtr(10),
			orderBy:                cutil.GetPtr("NAME_ASC"),
			expectedCount:          10,
			expectedTotal:          29,
			expectedFirstEntryName: instarr[0].Name,
			wantErr:                false,
		},
		{
			name: "test Instance getall with paging and second page",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			pageNumber:             cutil.GetPtr(2),
			pageSize:               cutil.GetPtr(10),
			orderBy:                cutil.GetPtr("NAME_ASC"),
			expectedCount:          10,
			expectedTotal:          29,
			expectedFirstEntryName: instarr[10].Name,
			wantErr:                false,
			expectSubnet:           true,
		},
		{
			name: "test Instance getall error with paging and bad orderby",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: uuid.New().String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusBadRequest,
			},
			pageNumber: cutil.GetPtr(1),
			pageSize:   cutil.GetPtr(10),
			orderBy:    cutil.GetPtr("TEST_DESC"),
			wantErr:    false,
		},
		{
			name: "test Instance getall API endpoint success include all relation",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},

			wantErr:                false,
			orderBy:                cutil.GetPtr("NAME_ASC"),
			expectedCount:          20,
			expectedTotal:          29,
			expectSubnet:           true,
			expectedFirstEntryName: instarr[0].Name,

			queryIncludeRelationTenant:               cutil.GetPtr(cdbm.TenantRelationName),
			queryIncludeRelationSite:                 cutil.GetPtr(cdbm.SiteRelationName),
			queryIncludeRelationVpc:                  cutil.GetPtr(cdbm.VpcRelationName),
			queryIncludeRelationInstanceType:         cutil.GetPtr(cdbm.InstanceTypeRelationName),
			queryIncludeRelationProvider:             cutil.GetPtr(cdbm.InfrastructureProviderRelationName),
			queryIncludeRelationMachine:              cutil.GetPtr(cdbm.MachineRelationName),
			queryIncludeRelationOperatingSystem:      cutil.GetPtr(cdbm.OperatingSystemRelationName),
			queryIncludeRelationNetworkSecurityGroup: cutil.GetPtr(cdbm.NetworkSecurityGroupRelationName),

			expectedInfrastructureProviderOrg: cutil.GetPtr(ip.Org),
			expectedSiteName:                  cutil.GetPtr(st1.Name),
			expectedTenantOrg:                 cutil.GetPtr(tn1.Org),
			expectedInstanceTypeName:          cutil.GetPtr(ist1.Name),
			expectedVpcName:                   cutil.GetPtr(vpc1.Name),
			expectedMachineControllerID:       cutil.GetPtr(mc1.ControllerMachineID),
			expectedOperatingSystemName:       cutil.GetPtr(os1.Name),
			expectedNetworkSecurityGroupName:  cutil.GetPtr(nsg1.Name),
		},
		{
			name: "test Instance getall API endpoint success multiple siteIDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String(), st2.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr:       false,
			orderBy:       cutil.GetPtr("NAME_ASC"),
			expectedCount: 20,
			expectedTotal: 29,
			expectSubnet:  true,
		},
		{
			name: "test Instance getall API endpoint success no instance with vpc ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc2.ID},
			},
			expectedCount: 0,
			expectedTotal: 0,
		},
		{
			name: "test Instance getall API endpoint success with vpc ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc3.ID},
			},
			expectedMachineIDOverride: cutil.GetPtr(mc2.ID),
			expectedCount:             1,
			expectedTotal:             1,
			expectSubnet:              true,
		},
		{
			name: "test Instance getall API endpoint success with multiple vpc IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				VpcIDs: []uuid.UUID{vpc1.ID, vpc3.ID},
			},
			expectSubnet:  true,
			expectedCount: 20,
			expectedTotal: 26,
		},
		{
			name: "test Instance getall API endpoint success no instance with instance type ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{ist3.ID},
			},
			expectedCount: 0,
			expectedTotal: 0,
		},
		{
			name: "test Instance getall API endpoint success with instance type ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{ist2.ID},
			},
			expectedMachineIDOverride: cutil.GetPtr(mc2.ID),
			expectedCount:             4,
			expectedTotal:             4,
		},
		{
			name: "test Instance getall API endpoint success with multiple instance type IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{ist1.ID, ist2.ID},
			},
			expectedCount: 20,
			expectedTotal: 29,
		},
		{
			name: "test Instance getall API endpoint failure, non-existent instance type id in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusNotFound,
			},
			filter: cdbm.InstanceFilterInput{
				InstanceTypeIDs: []uuid.UUID{uuid.New()},
			},
			wantErr: false,
		},
		{
			name: "test Instance getall API endpoint success no instance with operating system ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{os3.ID},
			},
			expectedCount: 0,
			expectedTotal: 0,
		},
		{
			name: "test Instance getall API endpoint success with operating system ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{os2.ID},
			},
			expectedMachineIDOverride: cutil.GetPtr(mc2.ID),
			expectedCount:             4,
			expectedTotal:             4,
		},
		{
			name: "test Instance getall API endpoint success with multiple operating system IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{os1.ID, os2.ID},
			},
			expectedCount: 20,
			expectedTotal: 29,
		},
		{
			name: "test Instance getall API endpoint failure, non-existent operating system id in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusNotFound,
			},
			filter: cdbm.InstanceFilterInput{
				OperatingSystemIDs: []uuid.UUID{uuid.New()},
			},
			wantErr: false,
		},
		{
			name: "test Instance getall API endpoint success with machine ID",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				MachineIDs: []string{mc2.ID},
			},
			expectedMachineIDOverride: cutil.GetPtr(mc2.ID),
			expectedCount:             1,
			expectedTotal:             1,
		},
		{
			name: "test Instance getall API endpoint success with multiple machine IDs",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			wantErr: false,
			orderBy: cutil.GetPtr("NAME_ASC"),
			filter: cdbm.InstanceFilterInput{
				MachineIDs: []string{mc1.ID, mc2.ID},
			},
			expectedCount: 20,
			expectedTotal: 26,
		},
		{
			name: "test Instance getall API endpoint success, non-existent machine ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1,
				reqSiteIDs:  []string{st1.ID.String()},
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				MachineIDs: []string{"fm100ht6dhn7omq0kkbsqa1bnofuvelfefg9sqg5543s1tf06uc8q9iftm0"},
			},
			wantErr:       false,
			expectedCount: 0,
			expectedTotal: 0,
		},
		{
			name: "test Instance getall API endpoint success with name search query",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				SearchQuery: cutil.GetPtr("test-instance"),
			},
			wantErr:       false,
			expectedCount: 20,
			expectedTotal: 29,
		},
		{
			name: "test Instance getall API endpoint success with status search query",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: "",
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				SearchQuery: cutil.GetPtr("ready"),
			},
			wantErr:       false,
			expectedCount: 20,
			expectedTotal: 29,
		},
		{
			name: "test Instance getall API endpoint success with combination of name and status search query",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: "",
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				SearchQuery: cutil.GetPtr("test-instance ready"),
			},
			wantErr:       false,
			expectedCount: 20,
			expectedTotal: 29,
		},
		{
			name: "test Instance getall API endpoint success with status search query",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: "",
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				SearchQuery: cutil.GetPtr("error"),
			},
			wantErr:       false,
			expectedCount: 0,
			expectedTotal: 0,
		},
		{
			name: "test Instance getall API endpoint success InstanceStatusReady status",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: "",
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				Statuses: []string{cdbm.InstanceStatusReady},
			},
			wantErr:       false,
			expectedCount: 20,
			expectedTotal: 29,
		},
		{
			name: "test Instance getall API endpoint success multiple statuses",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: "",
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				Statuses: []string{cdbm.InstanceStatusReady, cdbm.InstanceStatusPending},
			},
			wantErr:       false,
			expectedCount: 20,
			expectedTotal: 29,
		},
		{
			name: "test Instance getall API endpoint success with name filter",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst2,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				Names: []string{"test-instance-vpc"},
			},
			wantErr:                   false,
			expectedCount:             1,
			expectedTotal:             1,
			expectedFirstEntryName:    "test-instance-vpc",
			expectedMachineIDOverride: cutil.GetPtr(mc2.ID),
		},
		{
			name: "test Instance getall API endpoint multi-vpc inherited nsg propagation full state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				Names:  []string{"test-instance-vpc-full"},
				VpcIDs: []uuid.UUID{vpcSecondaryFull.ID},
			},
			wantErr:                   false,
			expectedCount:             1,
			expectedTotal:             1,
			expectedFirstEntryName:    "test-instance-vpc-full",
			expectedMachineIDOverride: cutil.GetPtr(mc4.ID),
			expectedNetworkSecurityGroupInheritedByName: map[string]bool{
				"test-instance-vpc-full": true,
			},
			expectedSecondaryVpcIDsByName: map[string][]string{
				"test-instance-vpc-full": {vpcSecondaryFull.ID.String()},
			},
			expectedPropagationDetailsByName: map[string]struct {
				DetailedStatus string
				Status         string
			}{
				"test-instance-vpc-full": {
					DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusFull,
					Status:         model.APINetworkSecurityGroupPropagationStatusSynchronized,
				},
			},
		},
		{
			name: "test Instance getall API endpoint multi-vpc inherited nsg propagation none state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				Names:  []string{"test-instance-vpc-none"},
				VpcIDs: []uuid.UUID{vpcSecondaryNone.ID},
			},
			wantErr:                   false,
			expectedCount:             1,
			expectedTotal:             1,
			expectedFirstEntryName:    "test-instance-vpc-none",
			expectedMachineIDOverride: cutil.GetPtr(mc5.ID),
			expectedNetworkSecurityGroupInheritedByName: map[string]bool{
				"test-instance-vpc-none": true,
			},
			expectedSecondaryVpcIDsByName: map[string][]string{
				"test-instance-vpc-none": {vpcSecondaryNone.ID.String()},
			},
			expectedPropagationDetailsByName: map[string]struct {
				DetailedStatus string
				Status         string
			}{
				"test-instance-vpc-none": {
					DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusNone,
					Status:         model.APINetworkSecurityGroupPropagationStatusSynchronizing,
				},
			},
		},
		{
			name: "test Instance getall API endpoint multi-vpc inherited nsg propagation partial state",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			filter: cdbm.InstanceFilterInput{
				Names:  []string{"test-instance-vpc-partial"},
				VpcIDs: []uuid.UUID{vpcSecondaryPartial.ID},
			},
			wantErr:                   false,
			expectedCount:             1,
			expectedTotal:             1,
			expectedFirstEntryName:    "test-instance-vpc-partial",
			expectedMachineIDOverride: cutil.GetPtr(mc6.ID),
			expectedNetworkSecurityGroupInheritedByName: map[string]bool{
				"test-instance-vpc-partial": true,
			},
			expectedSecondaryVpcIDsByName: map[string][]string{
				"test-instance-vpc-partial": {vpcSecondaryPartial.ID.String()},
			},
			expectedPropagationDetailsByName: map[string]struct {
				DetailedStatus string
				Status         string
			}{
				"test-instance-vpc-partial": {
					DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusPartial,
					Status:         model.APINetworkSecurityGroupPropagationStatusSynchronizing,
				},
			},
		},
		{
			name: "test Instance getall API endpoint success with single IP address filter",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			ipAddresses:            []string{"192.168.1.100"},
			wantErr:                false,
			expectedCount:          1,
			expectedTotal:          1,
			expectedFirstEntryName: "test-instance-11",
		},
		{
			name: "test Instance getall API endpoint success with multiple IP address filter",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			ipAddresses:   []string{"192.168.1.100", "192.168.2.200"},
			wantErr:       false,
			expectedCount: 2,
			expectedTotal: 2,
		},
		{
			name: "test Instance getall API endpoint success with non-existent IP address filter",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			ipAddresses:   []string{"10.0.0.1"},
			wantErr:       false,
			expectedCount: 0,
			expectedTotal: 0,
		},
		{
			name: "test Instance getall API endpoint success BadStatus status returns no objects",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 nil,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: "",
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusBadRequest,
			},
			filter: cdbm.InstanceFilterInput{
				Statuses: []string{"BadStatus"},
			},
			wantErr:       false,
			expectedCount: 0,
			expectedTotal: 0,
		},
		{
			name: "test Instance getall with sort by machine id",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			pageNumber:                cutil.GetPtr(1),
			pageSize:                  cutil.GetPtr(10),
			orderBy:                   cutil.GetPtr("MACHINE_ID_DESC"),
			expectedCount:             10,
			expectedTotal:             29,
			expectedFirstEntryName:    "test-instance-vpc-partial",
			wantErr:                   false,
			expectedMachineIDOverride: &mc6.ID,
			expectedAnyNetworkSecurityGroupNotInherited: true,
		},
		{
			name: "test Instance getall with sort by org display name",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			pageNumber:             cutil.GetPtr(1),
			pageSize:               cutil.GetPtr(10),
			orderBy:                cutil.GetPtr("TENANT_ORG_DISPLAY_NAME_ASC"),
			expectedCount:          10,
			expectedTotal:          29,
			expectedFirstEntryName: instarr[0].Name,
			wantErr:                false,
		},
		{
			name: "test Instance getall with sort by instance type name",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			pageNumber:             cutil.GetPtr(1),
			pageSize:               cutil.GetPtr(10),
			orderBy:                cutil.GetPtr("INSTANCE_TYPE_NAME_ASC"),
			expectedCount:          10,
			expectedTotal:          29,
			expectedFirstEntryName: instarr[0].Name,
			wantErr:                false,
		},
		{
			name: "test Instance getall with sort by has infiniband",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			args: args{
				reqInstance:                 inst1,
				reqSiteIDs:                  []string{st1.ID.String()},
				reqInfrastructureProviderID: ip.ID.String(),
				reqOrg:                      tnOrg1,
				reqUser:                     tnu1,
				respCode:                    http.StatusOK,
			},
			pageNumber:             cutil.GetPtr(1),
			pageSize:               cutil.GetPtr(10),
			orderBy:                cutil.GetPtr("HAS_INFINIBAND_ASC"),
			expectedCount:          10,
			expectedTotal:          29,
			expectedFirstEntryName: instarr[0].Name,
			wantErr:                false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := GetAllInstanceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				cfg:       tt.fields.cfg,
			}

			// Prepare siteId query param
			sq := make(url.Values)
			for _, siteID := range tt.args.reqSiteIDs {
				sq.Add("siteId", siteID)
			}

			// infrastructureProviderId query param is an optional
			if tt.args.reqInfrastructureProviderID != "" {
				sq.Set("infrastructureProviderId", tt.args.reqInfrastructureProviderID)
			}
			instanceIDPath := fmt.Sprintf("/v2/org/%s/nico/instance?%s", tn1.Org, sq.Encode())

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, instanceIDPath, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			if tt.queryIncludeRelationTenant != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationTenant)
			}
			if tt.queryIncludeRelationSite != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationSite)
			}
			if tt.queryIncludeRelationVpc != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationVpc)
			}
			if tt.queryIncludeRelationInstanceType != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationInstanceType)
			}
			if tt.queryIncludeRelationProvider != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationProvider)
			}
			if tt.queryIncludeRelationMachine != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationMachine)
			}
			if tt.queryIncludeRelationOperatingSystem != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationOperatingSystem)
			}

			if tt.queryIncludeRelationNetworkSecurityGroup != nil {
				q.Add("includeRelation", *tt.queryIncludeRelationNetworkSecurityGroup)
			}

			for _, instanceTypeID := range tt.filter.InstanceTypeIDs {
				q.Add("instanceTypeId", instanceTypeID.String())
			}
			for _, operatingSystemID := range tt.filter.OperatingSystemIDs {
				q.Add("operatingSystemId", operatingSystemID.String())
			}
			for _, machineID := range tt.filter.MachineIDs {
				q.Add("machineId", machineID)
			}
			for _, vpcID := range tt.filter.VpcIDs {
				q.Add("vpcId", vpcID.String())
			}
			if tt.filter.SearchQuery != nil {
				q.Add("query", *tt.filter.SearchQuery)
			}
			for _, name := range tt.filter.Names {
				q.Add("name", name)
			}
			for _, status := range tt.filter.Statuses {
				q.Add("status", status)
			}
			for _, ipAddress := range tt.ipAddresses {
				q.Add("ipAddress", ipAddress)
			}

			if tt.pageNumber != nil {
				q.Set("pageNumber", fmt.Sprintf("%v", *tt.pageNumber))
			}
			if tt.pageSize != nil {
				q.Set("pageSize", fmt.Sprintf("%v", *tt.pageSize))
			}
			if tt.orderBy != nil {
				q.Set("orderBy", *tt.orderBy)
			}
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.reqOrg)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("GetAllInstanceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("GetAllInstanceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusOK {
				return
			}

			rst := []model.APIInstance{}
			serr := json.Unmarshal(rec.Body.Bytes(), &rst)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, tt.expectedCount, len(rst))
			if tt.expectedFirstEntryName != "" {
				assert.Equal(t, tt.expectedFirstEntryName, rst[0].Name)
			}

			ph := rec.Header().Get(pagination.ResponseHeaderName)
			assert.NotEmpty(t, ph)

			pr := &pagination.PageResponse{}
			err := json.Unmarshal([]byte(ph), pr)
			assert.NoError(t, err)

			assert.Equal(t, tt.expectedTotal, pr.Total)

			if len(rst) > 0 {
				expectedMachineID := cutil.GetPtr(mc1.ID)
				if tt.expectedMachineIDOverride != nil {
					expectedMachineID = tt.expectedMachineIDOverride
				}

				assert.Equal(t, *rst[0].MachineID, *expectedMachineID)
				assert.Equal(t, rst[0].Status, cdbm.InstanceStatusReady)

				if tt.expectSubnet {
					assert.Equal(t, *rst[0].Interfaces[0].SubnetID, instsub1.SubnetID.String())
					assert.NotNil(t, rst[0].Interfaces[0].Subnet)
				}

				if tt.expectedAnyNetworkSecurityGroupInherited {

					found := false
					for _, ins := range rst {
						if ins.NetworkSecurityGroupInherited {
							found = true
							break
						}

					}

					assert.True(t, found)
				}

				if tt.expectedAnyNetworkSecurityGroupNotInherited {

					found := false
					for _, ins := range rst {
						if !ins.NetworkSecurityGroupInherited {
							found = true
							break
						}

					}

					assert.True(t, found)
				}
			}

			if tt.queryIncludeRelationTenant != nil {
				assert.Equal(t, *tt.expectedTenantOrg, rst[0].Tenant.Org)
			}

			if tt.queryIncludeRelationSite != nil {
				assert.Equal(t, *tt.expectedSiteName, rst[0].Site.Name)
			}

			if tt.queryIncludeRelationVpc != nil {
				assert.Equal(t, *tt.expectedVpcName, rst[0].Vpc.Name)
			}

			if tt.queryIncludeRelationInstanceType != nil {
				assert.Equal(t, *tt.expectedInstanceTypeName, rst[0].InstanceType.Name)
			}

			if tt.queryIncludeRelationProvider != nil {
				assert.Equal(t, *tt.expectedInfrastructureProviderOrg, rst[0].InfrastructureProvider.Org)
			}

			if tt.queryIncludeRelationMachine != nil {
				assert.Equal(t, *tt.expectedMachineControllerID, rst[0].Machine.ControllerMachineID)
			}

			if tt.queryIncludeRelationOperatingSystem != nil {
				assert.Equal(t, *tt.expectedOperatingSystemName, rst[0].OperatingSystem.Name)
			}

			if tt.queryIncludeRelationNetworkSecurityGroup != nil {
				assert.Equal(t, *tt.expectedNetworkSecurityGroupName, rst[0].NetworkSecurityGroup.Name)

				// If we expected an NSG, inherited should be false.
				if tt.expectedNetworkSecurityGroupName != nil {
					assert.Equal(t, false, rst[0].NetworkSecurityGroupInherited)
				}
			}

			for _, apiInst := range rst {
				assert.Equal(t, 2, len(apiInst.StatusHistory))
			}

			if tt.expectedSecondaryVpcIDsByName != nil {
				for _, apiInst := range rst {
					if expected, ok := tt.expectedSecondaryVpcIDsByName[apiInst.Name]; ok {
						assert.ElementsMatch(t, expected, apiInst.SecondaryVpcIDs, apiInst.Name)
					}
				}
			}

			if tt.expectedNetworkSecurityGroupInheritedByName != nil {
				for _, apiInst := range rst {
					if expected, ok := tt.expectedNetworkSecurityGroupInheritedByName[apiInst.Name]; ok {
						assert.Equal(t, expected, apiInst.NetworkSecurityGroupInherited, apiInst.Name)
					}
				}
			}

			if tt.expectedPropagationDetailsByName != nil {
				for _, apiInst := range rst {
					if expected, ok := tt.expectedPropagationDetailsByName[apiInst.Name]; ok {
						require.NotNil(t, apiInst.NetworkSecurityGroupPropagationDetails, apiInst.Name)
						assert.Equal(t, expected.DetailedStatus, apiInst.NetworkSecurityGroupPropagationDetails.DetailedStatus, apiInst.Name)
						assert.Equal(t, expected.Status, apiInst.NetworkSecurityGroupPropagationDetails.Status, apiInst.Name)
					}
				}
			}

			if tt.expectedIBInterfaceID != nil {
				assert.Equal(t, *tt.expectedIBInterfaceID, rst[0].InfiniBandInterfaces[0].ID)
				assert.NotNil(t, rst[0].InfiniBandInterfaces[0].InfiniBandPartition)
				assert.NotEqual(t, rst[0].InfiniBandInterfaces[0].InfiniBandPartition.SiteID, "")
			}

			if tt.expectedDpuExtensionServiceDeploymentID != nil {
				assert.Greater(t, len(rst[0].DpuExtensionServiceDeployments), 0)
				assert.Equal(t, *tt.expectedDpuExtensionServiceDeploymentID, rst[0].DpuExtensionServiceDeployments[0].ID)
				assert.NotNil(t, rst[0].DpuExtensionServiceDeployments[0].DpuExtensionService)
				assert.NotEqual(t, rst[0].DpuExtensionServiceDeployments[0].Version, "")
				assert.NotEqual(t, rst[0].DpuExtensionServiceDeployments[0].Status, "")
			}

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestDeleteInstanceHandler_Handle(t *testing.T) {
	ctx := context.Background()

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	stNotReady := testInstanceBuildSite(t, dbSession, ip, "test-site-not-ready", cdbm.SiteStatusPending, true, ipu)
	assert.NotNil(t, stNotReady)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	vpc2 := testInstanceBuildVPC(t, dbSession, "test-vpc-2", ip, tn1, st1, nil, nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusPending, tnu1)
	assert.NotNil(t, vpc2)

	vpc3 := testInstanceBuildVPC(t, dbSession, "test-vpc-3", ip, tn1, stNotReady, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc3)

	subnet1 := testInstanceBuildSubnet(t, dbSession, "test-subnet-1", tn1, vpc1, cutil.GetPtr(uuid.New()), cdbm.SubnetStatusReady, tnu1)
	assert.NotNil(t, subnet1)

	subnet2 := testInstanceBuildSubnet(t, dbSession, "test-subnet-2", tn1, vpc1, nil, cdbm.SubnetStatusPending, tnu1)
	assert.NotNil(t, subnet2)

	mci1 := testInstanceBuildMachineInterface(t, dbSession, subnet1.ID, mc1.ID)
	assert.NotNil(t, mci1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	instVpcNotReady := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc2.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instVpcNotReady)

	instSiteNotReady := testInstanceBuildInstance(t, dbSession, "test-instance-2", tn1.ID, ip.ID, stNotReady.ID, &ist1.ID, vpc3.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, instSiteNotReady)

	instsub1 := testInstanceBuildInstanceInterface(t, dbSession, inst1.ID, &subnet1.ID, nil, nil, cdbm.InterfaceStatusPending)
	assert.NotNil(t, instsub1)

	e := echo.New()
	cfg := common.GetTestConfig()
	tc := &tmocks.Client{}

	// Mock per-Site client for st1
	tsc := &tmocks.Client{}

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	//
	// Timeout mocking
	//
	scpWithTimeout := sc.NewClientPool(tcfg)
	tscWithTimeout := &tmocks.Client{}

	scpWithTimeout.IDClientMap[st1.ID.String()] = tscWithTimeout

	wrunTimeout := &tmocks.WorkflowRun{}
	wrunTimeout.On("GetID").Return("workflow-with-timeout")

	wrunTimeout.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewTimeoutError(enums.TIMEOUT_TYPE_UNSPECIFIED, nil, nil))

	tscWithTimeout.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInstanceV2", mock.Anything).Return(wrunTimeout, nil)

	tscWithTimeout.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// NICo not-found mocking
	//
	scpWithNICoNotFound := sc.NewClientPool(tcfg)
	tscWithNICoNotFound := &tmocks.Client{}

	scpWithNICoNotFound.IDClientMap[st1.ID.String()] = tscWithNICoNotFound

	wrunWithNICoNotFound := &tmocks.WorkflowRun{}
	wrunWithNICoNotFound.On("GetID").Return("workflow-WithNICoNotFound")

	wrunWithNICoNotFound.Mock.On("Get", mock.Anything, mock.Anything).Return(tp.NewNonRetryableApplicationError("NICo went bananas", swe.ErrTypeNICoObjectNotFound, errors.New("NICo went bananas")))

	tscWithNICoNotFound.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInstanceV2", mock.Anything).Return(wrunWithNICoNotFound, nil)

	tscWithNICoNotFound.Mock.On("TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(nil)

	//
	// Normal mocks
	//
	scp := sc.NewClientPool(tcfg)
	scp.IDClientMap[st1.ID.String()] = tsc

	wid := "test-workflow-id"
	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return(wid)

	wrun.Mock.On("Get", mock.Anything, mock.Anything).Return(nil)

	tc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		mock.AnythingOfType("func(internal.Context, uuid.UUID) error"), mock.AnythingOfType("uuid.UUID")).Return(wrun, nil)

	tsc.Mock.On("ExecuteWorkflow", mock.Anything, mock.AnythingOfType("internal.StartWorkflowOptions"),
		"DeleteInstanceV2", mock.Anything).Return(wrun, nil)

	type fields struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}
	type args struct {
		reqData     *model.APIInstanceDeleteRequest
		reqOrg      string
		reqUser     *cdbm.User
		reqInstance string
		respCode    int
	}

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "test Instance delete API endpoint with MachineHealthIssue passes issue to workflow with success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceDeleteRequest{MachineHealthIssue: &model.APIMachineHealthIssue{
					Category: "Hardware", Summary: cutil.GetPtr("Some summary"), Details: cutil.GetPtr("Some details"),
				}},
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusAccepted,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance delete API endpoint failure due to MachineHealthIssue with unspecified category",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqData: &model.APIInstanceDeleteRequest{MachineHealthIssue: &model.APIMachineHealthIssue{
					Category: "UNSPECIFIED", Summary: cutil.GetPtr("Some summary"), Details: cutil.GetPtr("Some details"),
				}},
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance delete API endpoint failure with nico not-found response",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithNICoNotFound,
				scp:       scpWithNICoNotFound,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusAccepted,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance delete API endpoint failure with timeout",
			fields: fields{
				dbSession: dbSession,
				tc:        tscWithTimeout,
				scp:       scpWithTimeout,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusInternalServerError,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance delete API endpoint success",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusAccepted,
			},
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			name: "test Instance delete API endpoint failure, org does not have a Tenant associated",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1.ID.String(),
				reqOrg:      ipOrg,
				reqUser:     ipu,
				respCode:    http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance delete API endpoint failure, invalid Instance ID in request",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqInstance: "",
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusBadRequest,
			},
			wantErr: false,
		},
		{
			name: "test Instance delete API endpoint failure, Instance not found",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqInstance: uuid.New().String(),
				reqOrg:      tnOrg1,
				reqUser:     tnu1,
				respCode:    http.StatusNotFound,
			},
			wantErr: false,
		},
		{
			name: "test Instance delete API endpoint failure, Instance not belong to current tenant",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqInstance: inst1.ID.String(),
				reqOrg:      tnOrg2,
				reqUser:     tnu2,
				respCode:    http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance delete API endpoint failure, VPC is not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqInstance: instVpcNotReady.ID.String(),
				reqOrg:      tnOrg2,
				reqUser:     tnu2,
				respCode:    http.StatusForbidden,
			},
			wantErr: false,
		},
		{
			name: "test Instance delete API endpoint failure, Site is not ready",
			fields: fields{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			args: args{
				reqInstance: instSiteNotReady.ID.String(),
				reqOrg:      tnOrg2,
				reqUser:     tnu2,
				respCode:    http.StatusForbidden,
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			csh := DeleteInstanceHandler{
				dbSession: tt.fields.dbSession,
				tc:        tt.fields.tc,
				scp:       tt.fields.scp,
				cfg:       tt.fields.cfg,
			}

			jsonData, _ := json.Marshal(tt.args.reqData)

			// Setup echo server/context
			req := httptest.NewRequest(http.MethodDelete, "/", strings.NewReader(string(jsonData)))
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/instance/%v", tt.args.reqOrg, tt.args.reqInstance))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.args.reqOrg, tt.args.reqInstance)
			ec.Set("user", tt.args.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			if err := csh.Handle(ec); (err != nil) != tt.wantErr {
				t.Errorf("DeleteInstanceHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.args.respCode != rec.Code {
				t.Errorf("DeleteInstanceHandler.Handle() resp = %v", rec.Body.String())
			}

			require.Equal(t, tt.args.respCode, rec.Code)
			if tt.args.respCode != http.StatusAccepted {
				return
			}
			assertDeletionAcceptedResponse(t, rec.Body.Bytes())

			// Verify Instance in terminating state
			insDAO := cdbm.NewInstanceDAO(dbSession)
			insID, _ := uuid.Parse(tt.args.reqInstance)
			dinstance, terr := insDAO.GetByID(context.Background(), nil, insID, nil)
			assert.Nil(t, terr)
			assert.Equal(t, cdbm.InstanceStatusTerminating, dinstance.Status)

			sdDAO := cdbm.NewStatusDetailDAO(dbSession)
			statusDetails, _, serr := sdDAO.GetAll(context.Background(), nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{tt.args.reqInstance}}, cdbp.PageInput{})
			require.NoError(t, serr)
			require.NotEmpty(t, statusDetails)
			require.NotNil(t, statusDetails[0].Message)

			siteClient, scErr := tt.fields.scp.GetClientByID(dinstance.SiteID)
			require.NoError(t, scErr)
			mockSiteClient, ok := siteClient.(*tmocks.Client)
			require.True(t, ok, "site temporal client should be a test mock")

			var releaseReq *corev1.InstanceReleaseRequest
			for i := len(mockSiteClient.Calls) - 1; i >= 0; i-- {
				call := mockSiteClient.Calls[i]
				if call.Method != "ExecuteWorkflow" || len(call.Arguments) <= 3 {
					continue
				}
				wfName, ok := call.Arguments[2].(string)
				if !ok || wfName != "DeleteInstanceV2" {
					continue
				}
				req, ok := call.Arguments[3].(*corev1.InstanceReleaseRequest)
				if !ok || req.GetId().GetValue() != tt.args.reqInstance {
					continue
				}
				releaseReq = req
				break
			}
			require.NotNil(t, releaseReq, "DeleteInstanceV2 workflow should have been called for this Instance")

			require.NotNil(t, releaseReq.DeleteAttribution)
			require.NotNil(t, releaseReq.DeleteAttribution.InitiatedBy)
			assert.Equal(t, tt.args.reqOrg, releaseReq.DeleteAttribution.InitiatedBy.Org)
			assert.Equal(t, tt.args.reqUser.ID.String(), releaseReq.DeleteAttribution.InitiatedBy.UserId)
			assert.Equal(t, dinstance.TenantID.String(), releaseReq.DeleteAttribution.InitiatedBy.TenantId)

			if tt.args.reqData != nil && tt.args.reqData.MachineHealthIssue != nil {
				require.NotNil(t, releaseReq.Issue)
				if tt.args.reqData.MachineHealthIssue.Details != nil {
					assert.Equal(t, *tt.args.reqData.MachineHealthIssue.Details, releaseReq.Issue.Details)
				}
				if tt.args.reqData.MachineHealthIssue.Summary != nil {
					assert.Equal(t, *tt.args.reqData.MachineHealthIssue.Summary, releaseReq.Issue.Summary)
				}
			}
		})
	}
}
func TestNewCreateInstanceHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want CreateInstanceHandler
	}{
		{
			name: "test CreateInstanceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
				scp:       scp,
			},
			want: CreateInstanceHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				dps:        nil,
				scp:        scp,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewCreateInstanceHandler(tt.args.dbSession, tt.args.tc, tt.args.scp, tt.args.cfg, nil)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestNewUpdateInstanceHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	tests := []struct {
		name string
		args args
		want UpdateInstanceHandler
	}{
		{
			name: "test UpdateInstanceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			want: UpdateInstanceHandler{
				dbSession:  dbSession,
				tc:         tc,
				scp:        scp,
				cfg:        cfg,
				dps:        nil,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewUpdateInstanceHandler(tt.args.dbSession, tt.args.tc, tt.args.scp, tt.args.cfg, nil)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestNewGetInstanceHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testVPCInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetInstanceHandler
	}{
		{
			name: "test GetInstanceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetInstanceHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetInstanceHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetInstanceHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewGetAllInstanceHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	tests := []struct {
		name string
		args args
		want GetAllInstanceHandler
	}{
		{
			name: "test GetAllInstanceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				cfg:       cfg,
			},
			want: GetAllInstanceHandler{
				dbSession:  dbSession,
				tc:         tc,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetAllInstanceHandler(tt.args.dbSession, tt.args.tc, tt.args.cfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetAllInstanceHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewDeleteInstanceHandler(t *testing.T) {

	tc := &tmocks.Client{}
	cfg := common.GetTestConfig()

	// Prepare client pool for sync calls
	// to site(s).
	tcfg, _ := cfg.GetTemporalConfig()
	scp := sc.NewClientPool(tcfg)

	type args struct {
		dbSession *cdb.Session
		tc        temporalClient.Client
		scp       *sc.ClientPool
		cfg       *config.Config
	}

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	tests := []struct {
		name string
		args args
		want DeleteInstanceHandler
	}{
		{
			name: "test DeleteInstanceHandler initialization",
			args: args{
				dbSession: dbSession,
				tc:        tc,
				scp:       scp,
				cfg:       cfg,
			},
			want: DeleteInstanceHandler{
				dbSession:  dbSession,
				tc:         tc,
				scp:        scp,
				cfg:        cfg,
				tracerSpan: sutil.NewTracerSpan(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewDeleteInstanceHandler(tt.args.dbSession, tt.args.tc, tt.args.scp, tt.args.cfg, nil)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestInstanceHandler_GetStatusDetails(t *testing.T) {
	ctx := context.Background()

	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testInstanceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipOrgRoles := []string{authz.ProviderAdminRole}

	tnOrg1 := "test-tenant-org-1"
	tnOrgRoles1 := []string{authz.TenantAdminRole}

	tnOrg2 := "test-tenant-org-2"
	tnOrgRoles2 := []string{authz.TenantAdminRole}

	ipu := testInstanceBuildUser(t, dbSession, "test-starfleet-id-1", ipOrg, ipOrgRoles)
	ip := testInstanceSiteBuildInfrastructureProvider(t, dbSession, "test-infrastructure-provider", ipOrg, ipu)

	st1 := testInstanceBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, true, ipu)
	assert.NotNil(t, st1)

	tnu1 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-2", tnOrg1, tnOrgRoles1)
	tn1 := testInstanceBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := testInstanceBuildUser(t, dbSession, "test-starfleet-id-3", tnOrg2, tnOrgRoles2)

	al1 := testInstanceSiteBuildAllocation(t, dbSession, st1, tn1, "test-allocation-1", ipu)
	assert.NotNil(t, al1)

	ist1 := testInstanceBuildInstanceType(t, dbSession, ip, "test-instance-type-1", st1, cdbm.InstanceStatusReady)
	assert.NotNil(t, ist1)

	alc1 := testInstanceSiteBuildAllocationContraints(t, dbSession, al1, cdbm.AllocationResourceTypeInstanceType, ist1.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	assert.NotNil(t, alc1)

	mc1 := testInstanceBuildMachine(t, dbSession, ip.ID, st1.ID, cutil.GetPtr(false), nil)
	assert.NotNil(t, mc1)

	mcinst1 := testInstanceBuildMachineInstanceType(t, dbSession, mc1, ist1)
	assert.NotNil(t, mcinst1)

	os1 := testInstanceBuildOperatingSystem(t, dbSession, "test-operating-system-1", tn1, cdbm.OperatingSystemTypeImage, false, nil, false, cdbm.OperatingSystemStatusReady, tnu1)
	assert.NotNil(t, os1)

	vpc1 := testInstanceBuildVPC(t, dbSession, "test-vpc-1", ip, tn1, st1, cutil.GetPtr(uuid.New()), nil, cutil.GetPtr(cdbm.VpcEthernetVirtualizer), nil, cdbm.VpcStatusReady, tnu1)
	assert.NotNil(t, vpc1)

	inst1 := testInstanceBuildInstance(t, dbSession, "test-instance-1", tn1.ID, ip.ID, st1.ID, &ist1.ID, vpc1.ID, cutil.GetPtr(mc1.ID), &os1.ID, nil, cdbm.InstanceStatusReady)
	assert.NotNil(t, inst1)

	// add status details objects
	totalCount := 30
	for i := 0; i < totalCount; i++ {
		if i%2 != 0 {
			testInstanceBuildStatusDetail(t, dbSession, inst1.ID, cdbm.InstanceStatusPending)
		} else {
			testInstanceBuildStatusDetail(t, dbSession, inst1.ID, cdbm.InstanceStatusReady)
		}
	}

	// init echo
	e := echo.New()

	// init handler
	handler := GetInstanceStatusDetailsHandler{
		dbSession: dbSession,
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name          string
		reqInstanceID string
		reqOrg        string
		reqUser       *cdbm.User
		respCode      int
	}{
		{
			name:          "success",
			reqInstanceID: inst1.ID.String(),
			reqOrg:        tnOrg1,
			reqUser:       tnu1,
			respCode:      http.StatusOK,
		},
		{
			name:          "failure org does not have a Tenant associated",
			reqInstanceID: inst1.ID.String(),
			reqOrg:        ipOrg,
			reqUser:       ipu,
			respCode:      http.StatusForbidden,
		},
		{
			name:          "failure invalid Instance ID in request",
			reqInstanceID: "",
			reqOrg:        tnOrg1,
			reqUser:       tnu1,
			respCode:      http.StatusBadRequest,
		},
		{
			name:          "failure Instance ID in request not found",
			reqInstanceID: uuid.New().String(),
			reqOrg:        tnOrg1,
			reqUser:       tnu1,
			respCode:      http.StatusNotFound,
		},
		{
			name:          "failure Instance not belong to current tenant",
			reqInstanceID: inst1.ID.String(),
			reqOrg:        tnOrg2,
			reqUser:       tnu2,
			respCode:      http.StatusForbidden,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Setup echo server/context
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			rec := httptest.NewRecorder()

			q := req.URL.Query()
			req.URL.RawQuery = q.Encode()

			ec := e.NewContext(req, rec)
			ec.SetPath(fmt.Sprintf("/v2/org/%v/nico/instance/%v/status-history", tt.reqOrg, tt.reqInstanceID))
			ec.SetParamNames("orgName", "id")
			ec.SetParamValues(tt.reqOrg, tt.reqInstanceID)
			ec.Set("user", tt.reqUser)

			ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
			ec.SetRequest(ec.Request().WithContext(ctx))

			assert.NoError(t, handler.Handle(ec))
			assert.Equal(t, tt.respCode, rec.Code)

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

// newTemplatedOsEchoContext returns a bare echo.Context whose request carries a
// background context, which is all the buildInstance*OsConfig helpers require.
func newTemplatedOsEchoContext(t *testing.T) echo.Context {
	t.Helper()
	e := echo.New()
	req := httptest.NewRequest(http.MethodPost, "/", nil)
	req = req.WithContext(context.Background())
	return e.NewContext(req, httptest.NewRecorder())
}

// assertTemplatedOsConfig asserts that a built InstanceOperatingSystemConfig
// references the Operating System by ID (the Templated iPXE variant) rather than
// carrying an inline iPXE script or an OS image reference.
func assertTemplatedOsConfig(t *testing.T, osConfig *corev1.InstanceOperatingSystemConfig, osID uuid.UUID) {
	t.Helper()
	require.NotNil(t, osConfig)
	variant, ok := osConfig.Variant.(*corev1.InstanceOperatingSystemConfig_OperatingSystemId)
	require.Truef(t, ok, "expected OperatingSystemId variant for Templated iPXE OS, got %T", osConfig.Variant)
	require.NotNil(t, variant.OperatingSystemId)
	assert.Equal(t, osID.String(), variant.OperatingSystemId.Value)
}

// TestBuildInstanceOsConfig_TemplatedIPXE verifies that a Templated iPXE
// Operating System is translated into an InstanceOperatingSystemConfig that
// references the OS by ID (corev1.InstanceOperatingSystemConfig_OperatingSystemId)
// across the create, update and batch-create instance paths, and that the shared
// templated-OS site validator rejects OSes that are not usable at the Site.
//
// Unlike raw iPXE (inline script) and Image (OS image ID) types, a Templated
// iPXE OS is rendered on the Site from its template, so only the OS ID is
// propagated to Core, and only once the definition is synchronized to that Site.
func TestBuildInstanceOsConfig_TemplatedIPXE(t *testing.T) {
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()

	common.TestSetupSchema(t, dbSession)

	cfg := common.GetTestConfig()
	logger := zerolog.Nop()

	org := "tmpl-os-instance-org"
	user := testInstanceBuildUser(t, dbSession, uuid.NewString(), org, []string{"tenant_admin"})
	tenant := testInstanceBuildTenant(t, dbSession, "tmpl-os-instance-tenant", org, user)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, "tmpl-os-instance-ip-org", "tmpl-os-instance-provider")
	site := testMachineBuildSite(t, dbSession, ip, "tmpl-os-instance-site", cdbm.SiteStatusRegistered)

	// buildOS builds a tenant-owned Templated iPXE OS.
	buildOS := func(name string) *cdbm.OperatingSystem {
		return testInstanceBuildOperatingSystem(t, dbSession, name, tenant,
			cdbm.OperatingSystemTypeTemplatedIPXE, false, nil, false, cdbm.OperatingSystemStatusReady, user)
	}

	// An OS synchronized (Synced association) to the Site: the happy path.
	osSynced := buildOS("tmpl-os-instance-os-synced")
	testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, site.ID, osSynced.ID)

	providerOS := &cdbm.OperatingSystem{
		ID:                       uuid.New(),
		Name:                     "tmpl-os-instance-provider-os",
		Org:                      ip.Org,
		InfrastructureProviderID: &ip.ID,
		Type:                     cdbm.OperatingSystemTypeTemplatedIPXE,
		IsActive:                 true,
		Status:                   cdbm.OperatingSystemStatusReady,
		CreatedBy:                user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(providerOS).Exec(context.Background())
	require.NoError(t, err)
	testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, site.ID, providerOS.ID)

	t.Run("create", func(t *testing.T) {
		ec := newTemplatedOsEchoContext(t)
		h := CreateInstanceHandler{dbSession: dbSession, cfg: cfg}
		apiReq := &model.APIInstanceCreateRequest{
			TenantID:          tenant.ID.String(),
			OperatingSystemID: cutil.GetPtr(osSynced.ID.String()),
		}

		osConfig, osID, apiErr := h.buildInstanceCreateRequestOsConfig(ec, &logger, apiReq, site)
		require.Nil(t, apiErr)
		require.NotNil(t, osID)
		assert.Equal(t, osSynced.ID, *osID)
		assertTemplatedOsConfig(t, osConfig, osSynced.ID)
	})

	t.Run("update", func(t *testing.T) {
		ec := newTemplatedOsEchoContext(t)
		h := UpdateInstanceHandler{dbSession: dbSession, cfg: cfg}
		// The instance is passed directly (not loaded from the DB) and only needs
		// its Tenant populated for the ownership check.
		instance := &cdbm.Instance{
			ID:       uuid.New(),
			TenantID: tenant.ID,
			Tenant:   tenant,
		}
		apiReq := &model.APIInstanceUpdateRequest{
			OperatingSystemID: cutil.GetPtr(osSynced.ID.String()),
		}

		osConfig, osID, apiErr := h.buildInstanceUpdateRequestOsConfig(ec, &logger, apiReq, instance, site)
		require.Nil(t, apiErr)
		require.NotNil(t, osID)
		assert.Equal(t, osSynced.ID, *osID)
		assertTemplatedOsConfig(t, osConfig, osSynced.ID)
	})

	t.Run("batch create", func(t *testing.T) {
		ec := newTemplatedOsEchoContext(t)
		h := BatchCreateInstanceHandler{dbSession: dbSession, cfg: cfg}
		apiReq := &model.APIBatchInstanceCreateRequest{
			TenantID:          tenant.ID.String(),
			OperatingSystemID: cutil.GetPtr(osSynced.ID.String()),
		}

		osConfig, osID, apiErr := h.buildBatchInstanceCreateRequestOsConfig(ec, &logger, apiReq, site)
		require.Nil(t, apiErr)
		require.NotNil(t, osID)
		assert.Equal(t, osSynced.ID, *osID)
		assertTemplatedOsConfig(t, osConfig, osSynced.ID)
	})

	providerCases := []struct {
		name  string
		build func() (*corev1.InstanceOperatingSystemConfig, *uuid.UUID, *cutil.APIError)
	}{
		{
			name: "create allows provider-owned OS",
			build: func() (*corev1.InstanceOperatingSystemConfig, *uuid.UUID, *cutil.APIError) {
				h := CreateInstanceHandler{dbSession: dbSession, cfg: cfg}
				req := &model.APIInstanceCreateRequest{
					TenantID:          tenant.ID.String(),
					OperatingSystemID: cutil.GetPtr(providerOS.ID.String()),
				}
				return h.buildInstanceCreateRequestOsConfig(newTemplatedOsEchoContext(t), &logger, req, site)
			},
		},
		{
			name: "update allows provider-owned OS",
			build: func() (*corev1.InstanceOperatingSystemConfig, *uuid.UUID, *cutil.APIError) {
				h := UpdateInstanceHandler{dbSession: dbSession, cfg: cfg}
				instance := &cdbm.Instance{ID: uuid.New(), TenantID: tenant.ID, Tenant: tenant}
				req := &model.APIInstanceUpdateRequest{OperatingSystemID: cutil.GetPtr(providerOS.ID.String())}
				return h.buildInstanceUpdateRequestOsConfig(newTemplatedOsEchoContext(t), &logger, req, instance, site)
			},
		},
		{
			name: "batch create allows provider-owned OS",
			build: func() (*corev1.InstanceOperatingSystemConfig, *uuid.UUID, *cutil.APIError) {
				h := BatchCreateInstanceHandler{dbSession: dbSession, cfg: cfg}
				req := &model.APIBatchInstanceCreateRequest{
					TenantID:          tenant.ID.String(),
					OperatingSystemID: cutil.GetPtr(providerOS.ID.String()),
				}
				return h.buildBatchInstanceCreateRequestOsConfig(newTemplatedOsEchoContext(t), &logger, req, site)
			},
		},
	}
	for _, tc := range providerCases {
		t.Run(tc.name, func(t *testing.T) {
			osConfig, osID, apiErr := tc.build()
			require.Nil(t, apiErr)
			require.NotNil(t, osID)
			assert.Equal(t, providerOS.ID, *osID)
			assertTemplatedOsConfig(t, osConfig, providerOS.ID)
		})
	}

	// The following cases exercise the shared validator through the create path;
	// the same validator gates the update and batch paths.

	// Selection is gated on site availability, not on ownership: any Templated iPXE
	// OS that is present (Synced association) at its Site is a usable definition.
	t.Run("allows any templated OS synchronized to Site", func(t *testing.T) {
		osOther := buildOS("tmpl-os-instance-os-other")
		testInstanceBuildOperatingSystemSiteAssociation(t, dbSession, site.ID, osOther.ID)

		ec := newTemplatedOsEchoContext(t)
		h := CreateInstanceHandler{dbSession: dbSession, cfg: cfg}
		apiReq := &model.APIInstanceCreateRequest{
			TenantID:          tenant.ID.String(),
			OperatingSystemID: cutil.GetPtr(osOther.ID.String()),
		}

		osConfig, osID, apiErr := h.buildInstanceCreateRequestOsConfig(ec, &logger, apiReq, site)
		require.Nil(t, apiErr)
		require.NotNil(t, osID)
		assert.Equal(t, osOther.ID, *osID)
		assertTemplatedOsConfig(t, osConfig, osOther.ID)
	})

	// Only a Synced association marks a definition as available at the Site. An
	// OS with no association, or one whose association is still Syncing or has
	// Errored, must be rejected with BadRequest and no osConfig/osID.
	rejectCases := []struct {
		name   string
		status *string // nil => no association at all
	}{
		{name: "rejects templated OS not synchronized to Site", status: nil},
		{name: "rejects templated OS with Syncing association", status: cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusSyncing)},
		{name: "rejects templated OS with Error association", status: cutil.GetPtr(cdbm.OperatingSystemSiteAssociationStatusError)},
	}
	for i, tc := range rejectCases {
		t.Run(tc.name, func(t *testing.T) {
			os := buildOS(fmt.Sprintf("tmpl-os-instance-os-reject-%d", i))
			if tc.status != nil {
				buildOperatingSystemSiteAssociationWithStatus(t, dbSession, site.ID, os.ID, *tc.status)
			}

			ec := newTemplatedOsEchoContext(t)
			h := CreateInstanceHandler{dbSession: dbSession, cfg: cfg}
			apiReq := &model.APIInstanceCreateRequest{
				TenantID:          tenant.ID.String(),
				OperatingSystemID: cutil.GetPtr(os.ID.String()),
			}

			osConfig, osID, apiErr := h.buildInstanceCreateRequestOsConfig(ec, &logger, apiReq, site)
			require.NotNil(t, apiErr)
			assert.Equal(t, http.StatusBadRequest, apiErr.Code)
			assert.Nil(t, osConfig)
			assert.Nil(t, osID)
		})
	}
}

// buildOperatingSystemSiteAssociationWithStatus inserts an
// OperatingSystemSiteAssociation with an explicit status, so tests can exercise
// non-Synced states (Syncing / Error) that the templated-OS site validator must
// reject.
func buildOperatingSystemSiteAssociationWithStatus(t *testing.T, dbSession *cdb.Session, siteID, osID uuid.UUID, status string) {
	t.Helper()
	ossa := &cdbm.OperatingSystemSiteAssociation{
		ID:                uuid.New(),
		OperatingSystemID: osID,
		SiteID:            siteID,
		Version:           cutil.GetPtr("1234"),
		Status:            status,
		Created:           cdb.GetCurTime(),
		Updated:           cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(ossa).Exec(context.Background())
	require.NoError(t, err)
}
