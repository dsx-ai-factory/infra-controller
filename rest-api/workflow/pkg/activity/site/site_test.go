// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/golang/mock/gomock"
	"github.com/google/uuid"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/uptrace/bun/extra/bundebug"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"
	"go.temporal.io/sdk/testsuite"

	tnsv1 "go.temporal.io/api/namespace/v1"
	tOperatorv1 "go.temporal.io/api/operatorservice/v1"
	tWorkflowv1 "go.temporal.io/api/workflowservice/v1"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	tosv1mock "go.temporal.io/api/operatorservicemock/v1"
	twsv1mock "go.temporal.io/api/workflowservicemock/v1"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/durationpb"
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

func testSiteInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func TestManageSite_DeleteSiteComponentsFromDB(t *testing.T) {
	ctx := context.Background()

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	tnOrg := "test-tenant-org-1"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg}, tnRoles)
	tenant := util.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, nil, tnu)

	vpcDAO := cdbm.NewVpcDAO(dbSession)
	ibpDAO := cdbm.NewInfiniBandPartitionDAO(dbSession)
	itDAO := cdbm.NewInstanceTypeDAO(dbSession)
	iDAO := cdbm.NewInstanceDAO(dbSession)

	// Site 1 that will be deleted normally
	site := util.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusPending, nil, ipu)
	vpc := util.TestBuildVpc(t, dbSession, ip, site, tenant, "test-vpc")
	machine := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr("x86"), cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine2 := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr("x86"), cutil.GetPtr(true), cdbm.MachineStatusReady)
	allocation := util.TestBuildAllocation(t, dbSession, ip, tenant, site, "test-allocation")
	instanceType := util.TestBuildInstanceType(t, dbSession, ip, site, "test-instance-type")
	_ = util.TestBuildAllocationContraints(t, dbSession, allocation, cdbm.AllocationResourceTypeInstanceType, instanceType.ID, cdbm.AllocationConstraintTypeReserved, 1, ipu)
	operatingSystem := util.TestBuildOperatingSystem(t, dbSession, "test-os")
	ibp := util.TestBuildInfiniBandPartition(t, dbSession, "test-infiniband-partition", site, tenant, nil, cdbm.InfiniBandInterfaceStatusReady, false)

	ins1, _ := iDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusRebooting),
			CreatedBy:                tnu.ID,
		},
	)

	ins2, _ := iDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-2",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site.ID,
			InstanceTypeID:           &instanceType.ID,
			VpcID:                    vpc.ID,
			MachineID:                &machine2.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusRebooting),
			CreatedBy:                tnu.ID,
		},
	)

	// Site 2 where the Machine components will be purged
	site2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusPending, nil, ipu)
	vpc2 := util.TestBuildVpc(t, dbSession, ip, site2, tenant, "test-vpc-2")
	machine3 := util.TestBuildMachine(t, dbSession, ip.ID, site2.ID, cutil.GetPtr("mcTypeTest2"), cutil.GetPtr(true), cdbm.MachineStatusReady)
	machine4 := util.TestBuildMachine(t, dbSession, ip.ID, site2.ID, cutil.GetPtr("mcTypeTest3"), cutil.GetPtr(true), cdbm.MachineStatusReady)

	allocation2 := util.TestBuildAllocation(t, dbSession, ip, tenant, site2, "test-allocation-2")
	instanceType2 := util.TestBuildInstanceType(t, dbSession, ip, site2, "test-instance-type-2")
	_ = util.TestBuildAllocationContraints(t, dbSession, allocation2, cdbm.AllocationResourceTypeInstanceType, instanceType2.ID, cdbm.AllocationConstraintTypeReserved, 2, ipu)
	operatingSystem2 := util.TestBuildOperatingSystem(t, dbSession, "test-os-2")

	ins3, _ := iDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-3",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site2.ID,
			InstanceTypeID:           &instanceType2.ID,
			VpcID:                    vpc2.ID,
			MachineID:                &machine3.ID,
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem2.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusRebooting),
			CreatedBy:                tnu.ID,
		},
	)
	ins4, _ := iDAO.Create(
		ctx, nil,
		cdbm.InstanceCreateInput{
			Name:                     "test-instance-4",
			TenantID:                 tenant.ID,
			InfrastructureProviderID: ip.ID,
			SiteID:                   site2.ID,
			InstanceTypeID:           &instanceType2.ID,
			VpcID:                    vpc2.ID,
			MachineID:                &machine4.ID,
			ControllerInstanceID:     cutil.GetPtr(uuid.New()),
			Hostname:                 cutil.GetPtr("test.com"),
			OperatingSystemID:        cutil.GetPtr(operatingSystem2.ID),
			IpxeScript:               cutil.GetPtr("ipxe"),
			AlwaysBootWithCustomIpxe: true,
			UserData:                 cutil.GetPtr("userdata"),
			Labels:                   map[string]string{},
			Status:                   cdbm.InstanceStatusPending,
			PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusRebooting),
			CreatedBy:                tnu.ID,
		},
	)

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx            context.Context
		siteID         uuid.UUID
		ipID           *uuid.UUID
		vpcID          *uuid.UUID
		ibpID          *uuid.UUID
		machineIDs     []string
		instanceTypeID *uuid.UUID
		instanceIDs    []uuid.UUID
		purgeMachines  bool
	}

	tests := []struct {
		name           string
		fields         fields
		args           args
		want           error
		wantErr        bool
		expectDeletion bool
	}{
		{
			name: "test Site delete component activity successfully completed",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:            context.Background(),
				siteID:         site.ID,
				ipID:           &ip.ID,
				vpcID:          &vpc.ID,
				ibpID:          &ibp.ID,
				machineIDs:     []string{machine.ID, machine2.ID},
				instanceTypeID: &instanceType.ID,
				instanceIDs:    []uuid.UUID{ins1.ID, ins2.ID},
			},
			want:           nil,
			expectDeletion: true,
		},
		{
			name: "test Site delete component activity successfully completed when site doesn't exits",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    context.Background(),
				siteID: uuid.New(),
				ipID:   cutil.GetPtr(uuid.New()),
			},
			want:           nil,
			expectDeletion: false,
		},
		{
			name: "test Site delete component activity successfully completed with purge",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:            context.Background(),
				siteID:         site2.ID,
				ipID:           &ip.ID,
				vpcID:          &vpc2.ID,
				machineIDs:     []string{machine3.ID, machine4.ID},
				instanceTypeID: &instanceType2.ID,
				instanceIDs:    []uuid.UUID{ins3.ID, ins4.ID},
				purgeMachines:  true,
			},
			want:           nil,
			expectDeletion: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageSite{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			err := mv.DeleteSiteComponentsFromDB(tt.args.ctx, tt.args.siteID, *tt.args.ipID, tt.args.purgeMachines)

			if tt.wantErr {
				assert.Error(t, err)
				return
			}

			// Check if the VPC was deleted in the DB
			if tt.args.vpcID != nil {
				_, err = vpcDAO.GetByID(ctx, nil, *tt.args.vpcID, nil)
				if tt.expectDeletion {
					assert.Equal(t, cdb.ErrDoesNotExist, err)
				}
			}

			// Check if Instance Type is deleted from DB
			if tt.args.instanceTypeID != nil {
				_, err = itDAO.GetByID(ctx, nil, *tt.args.instanceTypeID, nil)
				if tt.expectDeletion {
					assert.Equal(t, cdb.ErrDoesNotExist, err)
				}
			}

			// Check if InfinitBand Partition is deleted from DB
			if tt.args.ibpID != nil {
				_, err := ibpDAO.GetByID(ctx, nil, *tt.args.ibpID, nil)
				if tt.expectDeletion {
					assert.Equal(t, cdb.ErrDoesNotExist, err)
				}
			}

			if tt.expectDeletion {
				// Check if Machines are deleted from DB
				for _, mID := range tt.args.machineIDs {
					var res cdbm.Machine
					if tt.args.purgeMachines {
						err = dbSession.DB.NewSelect().Model(&res).Where("m.id = ?", mID).WhereAllWithDeleted().Scan(ctx)
					} else {
						err = dbSession.DB.NewSelect().Model(&res).Where("m.id = ?", mID).Scan(ctx)
					}

					assert.Equal(t, sql.ErrNoRows, err)
				}

				// Check if Instances are deleted from DB
				for _, iID := range tt.args.instanceIDs {
					var res cdbm.Instance

					err = dbSession.DB.NewSelect().Model(&res).Where("i.id = ?", iID).Scan(ctx)
					assert.Equal(t, sql.ErrNoRows, err)

					if tt.args.purgeMachines {
						err = dbSession.DB.NewSelect().Model(&res).Where("i.id = ?", iID).WhereAllWithDeleted().Scan(ctx)
						assert.NoError(t, err)
						assert.Nil(t, res.MachineID)
					}
				}
			}
		})
	}
}

