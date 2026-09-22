// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"context"
	"fmt"
	"reflect"
	"testing"
	"time"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	"os"

	"go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"go.temporal.io/sdk/testsuite"
)

func TestManageSSHKeyGroup_SyncSSHKeyGroupViaSiteAgent(t *testing.T) {

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		temporalsuit   testsuite.WorkflowTestSuite
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                                context.Context
		siteID                             uuid.UUID
		skgID                              uuid.UUID
		skgsaID                            uuid.UUID
		requestedVersion                   *string
		IsSSHKeyGroupDeleting              *bool
		IsSSHKeyGroupFailedOnInitialCreate *bool
		createRequest                      *corev1.CreateTenantKeysetRequest
		updateRequest                      *corev1.UpdateTenantKeysetRequest
	}

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, tnRoles)

	tn := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)
	assert.NotNil(t, tn)

	st1 := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st1)

	st2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st2)

	// Build SSHKeyGroup1
	skg1 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test1"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation1
	skgsa1 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa1)

	// Build SSH Keys
	sshKey1 := util.TestBuildSSHKey(t, dbSession, "test1", tn, "test1", tnu)
	assert.NotNil(t, sshKey1)

	sshKey2 := util.TestBuildSSHKey(t, dbSession, "test2", tn, "test2", tnu)
	assert.NotNil(t, sshKey2)

	// Build SSHKeyGroupAssociation1
	ska1 := util.TestBuildSSHKeyAssociation(t, dbSession, skg1.ID, sshKey1.ID, tnu.ID)
	assert.NotNil(t, ska1)

	ska2 := util.TestBuildSSHKeyAssociation(t, dbSession, skg1.ID, sshKey2.ID, tnu.ID)
	assert.NotNil(t, ska2)

	// Build SSHKeyGroup2
	skg2 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-2", tnOrg, cutil.GetPtr("test2"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg2)

	// Build SSHKeyGroupSiteAssociation1
	skgsa2 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg2.ID, st1.ID, cutil.GetPtr("1135"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa2)

	_ = util.TestBuildStatusDetail(t, dbSession, skgsa2.ID.String(), cdbm.SSHKeyGroupSiteAssociationStatusSynced, cutil.GetPtr(MsgSSHKeyGroupSynced))

	sshKey3 := util.TestBuildSSHKey(t, dbSession, "test3", tn, "test3", tnu)
	assert.NotNil(t, sshKey3)

	ska3 := util.TestBuildSSHKeyAssociation(t, dbSession, skg2.ID, sshKey3.ID, tnu.ID)
	assert.NotNil(t, ska3)

	ska4 := util.TestBuildSSHKeyAssociation(t, dbSession, skg2.ID, sshKey1.ID, tnu.ID)
	assert.NotNil(t, ska4)

	ska5 := util.TestBuildSSHKeyAssociation(t, dbSession, skg2.ID, sshKey2.ID, tnu.ID)
	assert.NotNil(t, ska5)

	// Build SSHKeyGroup3
	skg3 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-3", tnOrg, cutil.GetPtr("test3"), tn.ID, cutil.GetPtr("122746"), cdbm.SSHKeyGroupStatusDeleting, tnu.ID)
	assert.NotNil(t, skg3)

	// Build SSHKeyGroupSiteAssociation3
	skgsa3 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg3.ID, st2.ID, cutil.GetPtr("116735"), cdbm.SSHKeyGroupSiteAssociationStatusDeleting, tnu.ID)
	assert.NotNil(t, skgsa3)

	// Build SSHKeyGroup4
	skg4 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-4", tnOrg, cutil.GetPtr("test4"), tn.ID, cutil.GetPtr("112233"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg4)

	// Build SSHKeyGroup5
	skg5 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-5", tnOrg, cutil.GetPtr("test5"), tn.ID, cutil.GetPtr("112233"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg5)

	// Build  SSH key:group associations for SSHKeyGroup4
	assert.NotNil(t, util.TestBuildSSHKeyAssociation(t, dbSession, skg4.ID, sshKey3.ID, tnu.ID))
	assert.NotNil(t, util.TestBuildSSHKeyAssociation(t, dbSession, skg4.ID, sshKey1.ID, tnu.ID))
	assert.NotNil(t, util.TestBuildSSHKeyAssociation(t, dbSession, skg4.ID, sshKey2.ID, tnu.ID))

	// Build SSHKeyGroupSiteAssociation4 - Marked as missing on site.
	skgsa4 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg4.ID, st2.ID, cutil.GetPtr("445566"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa4)

	_ = util.TestBuildStatusDetail(t, dbSession, skgsa4.ID.String(), cdbm.SSHKeyGroupSiteAssociationStatusSynced, cutil.GetPtr(MsgSSHKeyGroupSynced))

	// Pretend something went wrong and the site reported a sshkeygroup list that didn't include skgsa4/SSHKeyGroupSiteAssociation4
	_, err := dbSession.DB.Exec("UPDATE ssh_key_group_site_association SET is_missing_on_site = ? WHERE id = ?", true, skgsa4.ID.String())
	assert.NoError(t, err)

	// Build SSHKeyGroupSiteAssociation5 - it is exists on site, but error mode in Cloud
	skgsa5 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg5.ID, st2.ID, cutil.GetPtr("445569"), cdbm.SSHKeyGroupSiteAssociationStatusError, tnu.ID)
	assert.NotNil(t, skgsa5)

	_ = util.TestBuildStatusDetail(t, dbSession, skgsa5.ID.String(), cdbm.SSHKeyGroupSiteAssociationStatusError, cutil.GetPtr("rpc error: code = Internal desc = Database Error: error returned from database: duplicate key value violates unique constraint \"tenant_keysets_pkey\" file=api/src/db/tenant.rs line=152 query=INSERT INTO tenant_keysets VALUES($1, $2, $3, $4) RETURNING *."))

	// Build SSHKeyGroup6 for duplicate key retry test
	skg6 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-6", tnOrg, cutil.GetPtr("test6"), tn.ID, cutil.GetPtr("122347"), cdbm.SSHKeyGroupStatusSyncing, tnu.ID)
	assert.NotNil(t, skg6)

	// Build SSHKeyGroupSiteAssociation6 - starts in Syncing, but has duplicate key error status detail
	// This simulates the state after first create attempt failed with duplicate key
	skgsa6 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg6.ID, st1.ID, cutil.GetPtr("1139"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa6)

	// Add status detail with duplicate key error - this makes IsSSHKeyGroupCreatedOnSite return true on retry
	// The error message contains ErrMsgSiteControllerDuplicateEntryFound so IsSSHKeyGroupCreatedOnSite returns true
	duplicateKeyErrorMsg := fmt.Sprintf("SSHKeyGroup already exists on Site: rpc error: code = Internal desc = Database Error: error returned from database: %s \"tenant_keysets_pkey\"", util.ErrMsgSiteControllerDuplicateEntryFound)
	_ = util.TestBuildStatusDetail(t, dbSession, skgsa6.ID.String(), cdbm.SSHKeyGroupSiteAssociationStatusError, cutil.GetPtr(duplicateKeyErrorMsg))

	// Build SSH Key associations for skg6
	ska6 := util.TestBuildSSHKeyAssociation(t, dbSession, skg6.ID, sshKey1.ID, tnu.ID)
	assert.NotNil(t, ska6)
	ska7 := util.TestBuildSSHKeyAssociation(t, dbSession, skg6.ID, sshKey2.ID, tnu.ID)
	assert.NotNil(t, ska7)

	// Build SSHKeyGroup7 for initial duplicate key failure test
	skg7 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-7", tnOrg, cutil.GetPtr("test7"), tn.ID, cutil.GetPtr("122348"), cdbm.SSHKeyGroupStatusSyncing, tnu.ID)
	assert.NotNil(t, skg7)

	// Build SSHKeyGroupSiteAssociation7 - starts in Syncing, no status detail (simulates first create attempt)
	skgsa7 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg7.ID, st1.ID, cutil.GetPtr("1140"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa7)

	// Build SSH Key associations for skg7
	ska8 := util.TestBuildSSHKeyAssociation(t, dbSession, skg7.ID, sshKey1.ID, tnu.ID)
	assert.NotNil(t, ska8)
	ska9 := util.TestBuildSSHKeyAssociation(t, dbSession, skg7.ID, sshKey2.ID, tnu.ID)
	assert.NotNil(t, ska9)

	// Build  SSH key:group associations for SSHKeyGroup4
	assert.NotNil(t, util.TestBuildSSHKeyAssociation(t, dbSession, skg5.ID, sshKey3.ID, tnu.ID))
	assert.NotNil(t, util.TestBuildSSHKeyAssociation(t, dbSession, skg5.ID, sshKey1.ID, tnu.ID))
	assert.NotNil(t, util.TestBuildSSHKeyAssociation(t, dbSession, skg5.ID, sshKey2.ID, tnu.ID))

	tSiteClientPool := util.TestTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	tests := []struct {
		name    string
		fields  fields
		args    args
		want    error
		wantErr bool
	}{
		{
			name: "test SSHKeyGroup creation sync via site agent",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           st1.ID,
				skgID:            skg1.ID,
				skgsaID:          skgsa1.ID,
				requestedVersion: skgsa1.Version,
				createRequest: &corev1.CreateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skg1.ID.String(),
						OrganizationId: skg1.Org,
					},
					KeysetContent: &corev1.TenantKeysetContent{
						PublicKeys: []*corev1.TenantPublicKey{
							{
								PublicKey: sshKey1.PublicKey,
								Comment:   sshKey1.Fingerprint,
							},
							{
								PublicKey: sshKey2.PublicKey,
								Comment:   sshKey2.Fingerprint,
							},
						},
					},
					Version: *skgsa1.Version,
				},
			},
			want: nil,
		},
		{
			name: "test SSHKeyGroup update sync via site agent",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           st1.ID,
				skgID:            skg2.ID,
				skgsaID:          skgsa2.ID,
				requestedVersion: skgsa2.Version,
				updateRequest: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skg2.ID.String(),
						OrganizationId: skg2.Org,
					},
					KeysetContent: &corev1.TenantKeysetContent{
						PublicKeys: []*corev1.TenantPublicKey{
							{
								PublicKey: sshKey3.PublicKey,
								Comment:   sshKey3.Fingerprint,
							},
							{
								PublicKey: sshKey1.PublicKey,
								Comment:   sshKey1.Fingerprint,
							},
							{
								PublicKey: sshKey2.PublicKey,
								Comment:   sshKey2.Fingerprint,
							},
						},
					},
					Version: *skgsa2.Version,
				},
			},
			want: nil,
		},
		{
			name: "test SSHKeyGroup sync in case of deleting via site agent, skip sync",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                   context.Background(),
				siteID:                st2.ID,
				skgID:                 skg3.ID,
				skgsaID:               skgsa3.ID,
				requestedVersion:      skgsa3.Version,
				IsSSHKeyGroupDeleting: cutil.GetPtr(true),

				updateRequest: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skg3.ID.String(),
						OrganizationId: skg3.Org,
					},
					KeysetContent: &corev1.TenantKeysetContent{
						PublicKeys: []*corev1.TenantPublicKey{
							{
								PublicKey: sshKey3.PublicKey,
								Comment:   sshKey3.Fingerprint,
							},
							{
								PublicKey: sshKey1.PublicKey,
								Comment:   sshKey1.Fingerprint,
							},
							{
								PublicKey: sshKey2.PublicKey,
								Comment:   sshKey2.Fingerprint,
							},
						},
					},
					Version: *skgsa3.Version,
				},
			},
			want: nil,
		},
		{
			name: "test SSHKeyGroup sync in case of group missing on site when update requested",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           st2.ID,
				skgID:            skg4.ID,
				skgsaID:          skgsa4.ID,
				requestedVersion: skgsa4.Version,
				createRequest: &corev1.CreateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skg4.ID.String(),
						OrganizationId: skg4.Org,
					},
					KeysetContent: &corev1.TenantKeysetContent{
						PublicKeys: []*corev1.TenantPublicKey{
							{
								PublicKey: sshKey3.PublicKey,
								Comment:   sshKey3.Fingerprint,
							},
							{
								PublicKey: sshKey1.PublicKey,
								Comment:   sshKey1.Fingerprint,
							},
							{
								PublicKey: sshKey2.PublicKey,
								Comment:   sshKey2.Fingerprint,
							},
						},
					},
					Version: *skgsa4.Version,
				},
			},
			want: nil,
		},
		{
			name: "test SSHKeyGroup sync error due to non-existent SSH Key Group ID",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           st1.ID,
				skgID:            uuid.New(),
				skgsaID:          uuid.New(),
				requestedVersion: cutil.GetPtr("1234"),
			},
			wantErr: true,
		},
		{
			name: "test SSHKeyGroup sync in case of group exists on site and error mode in cloud, update requested",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           st2.ID,
				skgID:            skg5.ID,
				skgsaID:          skgsa5.ID,
				requestedVersion: skgsa5.Version,
				updateRequest: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skg5.ID.String(),
						OrganizationId: skg5.Org,
					},
					KeysetContent: &corev1.TenantKeysetContent{
						PublicKeys: []*corev1.TenantPublicKey{
							{
								PublicKey: sshKey3.PublicKey,
								Comment:   sshKey3.Fingerprint,
							},
							{
								PublicKey: sshKey1.PublicKey,
								Comment:   sshKey1.Fingerprint,
							},
							{
								PublicKey: sshKey2.PublicKey,
								Comment:   sshKey2.Fingerprint,
							},
						},
					},
					Version: *skgsa5.Version,
				},
			},
			want: nil,
		},
		{
			name: "test SSHKeyGroup create fails with duplicate key, retry calls update and succeeds",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           st1.ID,
				skgID:            skg6.ID,
				skgsaID:          skgsa6.ID,
				requestedVersion: skgsa6.Version,
				// This test simulates the retry scenario after duplicate key error
				// The skgsa has an Error status detail with duplicate key message,
				// so IsSSHKeyGroupCreatedOnSite returns true, triggering update path
				updateRequest: &corev1.UpdateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skg6.ID.String(),
						OrganizationId: skg6.Org,
					},
					KeysetContent: &corev1.TenantKeysetContent{
						PublicKeys: []*corev1.TenantPublicKey{
							{
								PublicKey: sshKey1.PublicKey,
								Comment:   sshKey1.Fingerprint,
							},
							{
								PublicKey: sshKey2.PublicKey,
								Comment:   sshKey2.Fingerprint,
							},
						},
					},
					Version: *skgsa6.Version,
				},
			},
			want: nil,
		},
		{
			name: "test SSHKeyGroup create fails with duplicate key error on initial attempt",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                                context.Background(),
				siteID:                             st1.ID,
				skgID:                              skg7.ID,
				skgsaID:                            skgsa7.ID,
				requestedVersion:                   skgsa7.Version,
				IsSSHKeyGroupFailedOnInitialCreate: cutil.GetPtr(true),
				// This test simulates the initial create attempt that fails with duplicate key
				createRequest: &corev1.CreateTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skg7.ID.String(),
						OrganizationId: skg7.Org,
					},
					KeysetContent: &corev1.TenantKeysetContent{
						PublicKeys: []*corev1.TenantPublicKey{
							{
								PublicKey: sshKey1.PublicKey,
								Comment:   sshKey1.Fingerprint,
							},
							{
								PublicKey: sshKey2.PublicKey,
								Comment:   sshKey2.Fingerprint,
							},
						},
					},
					Version: *skgsa7.Version,
				},
			},
			wantErr: true, // Expect error to be returned for duplicate key
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageSSHKeyGroup{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			mtc := &tmocks.Client{}
			mv.siteClientPool.IDClientMap[tt.args.siteID.String()] = mtc

			testWorkflowID := "test-workflowid"
			testRunID := "test-runid"

			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return(testWorkflowID)
			mockWorkflowRun.On("GetRunID").Return(testRunID)

			// Handle duplicate key error case for initial create failure
			if tt.args.IsSSHKeyGroupFailedOnInitialCreate != nil && *tt.args.IsSSHKeyGroupFailedOnInitialCreate {
				duplicateKeyError := fmt.Errorf("rpc error: code = Internal desc = Database Error: error returned from database: %s \"tenant_keysets_pkey\"", util.ErrMsgSiteControllerDuplicateEntryFound)
				mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(duplicateKeyError).Once()
				mtc.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(opts client.StartWorkflowOptions) bool {
					return opts.ID == "site-ssh-key-group-create-"+tt.args.skgID.String()+"-"+*tt.args.requestedVersion
				}), "CreateSSHKeyGroupV2", tt.args.createRequest).Return(mockWorkflowRun, nil).Once()
			} else {
				// Both create and update workflows now wait for completion synchronously
				mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(nil).Once()
				mockWorkflowRun.On("GetWithOptions", mock.Anything, mock.Anything).Return(nil).Once()

				if tt.args.createRequest != nil {
					mtc.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(opts client.StartWorkflowOptions) bool {
						return opts.ID == "site-ssh-key-group-create-"+tt.args.skgID.String()+"-"+*tt.args.requestedVersion
					}), "CreateSSHKeyGroupV2", tt.args.createRequest).Return(mockWorkflowRun, nil).Once()
				}

				if tt.args.updateRequest != nil {
					mtc.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(opts client.StartWorkflowOptions) bool {
						return opts.ID == "site-ssh-key-group-update-"+tt.args.skgID.String()+"-"+*tt.args.requestedVersion
					}), "UpdateSSHKeyGroupV2", tt.args.updateRequest).Return(mockWorkflowRun, nil).Once()
				}
			}

			err := mv.SyncSSHKeyGroupViaSiteAgent(tt.args.ctx, tt.args.siteID, tt.args.skgID, *tt.args.requestedVersion)
			assert.Equal(t, tt.wantErr, err != nil)

			// Verify duplicate key error handling
			if tt.args.IsSSHKeyGroupFailedOnInitialCreate != nil && *tt.args.IsSSHKeyGroupFailedOnInitialCreate {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), "duplicate key constraint")
				assert.Contains(t, err.Error(), "workflow will retry")

				// Verify that error status detail was created
				skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dbSession)
				tvskgsa, err := skgsaDAO.GetByID(context.Background(), nil, tt.args.skgsaID, nil)
				assert.Nil(t, err)
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusError, tvskgsa.Status)

				statusDetailDAO := cdbm.NewStatusDetailDAO(mv.dbSession)
				tvskgsast, _, err := statusDetailDAO.GetAll(context.Background(), nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{tt.args.skgsaID.String()}}, cdbp.PageInput{})
				assert.Nil(t, err)
				assert.NotEqual(t, len(tvskgsast), 0)
				// Verify the error message contains duplicate key constraint
				assert.Contains(t, *tvskgsast[0].Message, util.ErrMsgSiteControllerDuplicateEntryFound)
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusError, tvskgsast[0].Status)
			}

			if !tt.wantErr {
				// Check if the SSHKeyGroup was updated in the DB
				skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dbSession)
				tvskgsa, err := skgsaDAO.GetByID(context.Background(), nil, tt.args.skgsaID, nil)
				assert.Nil(t, err)

				if tt.args.IsSSHKeyGroupDeleting == nil {
					// For successful workflows (both create and update), status should be Synced
					assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusSynced, tvskgsa.Status)
				} else {
					assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusDeleting, tvskgsa.Status)
				}

				if tt.args.IsSSHKeyGroupDeleting == nil {
					statusDetailDAO := cdbm.NewStatusDetailDAO(mv.dbSession)
					tvskgsast, _, err := statusDetailDAO.GetAll(context.Background(), nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{tt.args.skgsaID.String()}}, cdbp.PageInput{})
					assert.Nil(t, err)
					assert.NotEqual(t, len(tvskgsast), 0)
					// For successful workflows (both create and update), message should be Synced
					assert.Equal(t, *tvskgsast[0].Message, MsgSSHKeyGroupSynced)
				}
			}
		})
	}
}

