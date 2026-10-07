// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	"bytes"
	"context"
	"fmt"
	"reflect"
	"strings"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
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

func testDpuExtensionServiceInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

func testDpuExtensionServiceSetupSchema(t *testing.T, dbSession *cdb.Session) {
	t.Helper()

	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	require.NoError(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	require.NoError(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	require.NoError(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)
	// create TenantSite table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.TenantSite)(nil))
	require.NoError(t, err)
	// create DpuExtensionService table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.DpuExtensionService)(nil))
	require.NoError(t, err)
	// create StatusDetail table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	require.NoError(t, err)
}

func TestManageDpuExtensionService_UpdateDpuExtensionServicesInDB(t *testing.T) {
	ctx := context.Background()
	obsName := "service-metrics"

	dbSession := testDpuExtensionServiceInitDB(t)
	defer dbSession.Close()

	testDpuExtensionServiceSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	// Build test user and tenant
	user := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-org"}, []string{"ADMIN"})
	tenant := util.TestBuildTenant(t, dbSession, "test-tenant", "test-org", nil, user)

	st := util.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, user)
	st2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, user)
	st3 := util.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, user)
	st4 := util.TestBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, nil, user)
	st5 := util.TestBuildSite(t, dbSession, ip, "test-site-5", cdbm.SiteStatusRegistered, nil, user)
	st6 := util.TestBuildSite(t, dbSession, ip, "test-site-6", cdbm.SiteStatusRegistered, nil, user)
	st7 := util.TestBuildSite(t, dbSession, ip, "test-site-7", cdbm.SiteStatusRegistered, nil, user)
	st8 := util.TestBuildSite(t, dbSession, ip, "test-site-8", cdbm.SiteStatusRegistered, nil, user)
	st9 := util.TestBuildSite(t, dbSession, ip, "test-site-9", cdbm.SiteStatusRegistered, nil, user)
	st10 := util.TestBuildSite(t, dbSession, ip, "test-site-10", cdbm.SiteStatusRegistered, nil, user)
	st11 := util.TestBuildSite(t, dbSession, ip, "test-site-11", cdbm.SiteStatusRegistered, nil, user)
	util.TestBuildTenantSiteAssociation(t, dbSession, tenant.Org, tenant.ID, st8.ID, user.ID)
	util.TestBuildTenantSiteAssociation(t, dbSession, tenant.Org, tenant.ID, st9.ID, user.ID)
	util.TestBuildTenantSiteAssociation(t, dbSession, tenant.Org, tenant.ID, st10.ID, user.ID)
	util.TestBuildTenantSiteAssociation(t, dbSession, tenant.Org, tenant.ID, st11.ID, user.ID)

	// Create DPU Extension Services with different statuses
	version1 := fmt.Sprintf("V1-T%d", time.Now().Unix()*1000000)
	dpuExtensionService1 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-1", st, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, cutil.GetPtr(version1), &cdbm.DpuExtensionServiceVersionInfo{
		Version:        version1,
		Data:           "test-data",
		HasCredentials: false,
		Created:        time.Now().UTC().Round(time.Microsecond),
	}, []string{version1}, cdbm.DpuExtensionServiceStatusPending, user)
	dpuExtensionService2 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-2", st, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, cutil.GetPtr(version1), &cdbm.DpuExtensionServiceVersionInfo{
		Version:        version1,
		Data:           "test-data",
		HasCredentials: false,
		Created:        time.Now().UTC().Round(time.Microsecond),
		Observability: &cdbm.DpuExtensionServiceObservability{
			DpuExtensionServiceObservability: &corev1.DpuExtensionServiceObservability{
				Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
					{
						Name: &obsName,
						Config: &corev1.DpuExtensionServiceObservabilityConfig_Logging{
							Logging: &corev1.DpuExtensionServiceObservabilityConfigLogging{
								Path: "/var/log/service.log",
							},
						},
					},
				},
			},
		},
	}, []string{}, cdbm.DpuExtensionServiceStatusPending, user)
	dpuExtensionService3 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-3", st, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, cutil.GetPtr(version1), nil, []string{}, cdbm.DpuExtensionServiceStatusReady, user)
	dpuExtensionService4 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-4", st, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, nil, nil, []string{}, cdbm.DpuExtensionServiceStatusPending, user)
	dpuExtensionService5 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-5", st, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, nil, nil, []string{}, cdbm.DpuExtensionServiceStatusDeleting, user)
	dpuExtensionService6 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-6", st4, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, cutil.GetPtr(version1), &cdbm.DpuExtensionServiceVersionInfo{
		Version:        version1,
		Data:           "test-data",
		HasCredentials: false,
		Created:        time.Now().UTC().Round(time.Microsecond),
	}, []string{version1}, cdbm.DpuExtensionServiceStatusReady, user)
	dpuExtensionService7 := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-7", st5, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, cutil.GetPtr(version1), &cdbm.DpuExtensionServiceVersionInfo{
		Version:        version1,
		Data:           "test-data",
		HasCredentials: false,
		Created:        time.Now().UTC().Round(time.Microsecond),
	}, []string{version1}, cdbm.DpuExtensionServiceStatusReady, user)
	dpfVersionInfo := &cdbm.DpuExtensionServiceVersionInfo{
		Version:        version1,
		Data:           "test-data",
		HasCredentials: false,
		Created:        time.Now().UTC().Round(time.Microsecond),
	}
	dpfUpdating := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-dpf-updating", st6, tenant, cdbm.DpuExtensionServiceServiceTypeDpfHelmChart, cutil.GetPtr(version1), dpfVersionInfo, []string{version1}, cdbm.DpuExtensionServiceStatusReady, user)
	dpfDeleted := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-dpf-deleted", st6, tenant, cdbm.DpuExtensionServiceServiceTypeDpfHelmChart, cutil.GetPtr(version1), dpfVersionInfo, []string{version1}, cdbm.DpuExtensionServiceStatusReady, user)
	dpfNoLifecycle := util.TestBuildDpuExtensionService(t, dbSession, "test-dpu-extension-service-dpf-no-lifecycle", st7, tenant, cdbm.DpuExtensionServiceServiceTypeDpfHelmChart, cutil.GetPtr(version1), dpfVersionInfo, []string{version1}, cdbm.DpuExtensionServiceStatusPending, user)

	dpuExtensionServiceDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)

	recoveredServiceID := uuid.New()
	recoveredDpfServiceID := uuid.New()
	terminatingDpfServiceID := uuid.New()
	terminatedDpfServiceID := uuid.New()
	recoveredVersion := "V1-T1761856992377000"
	recoveredDescription := "recovered from Site inventory"
	staleDescription := "stale pre-deletion description"
	recoveredVersionCreated := time.Now().UTC().Round(time.Microsecond)

	deletedDpuExtensionService := util.TestBuildDpuExtensionService(
		t,
		dbSession,
		"test-dpu-extension-service-deleted",
		st9,
		tenant,
		cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
		cutil.GetPtr(version1),
		&cdbm.DpuExtensionServiceVersionInfo{
			Version:        version1,
			Data:           "old-data",
			HasCredentials: false,
			Created:        recoveredVersionCreated,
		},
		[]string{version1},
		cdbm.DpuExtensionServiceStatusDeleting,
		user,
	)
	_, err := dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{
		DpuExtensionServiceID: deletedDpuExtensionService.ID,
		Description:           &staleDescription,
	})
	require.NoError(t, err)
	err = dpuExtensionServiceDAO.Delete(ctx, nil, deletedDpuExtensionService.ID)
	require.NoError(t, err)
	_, err = dbSession.DB.Exec(
		"UPDATE dpu_extension_service SET deleted = ? WHERE id = ?",
		time.Now().Add(-cutil.DefaultInventoryReceiptInterval*2),
		deletedDpuExtensionService.ID,
	)
	require.NoError(t, err)

	// Build DPU Extension Services for paged testing
	pagedDpuExtensionServices := []*cdbm.DpuExtensionService{}
	pagedInvIds := []string{}

	for i := 0; i < 34; i++ {
		version := fmt.Sprintf("V1-T%d", time.Now().Unix()*1000000)
		dpuExtService := util.TestBuildDpuExtensionService(t, dbSession, fmt.Sprintf("test-dpu-extension-service-paged-%d", i), st3, tenant, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, cutil.GetPtr(version), &cdbm.DpuExtensionServiceVersionInfo{
			Version:        version,
			Data:           "test-data",
			HasCredentials: false,
			Created:        time.Now().UTC().Round(time.Microsecond),
		}, []string{version}, cdbm.DpuExtensionServiceStatusPending, user)
		pagedDpuExtensionServices = append(pagedDpuExtensionServices, dpuExtService)
		pagedInvIds = append(pagedInvIds, dpuExtService.ID.String())
	}

	// The activity defers an update for a row written within the staleness threshold, so age every
	// fixture once they all exist. The two cases that fall back to the row's own write time read
	// Updated from these structs, so refresh them to match what was stored.
	util.TestInventoryAgeUpdatedTimestamp(ctx, t, dbSession, (*cdbm.DpuExtensionService)(nil))
	for _, des := range []*cdbm.DpuExtensionService{dpuExtensionService6, dpuExtensionService7} {
		refreshed, rerr := cdbm.NewDpuExtensionServiceDAO(dbSession).GetByID(ctx, nil, des.ID, nil)
		assert.NoError(t, rerr)
		des.Updated = refreshed.Updated
	}

	pagedCtrlDpuExtensionServices := []*corev1.DpuExtensionService{}
	for i := 0; i < 30; i++ {
		version := fmt.Sprintf("V1-T%d", time.Now().Unix()*1000000)
		ctrlDpuExtService := &corev1.DpuExtensionService{
			ServiceId:  pagedDpuExtensionServices[i].ID.String(),
			VersionCtr: 2,
			LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
				Version:       version,
				Data:          "test-data",
				Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
				HasCredential: false,
			},
			ActiveVersions: []string{version},
		}
		pagedCtrlDpuExtensionServices = append(pagedCtrlDpuExtensionServices, ctrlDpuExtService)
	}

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
		ctx                          context.Context
		siteID                       uuid.UUID
		dpuExtensionServiceInventory *corev1.DpuExtensionServiceInventory
	}

	tests := []struct {
		name                        string
		fields                      fields
		args                        args
		updatedDpuExtensionServices []*cdbm.DpuExtensionService
		deletedDpuExtensionServices []*cdbm.DpuExtensionService
		expectedCreated             map[uuid.UUID]time.Time
		expectedStatuses            map[uuid.UUID]string
		expectTimestampParseError   bool
		expectLifecycleError        bool
		wantErr                     bool
		check                       func(t *testing.T)
	}{
		{
			name: "test DPU Extension Service inventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{},
				},
			},
			wantErr: true,
		},
		{
			name: "test DPU Extension Service inventory processing success with updates",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId:  dpuExtensionService1.ID.String(),
							DpuTarget:  cutil.GetPtr(corev1.DpuExtensionServiceDpuTarget_DPU_EXTENSION_SERVICE_DPU_TARGET_ALL),
							VersionCtr: 2,
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V1-T1761856992374052",
								Data:          "test-data",
								Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
								Observability: &corev1.DpuExtensionServiceObservability{
									Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
										{
											Name: &obsName,
											Config: &corev1.DpuExtensionServiceObservabilityConfig_Prometheus{
												Prometheus: &corev1.DpuExtensionServiceObservabilityConfigPrometheus{
													ScrapeIntervalSeconds: 15,
													Endpoint:              "http://service-1:9090/metrics",
												},
											},
										},
									},
								},
							},
							ActiveVersions: []string{"V1-T1761856992374052"},
						},
						{
							ServiceId:  dpuExtensionService2.ID.String(),
							VersionCtr: 2,
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V1-T1761856992374088",
								Data:          "test-data",
								Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
							},
							ActiveVersions: []string{"V1-T1761856992374088"},
						},
						{
							ServiceId:  dpuExtensionService3.ID.String(),
							VersionCtr: 1,
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V1-T1761856992375343",
								Data:          "test-data",
								Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
							},
							ActiveVersions: []string{"V1-T1761856992375343"},
						},
						{
							ServiceId:  dpuExtensionService4.ID.String(),
							VersionCtr: 3,
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V1-T1761856992376544",
								Data:          "test-data",
								Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
							},
							ActiveVersions: []string{"V1-T1761856992373071", "V1-T1761856992376544"},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			updatedDpuExtensionServices: []*cdbm.DpuExtensionService{dpuExtensionService1, dpuExtensionService2, dpuExtensionService3, dpuExtensionService4},
			deletedDpuExtensionServices: []*cdbm.DpuExtensionService{dpuExtensionService5},
			wantErr:                     false,
		},
		{
			name: "test DPU Extension Service inventory processing logs invalid version timestamp and uses fallback",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st4.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId: dpuExtensionService6.ID.String(),
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V2",
								Data:          "updated-test-data",
								Created:       "invalid timestamp",
								HasCredential: false,
							},
							ActiveVersions: []string{"V2"},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			updatedDpuExtensionServices: []*cdbm.DpuExtensionService{dpuExtensionService6},
			expectedCreated: map[uuid.UUID]time.Time{
				dpuExtensionService6.ID: dpuExtensionService6.Updated,
			},
			expectTimestampParseError: true,
			wantErr:                   false,
		},
		{
			name: "test DPU Extension Service inventory processing uses fallback for empty version timestamp without logging error",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st5.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId: dpuExtensionService7.ID.String(),
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V2",
								Data:          "updated-test-data",
								Created:       "",
								HasCredential: false,
							},
							ActiveVersions: []string{"V2"},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			updatedDpuExtensionServices: []*cdbm.DpuExtensionService{dpuExtensionService7},
			expectedCreated: map[uuid.UUID]time.Time{
				dpuExtensionService7.ID: dpuExtensionService7.Updated,
			},
			expectTimestampParseError: false,
			wantErr:                   false,
		},
		{
			name: "test DPF Helm chart DPU Extension Service inventory processing maps Core lifecycle state to status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st6.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId: dpfUpdating.ID.String(),
							DpuTarget: cutil.GetPtr(corev1.DpuExtensionServiceDpuTarget_DPU_EXTENSION_SERVICE_DPU_TARGET_ALL),
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V2",
								Data:          "updated-test-data",
								Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
							},
							ActiveVersions:  []string{"V2"},
							LifecycleStatus: &corev1.LifecycleStatus{State: `{"state":"updating"}`},
						},
						{
							ServiceId: dpfDeleted.ID.String(),
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V2",
								Data:          "updated-test-data",
								Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
							},
							ActiveVersions:  []string{"V2"},
							LifecycleStatus: &corev1.LifecycleStatus{State: `{"state":"deleted"}`},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			updatedDpuExtensionServices: []*cdbm.DpuExtensionService{dpfUpdating, dpfDeleted},
			expectedStatuses: map[uuid.UUID]string{
				dpfUpdating.ID: cdbm.DpuExtensionServiceStatusUpdating,
				dpfDeleted.ID:  cdbm.DpuExtensionServiceStatusDeleting,
			},
			wantErr: false,
		},
		{
			name: "test DPF Helm chart DPU Extension Service inventory processing keeps status when Core lifecycle is absent",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st7.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId: dpfNoLifecycle.ID.String(),
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       "V2",
								Data:          "updated-test-data",
								Created:       time.Now().UTC().Round(time.Microsecond).Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
							},
							ActiveVersions: []string{"V2"},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			updatedDpuExtensionServices: []*cdbm.DpuExtensionService{dpfNoLifecycle},
			expectedStatuses: map[uuid.UUID]string{
				dpfNoLifecycle.ID: cdbm.DpuExtensionServiceStatusPending,
			},
			expectLifecycleError: true,
			wantErr:              false,
		},
		{
			name: "test DPU Extension Service inventory auto-creates service found only on Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st8.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId:            recoveredServiceID.String(),
							ServiceType:          corev1.DpuExtensionServiceType_KUBERNETES_POD,
							ServiceName:          "site-only-dpu-extension-service",
							TenantOrganizationId: tenant.Org,
							Description:          recoveredDescription,
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       recoveredVersion,
								Data:          "recovered-data",
								Created:       recoveredVersionCreated.Format(DpuExtensionServiceTimeFormat),
								HasCredential: true,
							},
							ActiveVersions: []string{recoveredVersion},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				recovered, rerr := dpuExtensionServiceDAO.GetByID(ctx, nil, recoveredServiceID, nil)
				require.NoError(t, rerr)

				if !assert.NotNil(t, recovered) {
					return
				}

				assert.Equal(t, "site-only-dpu-extension-service", recovered.Name)
				assert.Equal(t, recoveredDescription, *recovered.Description)
				assert.Equal(t, cdbm.DpuExtensionServiceServiceTypeKubernetesPod, recovered.ServiceType)
				assert.Equal(t, st8.ID, recovered.SiteID)
				assert.Equal(t, tenant.ID, recovered.TenantID)
				assert.Equal(t, tenant.CreatedBy, recovered.CreatedBy)
				assert.Equal(t, cdbm.DpuExtensionServiceStatusReady, recovered.Status)
				assert.Equal(t, recoveredVersion, *recovered.Version)
				assert.Equal(t, "recovered-data", recovered.VersionInfo.Data)
				assert.True(t, recovered.VersionInfo.HasCredentials)
				assert.Equal(t, []string{recoveredVersion}, recovered.ActiveVersions)

				statusDetails, total, rerr := cdbm.NewStatusDetailDAO(dbSession).GetAll(
					ctx,
					nil,
					cdbm.StatusDetailFilterInput{EntityIDs: []string{recoveredServiceID.String()}},
					cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				)
				require.NoError(t, rerr)
				assert.Equal(t, 1, total)

				if assert.Len(t, statusDetails, 1) {
					assert.Equal(t, cdbm.DpuExtensionServiceStatusReady, statusDetails[0].Status)
					assert.Equal(t, dpuExtensionServiceRecoveredMessage, *statusDetails[0].Message)
				}
			},
		},
		{
			name: "test DPF Helm chart inventory auto-creates service found only on Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st10.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId:            recoveredDpfServiceID.String(),
							ServiceType:          corev1.DpuExtensionServiceType_DPF_HELM_CHART,
							ServiceName:          "site-only-dpf-extension-service",
							TenantOrganizationId: tenant.Org,
							DpuTarget:            cutil.GetPtr(corev1.DpuExtensionServiceDpuTarget_DPU_EXTENSION_SERVICE_DPU_TARGET_ALL),
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version: recoveredVersion,
								Data:    "recovered-dpf-data",
								Created: recoveredVersionCreated.Format(DpuExtensionServiceTimeFormat),
							},
							ActiveVersions:  []string{recoveredVersion},
							LifecycleStatus: &corev1.LifecycleStatus{State: `{"state":"ready"}`},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				recovered, rerr := dpuExtensionServiceDAO.GetByID(ctx, nil, recoveredDpfServiceID, nil)
				require.NoError(t, rerr)

				if !assert.NotNil(t, recovered) {
					return
				}

				assert.Equal(t, cdbm.DpuExtensionServiceServiceTypeDpfHelmChart, recovered.ServiceType)
				require.NotNil(t, recovered.DpuTarget)
				assert.Equal(t, cdbm.DpuExtensionServiceDpuTargetAll, *recovered.DpuTarget)
				assert.Equal(t, cdbm.DpuExtensionServiceStatusReady, recovered.Status)
				assert.Equal(t, st10.ID, recovered.SiteID)
				assert.Equal(t, tenant.ID, recovered.TenantID)
				assert.Equal(t, recoveredVersion, *recovered.Version)
			},
		},
		{
			name: "test DPF Helm chart inventory skips terminal Site-only services",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st11.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId:            terminatingDpfServiceID.String(),
							ServiceType:          corev1.DpuExtensionServiceType_DPF_HELM_CHART,
							ServiceName:          "terminating-site-only-dpf-service",
							TenantOrganizationId: tenant.Org,
							DpuTarget:            cutil.GetPtr(corev1.DpuExtensionServiceDpuTarget_DPU_EXTENSION_SERVICE_DPU_TARGET_ALL),
							LifecycleStatus:      &corev1.LifecycleStatus{State: `{"state":"deleting"}`},
						},
						{
							ServiceId:            terminatedDpfServiceID.String(),
							ServiceType:          corev1.DpuExtensionServiceType_DPF_HELM_CHART,
							ServiceName:          "terminated-site-only-dpf-service",
							TenantOrganizationId: tenant.Org,
							DpuTarget:            cutil.GetPtr(corev1.DpuExtensionServiceDpuTarget_DPU_EXTENSION_SERVICE_DPU_TARGET_ALL),
							LifecycleStatus:      &corev1.LifecycleStatus{State: `{"state":"deleted"}`},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				for _, serviceID := range []uuid.UUID{terminatingDpfServiceID, terminatedDpfServiceID} {
					_, rerr := dpuExtensionServiceDAO.GetByID(ctx, nil, serviceID, nil)
					assert.Equal(t, cdb.ErrDoesNotExist, rerr)
				}
			},
		},
		{
			name: "test DPU Extension Service inventory restores soft-deleted service",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st9.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{
						{
							ServiceId:            deletedDpuExtensionService.ID.String(),
							ServiceType:          corev1.DpuExtensionServiceType_KUBERNETES_POD,
							ServiceName:          deletedDpuExtensionService.Name,
							TenantOrganizationId: tenant.Org,
							Description:          recoveredDescription,
							LatestVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
								Version:       recoveredVersion,
								Data:          "restored-data",
								Created:       recoveredVersionCreated.Format(DpuExtensionServiceTimeFormat),
								HasCredential: false,
							},
							ActiveVersions: []string{version1, recoveredVersion},
						},
					},
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				},
			},
			check: func(t *testing.T) {
				t.Helper()

				restored, rerr := dpuExtensionServiceDAO.GetByID(ctx, nil, deletedDpuExtensionService.ID, nil)
				require.NoError(t, rerr)

				if !assert.NotNil(t, restored) {
					return
				}

				assert.Nil(t, restored.Deleted)
				assert.Equal(t, cdbm.DpuExtensionServiceStatusReady, restored.Status)
				assert.False(t, restored.IsMissingOnSite)
				require.NotNil(t, restored.Description)
				assert.Equal(t, recoveredDescription, *restored.Description)
				assert.Equal(t, recoveredVersion, *restored.Version)
				assert.Equal(t, "restored-data", restored.VersionInfo.Data)
				assert.Equal(t, []string{version1, recoveredVersion}, restored.ActiveVersions)

				statusDetails, total, rerr := cdbm.NewStatusDetailDAO(dbSession).GetAll(
					ctx,
					nil,
					cdbm.StatusDetailFilterInput{EntityIDs: []string{restored.ID.String()}},
					cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				)
				require.NoError(t, rerr)
				assert.Equal(t, 1, total)

				if assert.Len(t, statusDetails, 1) {
					assert.Equal(t, cdbm.DpuExtensionServiceStatusReady, statusDetails[0].Status)
					assert.Equal(t, dpuExtensionServiceRecoveredMessage, *statusDetails[0].Message)
				}
			},
		},
		{
			name: "test DPU Extension Service inventory processing with failed status",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: []*corev1.DpuExtensionService{},
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
				},
			},
			wantErr: true,
		},
		{
			name: "test paged DPU Extension Service inventory processing, first page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: pagedCtrlDpuExtensionServices[0:10],
					Timestamp:            timestamppb.Now(),
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  3,
						PageSize:    10,
						TotalItems:  30,
						ItemIds:     pagedInvIds[0:30],
					},
				},
			},
			updatedDpuExtensionServices: pagedDpuExtensionServices[0:10],
			wantErr:                     false,
		},
		{
			name: "test paged DPU Extension Service inventory processing, last page",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st3.ID,
				dpuExtensionServiceInventory: &corev1.DpuExtensionServiceInventory{
					DpuExtensionServices: pagedCtrlDpuExtensionServices[20:30],
					Timestamp:            timestamppb.Now(),
					InventoryStatus:      corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 3,
						TotalPages:  3,
						PageSize:    10,
						TotalItems:  30,
						ItemIds:     pagedInvIds[0:30],
					},
				},
			},
			updatedDpuExtensionServices: pagedDpuExtensionServices[20:30],
			wantErr:                     false,
		},
	}

	// Update dpuExtensionService5 to Deleting status and mark it as missing from inventory
	// to test the deletion path
	_, err = dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{
		DpuExtensionServiceID: dpuExtensionService5.ID,
		Status:                cutil.GetPtr(cdbm.DpuExtensionServiceStatusDeleting),
	})
	assert.NoError(t, err)

	// Set updated timestamp to be older than the stale inventory threshold so it can be deleted
	_, err = dbSession.DB.Exec("UPDATE dpu_extension_service SET updated = ? WHERE id = ?", time.Now().Add(-cutil.DefaultInventoryReceiptInterval*2), dpuExtensionService5.ID.String())
	assert.NoError(t, err)

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var logOutput bytes.Buffer
			originalLogger := log.Logger
			log.Logger = zerolog.New(&logOutput)
			defer func() {
				log.Logger = originalLogger
			}()

			mde := ManageDpuExtensionService{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tt.fields.siteClientPool,
			}

			err := mde.UpdateDpuExtensionServicesInDB(tt.args.ctx, tt.args.siteID, tt.args.dpuExtensionServiceInventory)
			assert.Equal(t, tt.wantErr, err != nil)
			assert.Equal(t, tt.expectTimestampParseError, bytes.Contains(logOutput.Bytes(), []byte("failed to parse timestamp for version info")))
			assert.Equal(t, tt.expectLifecycleError, bytes.Contains(logOutput.Bytes(), []byte("failed to derive DPU Extension Service status from Core lifecycle status")))

			if tt.wantErr {
				return
			}

			dpuExtensionServiceDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)

			// Check that DPU Extension Service status was updated in DB
			for _, dpuExtService := range tt.updatedDpuExtensionServices {
				updatedDpuExtService, _ := dpuExtensionServiceDAO.GetByID(ctx, nil, dpuExtService.ID, nil)
				if expectedStatus, ok := tt.expectedStatuses[dpuExtService.ID]; ok {
					assert.Equal(t, expectedStatus, updatedDpuExtService.Status)
				} else if dpuExtService.Status != cdbm.DpuExtensionServiceStatusDeleting {
					assert.Equal(t, cdbm.DpuExtensionServiceStatusReady, updatedDpuExtService.Status)
				}

				for _, controllerDes := range tt.args.dpuExtensionServiceInventory.DpuExtensionServices {
					if controllerDes.ServiceId == updatedDpuExtService.ID.String() {
						if controllerDes.DpuTarget != nil {
							if dpuExtService.ServiceType == cdbm.DpuExtensionServiceServiceTypeDpfHelmChart {
								require.NotNil(t, updatedDpuExtService.DpuTarget)
								assert.Equal(t, cdbm.DpuExtensionServiceDpuTargetAll, *updatedDpuExtService.DpuTarget)
							} else {
								assert.Nil(t, updatedDpuExtService.DpuTarget)
							}
						}
						if updatedDpuExtService.Version != nil {
							assert.Equal(t, controllerDes.LatestVersionInfo.Version, *updatedDpuExtService.Version)
						}
						assert.Equal(t, controllerDes.LatestVersionInfo.Data, updatedDpuExtService.VersionInfo.Data)
						assert.Equal(t, controllerDes.LatestVersionInfo.HasCredential, updatedDpuExtService.VersionInfo.HasCredentials)
						if expectedCreated, ok := tt.expectedCreated[updatedDpuExtService.ID]; ok {
							assert.Equal(t, expectedCreated, updatedDpuExtService.VersionInfo.Created)
						} else {
							assert.Equal(t, controllerDes.LatestVersionInfo.Created, updatedDpuExtService.VersionInfo.Created.Format(DpuExtensionServiceTimeFormat))
						}
						if controllerDes.LatestVersionInfo.Observability != nil {
							assert.Equal(t, controllerDes.LatestVersionInfo.GetObservability().Configs[0].GetPrometheus().Endpoint, updatedDpuExtService.VersionInfo.Observability.GetConfigs()[0].GetPrometheus().Endpoint)
						} else {
							assert.Nil(t, updatedDpuExtService.VersionInfo.Observability)
						}
						assert.Equal(t, controllerDes.ActiveVersions, updatedDpuExtService.ActiveVersions)
					}
				}
			}

			// Check that DPU Extension Services marked for deletion were deleted
			for _, dpuExtService := range tt.deletedDpuExtensionServices {
				_, err = dpuExtensionServiceDAO.GetByID(ctx, nil, dpuExtService.ID, nil)
				assert.Equal(t, cdb.ErrDoesNotExist, err)
			}

			if tt.check != nil {
				tt.check(t)
			}
		})
	}
}

