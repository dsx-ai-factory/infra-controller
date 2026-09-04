// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package migrations

import (
	"context"
	"fmt"
	"testing"
	"time"

	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
	"github.com/uptrace/bun/migrate"
)

func TestMigrations(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	// Create test DB
	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test Migrations",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			migrator := migrate.NewMigrator(dbSession.DB, Migrations)
			migrator.Init(tt.args.ctx)
			_, err := migrator.Migrate(tt.args.ctx)
			assert.NoError(t, err)
		})
	}
}

func TestVpcSlaacEnabledMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()
	model.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipUser := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, []string{authz.ProviderAdminRole})
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipUser)
	tenantOrg := "test-tenant-org"
	tenantUser := model.TestBuildUser(t, dbSession, uuid.NewString(), tenantOrg, []string{authz.TenantAdminRole})
	tenant := model.TestBuildTenant(t, dbSession, "test-tenant", tenantOrg, tenantUser)
	site := model.TestBuildSite(t, dbSession, ip, "test-site", ipUser)
	vpc := model.TestBuildVPC(t, dbSession, "test-vpc", ip, tenant, site, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tenantUser, nil)

	// Fresh installs create `slaac_enabled` before this migration runs.
	require.NoError(t, vpcSlaacEnabledUpMigration(ctx, dbSession.DB))
	var isNullable, columnDefault string
	err := dbSession.DB.QueryRowContext(ctx, `
		SELECT is_nullable, column_default
		FROM information_schema.columns
		WHERE table_schema = 'public' AND table_name = 'vpc' AND column_name = 'slaac_enabled'
	`).Scan(&isNullable, &columnDefault)
	require.NoError(t, err)
	require.Equal(t, "NO", isNullable)
	require.Equal(t, "false", columnDefault)

	// Recreate the immediate predecessor schema while retaining a realistic old row.
	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE vpc DROP COLUMN slaac_enabled`)
	require.NoError(t, err)

	var targetMigration migrate.Migration
	for _, migration := range Migrations.Sorted() {
		if migration.Name == "20260817184424" {
			targetMigration = migration
			break
		}
	}
	require.Equal(t, "vpc_slaac_enabled", targetMigration.Comment)

	targetMigrations := migrate.NewMigrations()
	targetMigrations.Add(targetMigration)
	migrator := migrate.NewMigrator(
		dbSession.DB,
		targetMigrations,
		migrate.WithTableName("vpc_slaac_enabled_migrations_test"),
		migrate.WithLocksTableName("vpc_slaac_enabled_migration_locks_test"),
		migrate.WithMarkAppliedOnSuccess(true),
	)
	require.NoError(t, migrator.Init(ctx))

	group, err := migrator.Migrate(ctx)
	require.NoError(t, err)
	require.Len(t, group.Migrations, 1)

	persisted, err := model.NewVpcDAO(dbSession).GetByID(ctx, nil, vpc.ID, nil)
	require.NoError(t, err)
	require.False(t, persisted.SlaacEnabled)

	persisted, err = model.NewVpcDAO(dbSession).Update(ctx, nil, model.VpcUpdateInput{
		VpcID:        vpc.ID,
		SlaacEnabled: cutil.GetPtr(true),
	})
	require.NoError(t, err)
	require.True(t, persisted.SlaacEnabled)

	_, err = migrator.Rollback(ctx)
	require.ErrorIs(t, err, errVpcSlaacEnabledRollback)

	statuses, err := migrator.MigrationsWithStatus(ctx)
	require.NoError(t, err)
	require.Len(t, statuses, 1)
	require.True(t, statuses[0].IsApplied())

	persisted, err = model.NewVpcDAO(dbSession).GetByID(ctx, nil, vpc.ID, nil)
	require.NoError(t, err)
	require.True(t, persisted.SlaacEnabled)

	// Fresh installs already contain `slaac_enabled` before this migration runs.
	// Reapplying the up callback must preserve its data.
	require.NoError(t, vpcSlaacEnabledUpMigration(ctx, dbSession.DB))
	persisted, err = model.NewVpcDAO(dbSession).GetByID(ctx, nil, vpc.ID, nil)
	require.NoError(t, err)
	require.True(t, persisted.SlaacEnabled)
}

func TestDomainOwnershipMigration(t *testing.T) {
	ctx := context.Background()
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()

	model.TestSetupSchema(t, dbSession)
	user := model.TestBuildUser(t, dbSession, uuid.NewString(), "test-org", []string{authz.TenantAdminRole})
	domain, err := model.NewDomainDAO(dbSession).Create(ctx, nil, model.DomainCreateInput{
		Hostname:  "legacy.example.com",
		Org:       "test-org",
		Status:    model.DomainStatusReady,
		CreatedBy: user.ID,
	})
	require.NoError(t, err)

	_, err = dbSession.DB.ExecContext(ctx, `ALTER TABLE domain DROP COLUMN tenant_id, DROP COLUMN site_id`)
	require.NoError(t, err)
	require.NoError(t, domainOwnershipUpMigration(ctx, dbSession.DB))

	var nullableOwnershipColumns int
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT COUNT(*)
		FROM information_schema.columns
		WHERE table_schema = 'public'
		  AND table_name = 'domain'
		  AND column_name IN ('tenant_id', 'site_id')
		  AND is_nullable = 'YES'
	`).Scan(&nullableOwnershipColumns)
	require.NoError(t, err)
	assert.Equal(t, 2, nullableOwnershipColumns)

	var legacyTenantID, legacySiteID *uuid.UUID
	err = dbSession.DB.QueryRowContext(ctx, `SELECT tenant_id, site_id FROM domain WHERE id = ?`, domain.ID).Scan(&legacyTenantID, &legacySiteID)
	require.NoError(t, err)
	assert.Nil(t, legacyTenantID)
	assert.Nil(t, legacySiteID)

	tenantID := uuid.New()
	siteID := uuid.New()
	_, err = dbSession.DB.ExecContext(ctx, `UPDATE domain SET tenant_id = ?, site_id = ? WHERE id = ?`, tenantID, siteID, domain.ID)
	require.NoError(t, err)
	require.NoError(t, domainOwnershipUpMigration(ctx, dbSession.DB))
	require.NoError(t, domainOwnershipDownMigration(ctx, dbSession.DB))

	var persistedTenantID, persistedSiteID uuid.UUID
	err = dbSession.DB.QueryRowContext(ctx, `SELECT tenant_id, site_id FROM domain WHERE id = ?`, domain.ID).Scan(&persistedTenantID, &persistedSiteID)
	require.NoError(t, err)
	assert.Equal(t, tenantID, persistedTenantID)
	assert.Equal(t, siteID, persistedSiteID)

	var ownershipIndexCount int
	err = dbSession.DB.QueryRowContext(ctx, `
		SELECT COUNT(*)
		FROM pg_indexes
		WHERE schemaname = 'public'
		  AND tablename = 'domain'
		  AND indexname = 'domain_tenant_site_idx'
	`).Scan(&ownershipIndexCount)
	require.NoError(t, err)
	assert.Equal(t, 1, ownershipIndexCount)
}