func TestNewManageSite(t *testing.T) {
	type args struct {
		dbSession         *cdb.Session
		siteClientPool    *sc.ClientPool
		tc                client.Client
		cfg               *config.Config
		siteHealthMetrics *cwm.SiteHealthMetrics
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

	tc := &tmocks.Client{}
	scp := sc.NewClientPool(tcfg)
	shm := cwm.NewSiteHealthMetrics(prometheus.NewRegistry(), "nico_rest_workflow")

	tests := []struct {
		name string
		args args
		want ManageSite
	}{
		{
			name: "test new ManageSite instantiation",
			args: args{
				dbSession:         dbSession,
				siteClientPool:    scp,
				tc:                tc,
				cfg:               cfg,
				siteHealthMetrics: shm,
			},
			want: ManageSite{
				dbSession:         dbSession,
				siteClientPool:    scp,
				tc:                tc,
				cfg:               cfg,
				siteHealthMetrics: shm,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, NewManageSite(
				tt.args.dbSession, tt.args.siteClientPool, tt.args.tc,
				tt.args.cfg, tt.args.siteHealthMetrics,
			))
		})
	}
}

func TestManageSite_MonitorInventoryReceiptForAllSites(t *testing.T) {
	ctx := context.Background()

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	site1 := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusPending, nil, ipu)
	site2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, cutil.GetPtr(time.Now().Add(-1*time.Hour)), ipu)
	site3 := util.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, cutil.GetPtr(time.Now()), ipu)
	site4 := util.TestBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, cutil.GetPtr(time.Now().Add(-1*time.Hour)), ipu)
	site5 := util.TestBuildSite(t, dbSession, ip, "test-site-5", cdbm.SiteStatusRegistered, nil, ipu)

	// Only site3 has ever reported a cert expiry, so the rest exercise the
	// never-reported case the gauge publishes as 0.
	site3CertExpiry := time.Now().Add(30 * 24 * time.Hour)
	_, err := cdbm.NewSiteDAO(dbSession).Update(ctx, nil, cdbm.SiteUpdateInput{
		SiteID:          site3.ID,
		AgentCertExpiry: &site3CertExpiry,
	})
	assert.NoError(t, err)

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	temporalsuit.NewTestWorkflowEnvironment()

	testServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ok"))
	}))

	cfg := config.NewConfig()
	cfg.SetNotificationsSlackWebhookURL(testServer.URL)

	cfg2 := config.NewConfig()
	cfg2.SetNotificationsSlackWebhookURL("")

	// One registry across every case, so a later run sees what the earlier ones
	// published and can prove a Site keeps or loses its series.
	reg := prometheus.NewRegistry()
	siteHealthMetrics := cwm.NewSiteHealthMetrics(reg, "nico_rest_workflow")

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		cfg            *config.Config
	}
	type args struct {
		ctx context.Context
	}
	tests := []struct {
		name          string
		fields        fields
		args          args
		setup         func(t *testing.T)
		wantStatus    map[uuid.UUID]string
		wantGauge     map[string]float64
		wantCertGauge map[string]float64
	}{
		{
			name: "test monitor inventory receipt for all sites with Slack notification",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				cfg:            cfg,
			},
			args: args{
				ctx: ctx,
			},
			wantStatus: map[uuid.UUID]string{
				site1.ID: cdbm.SiteStatusPending,
				site2.ID: cdbm.SiteStatusError,
				site3.ID: cdbm.SiteStatusRegistered,
			},
			// site1 is Pending so it is not published at all, and site5 has never
			// reported, so it publishes 0 rather than going missing.
			wantGauge: map[string]float64{
				site2.Name: float64(site2.InventoryReceived.Unix()),
				site3.Name: float64(site3.InventoryReceived.Unix()),
				site4.Name: float64(site4.InventoryReceived.Unix()),
				site5.Name: 0,
			},
			wantCertGauge: map[string]float64{
				site2.Name: 0,
				site3.Name: float64(site3CertExpiry.Unix()),
				site4.Name: 0,
				site5.Name: 0,
			},
		},
		{
			name: "test monitor inventory receipt for all sites without Slack notification",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				cfg:            cfg2,
			},
			args: args{
				ctx: ctx,
			},
			wantStatus: map[uuid.UUID]string{
				site4.ID: cdbm.SiteStatusError,
			},
			// site2 and site4 went to Error in the case above and are still
			// disconnected, so they have to keep reporting. Dropping them here
			// would resolve the alert while the outage continues.
			wantGauge: map[string]float64{
				site2.Name: float64(site2.InventoryReceived.Unix()),
				site3.Name: float64(site3.InventoryReceived.Unix()),
				site4.Name: float64(site4.InventoryReceived.Unix()),
				site5.Name: 0,
			},
			wantCertGauge: map[string]float64{
				site2.Name: 0,
				site3.Name: float64(site3CertExpiry.Unix()),
				site4.Name: 0,
				site5.Name: 0,
			},
		},
		{
			name: "test monitor inventory receipt drops a deleted Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				cfg:            cfg2,
			},
			args: args{
				ctx: ctx,
			},
			setup: func(t *testing.T) {
				derr := cdbm.NewSiteDAO(dbSession).Delete(ctx, nil, site4.ID)
				assert.NoError(t, derr)
			},
			// A Site that no longer exists is the one case the rebuild has to
			// clear, otherwise it ages into an alert nothing can resolve.
			wantGauge: map[string]float64{
				site2.Name: float64(site2.InventoryReceived.Unix()),
				site3.Name: float64(site3.InventoryReceived.Unix()),
				site5.Name: 0,
			},
			wantCertGauge: map[string]float64{
				site2.Name: 0,
				site3.Name: float64(site3CertExpiry.Unix()),
				site5.Name: 0,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.setup != nil {
				tt.setup(t)
			}

			mst := ManageSite{
				dbSession:         tt.fields.dbSession,
				siteClientPool:    tt.fields.siteClientPool,
				cfg:               tt.fields.cfg,
				siteHealthMetrics: siteHealthMetrics,
			}
			err := mst.MonitorInventoryReceiptForAllSites(tt.args.ctx)
			assert.NoError(t, err)

			for siteID, wantStatus := range tt.wantStatus {
				siteDAO := cdbm.NewSiteDAO(dbSession)
				site, err := siteDAO.GetByID(ctx, nil, siteID, nil, false)
				assert.NoError(t, err)
				assert.Equal(t, wantStatus, site.Status)
			}

			assert.Equal(t, tt.wantGauge, testSiteGauge(t, reg, "nico_rest_workflow_site_last_inventory_receipt_timestamp_seconds"))
			assert.Equal(t, tt.wantCertGauge, testSiteGauge(t, reg, "nico_rest_workflow_site_agent_cert_expiry_timestamp_seconds"))
		})
	}
}

// testSiteGauge reads a per-Site gauge back as Site name to published value.
func testSiteGauge(t *testing.T, reg *prometheus.Registry, name string) map[string]float64 {
	families, err := reg.Gather()
	require.NoError(t, err)

	values := map[string]float64{}
	for _, family := range families {
		if family.GetName() != name {
			continue
		}
		for _, metric := range family.GetMetric() {
			for _, label := range metric.GetLabel() {
				if label.GetName() == "site" {
					values[label.GetValue()] = metric.GetGauge().GetValue()
				}
			}
		}
	}

	return values
}

// MockTemporalClient is a mock for Temporal Client
type MockTemporalClient struct {
	mock.Mock
}

func (m *MockTemporalClient) ExecuteWorkflow(ctx context.Context, options client.StartWorkflowOptions, workflow interface{}, args ...interface{}) (client.WorkflowRun, error) {
	argsM := m.Called(ctx, options, workflow, args)
	return argsM.Get(0).(client.WorkflowRun), argsM.Error(1)
}