func TestManageSSHKeyGroup_UpdateSSHKeyGroupStatusInDB(t *testing.T) {
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, tnRoles)

	tn := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)
	assert.NotNil(t, tn)

	st1 := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st1)

	st2 := util.TestBuildSite(t, dbSession, ip, "test-site-2", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st2)

	st3 := util.TestBuildSite(t, dbSession, ip, "test-site-3", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st3)

	st4 := util.TestBuildSite(t, dbSession, ip, "test-site-4", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st4)

	st5 := util.TestBuildSite(t, dbSession, ip, "test-site-5", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st5)

	// Build SSHKeyGroup1
	skg1 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test1"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation1
	skgsa1 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skgsa1)

	// Build SSHKeyGroup2
	skg2 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-2", tnOrg, cutil.GetPtr("test2"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg2)

	// Build SSHKeyGroupSiteAssociation2
	skgsa2 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg2.ID, st2.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusError, tnu.ID)
	assert.NotNil(t, skgsa2)

	// Build SSHKeyGroup3
	skg3 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-3", tnOrg, cutil.GetPtr("test3"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg3)

	// Build SSHKeyGroupSiteAssociation3
	skgsa3 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg3.ID, st3.ID, cutil.GetPtr("1135"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa3)

	// Build SSHKeyGroup4
	skg4 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-4", tnOrg, cutil.GetPtr("test4"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusDeleting, tnu.ID)
	assert.NotNil(t, skg4)

	// Build SSH Key for Group 4
	sshKey1 := util.TestBuildSSHKey(t, dbSession, "test1", tn, "test1", tnu)
	assert.NotNil(t, sshKey1)

	// Build SSHKeyAssociation for Group 4
	ska4 := util.TestBuildSSHKeyAssociation(t, dbSession, skg4.ID, sshKey1.ID, tnu.ID)
	assert.NotNil(t, ska4)

	// Build Instance components
	vpc := util.TestBuildVpc(t, dbSession, ip, st1, tn, "test-vpc")
	mc := util.TestBuildMachine(t, dbSession, ip.ID, st1.ID, nil, cutil.GetPtr(true), cdbm.MachineStatusReady)
	al := util.TestBuildAllocation(t, dbSession, ip, tn, st1, "test-allocation")
	it := util.TestBuildInstanceType(t, dbSession, ip, st1, "test-instance-type")
	_ = util.TestBuildAllocationContraints(t, dbSession, al, cdbm.AllocationResourceTypeInstanceType, it.ID, cdbm.AllocationConstraintTypeReserved, 5, ipu)
	os := util.TestBuildOperatingSystem(t, dbSession, "test-os")
	instance := util.TestBuildInstance(t, dbSession, "test-instance", tn.ID, ip.ID, st1.ID, it.ID, vpc.ID, &mc.ID, os.ID, cdbm.InstanceStatusReady)

	// Build SSHKeyGroupInstanceAssociation for Group 4
	skgia1 := util.TestBuildSSHKeyGroupInstanceAssociation(t, dbSession, skg1.ID, st1.ID, instance.ID, tnu.ID)
	assert.NotNil(t, skgia1)

	// Build SSHKeyGroup5
	skg5 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-5", tnOrg, cutil.GetPtr("test5"), tn.ID, cutil.GetPtr("122569"), cdbm.SSHKeyGroupStatusSyncing, tnu.ID)
	assert.NotNil(t, skg5)

	// Build SSHKeyGroupSiteAssociation5
	skgsa5 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg5.ID, st1.ID, cutil.GetPtr("113085"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa5)

	// Build SSHKeyGroup6
	skg6 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-6", tnOrg, cutil.GetPtr("test6"), tn.ID, cutil.GetPtr("12234578"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skg6)

	tSiteClientPool := util.TestTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	type fields struct {
		dbSession      *cdb.Session
		env            *testsuite.TestWorkflowEnvironment
		siteClientPool *sc.ClientPool
	}

	type args struct {
		ctx                      context.Context
		skg                      *cdbm.SSHKeyGroup
		site                     *cdbm.Site
		expectSSHKeyGroupDelete  bool
		expectSSHKeyGroupError   bool
		expectSSHKeyGroupSynced  bool
		expectSSHKeyGroupSyncing bool
	}

	tests := []struct {
		name   string
		fields fields
		args   args
	}{
		{
			name: "test SSH Key Group status expect synced",
			fields: fields{
				dbSession:      dbSession,
				env:            env,
				siteClientPool: tSiteClientPool,
			},
			args: args{
				ctx:                     context.Background(),
				skg:                     skg1,
				expectSSHKeyGroupSynced: true,
				site:                    st1,
			},
		},
		{
			name: "test SSH Key Group status expect synced in case no association",
			fields: fields{
				dbSession:      dbSession,
				env:            env,
				siteClientPool: tSiteClientPool,
			},
			args: args{
				ctx:                     context.Background(),
				skg:                     skg6,
				expectSSHKeyGroupSynced: true,
				site:                    st1,
			},
		},
		{
			name: "test SSH Key Group status expect error",
			fields: fields{
				dbSession:      dbSession,
				env:            env,
				siteClientPool: tSiteClientPool,
			},
			args: args{
				ctx:                    context.Background(),
				skg:                    skg2,
				expectSSHKeyGroupError: true,
				site:                   st2,
			},
		},
		{
			name: "test SSH Key Group status expect syncing",
			fields: fields{
				dbSession:      dbSession,
				env:            env,
				siteClientPool: tSiteClientPool,
			},
			args: args{
				ctx:                      context.Background(),
				skg:                      skg3,
				expectSSHKeyGroupSyncing: true,
				site:                     st3,
			},
		},
		{
			name: "test SSH Key Group expect delete",
			fields: fields{
				dbSession:      dbSession,
				env:            env,
				siteClientPool: tSiteClientPool,
			},
			args: args{
				ctx:                     context.Background(),
				skg:                     skg4,
				expectSSHKeyGroupDelete: true,
				site:                    st4,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mskg := ManageSSHKeyGroup{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			mtc := &tmocks.Client{}
			mskg.siteClientPool.IDClientMap[tt.args.site.ID.String()] = mtc

			err := mskg.UpdateSSHKeyGroupStatusInDB(tt.args.ctx, tt.args.skg.ID.String())
			assert.NoError(t, err)

			skgDAO := cdbm.NewSSHKeyGroupDAO(dbSession)
			uskg, err := skgDAO.GetByID(context.Background(), nil, tt.args.skg.ID, nil)

			// Verify statuses
			if tt.args.expectSSHKeyGroupSynced {
				assert.Nil(t, err)
				assert.Equal(t, cdbm.SSHKeyGroupStatusSynced, uskg.Status)
			}
			if tt.args.expectSSHKeyGroupDelete {
				assert.Error(t, cdb.ErrDoesNotExist)
				assert.Nil(t, uskg)

				// Verify all SSHKeyAssociations are deleted
				skaDAO := cdbm.NewSSHKeyAssociationDAO(dbSession)
				_, skasCnt, err := skaDAO.GetAll(context.Background(), nil, cdbm.SSHKeyAssociationFilterInput{
					SSHKeyGroupIDs: []uuid.UUID{tt.args.skg.ID},
				}, cdbp.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 0, skasCnt)

				// Verify SSHKeyGroupInstanceAssociations are deleted
				skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(dbSession)
				_, skgiasCnt, err := skgiaDAO.GetAll(context.Background(), nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
					SSHKeyGroupIDs: []uuid.UUID{tt.args.skg.ID},
				}, cdbp.PageInput{}, nil)
				assert.Nil(t, err)
				assert.Equal(t, 0, skgiasCnt)
			}
			if tt.args.expectSSHKeyGroupError {
				assert.Nil(t, err)
				assert.Equal(t, cdbm.SSHKeyGroupStatusError, uskg.Status)
			}
			if tt.args.expectSSHKeyGroupSyncing {
				assert.Nil(t, err)
				assert.Equal(t, cdbm.SSHKeyGroupStatusSyncing, uskg.Status)
			}
		})
	}
}