func Test_vpcProviderIDUpMigration(t *testing.T) {
	ctx := context.Background()

	// Ensure test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	tnu := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := model.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site1 := model.TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	site2 := model.TestBuildSite(t, dbSession, ip, "Test Site 2", ipu)

	nsg1 := model.TestBuildNetworkSecurityGroup(t, dbSession, "Test NSG1", tn, site1)
	nsg2 := model.TestBuildNetworkSecurityGroup(t, dbSession, "Test NSG2", tn, site2)

	vpc1 := model.TestBuildVPC(t, dbSession, "Test VPC 1", ip, tn, site1, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, &nsg1.ID)
	vpc2 := model.TestBuildVPC(t, dbSession, "Test VPC 2", ip, tn, site2, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, &nsg2.ID)

	// Delete VPC2 and Site2 to test that the migration will not fail because of deleted rows
	_, err := dbSession.DB.NewDelete().Model(vpc2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(site2).WherePK().Exec(ctx)
	require.NoError(t, err)

	// Allow infrastructure_provider_id to be null
	_, err = dbSession.DB.Exec("ALTER TABLE vpc ALTER COLUMN infrastructure_provider_id DROP NOT NULL")
	assert.Nil(t, err)

	// Set infrastructure_provider_id to null
	_, err = dbSession.DB.NewUpdate().Table("public.vpc").Set("infrastructure_provider_id = ?", nil).Where("id = ?", vpc1.ID).Exec(ctx)
	assert.Nil(t, err)
	_, err = dbSession.DB.NewUpdate().Table("public.vpc").Set("infrastructure_provider_id = ?", nil).Where("id = ?", vpc2.ID).Exec(ctx)
	assert.Nil(t, err)

	var updatedVpc1, updatedVpc2 model.Vpc
	var emptyUUID uuid.UUID

	err = dbSession.DB.NewSelect().Model(&updatedVpc1).Where("id = ?", vpc1.ID).Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, emptyUUID, updatedVpc1.InfrastructureProviderID)

	err = dbSession.DB.NewSelect().Model(&updatedVpc2).Where("id = ?", vpc2.ID).WhereAllWithDeleted().Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, emptyUUID, updatedVpc2.InfrastructureProviderID)

	// Call up migration function
	err = vpcProviderIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that InfrastructureProviderID has been populated
	err = dbSession.DB.NewSelect().Model(&updatedVpc1).Where("id = ?", vpc1.ID).Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, ip.ID, updatedVpc1.InfrastructureProviderID)

	err = dbSession.DB.NewSelect().Model(&updatedVpc2).Where("id = ?", vpc2.ID).WhereAllWithDeleted().Scan(ctx)
	assert.NoError(t, err)

	assert.Equal(t, ip.ID, updatedVpc2.InfrastructureProviderID)
}