func TestManageSite_CheckOTPExpirationAndRenewForAllSites(t *testing.T) {
	ctx := context.Background()

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	// Initialize schema and mock data
	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	site1 := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	site2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)

	// Mock the HTTP server to simulate Site Manager responses
	almostExpired := time.Now().Add(-23 * time.Hour).Format("2006-01-02 15:04:05 -0700 MST")
	testServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`{
			"siteuuid": "` + uuid.New().String() + `",
			"otp": "mocked-otp",
			"otpexpiry": "` + almostExpired + `"
		}`))
	}))
	defer testServer.Close()

	// Mock Temporal Client
	wrun1 := &tmocks.WorkflowRun{}
	wrun1.On("GetID").Return("test-workflow-id-1")

	mockTemporalClient := &tmocks.Client{}
	mockTemporalClient.On("ExecuteWorkflow", mock.Anything, mock.Anything, "RotateTemporalCertAccessOTP", mock.Anything).Return(wrun1, nil)

	tSiteClientPool := sc.NewClientPool(nil)
	tSiteClientPool.IDClientMap[site1.ID.String()] = mockTemporalClient
	tSiteClientPool.IDClientMap[site2.ID.String()] = mockTemporalClient

	// Set up test environment
	cfg := config.NewConfig()
	cfg.SetSiteManagerEndpoint(testServer.URL)

	temporalsuit := testsuite.WorkflowTestSuite{}
	temporalsuit.NewTestWorkflowEnvironment()

	// Define test cases
	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
	}
	tests := []struct {
		name       string
		fields     fields
		wantErr    bool
		wantStatus map[uuid.UUID]string
	}{
		{
			name: "Test OTP expiration and renewal for all sites with no errors",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
			},
			wantErr: false,
			wantStatus: map[uuid.UUID]string{
				site1.ID: cdbm.SiteStatusRegistered,
				site2.ID: cdbm.SiteStatusRegistered,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mst := ManageSite{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
				cfg:            cfg,
			}

			err := mst.CheckOTPExpirationAndRenewForAllSites(context.Background())
			if (err != nil) != tt.wantErr {
				t.Errorf("CheckOTPExpirationAndRenewForAllSites() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			for siteID, wantStatus := range tt.wantStatus {
				siteDAO := cdbm.NewSiteDAO(dbSession)
				site, err := siteDAO.GetByID(ctx, nil, siteID, nil, false)
				assert.NoError(t, err)
				assert.Equal(t, wantStatus, site.Status)
			}
		})
	}
}

func TestManageSite_CheckOTPExpirationAndRenewForAllSites_MoreThanDefaultPageSize(t *testing.T) {
	ctx := context.Background()
	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	// Initialize schema and mock data
	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	// Create more than 20 sites to exceed the default page size.
	siteCount := 25
	siteIDs := make([]uuid.UUID, 0, siteCount)
	for i := 1; i <= siteCount; i++ {
		site := util.TestBuildSite(t, dbSession, ip, fmt.Sprintf("test-site-%d", i), cdbm.SiteStatusRegistered, nil, ipu)
		siteIDs = append(siteIDs, site.ID)
	}
	almostExpiredTime := time.Now().Add(24 * time.Hour).Format("2006-01-02 15:04:05 -0700 MST")

	requestCount := 0
	testServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requestCount++
		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`{
            "siteuuid": "` + uuid.New().String() + `",
            "otp": "mocked-otp",
            "otpexpiry": "` + almostExpiredTime + `"
        }`))
	}))
	defer testServer.Close()

	wrun := &tmocks.WorkflowRun{}
	wrun.On("GetID").Return("test-workflow-id")

	mockTemporalClient := &tmocks.Client{}
	mockTemporalClient.On(
		"ExecuteWorkflow",
		mock.Anything, // context
		mock.Anything, // StartWorkflowOptions
		"RotateTemporalCertAccessOTP",
		mock.Anything, // OTP
	).Return(wrun, nil)

	tSiteClientPool := sc.NewClientPool(nil)
	for _, sid := range siteIDs {
		tSiteClientPool.IDClientMap[sid.String()] = mockTemporalClient
	}

	// Set up config with the test server's endpoint
	cfg := config.NewConfig()
	cfg.SetSiteManagerEndpoint(testServer.URL)

	mst := ManageSite{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
		cfg:            cfg}

	err := mst.CheckOTPExpirationAndRenewForAllSites(ctx)
	require.NoError(t, err, "Expected no error from CheckOTPExpirationAndRenewForAllSites")

	// Assert that the site-manager endpoint was called as many times as the number of sites
	// times 2: Once because we'll call RollSite and once for GetSiteOTP again
	assert.Equal(t, siteCount*2, requestCount, "Expected site manager to be called for all sites")

	// Assert that Temporal client was called for each site
	assert.Equal(t, siteCount, len(mockTemporalClient.Calls), "Expected Temporal client to be called for all sites")
}

func TestManageSite_UpdateAgentCertExpiry_Activity(t *testing.T) {
	ctx := context.Background()

	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	// Create infrastructure provider org and user
	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)

	// Create infrastructure provider with a valid user
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	// Create a site without AgentCertExpiry, providing the same user as createdBy
	site := util.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	mst := ManageSite{
		dbSession: dbSession,
	}

	// Check initial condition: AgentCertExpiry is nil
	siteDAO := cdbm.NewSiteDAO(dbSession)
	existingSite, err := siteDAO.GetByID(ctx, nil, site.ID, nil, false)
	assert.NoError(t, err)
	assert.Nil(t, existingSite.AgentCertExpiry)

	// Now let's update AgentCertExpiry
	newCertExpiry := time.Now().Add(48 * time.Hour).UTC().Round(time.Microsecond)
	err = mst.UpdateAgentCertExpiry(ctx, site.ID, newCertExpiry)
	assert.NoError(t, err)

	// Verify AgentCertExpiry is updated
	updatedSite, err := siteDAO.GetByID(ctx, nil, site.ID, nil, false)
	assert.NoError(t, err)
	assert.NotNil(t, updatedSite.AgentCertExpiry)
	assert.True(t, updatedSite.AgentCertExpiry.Equal(newCertExpiry))
}

func TestManageSite_DeleteOrphanedSiteTemporalNamespaces_Activity(t *testing.T) {
	ctx := context.Background()

	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	// Create infrastructure provider org and user
	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)

	// Create infrastructure provider with a valid user
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	stDAO := cdbm.NewSiteDAO(dbSession)

	// Create a site with a namespace
	site1 := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, site1)
	site2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, site2)
	site3 := util.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)
	err := stDAO.Delete(ctx, nil, site3.ID)
	assert.Nil(t, err)
	site4 := util.TestBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, nil, ipu)
	err = stDAO.Delete(ctx, nil, site4.ID)
	assert.Nil(t, err)
	site5 := util.TestBuildSite(t, dbSession, ip, "test-site-5", cdbm.SiteStatusRegistered, nil, ipu)
	err = stDAO.Delete(ctx, nil, site5.ID)
	assert.Nil(t, err)

	tc := &tmocks.Client{}
	gmockctrl1 := gomock.NewController(t)

	tws1 := twsv1mock.NewMockWorkflowServiceClient(gmockctrl1)

	nextPageToken := []byte("next-page-token")
	tws1.EXPECT().ListNamespaces(gomock.Any(), &tWorkflowv1.ListNamespacesRequest{
		PageSize:      100,
		NextPageToken: nil,
	}).Return(&tWorkflowv1.ListNamespacesResponse{
		Namespaces: []*tWorkflowv1.DescribeNamespaceResponse{
			{
				NamespaceInfo: &tnsv1.NamespaceInfo{
					Name: site1.ID.String(),
				},
			},
			{
				NamespaceInfo: &tnsv1.NamespaceInfo{
					Name: site2.ID.String(),
				},
			},
			{
				NamespaceInfo: &tnsv1.NamespaceInfo{
					Name: "cloud",
				},
			},
			{
				NamespaceInfo: &tnsv1.NamespaceInfo{
					Name: site3.ID.String(),
				},
			},
			{
				NamespaceInfo: &tnsv1.NamespaceInfo{
					Name: site4.ID.String(),
				},
			},
		},
		NextPageToken: nextPageToken,
	}, nil).Times(1)

	tws1.EXPECT().ListNamespaces(gomock.Any(), &tWorkflowv1.ListNamespacesRequest{
		PageSize:      100,
		NextPageToken: nextPageToken,
	}).Return(&tWorkflowv1.ListNamespacesResponse{
		Namespaces: []*tWorkflowv1.DescribeNamespaceResponse{
			{
				NamespaceInfo: &tnsv1.NamespaceInfo{
					Name: site5.ID.String(),
				},
			},
		},
		NextPageToken: nil,
	}, nil).Times(1)

	tc.Mock.On("WorkflowService").Return(tws1)

	tosc1 := tosv1mock.NewMockOperatorServiceClient(gmockctrl1)

	// Delete namespace should be called for the 2 random UUID namespaces
	tosc1.EXPECT().DeleteNamespace(gomock.Any(), gomock.Any()).Return(&tOperatorv1.DeleteNamespaceResponse{}, nil).Times(3)

	tc.Mock.On("OperatorService").Return(tosc1)

	temporalsuit := testsuite.WorkflowTestSuite{}
	temporalsuit.NewTestWorkflowEnvironment()

	mst := ManageSite{
		dbSession: dbSession,
		tc:        tc,
	}

	err = mst.DeleteOrphanedSiteTemporalNamespaces(ctx)
	assert.NoError(t, err, "Expected no error when deleting orphaned site temporal namespaces")

	gmockctrl1.Finish()
}