func TestManageSSHKeyGroup_UpdateSSHKeyGroupsInDB(t *testing.T) {
	ctx := context.Background()

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, tnRoles)

	tn := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)

	st := util.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	st2 := util.TestBuildSite(t, dbSession, ip, "test-site2", cdbm.SiteStatusRegistered, nil, ipu)

	// Build SSHKeyGroup1
	skg1 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test1"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSyncing, tnu.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation1
	skgsa1 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa1)

	// Build SSHKeyGroup2
	skg2 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-2", tnOrg, cutil.GetPtr("test2"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSyncing, tnu.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation2
	skgsa2 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg2.ID, st.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skgsa2)

	// Build SSHKeyGroup3
	skg3 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-3", tnOrg, cutil.GetPtr("test3"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg3)

	// Build SSHKeyGroupSiteAssociation3
	skgsa3 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg3.ID, st.ID, cutil.GetPtr("1135"), cdbm.SSHKeyGroupSiteAssociationStatusDeleting, tnu.ID)
	assert.NotNil(t, skgsa3)

	// Build SSHKeyGroup4
	skg4 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-4", tnOrg, cutil.GetPtr("test4"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg4)

	// Build SSHKeyGroupSiteAssociation4
	skgsa4 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg4.ID, st.ID, cutil.GetPtr("1135"), cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa4)

	// Build SSHKeyGroup5
	skg5 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-5", tnOrg, cutil.GetPtr("test5"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg5)

	// Build SSHKeyGroupSiteAssociation5
	skgsa5 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg5.ID, st.ID, cutil.GetPtr("1136"), cdbm.SSHKeyGroupSiteAssociationStatusError, tnu.ID)
	assert.NotNil(t, skgsa5)

	// Build SSHKeyGroup6
	skg6 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-6", tnOrg, cutil.GetPtr("test6"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg6)

	// Build SSHKeyGroupSiteAssociation6
	skgsa6 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg6.ID, st.ID, cutil.GetPtr("1137"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skgsa6)
	// Set created earlier than the inventory receipt interval
	_, err := dbSession.DB.Exec("UPDATE ssh_key_group_site_association SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), skgsa6.ID.String())
	assert.NoError(t, err)

	// Build SSHKeyGroup7
	skg7 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-7", tnOrg, cutil.GetPtr("test7"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg7)

	// Build SSHKeyGroupSiteAssociation7
	skgsa7 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg7.ID, st.ID, cutil.GetPtr("1138"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
	assert.NotNil(t, skgsa7)

	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dbSession)
	skgsa7, err = skgsaDAO.Update(ctx, nil, cdbm.SSHKeyGroupSiteAssociationUpdateInput{
		ID:              skgsa7.ID,
		Status:          cutil.GetPtr(cdbm.SSHKeyGroupSiteAssociationStatusError),
		IsMissingOnSite: cutil.GetPtr(true),
	})
	assert.NoError(t, err)

	// Build SSHKeyGroup8
	skg8 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-8", tnOrg, cutil.GetPtr("test7"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusDeleting, tnu.ID)
	assert.NotNil(t, skg8)

	// Build SSHKeyGroupSiteAssociation8
	skgsa8 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg8.ID, st.ID, cutil.GetPtr("1138"), cdbm.SSHKeyGroupSiteAssociationStatusDeleting, tnu.ID)
	assert.NotNil(t, skgsa8)

	// Build Subnet inventory that is paginated
	// Generate data for 34 Subnets reported from Site Agent while Cloud has 38 Subnets
	pagedSSHKeyGroups := []*cdbm.SSHKeyGroup{}
	pagedInvSSHKeyGroupIDs := []string{}
	for i := 0; i < 38; i++ {
		keyGroup := util.TestBuildSSHKeyGroup(t, dbSession, fmt.Sprintf("test-sshkeygroup-paged-%d", i), tnOrg, cutil.GetPtr("description"), tn.ID, cutil.GetPtr("122346"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
		// Update creation timestamp to be earlier than inventory processing interval
		_, err = dbSession.DB.Exec("UPDATE sshkey_group SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), keyGroup.ID.String())
		assert.NoError(t, err)
		pagedSSHKeyGroups = append(pagedSSHKeyGroups, keyGroup)
		pagedInvSSHKeyGroupIDs = append(pagedInvSSHKeyGroupIDs, keyGroup.ID.String())

		skgsa := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, keyGroup.ID, st2.ID, cutil.GetPtr("1138"), cdbm.SSHKeyGroupSiteAssociationStatusSynced, tnu.ID)
		assert.NotNil(t, skgsa)
		_, err := dbSession.DB.Exec("UPDATE ssh_key_group_site_association SET created = ? WHERE id = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2), skgsa.ID.String())
		assert.NoError(t, err)
	}

	pagedCtrlSSHKeyGroups := []*corev1.TenantKeyset{}
	for i := 0; i < 34; i++ {
		ctrlIns := &corev1.TenantKeyset{
			KeysetIdentifier: &corev1.TenantKeysetIdentifier{
				OrganizationId: pagedSSHKeyGroups[i].Org,
				KeysetId:       pagedSSHKeyGroups[i].ID.String(),
			},
			Version: *pagedSSHKeyGroups[i].Version,
		}

		pagedCtrlSSHKeyGroups = append(pagedCtrlSSHKeyGroups, ctrlIns)
	}

	tSiteClientPool := util.TestTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                  context.Context
		siteID               uuid.UUID
		sshKeyGroupInventory *corev1.SSHKeyGroupInventory
	}

	tests := []struct {
		name                string
		fields              fields
		args                args
		syncingKeyset       *cdbm.SSHKeyGroupSiteAssociation
		outOfSyncKeyset     *cdbm.SSHKeyGroupSiteAssociation
		deletingKeyset      *cdbm.SSHKeyGroupSiteAssociation
		errorKeyset         *cdbm.SSHKeyGroupSiteAssociation
		missingKeyset       *cdbm.SSHKeyGroupSiteAssociation
		restoredKeyset      *cdbm.SSHKeyGroupSiteAssociation
		deletedKeyset       *cdbm.SSHKeyGroupSiteAssociation
		wantErr             bool
		expectedAssocChange int
	}{
		{
			name: "test SSHKeyGroupinventory processing error, non-existent Site",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: uuid.New(),
				sshKeyGroupInventory: &corev1.SSHKeyGroupInventory{
					TenantKeysets: []*corev1.TenantKeyset{
						{
							KeysetIdentifier: &corev1.TenantKeysetIdentifier{
								KeysetId: "1234",
							},
							Version: "1234",
						},
						{
							KeysetIdentifier: &corev1.TenantKeysetIdentifier{
								KeysetId: "1235",
							},
							Version: "1235",
						},
					},
				},
			},
			wantErr: true,
		},
		{
			name: "test SSHKeyGroupinventory processing success",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st.ID,
				sshKeyGroupInventory: &corev1.SSHKeyGroupInventory{
					TenantKeysets: []*corev1.TenantKeyset{
						{
							KeysetIdentifier: &corev1.TenantKeysetIdentifier{
								KeysetId: skgsa1.SSHKeyGroupID.String(),
							},
							Version: "1234",
						},
						{
							KeysetIdentifier: &corev1.TenantKeysetIdentifier{
								KeysetId: skgsa2.SSHKeyGroupID.String(),
							},
							Version: "1234",
						},
						{
							KeysetIdentifier: &corev1.TenantKeysetIdentifier{
								KeysetId: skgsa3.SSHKeyGroupID.String(),
							},
							Version: "1135",
						},
						{
							KeysetIdentifier: &corev1.TenantKeysetIdentifier{
								KeysetId: skgsa5.SSHKeyGroupID.String(),
							},
							Version: "1136",
						},
						{
							KeysetIdentifier: &corev1.TenantKeysetIdentifier{
								KeysetId: skgsa7.SSHKeyGroupID.String(),
							},
							Version: "1138",
						},
					},
				},
			},
			syncingKeyset:       skgsa1,
			outOfSyncKeyset:     skgsa2, // State change
			deletingKeyset:      skgsa3,
			errorKeyset:         skgsa5, // State change
			missingKeyset:       skgsa6, // State change
			restoredKeyset:      skgsa7,
			deletedKeyset:       skgsa8,
			wantErr:             false,
			expectedAssocChange: 3,
		},
		{
			name: "test paged SSHKeyGroup inventory processing",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:    ctx,
				siteID: st2.ID,
				sshKeyGroupInventory: &corev1.SSHKeyGroupInventory{
					TenantKeysets: pagedCtrlSSHKeyGroups[0:10],
					Timestamp:     timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvSSHKeyGroupIDs[0:34],
					},
				},
			},
			expectedAssocChange: 4,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageSSHKeyGroup{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			// Mock the "UpdateSSHKeyGroupInventory" workflow
			mtc := &tmocks.Client{}
			mv.siteClientPool.IDClientMap[tt.args.siteID.String()] = mtc

			testWorkflowID := "test-workflowid"
			testRunID := "test-runid"

			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return(testWorkflowID)
			mockWorkflowRun.On("GetRunID").Return(testRunID)
			mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(nil)
			mockWorkflowRun.On("GetWithOptions", mock.Anything, mock.Anything).Return(nil)

			mtc.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(mockWorkflowRun, nil)

			skgStrIDs, err := mv.UpdateSSHKeyGroupsInDB(tt.args.ctx, tt.args.siteID, tt.args.sshKeyGroupInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			// 3 of the associations had state change from inventory processing
			assert.Equal(t, tt.expectedAssocChange, len(skgStrIDs))

			sshKeyGroupDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dbSession)
			// Check that SSHKeyGroupstatus was updated in DB
			// SyncSSHKeyGroupViaSiteAgent is synchronous: it waits for the workflow to complete.
			// The mock workflow completes immediately with success, so status becomes Synced.
			if tt.syncingKeyset != nil {
				updatedKeyset, _ := sshKeyGroupDAO.GetByID(ctx, nil, tt.syncingKeyset.ID, nil)
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusSynced, updatedKeyset.Status)
			}

			if tt.outOfSyncKeyset != nil {
				outOfSyncKeyset, _ := sshKeyGroupDAO.GetByID(ctx, nil, tt.outOfSyncKeyset.ID, nil)
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusSynced, outOfSyncKeyset.Status)
			}

			if tt.deletingKeyset != nil {
				deletingKeyset, _ := sshKeyGroupDAO.GetByID(ctx, nil, tt.deletingKeyset.ID, nil)
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusDeleting, deletingKeyset.Status)
			}

			if tt.errorKeyset != nil {
				errorKeyset, _ := sshKeyGroupDAO.GetByID(ctx, nil, tt.errorKeyset.ID, nil)
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusSynced, errorKeyset.Status)
			}

			if tt.missingKeyset != nil {
				missingKeyset, _ := sshKeyGroupDAO.GetByID(ctx, nil, tt.missingKeyset.ID, nil)
				assert.True(t, missingKeyset.IsMissingOnSite)
				// SyncSSHKeyGroupViaSiteAgent is synchronous; mock completes successfully → Synced
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusSynced, missingKeyset.Status)
			}

			if tt.restoredKeyset != nil {
				restoredKeyset, _ := sshKeyGroupDAO.GetByID(ctx, nil, tt.restoredKeyset.ID, nil)
				assert.False(t, restoredKeyset.IsMissingOnSite)
				assert.Equal(t, cdbm.SSHKeyGroupSiteAssociationStatusSynced, restoredKeyset.Status)
			}

			if tt.deletedKeyset != nil {
				_, err = sshKeyGroupDAO.GetByID(ctx, nil, tt.deletedKeyset.ID, nil)
				assert.Error(t, cdb.ErrDoesNotExist)
				// Parent SSHKeyGroup should be deleted as it was in deleting state
				skgDAO := cdbm.NewSSHKeyGroupDAO(dbSession)
				_, err = skgDAO.GetByID(ctx, nil, tt.deletedKeyset.SSHKeyGroupID, nil)
				assert.Error(t, cdb.ErrDoesNotExist)
			}
		})
	}
}

