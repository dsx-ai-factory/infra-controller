// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"context"
	"os"
	"reflect"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/prometheus/client_golang/prometheus"
	dto "github.com/prometheus/client_model/go"
	"github.com/stretchr/testify/assert"

	"github.com/uptrace/bun/extra/bundebug"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/roles"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
)

// TestInitDB init DB
func TestInitDB(t *testing.T) *cdb.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// TestSetupSchema setup schema
// reset the tables needed for Instance tests
func TestSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
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
	// create NVLinkLogicalPartition table (before VPC to avoid foreign key issues)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkLogicalPartition)(nil))
	assert.Nil(t, err)
	// create VPC table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	// create VpcPeering table (depends on VPC and Site)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.VpcPeering)(nil))
	assert.Nil(t, err)
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Domain)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
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
	// create IpxeTemplate table (UNIQUE(name) applied by migration in production)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IpxeTemplate)(nil))
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template DROP CONSTRAINT IF EXISTS ipxe_template_name_key")
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template ADD CONSTRAINT ipxe_template_name_key UNIQUE (name)")
	assert.Nil(t, err)
	// create IpxeTemplateSiteAssociation table (composite UNIQUE applied by migration in production)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IpxeTemplateSiteAssociation)(nil))
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template_site_association DROP CONSTRAINT IF EXISTS ipxe_template_site_association_template_id_site_id_key")
	assert.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template_site_association ADD CONSTRAINT ipxe_template_site_association_template_id_site_id_key UNIQUE (ipxe_template_id, site_id)")
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Machine)(nil))
	assert.Nil(t, err)
	// create SKU table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SKU)(nil))
	assert.Nil(t, err)
	// create ExpectedMachine table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.ExpectedMachine)(nil))
	assert.Nil(t, err)
	// create ExpectedSwitch table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.ExpectedSwitch)(nil))
	assert.Nil(t, err)
	// create ExpectedPowerShelf table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.ExpectedPowerShelf)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Instance)(nil))
	assert.Nil(t, err)
	// create NVLinkInterface table (after Instance since it has a foreign key to Instance)
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.NVLinkInterface)(nil))
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
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandPartition)(nil))
	assert.Nil(t, err)
	// create InfiniBandInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InfiniBandInterface)(nil))
	assert.Nil(t, err)
	// create DpuExtensionService table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.DpuExtensionService)(nil))
	assert.Nil(t, err)
	// create DpuExtensionServiceDeployment table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.DpuExtensionServiceDeployment)(nil))
	assert.Nil(t, err)
	// create Interface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Interface)(nil))
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
	// create SSHKey table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKey)(nil))
	assert.Nil(t, err)
	// create SSHKeyAssociation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.SSHKeyAssociation)(nil))
	assert.Nil(t, err)
	// create Status Details table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
}

// TestBuildUser build user
func TestBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, orgs []string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	OrgData := cdbm.OrgData{}
	for _, org := range orgs {
		OrgData[org] = cdbm.Org{
			ID:      123,
			Name:    org,
			OrgType: "ENTERPRISE",
			Roles:   roles,
		}
	}
	u, err := uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
		AuxiliaryID: nil,
		StarfleetID: &starfleetID,
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData:     OrgData,
	})
	assert.Nil(t, err)

	return u
}