// TestManageSite_DeleteSiteComponentsFromDB_NewResources covers the additional
// site-scoped resources that DeleteSiteComponentsFromDB now cleans up
// (interfaces, vpc prefixes, vpc peerings, NVLink logical partitions,
// SSH key group site/instance associations, network security groups, DPU
// extension service deployments, SKUs, expected machine, expected switch, and
// expected powershelf records).
//
// The test builds the same set of records under two sites, runs the cleanup
// against site 1, and then asserts that:
//   - every site-1 record is gone (soft-deleted, or hard-deleted for SKU and
//     the expected_* tables which have no soft_delete column), and
//   - every site-2 record is still present and active.
//
// SSHKeyAssociation is intentionally not asserted on here: the workflow's
// current call to skaDAO.GetAll filters by sshKeyGroupIDs using the siteID,
// which is effectively a no-op (no group has a site UUID). It is built so the
// scenario is realistic but is not part of the cleanup contract being tested.
func TestManageSite_DeleteSiteComponentsFromDB_NewResources(t *testing.T) {
	ctx := context.Background()

	dbSession := testSiteInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org-1"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, ipu)

	tnOrg := "test-tenant-org-1"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}
	tnu := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{tnOrg}, tnRoles)
	tenant := util.TestBuildTenant(t, dbSession, "test-tenant", tnOrg, nil, tnu)

	iDAO := cdbm.NewInstanceDAO(dbSession)
	operatingSystem := util.TestBuildOperatingSystem(t, dbSession, "test-os")

	// siteResources captures the IDs we expect to verify after the cleanup
	// runs. Pointer types are used where the underlying ID type isn't
	// uuid.UUID (NSG, SKU all use string IDs).
	type siteResources struct {
		site               *cdbm.Site
		instance           *cdbm.Instance
		vpc                *cdbm.Vpc
		ipBlock            *cdbm.IPBlock
		vpcPrefixID        uuid.UUID
		vpcPeeringID       uuid.UUID
		nvllpID            uuid.UUID
		nsgID              string
		skuID              string
		dpuDeploymentID    uuid.UUID
		interfaceIDs       []uuid.UUID
		ibInterfaceIDs     []uuid.UUID
		nvlinkInterfaceIDs []uuid.UUID
		sshKeyGroupSiteID  uuid.UUID
		sshKeyGroupInstID  uuid.UUID
		sshKeyAssocID      uuid.UUID
		expectedMachineID  uuid.UUID
		expectedSwitchID   uuid.UUID
		expectedShelfID    uuid.UUID
		imageOSSAID        uuid.UUID
	}

	buildSiteResources := func(tag string) *siteResources {
		site := util.TestBuildSite(t, dbSession, ip, "test-site-"+tag, cdbm.SiteStatusPending, nil, ipu)

		// Core dependencies for an instance. A second VPC is needed so the
		// VpcPeering row can satisfy its (vpc1_id, vpc2_id) foreign keys.
		vpc := util.TestBuildVpc(t, dbSession, ip, site, tenant, "vpc-"+tag)
		vpc2 := util.TestBuildVpc(t, dbSession, ip, site, tenant, "vpc2-"+tag)
		machine := util.TestBuildMachine(t, dbSession, ip.ID, site.ID, cutil.GetPtr("x86"), cutil.GetPtr(true), cdbm.MachineStatusReady)
		instanceType := util.TestBuildInstanceType(t, dbSession, ip, site, "it-"+tag)

		instance, err := iDAO.Create(
			ctx, nil,
			cdbm.InstanceCreateInput{
				Name:                     "ins-" + tag,
				TenantID:                 tenant.ID,
				InfrastructureProviderID: ip.ID,
				SiteID:                   site.ID,
				InstanceTypeID:           &instanceType.ID,
				VpcID:                    vpc.ID,
				MachineID:                &machine.ID,
				Hostname:                 cutil.GetPtr(tag + ".test.com"),
				OperatingSystemID:        cutil.GetPtr(operatingSystem.ID),
				IpxeScript:               cutil.GetPtr("ipxe"),
				UserData:                 cutil.GetPtr("userdata"),
				Labels:                   map[string]string{},
				Status:                   cdbm.InstanceStatusPending,
				CreatedBy:                tnu.ID,
			},
		)
		require.NoError(t, err)

		// VPC Prefix needs an IPBlock.
		ipBlock := util.TestBuildBuildIPBlock(t, dbSession, "ipblock-"+tag, site, ip, &tenant.ID, cdbm.IPBlockRoutingTypeDatacenterOnly, "10.0.0.0/16", 16, cdbm.IPBlockProtocolVersionV4, true, cdbm.IPBlockStatusReady, ipu)
		vpcPrefix := util.TestBuildVPCPrefix(t, dbSession, "vpfx-"+tag, site, tenant, vpc.ID, &ipBlock.ID, cutil.GetPtr("10.1.0.0/24"), cutil.GetPtr(24), "Pending", ipu)

		// VpcPeering uses two distinct vpc1/vpc2 IDs and the schema enforces
		// real FKs on both, so we use the two VPCs created above.
		vpcPeering := util.TestBuildVpcPeering(t, dbSession, vpc.ID, vpc2.ID, site.ID, ip.ID, tenant.ID, ipu.ID)

		// NVLink logical partition + an NVLink interface attached to it.
		nvllp := util.TestBuildNVLinkLogicalPartition(t, dbSession, "nvllp-"+tag, nil, site, tenant, cdbm.NVLinkLogicalPartitionStatusReady, false)
		nvli := util.TestBuildNVLinkInterface(t, dbSession, instance.ID, site.ID, nvllp.ID, cutil.GetPtr("Nvidia GB200"), 0, cutil.GetPtr("guid-"+tag), nil, cdbm.NVLinkInterfaceStatusReady)

		// InfiniBand partition + an InfiniBand interface attached to it.
		ibp := util.TestBuildInfiniBandPartition(t, dbSession, "ibp-"+tag, site, tenant, nil, cdbm.InfiniBandPartitionStatusReady, false)
		ibi := util.TestBuildInfiniBandInterface(t, dbSession, instance.ID, site.ID, ibp.ID, "mlx5_0", 0, true, nil, cdbm.InfiniBandInterfaceStatusReady, false)

		// Two ethernet interfaces on the instance.
		iface1 := util.TestBuildInterface(t, dbSession, &instance.ID, nil, nil, true, cutil.GetPtr("eth0"), cutil.GetPtr(0), nil, &ipu.ID, cdbm.InterfaceStatusReady)
		iface2 := util.TestBuildInterface(t, dbSession, &instance.ID, nil, nil, false, cutil.GetPtr("eth1"), cutil.GetPtr(1), nil, &ipu.ID, cdbm.InterfaceStatusPending)

		// Network Security Group
		nsg := util.TestBuildNetworkSecurityGroup(t, dbSession, "nsg-"+tag, site, tenant, cdbm.NetworkSecurityGroupStatusReady, ipu)

		// DPU extension service + deployment
		des := util.TestBuildDpuExtensionService(t, dbSession, "des-"+tag, site, tenant, "test-type", cutil.GetPtr("v1"), nil, []string{"v1"}, cdbm.DpuExtensionServiceStatusReady, ipu)
		desd := util.TestBuildDpuExtensionServiceDeployment(t, dbSession, des.ID, site.ID, tenant.ID, instance.ID, "v1", cdbm.DpuExtensionServiceStatusReady, ipu)

		// SKU (hard delete)
		sku := util.TestBuildSku(t, dbSession, "sku-"+tag, site)

		// SSH key group + site/instance associations + key + key association.
		// The site/instance associations are scoped by SiteID; the key
		// association is intentionally unscoped (see test docstring).
		skg := util.TestBuildSSHKeyGroup(t, dbSession, "skg-"+tag, tenant.Org, nil, tenant.ID, cutil.GetPtr("v1"), cdbm.SSHKeyGroupStatusSynced, ipu.ID)
		skgsa := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg.ID, site.ID, cutil.GetPtr("v1"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, ipu.ID)
		skgia := util.TestBuildSSHKeyGroupInstanceAssociation(t, dbSession, skg.ID, site.ID, instance.ID, ipu.ID)
		sshKey := util.TestBuildSSHKey(t, dbSession, "key-"+tag, tenant, "ssh-rsa AAAA...", ipu)
		ska := util.TestBuildSSHKeyAssociation(t, dbSession, skg.ID, sshKey.ID, ipu.ID)

		// Expected records (hard-deleted by the workflow). MAC addresses are
		// scoped per-tag so the optional (bmc_mac_address, site_id) unique
		// constraint, if present, will not be tripped across sites.
		em := util.TestBuildExpectedMachine(t, dbSession, site, "00:11:22:33:44:0"+tag, "chassis-"+tag, ipu)
		es := util.TestBuildExpectedSwitch(t, dbSession, site, "00:11:22:33:55:0"+tag, "switch-"+tag, ipu)
		eps := util.TestBuildExpectedPowerShelf(t, dbSession, site, "00:11:22:33:66:0"+tag, "shelf-"+tag, ipu)

		// OperatingSystem with a single ossa on this site.
		imageOS := util.TestBuildImageOperatingSystem(t, dbSession, &ip.ID, nil, "img-"+tag, ipOrg, cutil.GetPtr("v1"), cdbm.OperatingSystemStatusReady)
		imageOSSA := util.TestBuildImageOperatingSystemSiteAssociation(t, dbSession, imageOS.ID, site.ID, cdbm.OperatingSystemSiteAssociationStatusSynced, "v1", false)

		return &siteResources{
			site:               site,
			instance:           instance,
			vpc:                vpc,
			ipBlock:            ipBlock,
			vpcPrefixID:        vpcPrefix.ID,
			vpcPeeringID:       vpcPeering.ID,
			nvllpID:            nvllp.ID,
			nsgID:              nsg.ID,
			skuID:              sku.ID,
			dpuDeploymentID:    desd.ID,
			interfaceIDs:       []uuid.UUID{iface1.ID, iface2.ID},
			ibInterfaceIDs:     []uuid.UUID{ibi.ID},
			nvlinkInterfaceIDs: []uuid.UUID{nvli.ID},
			sshKeyGroupSiteID:  skgsa.ID,
			sshKeyGroupInstID:  skgia.ID,
			sshKeyAssocID:      ska.ID,
			expectedMachineID:  em.ID,
			expectedSwitchID:   es.ID,
			expectedShelfID:    eps.ID,
			imageOSSAID:        imageOSSA.ID,
		}
	}

	site1Resources := buildSiteResources("a")
	site2Resources := buildSiteResources("b")

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	mv := ManageSite{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
	}

	err := mv.DeleteSiteComponentsFromDB(ctx, site1Resources.site.ID, ip.ID, false)
	require.NoError(t, err)

	// assertGone checks that a soft-deletable row identified by `id` is no
	// longer visible to a default (non-WhereAllWithDeleted) select. Caller
	// passes a fresh empty model pointer so we can vary the type.
	assertGone := func(label string, model interface{}, idColumn string, id interface{}) {
		t.Helper()
		err := dbSession.DB.NewSelect().Model(model).Where(idColumn+" = ?", id).Scan(ctx)
		assert.Equal(t, sql.ErrNoRows, err, "%s with id %v should be soft-deleted/hard-deleted", label, id)
	}

	// assertPresent checks that a row is still visible to a default select
	// (i.e. not soft-deleted).
	assertPresent := func(label string, model interface{}, idColumn string, id interface{}) {
		t.Helper()
		err := dbSession.DB.NewSelect().Model(model).Where(idColumn+" = ?", id).Scan(ctx)
		assert.NoError(t, err, "%s with id %v should still be present", label, id)
	}

	// --- Site 1: every site-scoped resource we built should be gone. ---

	// Ethernet interfaces (DeleteAllByInstanceIDs)
	for _, id := range site1Resources.interfaceIDs {
		assertGone("interface", &cdbm.Interface{}, "ifc.id", id)
	}
	// InfiniBand interfaces (DeleteAllBySiteID)
	for _, id := range site1Resources.ibInterfaceIDs {
		assertGone("infiniband_interface", &cdbm.InfiniBandInterface{}, "ibi.id", id)
	}
	// NVLink interfaces (DeleteAllBySiteID)
	for _, id := range site1Resources.nvlinkInterfaceIDs {
		assertGone("nvlink_interface", &cdbm.NVLinkInterface{}, "nvli.id", id)
	}
	assertGone("vpc_prefix", &cdbm.VpcPrefix{}, "vp.id", site1Resources.vpcPrefixID)
	assertGone("vpc_peering", &cdbm.VpcPeering{}, "vp.id", site1Resources.vpcPeeringID)
	assertGone("nvlink_logical_partition", &cdbm.NVLinkLogicalPartition{}, "nvllp.id", site1Resources.nvllpID)
	assertGone("ssh_key_group_site_association", &cdbm.SSHKeyGroupSiteAssociation{}, "skgsa.id", site1Resources.sshKeyGroupSiteID)
	assertGone("ssh_key_group_instance_association", &cdbm.SSHKeyGroupInstanceAssociation{}, "skgia.id", site1Resources.sshKeyGroupInstID)
	assertGone("network_security_group", &cdbm.NetworkSecurityGroup{}, "nsg.id", site1Resources.nsgID)
	assertGone("dpu_extension_service_deployment", &cdbm.DpuExtensionServiceDeployment{}, "desd.id", site1Resources.dpuDeploymentID)
	assertGone("sku", &cdbm.SKU{}, "sk.id", site1Resources.skuID)
	assertGone("expected_machine", &cdbm.ExpectedMachine{}, "em.id", site1Resources.expectedMachineID)
	assertGone("expected_switch", &cdbm.ExpectedSwitch{}, "es.id", site1Resources.expectedSwitchID)
	assertGone("expected_power_shelf", &cdbm.ExpectedPowerShelf{}, "eps.id", site1Resources.expectedShelfID)
	assertGone("operating_system_site_association (site 1 image OS)", &cdbm.OperatingSystemSiteAssociation{}, "ossa.id", site1Resources.imageOSSAID)

	// --- Site 2: nothing should have been touched. ---

	for _, id := range site2Resources.interfaceIDs {
		assertPresent("interface", &cdbm.Interface{}, "ifc.id", id)
	}
	for _, id := range site2Resources.ibInterfaceIDs {
		assertPresent("infiniband_interface", &cdbm.InfiniBandInterface{}, "ibi.id", id)
	}
	for _, id := range site2Resources.nvlinkInterfaceIDs {
		assertPresent("nvlink_interface", &cdbm.NVLinkInterface{}, "nvli.id", id)
	}
	assertPresent("vpc_prefix", &cdbm.VpcPrefix{}, "vp.id", site2Resources.vpcPrefixID)
	assertPresent("vpc_peering", &cdbm.VpcPeering{}, "vp.id", site2Resources.vpcPeeringID)
	assertPresent("nvlink_logical_partition", &cdbm.NVLinkLogicalPartition{}, "nvllp.id", site2Resources.nvllpID)
	assertPresent("ssh_key_group_site_association", &cdbm.SSHKeyGroupSiteAssociation{}, "skgsa.id", site2Resources.sshKeyGroupSiteID)
	assertPresent("ssh_key_group_instance_association", &cdbm.SSHKeyGroupInstanceAssociation{}, "skgia.id", site2Resources.sshKeyGroupInstID)
	assertPresent("network_security_group", &cdbm.NetworkSecurityGroup{}, "nsg.id", site2Resources.nsgID)
	assertPresent("dpu_extension_service_deployment", &cdbm.DpuExtensionServiceDeployment{}, "desd.id", site2Resources.dpuDeploymentID)
	assertPresent("sku", &cdbm.SKU{}, "sk.id", site2Resources.skuID)
	assertPresent("expected_machine", &cdbm.ExpectedMachine{}, "em.id", site2Resources.expectedMachineID)
	assertPresent("expected_switch", &cdbm.ExpectedSwitch{}, "es.id", site2Resources.expectedSwitchID)
	assertPresent("expected_power_shelf", &cdbm.ExpectedPowerShelf{}, "eps.id", site2Resources.expectedShelfID)
	// Site 2's image OS and its association are independent of site 1
	assertPresent("operating_system_site_association (site 2 image OS)", &cdbm.OperatingSystemSiteAssociation{}, "ossa.id", site2Resources.imageOSSAID)

	// SSHKeyAssociation for site 1 should still be present since the
	// workflow's call effectively no-ops against site IDs (see test docstring).
	assertPresent("ssh_key_association (intentionally not cleaned)", &cdbm.SSHKeyAssociation{}, "ska.id", site1Resources.sshKeyAssocID)
}

