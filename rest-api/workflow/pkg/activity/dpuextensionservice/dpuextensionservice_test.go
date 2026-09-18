// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dpuextensionservice

import (
	"bytes"
	"context"
	"fmt"
	"reflect"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/uptrace/bun/extra/bundebug"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"

	"os"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
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
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create DpuExtensionService table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.DpuExtensionService)(nil))
	assert.Nil(t, err)
	// create StatusDetail table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
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
		expectTimestampParseError   bool
		wantErr                     bool
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
	dpuExtensionServiceDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)
	_, err := dpuExtensionServiceDAO.Update(ctx, nil, cdbm.DpuExtensionServiceUpdateInput{
		DpuExtensionServiceID: dpuExtensionService5.ID,
		Status:                cutil.GetPtr(cdbm.DpuExtensionServiceStatusDeleting),
	})
	assert.NoError(t, err)

	// Set updated timestamp to be older than the stale inventory threshold so it can be deleted
	_, err = dbSession.DB.Exec("UPDATE dpu_extension_service SET updated = ? WHERE id = ?", time.Now().Add(-time.Duration(cwutil.DefaultInventoryReceiptInterval)*2), dpuExtensionService5.ID.String())
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

			if tt.wantErr {
				return
			}

			dpuExtensionServiceDAO := cdbm.NewDpuExtensionServiceDAO(dbSession)

			// Check that DPU Extension Service status was updated in DB
			for _, dpuExtService := range tt.updatedDpuExtensionServices {
				updatedDpuExtService, _ := dpuExtensionServiceDAO.GetByID(ctx, nil, dpuExtService.ID, nil)
				if dpuExtService.Status != cdbm.DpuExtensionServiceStatusDeleting {
					assert.Equal(t, cdbm.DpuExtensionServiceStatusReady, updatedDpuExtService.Status)
				}

				for _, controllerDes := range tt.args.dpuExtensionServiceInventory.DpuExtensionServices {
					if controllerDes.ServiceId == updatedDpuExtService.ID.String() {
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