func Test_subnetSiteIDUpMigration(t *testing.T) {
	ctx := context.Background()

	// Ensure test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	tnu := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := model.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, tnu)

	site1 := model.TestBuildSite(t, dbSession, ip, "Test Site 1", ipu)
	vpc1 := model.TestBuildVPC(t, dbSession, "Test VPC 1", ip, tn, site1, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, nil)
	ipv4Block1 := model.TestBuildIPBlock(t, dbSession, "Test IPv4 Block 1", site1, tn, model.IPBlockRoutingTypeDatacenterOnly, "192.0.2.0", 24, model.IPBlockProtocolVersionV4)
	subnet1 := model.TestBuildSubnet(t, dbSession, "Test Subnet 1", tn, vpc1, nil, ipv4Block1, model.SubnetStatusPending, tnu)

	site2 := model.TestBuildSite(t, dbSession, ip, "Test Site 2", ipu)
	vpc2 := model.TestBuildVPC(t, dbSession, "Test VPC 2", ip, tn, site2, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusReady, tnu, nil)
	ipv4Block2 := model.TestBuildIPBlock(t, dbSession, "Test IPv4 Block 2", site2, tn, model.IPBlockRoutingTypeDatacenterOnly, "192.0.3.0", 24, model.IPBlockProtocolVersionV4)
	subnet2 := model.TestBuildSubnet(t, dbSession, "Test Subnet 2", tn, vpc2, nil, ipv4Block2, model.SubnetStatusPending, tnu)

	// Delete the second set to test that the migration will not fail because of deleted rows
	_, err := dbSession.DB.NewDelete().Model(subnet2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(ipv4Block2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(vpc2).WherePK().Exec(ctx)
	require.NoError(t, err)
	_, err = dbSession.DB.NewDelete().Model(site2).WherePK().Exec(ctx)
	require.NoError(t, err)

	// Allow site_id to be null
	_, err = dbSession.DB.Exec("ALTER TABLE subnet ALTER COLUMN site_id DROP NOT NULL")
	assert.Nil(t, err)

	// Set site_id to null
	_, err = dbSession.DB.NewUpdate().Table("public.subnet").Set("site_id = ?", nil).Where("id = ?", subnet1.ID).Exec(ctx)
	assert.Nil(t, err)
	_, err = dbSession.DB.NewUpdate().Table("public.subnet").Set("site_id = ?", nil).Where("id = ?", subnet2.ID).Exec(ctx)
	assert.Nil(t, err)

	var updatedSubnet1, updatedSubnet2 model.Subnet
	var emptyUUID uuid.UUID

	err = dbSession.DB.NewSelect().Model(&updatedSubnet1).Where("id = ?", subnet1.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, emptyUUID, updatedSubnet1.SiteID)
	err = dbSession.DB.NewSelect().Model(&updatedSubnet2).WhereAllWithDeleted().Where("id = ?", subnet2.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, emptyUUID, updatedSubnet2.SiteID)

	// Call up migration function
	err = subnetSiteIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that Site ID has been populated
	err = dbSession.DB.NewSelect().Model(&updatedSubnet1).Where("id = ?", subnet1.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, site1.ID, updatedSubnet1.SiteID)

	err = dbSession.DB.NewSelect().Model(&updatedSubnet2).WhereAllWithDeleted().Where("id = ?", subnet2.ID).Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, site2.ID, updatedSubnet2.SiteID)
}

func Test_machineInstanceTypeIDUpMigration(t *testing.T) {
	ctx := context.Background()

	// Ensure test DB
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	defer dbSession.Close()

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	instanceType := model.TestBuildInstanceType(t, dbSession, "Test Instance Type", ip, site, ipu)

	// Create Machine without Instance Type
	machine := model.TestBuildMachine(t, dbSession, ip, site, nil, nil)
	require.Nil(t, machine.InstanceTypeID)

	// Create Machine/Instance Type association
	model.TestBuildMachineInstanceType(t, dbSession, machine, instanceType)

	// Call up migration function
	err := machineInstanceTypeIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that Instance Type ID has been populated for Machine
	var updatedMachine model.Machine
	err = dbSession.DB.NewSelect().Model(&updatedMachine).Where("id = ?", machine.ID).Scan(ctx)
	assert.NoError(t, err)

	require.NotNil(t, updatedMachine.InstanceTypeID)
	assert.Equal(t, instanceType.ID, *updatedMachine.InstanceTypeID)
}

func Test_machineControllerMachineIDUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// Machine is the baremetal server that sits in the datacenter
	type MachineOlder struct {
		bun.BaseModel `bun:"table:machine,alias:m"`

		ID                       uuid.UUID                     `bun:"type:uuid,pk"`
		InfrastructureProviderID uuid.UUID                     `bun:"infrastructure_provider_id,type:uuid,notnull"`
		InfrastructureProvider   *model.InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
		SiteID                   uuid.UUID                     `bun:"site_id,type:uuid,notnull"`
		Site                     *model.Site                   `bun:"rel:belongs-to,join:site_id=id"`
		InstanceTypeID           *uuid.UUID                    `bun:"instance_type_id,type:uuid"`
		InstanceType             *model.InstanceType           `bun:"rel:belongs-to,join:instance_type_id=id"`
		ControllerMachineID      uuid.UUID                     `bun:"controller_machine_id,type:uuid,notnull"`
		ControllerMachineType    *string                       `bun:"controller_machine_type"`
		HwSkuDeviceType          *string                       `bun:"hw_sku_device_type"`
		Metadata                 map[string]interface{}        `bun:"metadata,type:jsonb,json_use_number"`
		DefaultMacAddress        *string                       `bun:"default_mac_address"`
		IsAssigned               bool                          `bun:"is_assigned,notnull"`
		Status                   string                        `bun:"status,notnull"`
		IsMissingOnSite          bool                          `bun:"is_missing_on_site,notnull"`
		Created                  time.Time                     `bun:"created,nullzero,notnull,default:current_timestamp"`
		Updated                  time.Time                     `bun:"updated,nullzero,notnull,default:current_timestamp"`
		Deleted                  *time.Time                    `bun:"deleted,soft_delete"`
	}

	type MachineNewer struct {
		bun.BaseModel `bun:"table:machine,alias:m"`

		ID                       uuid.UUID                     `bun:"type:uuid,pk"`
		InfrastructureProviderID uuid.UUID                     `bun:"infrastructure_provider_id,type:uuid,notnull"`
		InfrastructureProvider   *model.InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
		SiteID                   uuid.UUID                     `bun:"site_id,type:uuid,notnull"`
		Site                     *model.Site                   `bun:"rel:belongs-to,join:site_id=id"`
		InstanceTypeID           *uuid.UUID                    `bun:"instance_type_id,type:uuid"`
		InstanceType             *model.InstanceType           `bun:"rel:belongs-to,join:instance_type_id=id"`
		ControllerMachineID      string                        `bun:"controller_machine_id,notnull"`
		ControllerMachineType    *string                       `bun:"controller_machine_type"`
		HwSkuDeviceType          *string                       `bun:"hw_sku_device_type"`
		Metadata                 map[string]interface{}        `bun:"metadata,type:jsonb,json_use_number"`
		DefaultMacAddress        *string                       `bun:"default_mac_address"`
		IsAssigned               bool                          `bun:"is_assigned,notnull"`
		Status                   string                        `bun:"status,notnull"`
		IsMissingOnSite          bool                          `bun:"is_missing_on_site,notnull"`
		Created                  time.Time                     `bun:"created,nullzero,notnull,default:current_timestamp"`
		Updated                  time.Time                     `bun:"updated,nullzero,notnull,default:current_timestamp"`
		Deleted                  *time.Time                    `bun:"deleted,soft_delete"`
	}

	// create Allocation table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Allocation)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InstanceType)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Machine)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*model.OperatingSystem)(nil))
	assert.Nil(t, err)
	// create OperatingSystemSiteAssociation table
	err = dbSession.DB.ResetModel(context.Background(), (*model.OperatingSystemSiteAssociation)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*model.User)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Instance)(nil))
	assert.Nil(t, err)
	// create InfiniBandPartition table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfiniBandPartition)(nil))
	assert.Nil(t, err)
	// create InfiniBandInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfiniBandInterface)(nil))
	assert.Nil(t, err)
	// create old Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*MachineOlder)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}

	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)

	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	instanceType := model.TestBuildInstanceType(t, dbSession, "Test Instance Type", ip, site, ipu)

	defMacAddr := "00:1B:44:11:3A:B7"
	controllerMachineType := "machineTypeTest"

	// Create Machines
	mcs := []uuid.UUID{}
	for i := 0; i < 5; i++ {
		mc := uuid.New()
		mcs = append(mcs, mc)

		machine := &MachineOlder{
			ID:                       uuid.New(),
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			ControllerMachineID:      mc,
			ControllerMachineType:    &controllerMachineType,
			Metadata:                 nil,
			DefaultMacAddress:        &defMacAddr,
			Status:                   model.MachineStatusInitializing,
		}

		if instanceType != nil {
			machine.InstanceTypeID = &instanceType.ID
		}

		_, err = dbSession.DB.NewInsert().Model(machine).Exec(context.Background())
		assert.Nil(t, err)
	}

	// Call up migration function
	err = machineControllerMachineIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll machines, and verify that controller_machine_id matches
	nms := []MachineNewer{}

	err = dbSession.DB.NewSelect().Model(&nms).Scan(ctx)
	assert.Nil(t, err)

	assert.Equal(t, len(mcs), len(nms))
	assert.Nil(t, err)
	for i, m := range nms {
		assert.Equal(t, m.ControllerMachineID, mcs[i].String())
	}
}

func Test_ipBlockBlockSizeRenameUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)
	tenant := model.TestBuildTenant(t, dbSession, "testTen", "testOrg", ipu)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", site, tenant, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)

	// rename column prefixLength back to block_size for up migration testing
	_, err = dbSession.DB.Exec("ALTER TABLE ip_block RENAME COLUMN prefix_length TO block_size")
	assert.NoError(t, err)

	// Call up migration function
	err = ipBlockBlockSizeRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll ipblocks and verify
	ipbDAO := model.NewIPBlockDAO(dbSession)
	ipbs, tot, err := ipbDAO.GetAll(context.Background(), nil, model.IPBlockFilterInput{}, paginator.PageInput{}, nil)
	assert.Equal(t, tot, len(ipbs))
	assert.Equal(t, 1, tot)
	assert.Nil(t, err)
	assert.Equal(t, ipb.PrefixLength, ipbs[0].PrefixLength)
	assert.Equal(t, ipb.Name, ipbs[0].Name)
}

func Test_subnetIPBlockSizeRenameUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Vpc)(nil))
	assert.Nil(t, err)
	// create domain table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Domain)(nil))
	assert.Nil(t, err)
	// create ipblock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*model.User)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Subnet)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)
	tenant := model.TestBuildTenant(t, dbSession, "testTen", "testOrg", ipu)
	vpc := model.TestBuildVPC(t, dbSession, "testvpc", ip, tenant, site, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusProvisioning, ipu, nil)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", site, tenant, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	subnet := model.TestBuildSubnet(t, dbSession, "testsubnet", tenant, vpc, nil, ipb, model.SubnetStatusProvisioning, ipu)

	// rename column prefixLength back to block_size for up migration testing
	_, err = dbSession.DB.Exec("ALTER TABLE subnet RENAME COLUMN prefix_length TO ip_block_size")
	assert.NoError(t, err)

	// Call up migration function
	err = subnetIPBlockSizeRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll ipblocks and verify
	subnetDAO := model.NewSubnetDAO(dbSession)
	subnets, tot, err := subnetDAO.GetAll(context.Background(), nil, model.SubnetFilterInput{}, paginator.PageInput{}, []string{})
	assert.Equal(t, tot, len(subnets))
	assert.Equal(t, 1, tot)
	assert.Nil(t, err)
	assert.Equal(t, subnet.PrefixLength, subnets[0].PrefixLength)
	assert.Equal(t, subnet.Name, subnets[0].Name)
}

func Test_renameInstanceSubnetToInterfaceUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{authz.TenantAdminRole}
	tnu := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg, tnRoles)
	tn := model.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, tnu)

	st := model.TestBuildSite(t, dbSession, ip, "test-site", ipu)
	al := model.TestBuildAllocation(t, dbSession, "test-allocation", st, tn, ipu)
	it := model.TestBuildInstanceType(t, dbSession, "test-instance-type", ip, st, ipu)
	model.TestBuildAllocationConstraint(t, dbSession, al, it, nil, 40, ipu)

	vpc := model.TestBuildVPC(t, dbSession, "test-vpc", ip, tn, st, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusProvisioning, ipu, nil)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", st, tn, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	sb := model.TestBuildSubnet(t, dbSession, "test-subnet", tn, vpc, nil, ipb, model.SubnetStatusProvisioning, ipu)
	os := model.TestBuildOperatingSystem(t, dbSession, "test-os", tn, model.OperatingSystemStatusReady, ipu)

	ifcCount := 30
	for i := 0; i < ifcCount; i++ {
		m := model.TestBuildMachine(t, dbSession, ip, st, it, nil)
		ins := model.TestBuildInstance(t, dbSession, fmt.Sprintf("test-instance-%d", i), tn, ip, st, it, vpc, m, os)
		model.TestBuildInterface(t, dbSession, ins, &sb.ID, nil, true, model.InterfaceStatusProvisioning)
	}

	// rename column prefixLength back to block_size for up migration testing
	_, err := dbSession.DB.Exec("ALTER TABLE IF EXISTS interface RENAME TO instance_subnet")
	assert.NoError(t, err)

	// Call up migration function
	err = renameInstanceSubnetToInterfaceUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll Interfaces and verify
	ifcDAO := model.NewInterfaceDAO(dbSession)
	_, tot, err := ifcDAO.GetAll(context.Background(), nil, model.InterfaceFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.NoError(t, err)
	assert.Equal(t, ifcCount, tot)
}