func TestManageSite_UpdateSiteInDB(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)
	siteDAO := cdbm.NewSiteDAO(resources.dbSession)

	// The stored Site Agent version every case starts from, so a case that expects it untouched
	// does not have to restate it.
	const existingAgentVersion = "1.0.0"

	createSite := func(t *testing.T, version *string, config *cdbm.SiteConfig, intervalSeconds *int) *cdbm.Site {
		t.Helper()
		site := &cdbm.Site{
			ID:                       uuid.New(),
			Name:                     "test-site-" + uuid.NewString(),
			DisplayName:              cutil.GetPtr("Test"),
			Org:                      "test",
			InfrastructureProviderID: resources.provider.ID,
			SiteControllerVersion:    version,
			SiteAgentVersion:         cutil.GetPtr(existingAgentVersion),
			InventoryIntervalSeconds: intervalSeconds,
			IsInfinityEnabled:        true,
			Status:                   cdbm.SiteStatusRegistered,
			CreatedBy:                resources.user.ID,
			Config:                   config,
		}
		_, err := resources.dbSession.DB.NewInsert().Model(site).Exec(ctx)
		require.NoError(t, err)
		return site
	}

	tests := []struct {
		name                  string
		existingVersion       *string
		existingConfig        *cdbm.SiteConfig
		existingInterval      *int
		buildInfo             *corev1.BuildInfo
		siteAgentBuildInfo    *corev1.SiteAgentBuildInfo
		wantVersion           *string
		wantVpcSlaac          bool
		wantFlow              bool
		wantAgentVersion      *string
		wantInterval          *int
		wantDBUpdate          bool
		wantVpcSlaacKeyAbsent bool
		wantErr               bool
		wantNonRetryable      bool
		useUnknownSiteID      bool
		omitVpcSlaacKey       bool
	}{
		{
			name:           "stores the reported Site Agent version and interval",
			existingConfig: &cdbm.SiteConfig{},
			buildInfo:      &corev1.BuildInfo{},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{
				Version:           "2.0.0",
				InventoryInterval: durationpb.New(time.Minute),
			},
			wantAgentVersion: cutil.GetPtr("2.0.0"),
			wantInterval:     cutil.GetPtr(60),
			wantDBUpdate:     true,
		},
		{
			name:             "updates a changed interval",
			existingConfig:   &cdbm.SiteConfig{},
			existingInterval: cutil.GetPtr(180),
			buildInfo:        &corev1.BuildInfo{},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{
				Version:           existingAgentVersion,
				InventoryInterval: durationpb.New(time.Minute),
			},
			wantInterval: cutil.GetPtr(60),
			wantDBUpdate: true,
		},
		{
			// An older Site Agent reports nothing about itself, which must not erase what an
			// earlier report established.
			name:             "leaves Site Agent values alone when nothing is reported",
			existingConfig:   &cdbm.SiteConfig{Flow: true},
			existingInterval: cutil.GetPtr(180),
			buildInfo:        &corev1.BuildInfo{},
			wantInterval:     cutil.GetPtr(180),
			wantFlow:         true,
		},
		{
			name:               "keeps the stored interval when the report omits it",
			existingConfig:     &cdbm.SiteConfig{},
			existingInterval:   cutil.GetPtr(180),
			buildInfo:          &corev1.BuildInfo{},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: existingAgentVersion},
			wantInterval:       cutil.GetPtr(180),
		},
		{
			// A sub-second interval cannot come from a cron schedule, so it is not stored.
			name:           "ignores a sub-second interval",
			existingConfig: &cdbm.SiteConfig{},
			buildInfo:      &corev1.BuildInfo{},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{
				Version:           existingAgentVersion,
				InventoryInterval: durationpb.New(500 * time.Millisecond),
			},
		},
		{
			name:            "updates version while VPC SLAAC remains false",
			existingVersion: nil,
			existingConfig:  &cdbm.SiteConfig{},
			buildInfo:       &corev1.BuildInfo{BuildVersion: "1.2.3"},
			wantVersion:     cutil.GetPtr("1.2.3"),
			wantDBUpdate:    true,
		},
		{
			name:            "updates version and advertised VPC SLAAC together",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  &cdbm.SiteConfig{},
			buildInfo: &corev1.BuildInfo{
				BuildVersion: "2.0.0",
				Capabilities: []corev1.BuildCapability{
					corev1.BuildCapability_BUILD_CAPABILITY_VPC_SLAAC,
				},
			},
			wantVersion:  cutil.GetPtr("2.0.0"),
			wantVpcSlaac: true,
			wantDBUpdate: true,
		},
		{
			name:            "skips update when version and false VPC SLAAC match",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  &cdbm.SiteConfig{},
			buildInfo:       &corev1.BuildInfo{BuildVersion: "1.0.0"},
			wantVersion:     cutil.GetPtr("1.0.0"),
		},
		{
			name:            "skips update when version and true VPC SLAAC match",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  &cdbm.SiteConfig{VpcSlaac: true},
			buildInfo: &corev1.BuildInfo{
				BuildVersion: "1.0.0",
				Capabilities: []corev1.BuildCapability{
					corev1.BuildCapability_BUILD_CAPABILITY_VPC_SLAAC,
				},
			},
			wantVersion:  cutil.GetPtr("1.0.0"),
			wantVpcSlaac: true,
		},
		{
			name:            "preserves stored version when build info omits it",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  &cdbm.SiteConfig{},
			buildInfo:       &corev1.BuildInfo{},
			wantVersion:     cutil.GetPtr("1.0.0"),
		},
		{
			name:                  "leaves equivalent missing VPC SLAAC key untouched",
			existingVersion:       cutil.GetPtr("1.0.0"),
			existingConfig:        &cdbm.SiteConfig{NativeNetworking: true},
			buildInfo:             &corev1.BuildInfo{BuildVersion: "1.0.0"},
			wantVersion:           cutil.GetPtr("1.0.0"),
			wantVpcSlaac:          false,
			wantVpcSlaacKeyAbsent: true,
			omitVpcSlaacKey:       true,
		},
		{
			name:                  "updates version without adding an equivalent missing VPC SLAAC key",
			existingVersion:       cutil.GetPtr("1.0.0"),
			existingConfig:        &cdbm.SiteConfig{NativeNetworking: true},
			buildInfo:             &corev1.BuildInfo{BuildVersion: "2.0.0"},
			wantVersion:           cutil.GetPtr("2.0.0"),
			wantVpcSlaac:          false,
			wantDBUpdate:          true,
			wantVpcSlaacKeyAbsent: true,
			omitVpcSlaacKey:       true,
		},
		{
			name:            "leaves version empty when site and build info both lack it",
			existingVersion: nil,
			existingConfig:  &cdbm.SiteConfig{},
			buildInfo:       &corev1.BuildInfo{},
			wantVersion:     nil,
		},
		{
			name:            "updates advertised VPC SLAAC when version is unchanged",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  &cdbm.SiteConfig{},
			buildInfo: &corev1.BuildInfo{
				BuildVersion: "1.0.0",
				Capabilities: []corev1.BuildCapability{
					corev1.BuildCapability_BUILD_CAPABILITY_VPC_SLAAC,
				},
			},
			wantVersion:  cutil.GetPtr("1.0.0"),
			wantVpcSlaac: true,
			wantDBUpdate: true,
		},
		{
			name:            "clears stale VPC SLAAC when capability is absent",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  &cdbm.SiteConfig{VpcSlaac: true},
			buildInfo:       &corev1.BuildInfo{BuildVersion: "1.0.0"},
			wantVersion:     cutil.GetPtr("1.0.0"),
			wantVpcSlaac:    false,
			wantDBUpdate:    true,
		},
		{
			name:               "enables Flow when Site Agent reports it enabled",
			existingVersion:    cutil.GetPtr("1.0.0"),
			existingConfig:     &cdbm.SiteConfig{},
			buildInfo:          &corev1.BuildInfo{BuildVersion: "1.0.0"},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: existingAgentVersion, FlowEnabled: proto.Bool(true)},
			wantVersion:        cutil.GetPtr("1.0.0"),
			wantFlow:           true,
			wantDBUpdate:       true,
		},
		{
			name:               "disables Flow when Site Agent reports it disabled",
			existingVersion:    cutil.GetPtr("1.0.0"),
			existingConfig:     &cdbm.SiteConfig{Flow: true},
			buildInfo:          &corev1.BuildInfo{BuildVersion: "1.0.0"},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: existingAgentVersion, FlowEnabled: proto.Bool(false)},
			wantVersion:        cutil.GetPtr("1.0.0"),
			wantDBUpdate:       true,
		},
		{
			name:               "preserves Flow when queued Site inventory omits configuration",
			existingVersion:    cutil.GetPtr("1.0.0"),
			existingConfig:     &cdbm.SiteConfig{Flow: true},
			buildInfo:          &corev1.BuildInfo{BuildVersion: "1.0.0"},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: existingAgentVersion},
			wantVersion:        cutil.GetPtr("1.0.0"),
			wantFlow:           true,
		},
		{
			name:               "skips update when reported Flow configuration matches",
			existingVersion:    cutil.GetPtr("1.0.0"),
			existingConfig:     &cdbm.SiteConfig{Flow: true},
			buildInfo:          &corev1.BuildInfo{BuildVersion: "1.0.0"},
			siteAgentBuildInfo: &corev1.SiteAgentBuildInfo{Version: existingAgentVersion, FlowEnabled: proto.Bool(true)},
			wantVersion:        cutil.GetPtr("1.0.0"),
			wantFlow:           true,
		},
		{
			name:            "initializes nil config with advertised VPC SLAAC",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  nil,
			buildInfo: &corev1.BuildInfo{
				BuildVersion: "1.0.0",
				Capabilities: []corev1.BuildCapability{
					corev1.BuildCapability_BUILD_CAPABILITY_VPC_SLAAC,
				},
			},
			wantVersion:  cutil.GetPtr("1.0.0"),
			wantVpcSlaac: true,
			wantDBUpdate: true,
		},
		{
			name:            "initializes nil config when VPC SLAAC is unsupported",
			existingVersion: cutil.GetPtr("1.0.0"),
			existingConfig:  nil,
			buildInfo:       &corev1.BuildInfo{BuildVersion: "1.0.0"},
			wantVersion:     cutil.GetPtr("1.0.0"),
			wantVpcSlaac:    false,
			wantDBUpdate:    true,
		},
		{
			name:             "unknown site returns non-retryable error",
			existingConfig:   &cdbm.SiteConfig{},
			buildInfo:        &corev1.BuildInfo{BuildVersion: "1.2.3"},
			wantErr:          true,
			wantNonRetryable: true,
			useUnknownSiteID: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			site := createSite(t, tt.existingVersion, tt.existingConfig, tt.existingInterval)
			if tt.omitVpcSlaacKey {
				_, err := resources.dbSession.DB.NewUpdate().
					Model((*cdbm.Site)(nil)).
					Set("config = config - 'vpc_slaac'").
					Where("id = ?", site.ID).
					Exec(ctx)
				require.NoError(t, err)
			}

			originalUpdated := time.Date(2000, time.January, 1, 0, 0, 0, 0, time.UTC)
			_, err := resources.dbSession.DB.NewUpdate().
				Model((*cdbm.Site)(nil)).
				Set("updated = ?", originalUpdated).
				Where("id = ?", site.ID).
				Exec(ctx)
			require.NoError(t, err)

			siteID := site.ID
			if tt.useUnknownSiteID {
				siteID = uuid.New()
			}

			err = mst.UpdateSiteInDB(ctx, siteID, tt.buildInfo, tt.siteAgentBuildInfo)
			if tt.wantErr {
				require.Error(t, err)
				if tt.wantNonRetryable {
					var applicationErr *temporal.ApplicationError
					require.ErrorAs(t, err, &applicationErr)
					assert.True(t, applicationErr.NonRetryable())
					assert.Equal(t, "Site not found", applicationErr.Type())
					assert.True(t, errors.Is(applicationErr.Unwrap(), cdb.ErrDoesNotExist))
				}
				return
			}

			require.NoError(t, err)

			got, err := siteDAO.GetByID(ctx, nil, site.ID, nil, false)
			require.NoError(t, err)
			if tt.wantVersion == nil {
				assert.Nil(t, got.SiteControllerVersion)
			} else {
				require.NotNil(t, got.SiteControllerVersion)
				assert.Equal(t, *tt.wantVersion, *got.SiteControllerVersion)
			}
			require.NotNil(t, got.Config)
			assert.Equal(t, tt.wantVpcSlaac, got.Config.VpcSlaac)
			assert.Equal(t, tt.wantFlow, got.Config.Flow)

			// A nil expectation means the report left the stored value as createSite wrote it.
			wantAgentVersion := existingAgentVersion
			if tt.wantAgentVersion != nil {
				wantAgentVersion = *tt.wantAgentVersion
			}
			require.NotNil(t, got.SiteAgentVersion)
			assert.Equal(t, wantAgentVersion, *got.SiteAgentVersion)

			if tt.wantInterval == nil {
				assert.Nil(t, got.InventoryIntervalSeconds)
			} else {
				require.NotNil(t, got.InventoryIntervalSeconds)
				assert.Equal(t, *tt.wantInterval, *got.InventoryIntervalSeconds)
			}

			if tt.wantDBUpdate {
				assert.True(t, got.Updated.After(originalUpdated))
			} else {
				assert.True(t, got.Updated.Equal(originalUpdated))
			}

			var persistedVpcSlaac sql.NullBool
			err = resources.dbSession.DB.NewRaw(
				`SELECT (config->>'vpc_slaac')::boolean FROM site WHERE id = ?`,
				site.ID,
			).Scan(ctx, &persistedVpcSlaac)
			require.NoError(t, err)
			if tt.wantVpcSlaacKeyAbsent {
				assert.False(t, persistedVpcSlaac.Valid)
			} else {
				require.True(t, persistedVpcSlaac.Valid)
				assert.Equal(t, tt.wantVpcSlaac, persistedVpcSlaac.Bool)
			}
		})
	}
}