func TestManageSSHKeyGroup_DeleteSSHKeyGroupViaSiteAgent(t *testing.T) {
	type fields struct {
		dbSession      *cdb.Session
		siteClientPool *sc.ClientPool
		env            *testsuite.TestWorkflowEnvironment
	}

	type args struct {
		ctx                        context.Context
		siteID                     uuid.UUID
		sshKeyGroupSiteAssociation *cdbm.SSHKeyGroupSiteAssociation
		deleteRequest              *corev1.DeleteTenantKeysetRequest
	}

	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, tnRoles)

	tn := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)
	assert.NotNil(t, tn)

	st1 := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, st1)

	// Build SSHKeyGroup1
	skg1 := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup-1", tnOrg, cutil.GetPtr("test1"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSynced, tnu.ID)
	assert.NotNil(t, skg1)

	// Build SSHKeyGroupSiteAssociation1
	skgsa1 := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg1.ID, st1.ID, cutil.GetPtr("1134"), cdbm.SSHKeyGroupSiteAssociationStatusDeleting, tnu.ID)
	assert.NotNil(t, skgsa1)

	sshKey1 := util.TestBuildSSHKey(t, dbSession, "test1", tn, "test1", tnu)
	assert.NotNil(t, sshKey1)

	sshKey2 := util.TestBuildSSHKey(t, dbSession, "test2", tn, "test2", tnu)
	assert.NotNil(t, sshKey2)

	tSiteClientPool := util.TestTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	temporalsuit := testsuite.WorkflowTestSuite{}
	env := temporalsuit.NewTestWorkflowEnvironment()

	tests := []struct {
		name           string
		fields         fields
		args           args
		want           error
		wantErr        bool
		expectDeletion bool
	}{
		{
			name: "test SSHKeyGroupSiteAssociation delete activity from site agent successfully",
			fields: fields{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
				env:            env,
			},
			args: args{
				ctx:                        context.Background(),
				siteID:                     st1.ID,
				sshKeyGroupSiteAssociation: skgsa1,
				deleteRequest: &corev1.DeleteTenantKeysetRequest{
					KeysetIdentifier: &corev1.TenantKeysetIdentifier{
						KeysetId:       skgsa1.SSHKeyGroupID.String(),
						OrganizationId: skg1.Org,
					},
				},
			},
			want: nil,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mv := ManageSSHKeyGroup{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			mtc := &tmocks.Client{}
			mv.siteClientPool.IDClientMap[tt.args.siteID.String()] = mtc

			testWorkflowID := "test-workflowid"
			testRunID := "test-runid"

			mockWorkflowRun := &tmocks.WorkflowRun{}
			mockWorkflowRun.On("GetID").Return(testWorkflowID).Times(4)
			mockWorkflowRun.On("GetRunID").Return(testRunID).Times(4)
			mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(nil).Times(2)
			mockWorkflowRun.On("GetWithOptions", mock.Anything, mock.Anything).Return(nil).Times(2)

			mtc.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, tt.args.deleteRequest).Return(mockWorkflowRun, nil).Once()

			err := mv.DeleteSSHKeyGroupViaSiteAgent(tt.args.ctx, tt.args.siteID, tt.args.sshKeyGroupSiteAssociation.SSHKeyGroupID)
			if (err != nil) != tt.wantErr {
				t.Errorf("ManageVpc.DeleteSSHKeyGroupViaSiteAgent() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			// Check if the SSHKeyGroupSiteAssociation was updated or deleted in the DB, we will check for `deleting`
			skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(dbSession)
			dskgsa, err := skgsaDAO.GetByID(context.Background(), nil, tt.args.sshKeyGroupSiteAssociation.ID, nil)
			if tt.expectDeletion {
				assert.Equal(t, cdb.ErrDoesNotExist, err)
			} else {
				assert.Nil(t, err)
				assert.Equal(t, dskgsa.Status, cdbm.SSHKeyGroupSiteAssociationStatusDeleting)
			}
		})
	}
}