//nolint:contextcheck,funlen,maintidx,paralleltest,thelper // DB-backed table cases share schema and builder context.
func TestManageDpuExtensionService_CreateOrUpdateDpuExtensionServiceFromSite(t *testing.T) {
	ctx := context.Background()
	dbSession := testDpuExtensionServiceInitDB(t)

	defer dbSession.Close()

	type recoveryState struct {
		manager                ManageDpuExtensionService
		site                   *cdbm.Site
		tenant                 *cdbm.Tenant
		tenantSite             *cdbm.TenantSite
		user                   *cdbm.User
		controllerService      *corev1.DpuExtensionService
		dpuExtensionServiceDAO cdbm.DpuExtensionServiceDAO
		persistedServiceID     uuid.UUID
	}

	assertRemainsSoftDeleted := func(t *testing.T, state *recoveryState, _ *cdbm.DpuExtensionService) {
		t.Helper()

		services, _, err := state.dpuExtensionServiceDAO.GetAll(
			ctx,
			nil,
			cdbm.DpuExtensionServiceFilterInput{
				DpuExtensionServiceIDs: []uuid.UUID{state.persistedServiceID},
				IncludeDeleted:         true,
			},
			cdbp.PageInput{},
			nil,
		)
		require.NoError(t, err)

		if assert.Len(t, services, 1) {
			assert.NotNil(t, services[0].Deleted)
		}
	}

	tests := []struct {
		name    string
		setup   func(t *testing.T, state *recoveryState)
		wantNil bool
		check   func(t *testing.T, state *recoveryState, got *cdbm.DpuExtensionService)
	}{
		{
			name: "selects another recovered suffix when candidate name is also used",
			setup: func(t *testing.T, state *recoveryState) {
				util.TestBuildDpuExtensionService(
					t,
					dbSession,
					state.controllerService.GetServiceName(),
					state.site,
					state.tenant,
					cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
					nil,
					nil,
					[]string{},
					cdbm.DpuExtensionServiceStatusReady,
					state.user,
				)
				util.TestBuildDpuExtensionService(
					t,
					dbSession,
					fmt.Sprintf(
						"%s-recovered-%s",
						state.controllerService.GetServiceName(),
						state.controllerService.GetServiceId()[:8],
					),
					state.site,
					state.tenant,
					cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
					nil,
					nil,
					[]string{},
					cdbm.DpuExtensionServiceStatusReady,
					state.user,
				)
			},
			check: func(t *testing.T, state *recoveryState, got *cdbm.DpuExtensionService) {
				if !assert.NotNil(t, got) {
					return
				}

				expectedName := fmt.Sprintf(
					"%s-recovered-%s-2",
					state.controllerService.GetServiceName(),
					state.controllerService.GetServiceId()[:8],
				)
				assert.Equal(t, expectedName, got.Name)
			},
		},
		{
			name: "truncates recovered name to API length limit",
			setup: func(t *testing.T, state *recoveryState) {
				state.controllerService.ServiceName = strings.Repeat("a", 256)
				util.TestBuildDpuExtensionService(
					t,
					dbSession,
					state.controllerService.GetServiceName(),
					state.site,
					state.tenant,
					cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
					nil,
					nil,
					[]string{},
					cdbm.DpuExtensionServiceStatusReady,
					state.user,
				)
			},
			check: func(t *testing.T, state *recoveryState, got *cdbm.DpuExtensionService) {
				if !assert.NotNil(t, got) {
					return
				}

				assert.Len(t, []rune(got.Name), 256)
				assert.True(t, strings.HasSuffix(got.Name, "-recovered-"+state.controllerService.GetServiceId()[:8]))
			},
		},
		{
			name: "defers restore after a recent soft delete",
			setup: func(t *testing.T, state *recoveryState) {
				deleted := util.TestBuildDpuExtensionService(
					t,
					dbSession,
					state.controllerService.GetServiceName(),
					state.site,
					state.tenant,
					cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
					nil,
					nil,
					[]string{},
					cdbm.DpuExtensionServiceStatusDeleting,
					state.user,
				)
				state.controllerService.ServiceId = deleted.ID.String()
				state.persistedServiceID = deleted.ID
				err := state.dpuExtensionServiceDAO.Delete(ctx, nil, deleted.ID)
				require.NoError(t, err)
			},
			wantNil: true,
			check:   assertRemainsSoftDeleted,
		},
		{
			name: "renames restored service when its old name was reused",
			setup: func(t *testing.T, state *recoveryState) {
				deleted := util.TestBuildDpuExtensionService(
					t,
					dbSession,
					state.controllerService.GetServiceName(),
					state.site,
					state.tenant,
					cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
					nil,
					nil,
					[]string{},
					cdbm.DpuExtensionServiceStatusDeleting,
					state.user,
				)
				state.controllerService.ServiceId = deleted.ID.String()
				state.persistedServiceID = deleted.ID
				oldDescription := "description that Site has cleared"
				_, err := state.dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{
					DpuExtensionServiceID: deleted.ID,
					Description:           &oldDescription,
				})
				require.NoError(t, err)
				err = state.dpuExtensionServiceDAO.Delete(ctx, nil, deleted.ID)
				require.NoError(t, err)
				_, err = dbSession.DB.Exec(
					"UPDATE dpu_extension_service SET deleted = ? WHERE id = ?",
					time.Now().Add(-cutil.DefaultInventoryReceiptInterval*2),
					deleted.ID,
				)
				require.NoError(t, err)

				util.TestBuildDpuExtensionService(
					t,
					dbSession,
					state.controllerService.GetServiceName(),
					state.site,
					state.tenant,
					cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
					nil,
					nil,
					[]string{},
					cdbm.DpuExtensionServiceStatusReady,
					state.user,
				)
			},
			check: func(t *testing.T, state *recoveryState, got *cdbm.DpuExtensionService) {
				if !assert.NotNil(t, got) {
					return
				}

				assert.Nil(t, got.Deleted)
				assert.Nil(t, got.Description)
				assert.Equal(
					t,
					fmt.Sprintf("%s-recovered-%s", state.controllerService.GetServiceName(), state.controllerService.GetServiceId()[:8]),
					got.Name,
				)
			},
		},
		{
			name: "does not restore service owned by a different tenant",
			setup: func(t *testing.T, state *recoveryState) {
				otherTenant := util.TestBuildTenant(t, dbSession, "other-tenant", "other-org", nil, state.user)
				deleted := util.TestBuildDpuExtensionService(
					t,
					dbSession,
					state.controllerService.GetServiceName(),
					state.site,
					otherTenant,
					cdbm.DpuExtensionServiceServiceTypeKubernetesPod,
					nil,
					nil,
					[]string{},
					cdbm.DpuExtensionServiceStatusDeleting,
					state.user,
				)
				state.controllerService.ServiceId = deleted.ID.String()
				state.persistedServiceID = deleted.ID
				err := state.dpuExtensionServiceDAO.Delete(ctx, nil, deleted.ID)
				require.NoError(t, err)
				_, err = dbSession.DB.Exec(
					"UPDATE dpu_extension_service SET deleted = ? WHERE id = ?",
					time.Now().Add(-cutil.DefaultInventoryReceiptInterval*2),
					deleted.ID,
				)
				require.NoError(t, err)
			},
			wantNil: true,
			check:   assertRemainsSoftDeleted,
		},
		{
			name: "skips service when tenant organization is unknown",
			setup: func(_ *testing.T, state *recoveryState) {
				state.controllerService.TenantOrganizationId = "unknown-org"
			},
			wantNil: true,
		},
		{
			name: "skips service when tenant does not have Site access",
			setup: func(t *testing.T, state *recoveryState) {
				err := cdbm.NewTenantSiteDAO(dbSession).Delete(ctx, nil, state.tenantSite.ID)
				require.NoError(t, err)
			},
			wantNil: true,
		},
		{
			name: "skips service with invalid ID",
			setup: func(_ *testing.T, state *recoveryState) {
				state.controllerService.ServiceId = "invalid"
			},
			wantNil: true,
		},
	}

	for _, testCase := range tests {
		t.Run(testCase.name, func(t *testing.T) {
			testDpuExtensionServiceSetupSchema(t, dbSession)

			ipOrg := "test-provider-org"
			ipUser := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, []string{"FORGE_PROVIDER_ADMIN"})
			ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipUser)
			user := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{"test-org"}, []string{"ADMIN"})
			tenant := util.TestBuildTenant(t, dbSession, "test-tenant", "test-org", nil, user)
			site := util.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, user)
			tenantSite := util.TestBuildTenantSiteAssociation(t, dbSession, tenant.Org, tenant.ID, site.ID, user.ID)

			serviceID := uuid.New()
			state := &recoveryState{
				manager: ManageDpuExtensionService{
					dbSession: dbSession,
				},
				site:                   site,
				tenant:                 tenant,
				tenantSite:             tenantSite,
				user:                   user,
				dpuExtensionServiceDAO: cdbm.NewDpuExtensionServiceDAO(dbSession),
				controllerService: &corev1.DpuExtensionService{
					ServiceId:            serviceID.String(),
					ServiceType:          corev1.DpuExtensionServiceType_KUBERNETES_POD,
					ServiceName:          "site-only-service",
					TenantOrganizationId: tenant.Org,
				},
			}

			testCase.setup(t, state)

			got := state.manager.createOrUpdateDpuExtensionServiceFromSite(ctx, state.site, state.controllerService)
			assert.Equal(t, testCase.wantNil, got == nil)

			if testCase.check != nil {
				testCase.check(t, state, got)
			}
		})
	}
}

func TestNewManageDpuExtensionService(t *testing.T) {
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
		want ManageDpuExtensionService
	}{
		{
			name: "test new ManageDpuExtensionService instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageDpuExtensionService{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageDpuExtensionService(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageDpuExtensionService() = %v, want %v", got, tt.want)
			}
		})
	}
}