type siteFabricIPBlockTestResources struct {
	dbSession *cdb.Session
	user      *cdbm.User
	provider  *cdbm.InfrastructureProvider
	site      *cdbm.Site
}

func setupSiteFabricIPBlockTest(t *testing.T) siteFabricIPBlockTestResources {
	t.Helper()

	dbSession := testSiteInitDB(t)
	t.Cleanup(dbSession.Close)

	util.TestSetupSchema(t, dbSession)

	ipamStorage := cipam.NewBunStorage(dbSession.DB, nil)
	require.NoError(t, ipamStorage.ApplyDbSchema())
	require.NoError(t, ipamStorage.DeleteAllPrefixesFromAllNamespaces(context.Background()))

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	user := util.TestBuildUser(t, dbSession, uuid.New().String(), []string{ipOrg}, ipRoles)
	provider := util.TestBuildInfrastructureProvider(t, dbSession, "testIP", ipOrg, user)
	site := util.TestBuildSite(t, dbSession, provider, "test-site", cdbm.SiteStatusRegistered, nil, user)

	return siteFabricIPBlockTestResources{
		dbSession: dbSession,
		user:      user,
		provider:  provider,
		site:      site,
	}
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_CreatesMissingBlocks(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	err := mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, []string{
		"10.0.1.12/16",
		"2001:db8:1::1/64",
		"10.0.0.0/16",
	})
	require.NoError(t, err)

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	require.Len(t, ipBlocks, 2)

	ipBlocksByPrefix := map[string]cdbm.IPBlock{}
	for _, ipBlock := range ipBlocks {
		ipBlocksByPrefix[ipam.GetCidrForIPBlock(ctx, ipBlock.Prefix, ipBlock.PrefixLength)] = ipBlock
	}

	assertSiteFabricIPBlock(t, ipBlocksByPrefix["10.0.0.0/16"], "site-fabric-ipv4-10-0-0-0-16", cdbm.IPBlockProtocolVersionV4)
	assertSiteFabricIPBlock(t, ipBlocksByPrefix["2001:db8:1::/64"], "site-fabric-ipv6-20010db8000100000000000000000000-64", cdbm.IPBlockProtocolVersionV6)

	namespace := ipam.GetIpamNamespaceForIPBlock(ctx, cdbm.IPBlockRoutingTypeDatacenterOnly, resources.provider.ID.String(), resources.site.ID.String())
	ipamStorage := ipam.NewIpamStorage(resources.dbSession.DB, nil)
	for cidr := range ipBlocksByPrefix {
		_, err = ipamStorage.ReadPrefix(ctx, cidr, namespace)
		require.NoError(t, err)
	}

	statusDetailDAO := cdbm.NewStatusDetailDAO(resources.dbSession)
	for _, ipBlock := range ipBlocks {
		statusDetails, total, err := statusDetailDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{ipBlock.ID.String()}}, cdbp.PageInput{})
		require.NoError(t, err)
		require.Equal(t, 1, total)
		require.Len(t, statusDetails, 1)
		assert.Equal(t, cdbm.IPBlockStatusReady, statusDetails[0].Status)
		require.NotNil(t, statusDetails[0].Message)
		assert.Equal(t, siteFabricIPBlockReadyMsg, *statusDetails[0].Message)
	}
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_IsIdempotent(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	prefixes := []string{"10.42.0.0/16", "2001:db8:42::/64"}
	require.NoError(t, mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, prefixes))
	require.NoError(t, mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, prefixes))

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	require.Len(t, ipBlocks, 2)

	statusDetailDAO := cdbm.NewStatusDetailDAO(resources.dbSession)
	for _, ipBlock := range ipBlocks {
		_, total, err := statusDetailDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{ipBlock.ID.String()}}, cdbp.PageInput{})
		require.NoError(t, err)
		assert.Equal(t, 1, total)
	}
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_LeavesExistingManualBlock(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	existing := util.TestBuildBuildIPBlock(
		t,
		resources.dbSession,
		"manual-site-block",
		resources.site,
		resources.provider,
		nil,
		cdbm.IPBlockRoutingTypeDatacenterOnly,
		"172.16.0.0",
		12,
		cdbm.IPBlockProtocolVersionV4,
		false,
		cdbm.IPBlockStatusReady,
		resources.user,
	)

	require.NoError(t, mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, []string{"172.16.0.0/12"}))

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	require.Len(t, ipBlocks, 1)
	assert.Equal(t, existing.ID, ipBlocks[0].ID)
	assert.Equal(t, "manual-site-block", ipBlocks[0].Name)
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_CreatesDatacenterOnlyBlockWhenOtherRoutingTypeExists(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	existing := util.TestBuildBuildIPBlock(
		t,
		resources.dbSession,
		"public-site-block",
		resources.site,
		resources.provider,
		nil,
		cdbm.IPBlockRoutingTypePublic,
		"172.16.0.0",
		12,
		cdbm.IPBlockProtocolVersionV4,
		false,
		cdbm.IPBlockStatusReady,
		resources.user,
	)

	require.NoError(t, mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, []string{"172.16.0.0/12"}))

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	require.Len(t, ipBlocks, 2)

	ipBlocksByRoutingType := map[string]cdbm.IPBlock{}
	for _, ipBlock := range ipBlocks {
		ipBlocksByRoutingType[ipBlock.RoutingType] = ipBlock
	}

	require.Contains(t, ipBlocksByRoutingType, cdbm.IPBlockRoutingTypePublic)
	require.Contains(t, ipBlocksByRoutingType, cdbm.IPBlockRoutingTypeDatacenterOnly)
	assert.Equal(t, existing.ID, ipBlocksByRoutingType[cdbm.IPBlockRoutingTypePublic].ID)
	assertSiteFabricIPBlock(
		t,
		ipBlocksByRoutingType[cdbm.IPBlockRoutingTypeDatacenterOnly],
		"site-fabric-ipv4-172-16-0-0-12",
		cdbm.IPBlockProtocolVersionV4,
	)
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_ReturnsErrorWhenFabricBlockLockHeld(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	err := cdb.WithTx(ctx, resources.dbSession, func(tx *cdb.Tx) error {
		require.NoError(t, tx.AcquireAdvisoryLock(ctx, getSiteFabricIPBlockLockID(resources.site), false))

		derr := mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, []string{"10.0.0.0/16"})
		assert.ErrorIs(t, derr, cdb.ErrXactAdvisoryLockFailed)

		return nil
	})
	require.NoError(t, err)

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	assert.Empty(t, ipBlocks)
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_InvalidPrefixDoesNotCreateBlocks(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	err := mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, []string{"not-a-cidr"})
	require.Error(t, err)

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	assert.Empty(t, ipBlocks)
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_NoPrefixesIsNoOp(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	require.NoError(t, mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, resources.site.ID, nil))

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	assert.Empty(t, ipBlocks)
}