func TestNewManageSSHKeyGroup(t *testing.T) {
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
		want ManageSSHKeyGroup
	}{
		{
			name: "test new ManageSSHKeyGroup instantiation",
			args: args{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
			want: ManageSSHKeyGroup{
				dbSession:      dbSession,
				siteClientPool: scp,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageSSHKeyGroup(tt.args.dbSession, tt.args.siteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageSSHKeyGroup() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestSSHKeyAssociationNoPaginator(t *testing.T) {
	// Check that paginator doesn't affect synchronization of SSH keys
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	tnOrg := "test-tenant-org"
	tnRoles := []string{"FORGE_TENANT_ADMIN"}

	tnu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{tnOrg}, tnRoles)
	assert.NotNil(t, tnu)

	tn := util.TestBuildTenant(t, dbSession, "Test Tenant", tnOrg, nil, tnu)
	assert.NotNil(t, tn)

	site := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, site)

	// Build SSHKeyGroup
	skg := util.TestBuildSSHKeyGroup(t, dbSession, "test-sshkeygroup", tnOrg, cutil.GetPtr("test1"), tn.ID, cutil.GetPtr("122345"), cdbm.SSHKeyGroupStatusSyncing, tnu.ID)
	assert.NotNil(t, skg)

	// Build number of ssh keys paginator + 1.
	expectedKeysNumber := cdbp.DefaultLimit + 1
	expectedPublicKeys := []*corev1.TenantPublicKey{}
	for i := 0; i < expectedKeysNumber; i++ {
		name := fmt.Sprintf("test%v", i)
		sshKey := util.TestBuildSSHKey(t, dbSession, name, tn, name, tnu)
		assert.NotNil(t, sshKey)
		expectedPublicKeys = append(expectedPublicKeys, &corev1.TenantPublicKey{
			PublicKey: name,
			Comment:   &name,
		})
		ska := util.TestBuildSSHKeyAssociation(t, dbSession, skg.ID, sshKey.ID, tnu.ID)
		assert.NotNil(t, ska)
	}

	skgsaVersion := cutil.GetPtr("122345")
	skgsa := util.TestBuildSSHKeyGroupSiteAssociation(t, dbSession, skg.ID, site.ID, skgsaVersion, cdbm.SSHKeyGroupSiteAssociationStatusSyncing, tnu.ID)
	assert.NotNil(t, skgsa)

	tSiteClientPool := util.TestTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	mv := ManageSSHKeyGroup{
		dbSession:      dbSession,
		siteClientPool: tSiteClientPool,
	}

	mtc := &tmocks.Client{}
	mv.siteClientPool.IDClientMap[site.ID.String()] = mtc

	testWorkflowID := "test-workflowid"
	testRunID := "test-runid"

	mockWorkflowRun := &tmocks.WorkflowRun{}
	mockWorkflowRun.On("GetID").Return(testWorkflowID).Times(4)
	mockWorkflowRun.On("GetRunID").Return(testRunID).Times(4)
	mockWorkflowRun.On("Get", mock.Anything, mock.Anything).Return(nil).Times(2)
	mockWorkflowRun.On("GetWithOptions", mock.Anything, mock.Anything).Return(nil).Times(2)
	mtc.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).
		Run(func(args mock.Arguments) {
			switch req := args.Get(3).(type) {
			case *corev1.CreateTenantKeysetRequest:
				assert.Len(t, req.KeysetContent.PublicKeys, expectedKeysNumber)
			default:
				t.Fatalf("unexpected workflow request type %T", req)
			}
		}).Return(mockWorkflowRun, nil)

	err := mv.SyncSSHKeyGroupViaSiteAgent(context.Background(), site.ID, skg.ID, *skgsaVersion)
	assert.NoError(t, err)
}