func Test_createAndPopulateTenantSiteUpMigrationfunc(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnRoles := []string{authz.TenantAdminRole}

	tnu1 := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg1, tnRoles)
	tn1 := model.TestBuildTenant(t, dbSession, "test-tenant", tnOrg1, tnu1)

	tnu2 := model.TestBuildUser(t, dbSession, uuid.NewString(), tnOrg2, tnRoles)
	tn2 := model.TestBuildTenant(t, dbSession, "test-tenant", tnOrg2, tnu2)

	st := model.TestBuildSite(t, dbSession, ip, "test-site", ipu)

	al1 := model.TestBuildAllocation(t, dbSession, "test-instance-allocation", st, tn1, ipu)
	it := model.TestBuildInstanceType(t, dbSession, "test-instance-type", ip, st, ipu)
	model.TestBuildAllocationConstraint(t, dbSession, al1, it, nil, 40, ipu)

	al2 := model.TestBuildAllocation(t, dbSession, "test-ip-allocation", st, tn2, ipu)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", st, tn2, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	model.TestBuildAllocationConstraint(t, dbSession, al2, nil, ipb, 40, ipu)

	// Delete TenantSite table
	_, err := dbSession.DB.NewDropTable().IfExists().Model((*model.TenantSite)(nil)).Exec(ctx)
	assert.NoError(t, err)

	// Call up migration function
	err = createAndPopulateTenantSiteUpMigrationfunc(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that 2 TenantSite entries were create
	tsDAO := model.NewTenantSiteDAO(dbSession)
	_, tot, err := tsDAO.GetAll(context.Background(), nil, model.TenantSiteFilterInput{}, paginator.PageInput{}, nil)
	assert.NoError(t, err)
	assert.Equal(t, 2, tot)
}

func Test_siteSshHostnameRenameUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)

	// rename column serial_console_hostname back to ssh_hostname for up migration testing
	_, err := dbSession.DB.Exec("ALTER TABLE site RENAME COLUMN serial_console_hostname TO ssh_hostname")
	assert.NoError(t, err)

	// Call up migration function
	err = siteSshHostnameRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Get site and verify
	siteDAO := model.NewSiteDAO(dbSession)
	sites, tot, err := siteDAO.GetAll(context.Background(), nil, model.SiteFilterInput{}, paginator.PageInput{}, nil)
	assert.Equal(t, tot, len(sites))
	assert.Equal(t, 1, tot)
	assert.Nil(t, err)
	assert.Equal(t, site.SerialConsoleHostname, sites[0].SerialConsoleHostname)
	assert.Equal(t, site.Name, sites[0].Name)

	// Call up migration function again, here serialConsoleHostname already exists - this should be ok
	err = siteSshHostnameRenameUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)
}