func TestManageSite_UpdateIPBlocksInDBFromFabricPrefixes_UnknownSiteReturnsError(t *testing.T) {
	ctx := context.Background()
	resources := setupSiteFabricIPBlockTest(t)
	mst := NewManageSite(resources.dbSession, nil, nil, nil, nil)

	err := mst.UpdateIPBlocksInDBFromFabricPrefixes(ctx, uuid.New(), []string{"10.0.0.0/16"})
	require.ErrorIs(t, err, cdb.ErrDoesNotExist)

	ipBlocks := getSiteFabricIPBlocks(t, ctx, resources)
	assert.Empty(t, ipBlocks)
}

func getSiteFabricIPBlocks(t *testing.T, ctx context.Context, resources siteFabricIPBlockTestResources) []cdbm.IPBlock {
	t.Helper()

	ipBlockDAO := cdbm.NewIPBlockDAO(resources.dbSession)
	ipBlocks, _, err := ipBlockDAO.GetAll(
		ctx,
		nil,
		cdbm.IPBlockFilterInput{
			SiteIDs:                   []uuid.UUID{resources.site.ID},
			InfrastructureProviderIDs: []uuid.UUID{resources.provider.ID},
			ExcludeDerived:            true,
		},
		cdbp.PageInput{},
		nil,
	)
	require.NoError(t, err)

	return ipBlocks
}

func assertSiteFabricIPBlock(t *testing.T, ipBlock cdbm.IPBlock, name string, protocolVersion string) {
	t.Helper()

	assert.Equal(t, name, ipBlock.Name)
	require.NotNil(t, ipBlock.Description)
	assert.Equal(t, siteFabricIPBlockDescription, *ipBlock.Description)
	assert.Equal(t, cdbm.IPBlockRoutingTypeDatacenterOnly, ipBlock.RoutingType)
	assert.Equal(t, protocolVersion, ipBlock.ProtocolVersion)
	assert.Equal(t, cdbm.IPBlockStatusReady, ipBlock.Status)
	assert.False(t, ipBlock.FullGrant)
	assert.Nil(t, ipBlock.TenantID)
}