// TestBuildInfrastructureProvider build infrastructure provider
func TestBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, name string, org string, user *cdbm.User) *cdbm.InfrastructureProvider {
	ip := &cdbm.InfrastructureProvider{
		ID:             uuid.New(),
		Name:           name,
		Org:            org,
		OrgDisplayName: cutil.GetPtr(org),
		CreatedBy:      user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

// TestBuildSite build site
func TestBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, status string, inventoryReceived *time.Time, user *cdbm.User) *cdbm.Site {
	st := &cdbm.Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(cdb.GetCurTime()),
		IsInfinityEnabled:           true,
		InventoryReceived:           inventoryReceived,
		Status:                      status,
		CreatedBy:                   user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

// TestBuildTenant build tenant
func TestBuildTenant(t *testing.T, dbSession *cdb.Session, orgDisplayName string, org string, config *cdbm.TenantConfig, user *cdbm.User) *cdbm.Tenant {
	tenant := &cdbm.Tenant{
		ID:             uuid.New(),
		Name:           orgDisplayName,
		Org:            org,
		OrgDisplayName: cutil.GetPtr(orgDisplayName),
		Config:         config,
		CreatedBy:      user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

// TestBuildVpc build vpc
func TestBuildVpc(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, tenant *cdbm.Tenant, name string) *cdbm.Vpc {
	vpc := &cdbm.Vpc{
		ID:                       uuid.New(),
		Name:                     name,
		Org:                      "test",
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

// TestBuildSubnet build subnet
func TestBuildSubnet(t *testing.T, dbSession *cdb.Session, tenant *cdbm.Tenant, vpc *cdbm.Vpc, name, status string, segmentID *uuid.UUID) *cdbm.Subnet {
	subnet := &cdbm.Subnet{
		ID:                         uuid.New(),
		Name:                       name,
		SiteID:                     vpc.SiteID,
		VpcID:                      vpc.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: segmentID,
		Status:                     status,
		CreatedBy:                  uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(subnet).Exec(context.Background())
	assert.Nil(t, err)
	return subnet
}

// TestInventoryAgeUpdatedTimestamp backdates every row of a table past the inventory staleness
// threshold. The inventory activities skip updating a row written more recently than that, so a
// fixture that seeds rows and then feeds them a competing inventory has to age them first. Each
// test builds its own schema, so ageing the whole table keeps the fixture setup to one call.
// Pass a typed nil model, for example (*cdbm.Vpc)(nil).
func TestInventoryAgeUpdatedTimestamp(ctx context.Context, t *testing.T, dbSession *cdb.Session, models ...any) {
	t.Helper()

	for _, model := range models {
		query := dbSession.DB.NewUpdate().
			Model(model).
			Set("updated = ?", time.Now().Add(-2*cutil.DefaultInventoryReceiptInterval)).
			Where("1 = 1")

		// Bun restricts an update on a soft-delete model to live rows, and a test that reconciles
		// a deleted row still needs that row aged. Asking for deleted rows on a model without the
		// field is an error, so only widen the scope where the field exists.
		if dbSession.DB.Table(reflect.TypeOf(model).Elem()).SoftDeleteField != nil {
			query = query.WhereAllWithDeleted()
		}

		_, err := query.Exec(ctx)
		assert.NoError(t, err)
	}
}

// TestInventoryAgeDeletedTimestamp backdates a soft-deleted row's delete time past the inventory
// staleness threshold. The inventory activities refuse to undelete a row deleted more recently
// than that, so a fixture that soft-deletes a row and then feeds an inventory still reporting it
// has to age the delete first. Pass a typed nil model, for example (*cdbm.Vpc)(nil).
func TestInventoryAgeDeletedTimestamp(ctx context.Context, t *testing.T, dbSession *cdb.Session, model any, id any) {
	t.Helper()

	_, err := dbSession.DB.NewUpdate().
		Model(model).
		Set("deleted = ?", time.Now().Add(-2*cutil.DefaultInventoryReceiptInterval)).
		Where("id = ?", id).
		WhereAllWithDeleted().
		Exec(ctx)
	assert.NoError(t, err)
}

// TestBuildInfiniBandPartition builds and returns an InfiniBandPartition
func TestBuildInfiniBandPartition(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, tenant *cdbm.Tenant, controllerIBPartitionID *uuid.UUID, status cdbm.InfiniBandPartitionStatus, isMissingOnSite bool) *cdbm.InfiniBandPartition {
	ibp := &cdbm.InfiniBandPartition{
		ID:                      uuid.New(),
		Name:                    name,
		Description:             cutil.GetPtr("Test InfiniBand Partition"),
		Org:                     tenant.Org,
		SiteID:                  site.ID,
		TenantID:                tenant.ID,
		ControllerIBPartitionID: controllerIBPartitionID,
		Status:                  status,
		IsMissingOnSite:         isMissingOnSite,
	}

	_, err := dbSession.DB.NewInsert().Model(ibp).Exec(context.Background())
	assert.Nil(t, err)
	return ibp
}

// TestBuildNVLinkLogicalPartition builds and returns an NVLinkLogicalPartition
func TestBuildNVLinkLogicalPartition(t *testing.T, dbSession *cdb.Session, name string, description *string, site *cdbm.Site, tenant *cdbm.Tenant, status cdbm.NVLinkLogicalPartitionStatus, isMissingOnSite bool) *cdbm.NVLinkLogicalPartition {
	nvllp := &cdbm.NVLinkLogicalPartition{
		ID:              uuid.New(),
		Name:            name,
		Description:     description,
		Org:             tenant.Org,
		SiteID:          site.ID,
		TenantID:        tenant.ID,
		Status:          status,
		IsMissingOnSite: isMissingOnSite,
	}
	_, err := dbSession.DB.NewInsert().Model(nvllp).Exec(context.Background())
	assert.Nil(t, err)
	return nvllp
}

// TestBuildInstanceType build instance type
func TestBuildInstanceType(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, name string) *cdbm.InstanceType {
	instanceType := &cdbm.InstanceType{
		ID:                       uuid.New(),
		Name:                     name,
		InfrastructureProviderID: ip.ID,
		SiteID:                   &site.ID,
		Status:                   cdbm.InstanceTypeStatusPending,
	}
	_, err := dbSession.DB.NewInsert().Model(instanceType).Exec(context.Background())
	assert.Nil(t, err)
	return instanceType
}

// TestBuildAllocation build allocation
func TestBuildAllocation(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, site *cdbm.Site, name string) *cdbm.Allocation {
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

// testBuildAllocationContraints build allocation
func TestBuildAllocationContraints(t *testing.T, dbSession *cdb.Session, al *cdbm.Allocation, rt string, rtID uuid.UUID, ct string, cv int, user *cdbm.User) *cdbm.AllocationConstraint {
	alctDAO := cdbm.NewAllocationConstraintDAO(dbSession)

	alct, err := alctDAO.Create(context.Background(), nil, cdbm.AllocationConstraintCreateInput{
		AllocationID: al.ID, ResourceType: rt, ResourceTypeID: rtID,
		ConstraintType: ct, ConstraintValue: cv, CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	return alct
}

// TestBuildOperatingSystem build operating system
func TestBuildOperatingSystem(t *testing.T, dbSession *cdb.Session, name string) *cdbm.OperatingSystem {
	operatingSystem := &cdbm.OperatingSystem{
		ID:        uuid.New(),
		Name:      name,
		Status:    cdbm.OperatingSystemStatusPending,
		CreatedBy: uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

// TestBuildOperatingSystem build operating system
func TestBuildImageOperatingSystem(t *testing.T, dbSession *cdb.Session, ipID *uuid.UUID, tenantID *uuid.UUID, name string, org string, version *string, status string) *cdbm.OperatingSystem {
	operatingSystem := &cdbm.OperatingSystem{
		ID:                          uuid.New(),
		Name:                        name,
		Org:                         org,
		InfrastructureProviderID:    ipID,
		TenantID:                    tenantID,
		ControllerOperatingSystemID: nil,
		Version:                     version,
		Type:                        cdbm.OperatingSystemTypeImage,
		ImageURL:                    cutil.GetPtr("http://testos.net"),
		ImageSHA:                    cutil.GetPtr("123213ddddsa1231asd"),
		ImageAuthType:               cutil.GetPtr("bear"),
		ImageAuthToken:              cutil.GetPtr("1211331asdadad21123"),
		ImageDisk:                   cutil.GetPtr("disk"),
		RootFsID:                    cutil.GetPtr("rootfsID"),
		RootFsLabel:                 cutil.GetPtr("rootFsLabel"),
		Status:                      status,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(operatingSystem).Exec(context.Background())
	assert.Nil(t, err)
	return operatingSystem
}

// TestBuildImageOperatingSystemSiteAssociation build operating system site association
func TestBuildImageOperatingSystemSiteAssociation(t *testing.T, dbSession *cdb.Session, osID uuid.UUID, siteID uuid.UUID, status string, version string, isMissingOnSite bool) *cdbm.OperatingSystemSiteAssociation {
	osa := &cdbm.OperatingSystemSiteAssociation{
		ID:                uuid.New(),
		OperatingSystemID: osID,
		SiteID:            siteID,
		Version:           &version,
		Status:            status,
		IsMissingOnSite:   isMissingOnSite,
		CreatedBy:         uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(osa).Exec(context.Background())
	assert.Nil(t, err)
	return osa
}

// TestBuildInterface builds and returns a test Interface
func TestBuildInterface(t *testing.T, dbSession *cdb.Session, instanceID, subnetID *uuid.UUID, vpID *uuid.UUID, isPhysical bool, device *string, deviceInstance *int, vfID *int, userID *uuid.UUID, status string) *cdbm.Interface {
	is := &cdbm.Interface{
		ID:                uuid.New(),
		InstanceID:        *instanceID,
		SubnetID:          subnetID,
		VpcPrefixID:       vpID,
		Device:            device,
		DeviceInstance:    deviceInstance,
		IsPhysical:        isPhysical,
		VirtualFunctionID: vfID,
		Status:            status,
		CreatedBy:         *userID,
	}
	_, err := dbSession.DB.NewInsert().Model(is).Exec(context.Background())
	assert.Nil(t, err)
	return is
}

// TestBuildInfiniBandInterface builds and returns a test InfiniBandInterface
func TestBuildInfiniBandInterface(t *testing.T, dbSession *cdb.Session, instanceID, siteID, infiniBandPartitionID uuid.UUID, device string, deviceInstance int, isPhysical bool, vfID *int, status string, isMissingOnSite bool) *cdbm.InfiniBandInterface {
	ibi := &cdbm.InfiniBandInterface{
		ID:                    uuid.New(),
		InstanceID:            instanceID,
		SiteID:                siteID,
		InfiniBandPartitionID: infiniBandPartitionID,
		Device:                device,
		DeviceInstance:        deviceInstance,
		IsPhysical:            isPhysical,
		VirtualFunctionID:     vfID,
		Status:                status,
		IsMissingOnSite:       isMissingOnSite,
	}
	_, err := dbSession.DB.NewInsert().Model(ibi).Exec(context.Background())
	assert.Nil(t, err)
	return ibi
}

// TestBuildDpuExtensionService build DPU Extension Service
func TestBuildDpuExtensionService(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, tenant *cdbm.Tenant, serviceType string, version *string, versionInfo *cdbm.DpuExtensionServiceVersionInfo, activeVersions []string, status string, user *cdbm.User) *cdbm.DpuExtensionService {
	desdDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)
	des, err := desdDAO.Create(context.Background(), nil, cdbm.DpuExtensionServiceCreateInput{
		Name:           name,
		SiteID:         site.ID,
		TenantID:       tenant.ID,
		ServiceType:    serviceType,
		Version:        version,
		VersionInfo:    versionInfo,
		ActiveVersions: activeVersions,
		Status:         status,
		CreatedBy:      user.ID,
	})
	assert.Nil(t, err)
	return des
}

// TestBuildDpuExtensionServiceDeployment build DPU Extension Service Deployment
func TestBuildDpuExtensionServiceDeployment(t *testing.T, dbSession *cdb.Session, dpuExtensionServiceID, siteID, tenantID, instanceID uuid.UUID, version string, status string, user *cdbm.User) *cdbm.DpuExtensionServiceDeployment {
	desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(dbSession)
	desd, err := desdDAO.Create(context.Background(), nil, cdbm.DpuExtensionServiceDeploymentCreateInput{
		DpuExtensionServiceID: dpuExtensionServiceID,
		SiteID:                siteID,
		TenantID:              tenantID,
		InstanceID:            instanceID,
		Version:               version,
		Status:                status,
		CreatedBy:             user.ID,
	})
	assert.Nil(t, err)
	return desd
}

// TestBuildNVLinkInterface builds and returns a test NVLinkInterface
func TestBuildNVLinkInterface(t *testing.T, dbSession *cdb.Session, instanceID, siteID, nvllPartitionID uuid.UUID, device *string, deviceInstance int, gpuGuid *string, nvlinkDomainID *uuid.UUID, status string) *cdbm.NVLinkInterface {
	nvlifc := &cdbm.NVLinkInterface{
		ID:                       uuid.New(),
		InstanceID:               instanceID,
		SiteID:                   siteID,
		NVLinkLogicalPartitionID: nvllPartitionID,
		NVLinkDomainID:           nvlinkDomainID,
		Device:                   device,
		DeviceInstance:           deviceInstance,
		GpuGUID:                  gpuGuid,
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(nvlifc).Exec(context.Background())
	assert.Nil(t, err)
	return nvlifc
}

// TestBuildMachine build machine
func TestBuildMachine(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, controllerMachineType *string, isAssiged *bool, status string) *cdbm.Machine {
	defMacAddr := "00:1B:44:11:3A:B7"
	mid := uuid.NewString()
	m := &cdbm.Machine{
		ID:                       mid,
		InfrastructureProviderID: ip,
		SiteID:                   site,
		ControllerMachineID:      mid,
		ControllerMachineType:    controllerMachineType,
		Metadata:                 nil,
		DefaultMacAddress:        &defMacAddr,
		IsAssigned:               *isAssiged,
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

// TestBuildMachine build machine interface
func TestBuildMachineInterface(t *testing.T, dbSession *cdb.Session, machineID string, controllerInterfaceID, controllerSegmentID, subnetID *uuid.UUID, hostname *string) *cdbm.MachineInterface {
	miDAO := cdbm.NewMachineInterfaceDAO(dbSession)
	ctx := context.Background()
	mi, err := miDAO.Create(
		ctx,
		nil,
		cdbm.MachineInterfaceCreateInput{
			MachineID:             machineID,
			ControllerInterfaceID: controllerInterfaceID,
			ControllerSegmentID:   controllerSegmentID,
			SubnetID:              subnetID,
			Hostname:              cutil.GetPtr("hostname"),
			IsPrimary:             true,
			MacAddress:            cutil.GetPtr("0:0:0:0:0:0"),
			IpAddresses:           []string{"192.168.0.1, 172.168.0.1"},
		},
	)
	assert.Nil(t, err)
	return mi
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

// TestBuildVPC creates a test VPC
func TestBuildVPC(t *testing.T, dbSession *cdb.Session, name string, ip *cdbm.InfrastructureProvider, tn *cdbm.Tenant, st *cdbm.Site, cnvID *uuid.UUID, lb map[string]string, status string, user *cdbm.User) *cdbm.Vpc {
	vDAO := cdbm.NewVpcDAO(dbSession)

	input := cdbm.VpcCreateInput{
		Name:                      name,
		Description:               cutil.GetPtr("Test Vpc"),
		Org:                       st.Org,
		InfrastructureProviderID:  ip.ID,
		TenantID:                  tn.ID,
		SiteID:                    st.ID,
		NetworkVirtualizationType: cutil.GetPtr(cdbm.VpcEthernetVirtualizer),
		ControllerVpcID:           cnvID,
		Labels:                    lb,
		Status:                    status,
		CreatedBy:                 *user,
	}

	vpc, err := vDAO.Create(context.Background(), nil, input)
	assert.Nil(t, err)

	return vpc
}

func TestUpdateVPC(t *testing.T, dbSession *cdb.Session, v *cdbm.Vpc) {
	_, err := dbSession.DB.NewUpdate().Where("id = ?", v.ID).Model(v).Exec(context.Background())
	assert.Nil(t, err)
}

// TestBuildAllocationConstraint creates a test Allocation Constraint of Instance Type
func TestBuildAllocationConstraint(t *testing.T, dbSession *cdb.Session, al *cdbm.Allocation, it *cdbm.InstanceType, constraintValue int, user *cdbm.User) *cdbm.AllocationConstraint {
	acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
	ac, err := acDAO.Create(context.Background(), nil, cdbm.AllocationConstraintCreateInput{
		AllocationID: al.ID, ResourceType: cdbm.AllocationResourceTypeInstanceType,
		ResourceTypeID: it.ID, ConstraintType: cdbm.AllocationConstraintTypeReserved,
		ConstraintValue: constraintValue, CreatedBy: user.ID,
	})
	assert.Nil(t, err)

	return ac
}

// TestBuildInstance creates a test instance
func TestBuildInstance(t *testing.T, dbSession *cdb.Session, name string, tn uuid.UUID, ip uuid.UUID, st uuid.UUID, ist uuid.UUID, vpc uuid.UUID, mc *string, os uuid.UUID, status string) *cdbm.Instance {
	ins := &cdbm.Instance{
		ID:                       uuid.New(),
		Name:                     name,
		TenantID:                 tn,
		InfrastructureProviderID: ip,
		SiteID:                   st,
		InstanceTypeID:           &ist,
		VpcID:                    vpc,
		MachineID:                mc,
		OperatingSystemID:        &os,
		Hostname:                 nil,
		PhoneHomeEnabled:         false,
		UserData:                 nil,
		IpxeScript:               nil,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
		Status:                   status,
	}
	_, err := dbSession.DB.NewInsert().Model(ins).Exec(context.Background())
	assert.Nil(t, err)
	return ins
}

func TestUpdateInstance(t *testing.T, dbSession *cdb.Session, ins *cdbm.Instance) {
	_, err := dbSession.DB.NewUpdate().Where("id = ?", ins.ID).Model(ins).Exec(context.Background())
	assert.Nil(t, err)
}

func TestBuildSSHKeyGroup(t *testing.T, dbSession *cdb.Session, name, org string, description *string, tenantID uuid.UUID, version *string, status string, createdBy uuid.UUID) *cdbm.SSHKeyGroup {
	sshKeyGroup := &cdbm.SSHKeyGroup{
		ID:          uuid.New(),
		Name:        name,
		Org:         org,
		Description: description,
		TenantID:    tenantID,
		Version:     version,
		Status:      status,
		CreatedBy:   createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshKeyGroup).Exec(context.Background())
	assert.Nil(t, err)
	return sshKeyGroup
}

func TestBuildSSHKeyGroupSiteAssociation(t *testing.T, dbSession *cdb.Session, sshKeyGroupID uuid.UUID, siteID uuid.UUID, version *string, status string, createdBy uuid.UUID) *cdbm.SSHKeyGroupSiteAssociation {
	skgsa := &cdbm.SSHKeyGroupSiteAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: sshKeyGroupID,
		SiteID:        siteID,
		Version:       version,
		Status:        status,
		CreatedBy:     createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(skgsa).Exec(context.Background())
	assert.Nil(t, err)
	return skgsa
}

func TestBuildSSHKeyGroupInstanceAssociation(t *testing.T, dbSession *cdb.Session, sshKeyGroupID uuid.UUID, siteID uuid.UUID, instanceID uuid.UUID, createdBy uuid.UUID) *cdbm.SSHKeyGroupInstanceAssociation {
	skgia := &cdbm.SSHKeyGroupInstanceAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: sshKeyGroupID,
		SiteID:        siteID,
		InstanceID:    instanceID,
		CreatedBy:     createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(skgia).Exec(context.Background())
	assert.Nil(t, err)
	return skgia
}

// TestBuildSSHKey creates a test SSH Key
func TestBuildSSHKey(t *testing.T, dbSession *cdb.Session, name string, tenant *cdbm.Tenant, publicKey string, user *cdbm.User) *cdbm.SSHKey {
	sshDAO := cdbm.NewSSHKeyDAO(dbSession)

	sshKey, err := sshDAO.Create(
		context.Background(),
		nil,
		cdbm.SSHKeyCreateInput{
			Name:      name,
			TenantOrg: tenant.Org,
			TenantID:  tenant.ID,
			PublicKey: publicKey,
			CreatedBy: user.ID,
		},
	)
	assert.Nil(t, err)

	return sshKey
}

func TestBuildSSHKeyAssociation(t *testing.T, dbSession *cdb.Session, sshKeyGroupID uuid.UUID, sshKeyID uuid.UUID, createdBy uuid.UUID) *cdbm.SSHKeyAssociation {
	sshKeyAssociation := &cdbm.SSHKeyAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: sshKeyGroupID,
		SSHKeyID:      sshKeyID,
		CreatedBy:     createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(sshKeyAssociation).Exec(context.Background())
	assert.Nil(t, err)
	return sshKeyAssociation
}

func TestBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID string, status string, message *string) *cdbm.StatusDetail {
	statusDetails := &cdbm.StatusDetail{
		ID:       uuid.New(),
		EntityID: entityID,
		Status:   status,
		Message:  message,
	}
	_, err := dbSession.DB.NewInsert().Model(statusDetails).Exec(context.Background())
	assert.Nil(t, err)
	return statusDetails
}

func TestBuildTenantSiteAssociation(t *testing.T, dbSession *cdb.Session, org string, tenantID uuid.UUID, siteID uuid.UUID, createdBy uuid.UUID) *cdbm.TenantSite {
	tenantsiteassociation := &cdbm.TenantSite{
		ID:                  uuid.New(),
		TenantID:            tenantID,
		SiteID:              siteID,
		EnableSerialConsole: false,
		CreatedBy:           createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(tenantsiteassociation).Exec(context.Background())
	assert.Nil(t, err)
	return tenantsiteassociation
}

func TestBuildBuildIPBlock(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, ip *cdbm.InfrastructureProvider, tenantID *uuid.UUID, routingType, prefix string, blockSize int, protocolVersion string, fullGrant bool, status string, user *cdbm.User) *cdbm.IPBlock {
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

// TestBuildVpcPeering inserts a VpcPeering row. The schema enforces FK
// constraints on (vpc1_id, vpc2_id, site_id, infrastructure_provider_id,
// tenant_id), so callers must pass IDs of rows that already exist.
func TestBuildVpcPeering(t *testing.T, dbSession *cdb.Session, vpc1ID, vpc2ID, siteID uuid.UUID, ipID uuid.UUID, tenantID uuid.UUID, createdBy uuid.UUID) *cdbm.VpcPeering {
	vp := &cdbm.VpcPeering{
		ID:                       uuid.New(),
		Vpc1ID:                   vpc1ID,
		Vpc2ID:                   vpc2ID,
		SiteID:                   siteID,
		InfrastructureProviderID: &ipID,
		TenantID:                 &tenantID,
		IsMultiTenant:            false,
		Status:                   cdbm.VpcPeeringStatusReady,
		CreatedBy:                createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(vp).Exec(context.Background())
	assert.Nil(t, err)
	return vp
}

// TestBuildNetworkSecurityGroup inserts a minimal NetworkSecurityGroup row for
// a given site/tenant. Rules are intentionally left empty since callers care
// about the cleanup behaviour, not the rule contents.
func TestBuildNetworkSecurityGroup(t *testing.T, dbSession *cdb.Session, name string, site *cdbm.Site, tenant *cdbm.Tenant, status string, user *cdbm.User) *cdbm.NetworkSecurityGroup {
	nsg := &cdbm.NetworkSecurityGroup{
		ID:        uuid.NewString(),
		Name:      name,
		SiteID:    site.ID,
		TenantOrg: tenant.Org,
		TenantID:  tenant.ID,
		Status:    status,
		CreatedBy: user.ID,
		UpdatedBy: user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(nsg).Exec(context.Background())
	assert.Nil(t, err)
	return nsg
}

// TestBuildExpectedMachine inserts a minimal ExpectedMachine row for the given
// site.
func TestBuildExpectedMachine(t *testing.T, dbSession *cdb.Session, site *cdbm.Site, bmcMacAddress, chassisSerialNumber string, user *cdbm.User) *cdbm.ExpectedMachine {
	em := &cdbm.ExpectedMachine{
		ID:                  uuid.New(),
		SiteID:              site.ID,
		BmcMacAddress:       bmcMacAddress,
		ChassisSerialNumber: chassisSerialNumber,
		CreatedBy:           user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(em).Exec(context.Background())
	assert.Nil(t, err)
	return em
}

// TestBuildExpectedSwitch inserts a minimal ExpectedSwitch row for the given
// site.
func TestBuildExpectedSwitch(t *testing.T, dbSession *cdb.Session, site *cdbm.Site, bmcMacAddress, switchSerialNumber string, user *cdbm.User) *cdbm.ExpectedSwitch {
	es := &cdbm.ExpectedSwitch{
		ID:                 uuid.New(),
		SiteID:             site.ID,
		BmcMacAddress:      bmcMacAddress,
		SwitchSerialNumber: switchSerialNumber,
		CreatedBy:          user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(es).Exec(context.Background())
	assert.Nil(t, err)
	return es
}

// TestBuildExpectedPowerShelf inserts a minimal ExpectedPowerShelf row for the
// given site.
func TestBuildExpectedPowerShelf(t *testing.T, dbSession *cdb.Session, site *cdbm.Site, bmcMacAddress, shelfSerialNumber string, user *cdbm.User) *cdbm.ExpectedPowerShelf {
	eps := &cdbm.ExpectedPowerShelf{
		ID:                uuid.New(),
		SiteID:            site.ID,
		BmcMacAddress:     bmcMacAddress,
		ShelfSerialNumber: shelfSerialNumber,
		CreatedBy:         user.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(eps).Exec(context.Background())
	assert.Nil(t, err)
	return eps
}

// TestBuildSku inserts a minimal SKU row for the given site.
func TestBuildSku(t *testing.T, dbSession *cdb.Session, skuID string, site *cdbm.Site) *cdbm.SKU {
	sk := &cdbm.SKU{
		ID:     skuID,
		SiteID: site.ID,
	}
	_, err := dbSession.DB.NewInsert().Model(sk).Exec(context.Background())
	assert.Nil(t, err)
	return sk
}

func TestBuildVPCPrefix(t *testing.T, dbSession *cdb.Session, name string, st *cdbm.Site, tenant *cdbm.Tenant, vpcID uuid.UUID, ipv4BlockID *uuid.UUID, prefix *string, prefixLength *int, status string, user *cdbm.User) *cdbm.VpcPrefix {
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dbSession)

	vpcprefix, err := vpcPrefixDAO.Create(context.Background(), nil, cdbm.VpcPrefixCreateInput{Name: name, TenantOrg: st.Org, SiteID: st.ID, VpcID: vpcID, TenantID: tenant.ID, IpBlockID: ipv4BlockID, Prefix: *prefix, PrefixLength: *prefixLength, Status: status, CreatedBy: user.ID})
	assert.Nil(t, err)

	return vpcprefix
}

// TestTemporalSiteClientPool creates a Temporal site client pool
func TestTemporalSiteClientPool(t *testing.T) *sc.ClientPool {
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

// TestBuildStatusDetailWithTime creates a StatusDetail with a specific timestamp
// This is useful for metrics testing where precise timing is needed
func TestBuildStatusDetailWithTime(t *testing.T, dbSession *cdb.Session, entityID string, status string, message *string, timestamp time.Time) *cdbm.StatusDetail {
	// Create status detail using DAO
	statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)
	statusDetail, err := statusDetailDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID, Status: status, Message: message})
	assert.NoError(t, err)

	// Update the created timestamp directly in the database
	_, err = dbSession.DB.NewUpdate().
		Model((*cdbm.StatusDetail)(nil)).
		Set("created = ?", timestamp).
		Set("updated = ?", timestamp).
		Where("id = ?", statusDetail.ID).
		Exec(context.Background())
	assert.NoError(t, err)

	// Update the returned object
	statusDetail.Created = timestamp
	statusDetail.Updated = timestamp
	return statusDetail
}

// TestAssertMetricExistsTimes verifies that a specific metric exists with expected count and labels
// This is a common utility for all metrics testing
// Parameters:
//   - metricName: the name of the metric to check
//   - expectedCount: expected number of metric samples (0 means no metrics should exist)
//   - expectedLabels: map of label names to expected values (nil to skip label validation)
//   - expectedDuration: exact expected duration for summary metrics (0 to skip duration check)
func TestAssertMetricExistsTimes(t *testing.T, reg *prometheus.Registry, metricName string, expectedCount int, expectedLabels map[string]string, expectedDuration time.Duration) {
	metrics, err := reg.Gather()
	assert.NoError(t, err)

	var foundMetricFamily *dto.MetricFamily
	for _, metricFamily := range metrics {
		if *metricFamily.Name == metricName {
			foundMetricFamily = metricFamily
			break
		}
	}

	if expectedCount == 0 {
		// Should not have any metrics
		if foundMetricFamily != nil {
			assert.Equal(t, 0, len(foundMetricFamily.Metric), "Should not have any metrics for %s", metricName)
		}
		return
	}

	// Should have metrics
	assert.NotNil(t, foundMetricFamily, "Expected to find metric family %s", metricName)
	assert.Equal(t, expectedCount, len(foundMetricFamily.Metric), "Expected %d metrics for %s", expectedCount, metricName)

	if expectedLabels != nil && len(foundMetricFamily.Metric) > 0 {
		// Validate labels on the first metric
		metric := foundMetricFamily.Metric[0]
		actualLabels := make(map[string]string)
		for _, label := range metric.Label {
			actualLabels[*label.Name] = *label.Value
		}

		for expectedKey, expectedValue := range expectedLabels {
			assert.Equal(t, expectedValue, actualLabels[expectedKey],
				"Label %s should be %s, got %s", expectedKey, expectedValue, actualLabels[expectedKey])
		}
	}

	// Check duration for gauge metrics
	if expectedDuration > 0 && len(foundMetricFamily.Metric) > 0 {
		metric := foundMetricFamily.Metric[0]
		assert.NotNil(t, metric.Gauge, "Expected gauge metric for duration check")

		// Gauge metrics store the value directly in seconds
		actualDuration := time.Duration(*metric.Gauge.Value * float64(time.Second))

		// Strict duration comparison with small tolerance for floating point precision
		tolerance := time.Microsecond * 100 // 100μs tolerance for float64 precision
		assert.InDelta(t, expectedDuration.Nanoseconds(), actualDuration.Nanoseconds(), float64(tolerance.Nanoseconds()),
			"Duration should be exactly %v (±%v), got %v", expectedDuration, tolerance, actualDuration)
	}
}

// TestSetupSite creates a complete site setup for testing (user, infrastructure provider, and site)
// This is a common utility for all inventory metrics testing
func TestSetupSite(t *testing.T, dbSession *cdb.Session) *cdbm.Site {
	ipOrg := "test-provider-org"
	ipRoles := []string{roles.ProviderAdminRole}

	ipu := TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	return TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
}