// TODO: evaluate if this test centered around a 2023 migration is still needed/
func Test_alterMachineIDUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, true)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Setup schemas
	model.TestSetupSchema(t, dbSession)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	st := model.TestBuildSite(t, dbSession, ip, "test-site", ipu)
	it := model.TestBuildInstanceType(t, dbSession, "test-instance-type", ip, st, ipu)

	// Create Machines
	mCount := 30
	for i := 0; i < mCount; i++ {
		m := model.TestBuildMachine(t, dbSession, ip, st, it, nil)
		assert.NotEqual(t, m.ID, m.ControllerMachineID)
	}

	// Drop foreign key constraints
	_, err := dbSession.DB.Exec("ALTER TABLE machine_capability DROP CONSTRAINT machine_capability_machine_id_fkey")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_instance_type DROP CONSTRAINT machine_instance_type_machine_id_fkey")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_interface DROP CONSTRAINT machine_interface_machine_id_fkey")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE instance DROP CONSTRAINT instance_machine_id_fkey")
	assert.NoError(t, err)

	// Drop expected_machine foreign key constraint if it exists
	_, err = dbSession.DB.Exec("ALTER TABLE expected_machine DROP CONSTRAINT IF EXISTS expected_machine_machine_id_fkey")
	assert.NoError(t, err)

	// Change back all Machine ID to UUID
	_, err = dbSession.DB.Exec("ALTER TABLE machine_capability ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_instance_type ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_interface ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE instance ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	assert.NoError(t, err)

	// Change expected_machine.machine_id to UUID if the column exists
	_, err = dbSession.DB.Exec("ALTER TABLE expected_machine ALTER COLUMN machine_id TYPE uuid USING machine_id::uuid")
	if err != nil {
		// Column might not exist in older schema versions, that's okay
		fmt.Printf("Note: expected_machine.machine_id column might not exist yet: %v\n", err)
	}

	_, err = dbSession.DB.Exec("ALTER TABLE machine ALTER COLUMN id TYPE uuid USING id::uuid")
	assert.NoError(t, err)

	// Add back foreign key constraint
	_, err = dbSession.DB.Exec("ALTER TABLE machine_capability ADD CONSTRAINT machine_capability_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_instance_type ADD CONSTRAINT machine_instance_type_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE machine_interface ADD CONSTRAINT machine_interface_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	_, err = dbSession.DB.Exec("ALTER TABLE instance ADD CONSTRAINT instance_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES public.machine(id)")
	assert.NoError(t, err)

	// Note: We do NOT add back expected_machine foreign key constraint here because:
	// - expected_machine.machine_id is TEXT (from TestSetupSchema)
	// - machine.id is UUID at this point (we just converted it back)
	// - The types are mismatched, so the constraint would fail
	// - The alterMachineIDUpMigration will convert machine.id to TEXT
	// - After migration, we can add the constraint if needed

	// Run migration
	err = alterMachineIDUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// Check that all Machine IDs are now strings
	mDAO := model.NewMachineDAO(dbSession)
	ms, tot, err := mDAO.GetAll(context.Background(), nil, model.MachineFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.NoError(t, err)
	assert.Equal(t, tot, mCount)

	for _, m := range ms {
		assert.Equal(t, m.ID, m.ControllerMachineID)
	}
}

func Test_operatingSystemImageAttributeUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// setup schemas
	// create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)
	// create Infrastructure Provider table
	err = dbSession.DB.ResetModel(context.Background(), (*model.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Site)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Vpc)(nil))
	assert.Nil(t, err)
	// create domain table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Domain)(nil))
	assert.Nil(t, err)
	// create ipblock table
	err = dbSession.DB.ResetModel(context.Background(), (*model.IPBlock)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*model.User)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*model.Subnet)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*model.OperatingSystem)(nil))
	assert.Nil(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)
	ip := model.TestBuildInfrastructureProvider(t, dbSession, "Test Provider", ipOrg, ipu)
	site := model.TestBuildSite(t, dbSession, ip, "Test Site", ipu)
	tenant := model.TestBuildTenant(t, dbSession, "testTen", "testOrg", ipu)
	vpc := model.TestBuildVPC(t, dbSession, "testvpc", ip, tenant, site, cutil.GetPtr(model.VpcEthernetVirtualizer), nil, nil, model.VpcStatusProvisioning, ipu, nil)
	ipb := model.TestBuildIPBlock(t, dbSession, "test-ipb", site, tenant, model.IPBlockRoutingTypeDatacenterOnly, "192.168.1.0", 24, model.IPBlockProtocolVersionV4)
	_ = model.TestBuildSubnet(t, dbSession, "testsubnet", tenant, vpc, nil, ipb, model.SubnetStatusProvisioning, ipu)
	_ = model.TestBuildOperatingSystem(t, dbSession, "testos", tenant, model.OperatingSystemStatusProvisioning, ipu)

	// Call up migration function
	err = operatingSystemImageAttributeUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// GetAll operating systems and verify
	osDAO := model.NewOperatingSystemDAO(dbSession)
	oss, tos, err := osDAO.GetAll(context.Background(), nil, model.OperatingSystemFilterInput{}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	assert.Equal(t, tos, len(oss))
	assert.Equal(t, 1, tos)
	assert.Nil(t, err)
	assert.Equal(t, "iPXE", oss[0].Type)
}

func Test_tenantConfigUpMigration(t *testing.T) {
	ctx := context.Background()

	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	defer dbSession.Close()

	// Create Tenant table
	err := dbSession.DB.ResetModel(context.Background(), (*model.Tenant)(nil))
	assert.Nil(t, err)

	// Tenant.Config is scan-only, so ResetModel does not create its column.
	// Recreate the historical schema before exercising the migration.
	_, err = dbSession.DB.Exec("ALTER TABLE tenant ADD COLUMN config jsonb")
	assert.NoError(t, err)

	// Create initial data
	ipOrg := "test-provider-org"
	ipRoles := []string{authz.ProviderAdminRole}
	ipu := model.TestBuildUser(t, dbSession, uuid.NewString(), ipOrg, ipRoles)

	tnOrg1 := "test-tenant-org-1"
	tnOrg2 := "test-tenant-org-2"
	tnOrg3 := "test-tenant-org-3"

	tenant1 := model.TestBuildTenant(t, dbSession, "test-tenant-1", tnOrg1, ipu)
	tenant2 := model.TestBuildTenant(t, dbSession, "test-tenant-2", tnOrg2, ipu)
	tenant3 := model.TestBuildTenant(t, dbSession, "test-tenant-3", tnOrg3, ipu)

	// Simulate legacy rows with NULL configs prior to the migration.
	_, err = dbSession.DB.Exec("UPDATE tenant SET config = NULL WHERE id IN (?)", bun.In([]uuid.UUID{tenant1.ID, tenant2.ID}))
	assert.NoError(t, err)

	// Seed one tenant with a pre-existing non-NULL config that must survive the migration.
	existingConfig := map[string]interface{}{
		"targetedInstanceCreation": true,
		"enableSshAccess":          false,
	}
	_, err = dbSession.DB.NewUpdate().
		Table("tenant").
		Set("config = ?", existingConfig).
		Where("id = ?", tenant3.ID).
		Exec(ctx)
	assert.NoError(t, err)

	// Call up migration function
	err = tenantConfigUpMigration(ctx, dbSession.DB)
	assert.NoError(t, err)

	// The migration backfills NULL configs with an empty JSON object and leaves
	// pre-existing non-NULL values untouched.
	type tenantConfigRow struct {
		ID     uuid.UUID              `bun:"id"`
		Config map[string]interface{} `bun:"config,type:jsonb"`
	}

	var rows []tenantConfigRow
	err = dbSession.DB.NewSelect().
		Table("tenant").
		Column("id", "config").
		Where("id IN (?)", bun.In([]uuid.UUID{tenant1.ID, tenant2.ID, tenant3.ID})).
		Scan(ctx, &rows)
	assert.NoError(t, err)
	require.Len(t, rows, 3)

	configByID := make(map[uuid.UUID]map[string]interface{}, len(rows))
	for _, row := range rows {
		configByID[row.ID] = row.Config
	}

	assert.Equal(t, map[string]interface{}{}, configByID[tenant1.ID])
	assert.Equal(t, map[string]interface{}{}, configByID[tenant2.ID])
	assert.Equal(t, existingConfig, configByID[tenant3.ID])

	// The migration also establishes the column's schema contract: it sets the
	// '{}'::jsonb default and a NOT NULL constraint. Exercise both behaviors so a
	// regression that drops either is caught.

	// A new row that omits config must receive the '{}'::jsonb default.
	defaultTenant := model.TestBuildTenant(t, dbSession, "test-tenant-default", "test-tenant-org-default", ipu)

	var defaultRow tenantConfigRow
	err = dbSession.DB.NewSelect().
		Table("tenant").
		Column("id", "config").
		Where("id = ?", defaultTenant.ID).
		Scan(ctx, &defaultRow)
	assert.NoError(t, err)
	assert.Equal(t, map[string]interface{}{}, defaultRow.Config)

	// Explicitly writing NULL must violate the NOT NULL constraint.
	_, err = dbSession.DB.NewUpdate().
		Table("tenant").
		Set("config = NULL").
		Where("id = ?", defaultTenant.ID).
		Exec(ctx)
	assert.Error(t, err)
}
