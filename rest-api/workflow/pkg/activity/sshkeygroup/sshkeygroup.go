// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strings"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/client"
	"golang.org/x/crypto/ssh"
	"google.golang.org/protobuf/encoding/protojson"

	temporalEnums "go.temporal.io/api/enums/v1"

	tsdk "go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

const (
	MsgSSHKeyGroupSynced          = "SSHKeyGroup successfully synced to Site"
	MsgSSHKeyGroupCreateInitiated = "initiated SSHKeyGroup syncing for create via Site Agent"
	MsgSSHKeyGroupUpdateInitiated = "initiated SSHKeyGroup syncing for update via Site Agent"
	msgSSHKeyGroupRecovered       = "SSH Key Group recovered from Site inventory"
	msgSSHKeyGroupSiteRecovered   = "SSH Key Group Site association recovered from Site inventory"
)

// ManageSSHKeyGroup is an activity wrapper for managing SSHKeyGroup lifecycle for a Site and allows
// injecting DB access
type ManageSSHKeyGroup struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// SyncSSHKeyGroupViaSiteAgent is a Temporal activity that create/update SSHKeyGroup in Site Controller via Site agent
func (mskg ManageSSHKeyGroup) SyncSSHKeyGroupViaSiteAgent(ctx context.Context, siteID uuid.UUID, sshKeyGroupID uuid.UUID, version string) error {
	logger := log.With().Str("Activity", "SyncSSHKeyGroupViaSiteAgent").Str("SSH Key Group ID", sshKeyGroupID.String()).
		Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(mskg.dbSession)
	skgsa, err := skgsaDAO.GetBySSHKeyGroupIDAndSiteID(ctx, nil, sshKeyGroupID, siteID, []string{cdbm.SSHKeyGroupRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve SSH Key Group Site Association from DB by SSH Key Group ID & Site ID")
		return err
	}

	logger.Info().Msg("retrieved SSH Key Group Site Association from DB")

	// Return nil if SSHKeyGroup is being deleting
	if skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
		logger.Warn().Msgf("SSHKeyGroup %s is in deleting state, cannot sync to the Site %s", sshKeyGroupID, siteID)
		return nil
	}

	// Verify if provided version matches with current one
	// if not someone updated
	if *skgsa.Version != version {
		message := "provided version and current SSH Key Group Site Association version doesn't match"
		logger.Error().Msg(message)
		return errors.New(message)
	}

	// Determine if SSHKeyGroup created at site or not.
	// IsMissingOnSite takes precedence.
	// If multiple `Create` attempts are made after the group exists on the site,
	// subsequent ones will simply fail.  Things will settle eventually as
	// updates from site-manager make it back to us.
	// We need to catch the case where status history says "created" but the site says
	// "not created."
	isSSHKeyGroupCreated := cwutil.GetPtr(false)
	if !skgsa.IsMissingOnSite {
		isSSHKeyGroupCreated, err = mskg.IsSSHKeyGroupCreatedOnSite(ctx, nil, skgsa.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to determine if SSHKeyGroup has already been created on Site")
			return err
		}
	}

	// Get the temporal client for the site we are working with.
	stc, err := mskg.siteClientPool.GetClientByID(siteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return err
	}

	// Sync SSHKeyGroup request
	keysetContent := &corev1.TenantKeysetContent{}

	tx, terr := cdb.BeginTx(ctx, mskg.dbSession, &sql.TxOptions{})
	if terr != nil {
		logger.Error().Err(terr).Msg("failed to start DB transaction")
		return terr
	}

	// acquire an advisory lock on the SSH Key Group on which there could be contention
	// this lock is released when the transaction commits or rolls back
	err = tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(skgsa.SSHKeyGroupID.String()), false)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to acquire advisory lock on SSH Key Group")
		terr = tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}

	// Get public keys associated to this SSHKeyGroup
	skaDAO := cdbm.NewSSHKeyAssociationDAO(mskg.dbSession)
	skas, total, err := skaDAO.GetAll(ctx, nil, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skgsa.SSHKeyGroupID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.SSHKeyRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve SSH Key Associations from DB by SSHKeyGroup ID")
		return err
	}
	if total > 0 {
		for _, ska := range skas {
			keysetContent.PublicKeys = append(keysetContent.PublicKeys, &corev1.TenantPublicKey{
				PublicKey: ska.SSHKey.PublicKey,
				Comment:   ska.SSHKey.Fingerprint,
			})
		}
	}

	// Commit tx
	terr = tx.Commit()
	if terr != nil {
		logger.Error().Err(terr).Msg("failed to commit DB transaction")
		return terr
	}

	workflowOptions := client.StartWorkflowOptions{
		ID:                    "site-ssh-key-group-create-" + sshKeyGroupID.String() + "-" + *skgsa.Version,
		TaskQueue:             queue.SiteTaskQueue,
		WorkflowIDReusePolicy: temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
	}

	status := cdbm.SSHKeyGroupSiteAssociationStatusSyncing
	statusMessage := MsgSSHKeyGroupCreateInitiated

	// Execute the site workflow to create/update the SSH Key Group in synchronous
	// Add context deadlines
	ctx, cancel := context.WithTimeout(ctx, cwutil.WorkflowContextTimeout)
	defer cancel()

	var we client.WorkflowRun
	workflowMethod := "create"

	if !*isSSHKeyGroupCreated {
		// Set the workflow ID and KeysetIdentifier for the create request
		workflowOptions.ID = "site-ssh-key-group-create-" + sshKeyGroupID.String() + "-" + *skgsa.Version

		createSSHKeyGroupRequest := skgsa.ToCreateRequestProto(keysetContent)

		// Trigger Site create SSHKeyGroup workflow
		we, err = stc.ExecuteWorkflow(ctx, workflowOptions, "CreateSSHKeyGroupV2", createSSHKeyGroupRequest)
	} else {
		workflowMethod = "update"
		// Set the workflow ID and KeysetIdentifier for the update request
		workflowOptions.ID = "site-ssh-key-group-update-" + sshKeyGroupID.String() + "-" + *skgsa.Version

		updateSSHKeyGroupRequest := skgsa.ToUpdateRequestProto(keysetContent)

		// Trigger Site update SSHKeyGroup workflow
		we, err = stc.ExecuteWorkflow(ctx, workflowOptions, "UpdateSSHKeyGroupV2", updateSSHKeyGroupRequest)
	}

	if err != nil {
		status = cdbm.SSHKeyGroupSiteAssociationStatusError
		statusMessage = fmt.Sprintf("failed to initiate workflow to %s SSHKeyGroup via Site Agent", workflowMethod)

		// Log the error and the status message
		logger.Error().Err(err).Msg(statusMessage)
	} else {
		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msgf("executed synchronous %s SSHKeyGroup workflow", workflowMethod)

		// Block until the workflow has completed and returned success/error.
		err = we.Get(ctx, nil)
		if err != nil {
			var timeoutErr *tsdk.TimeoutError
			// Check for timeout errors
			if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded {
				logger.Error().Err(err).Msgf("failed to %s SSHKeyGroup, timeout occurred executing workflow on Site.", workflowMethod)

				// Create a new context deadlines
				newctx, newcancel := context.WithTimeout(context.Background(), cwutil.WorkflowContextNewAfterTimeout)
				defer newcancel()

				// Initiate termination workflow
				serr := stc.TerminateWorkflow(newctx, wid, "", fmt.Sprintf("timeout occurred executing %s SSHKeyGroup workflow", workflowMethod))
				if serr != nil {
					logger.Error().Err(serr).Msgf("failed to execute terminate Temporal workflow for %s SSHKeyGroup", workflowMethod)
				}
				logger.Info().Str("Workflow ID", wid).Msgf("initiated terminate synchronous %s SSHKeyGroup workflow successfully", workflowMethod)

				status = cdbm.SSHKeyGroupSiteAssociationStatusError
				statusMessage = fmt.Sprintf("failed to %s SSHKeyGroup, timeout occurred executing workflow on Site.", workflowMethod)
				err = nil // Clear error so function returns nil after updating status
			} else if isAlreadyExistsOnSite(err) {
				// Handle duplicate key error - record error and fail workflow for retry
				// On retry, IsSSHKeyGroupCreatedOnSite will return true because the Error status detail
				// contains ErrMsgSiteControllerDuplicateEntryFound, causing the update path to be taken
				logger.Warn().Err(err).Msg("SSHKeyGroup already exists on Site (duplicate key constraint), recording error and failing workflow for retry")

				status = cdbm.SSHKeyGroupSiteAssociationStatusError
				statusMessage = fmt.Sprintf("SSH Key Group already exists on Site: %s", util.ErrMsgSiteControllerDuplicateEntryFound)

				_ = mskg.updateSSHKeyGroupSiteAssociationStatusInDB(ctx, nil, skgsa.ID, &status, &statusMessage)

				return fmt.Errorf("%s SSHKeyGroup failed due to duplicate key constraint, workflow will retry: %w", workflowMethod, err)
			} else {
				// Other errors
				status = cdbm.SSHKeyGroupSiteAssociationStatusError
				statusMessage = fmt.Sprintf("failed to execute %s SSHKeyGroup workflow via Site Agent", workflowMethod)
			}
		} else {
			status = cdbm.SSHKeyGroupSiteAssociationStatusSynced
			statusMessage = MsgSSHKeyGroupSynced
		}
	}

	// Log status detail regardless of success or failure
	_ = mskg.updateSSHKeyGroupSiteAssociationStatusInDB(ctx, nil, skgsa.ID, &status, &statusMessage)

	// If workflow wasn't successful, return error to retry workflow
	if err != nil {
		logger.Error().Err(err).Msg("failed to trigger site agent SyncSSHKeyGroup workflow")
		return err
	}

	// Create/update was successful
	if we != nil {
		logger.Info().Str("Workflow ID", we.GetID()).Msgf("successfully executed %s SSHKeyGroup workflow on Site", workflowMethod)
	}

	logger.Info().Msg("completed activity")

	return nil
}

// UpdateSSHKeyGroupsInDB takes information pushed by Site Agent for a collection of SSH Key Groups associated with the Site and updates the DB
func (mskg ManageSSHKeyGroup) UpdateSSHKeyGroupsInDB(ctx context.Context, siteID uuid.UUID, sshKeyGroupInventory *corev1.SSHKeyGroupInventory) ([]string, error) {
	logger := log.With().Str("Activity", "UpdateSSHKeyGroupsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mskg.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received SSH Key Group inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return nil, err
	}

	if sshKeyGroupInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil, nil
	}

	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(mskg.dbSession)

	skgsas, _, err := skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{SiteID: &site.ID}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get SSH Key Group Site Associations for Site from DB")
		return nil, err
	}

	// Construct a map ID of SSHKeyGroups to SSHKeyGroup
	skgIDSkgsaMap := make(map[string]*cdbm.SSHKeyGroupSiteAssociation)
	for _, skgsa := range skgsas {
		foundskgsa := skgsa
		skgIDSkgsaMap[skgsa.SSHKeyGroupID.String()] = &foundskgsa
	}

	reportedSSHKeyGroupIDMap := map[string]bool{}

	if sshKeyGroupInventory.InventoryPage != nil {
		logger.Info().Msgf("Received SSHKeyGroup inventory page: %d of %d, page size: %d, total count: %d",
			sshKeyGroupInventory.InventoryPage.CurrentPage, sshKeyGroupInventory.InventoryPage.TotalPages,
			sshKeyGroupInventory.InventoryPage.PageSize, sshKeyGroupInventory.InventoryPage.TotalItems)

		for _, strId := range sshKeyGroupInventory.InventoryPage.ItemIds {
			reportedSSHKeyGroupIDMap[strId] = true
		}
	}

	// Absence only means anything once every reported ID has arrived. Acting on a page that
	// carries no list would read every SSH Key Group it does not hold as missing from the Site.
	reconcileDeletions := util.ShouldReconcileDeletions(sshKeyGroupInventory.GetInventoryPage())

	existingSkgsaTkMap := map[string]*corev1.TenantKeyset{}
	updatedSkgMap := map[string]bool{}

	var identifiersToFetch []*corev1.TenantKeysetIdentifier
	recoverableWithoutKeys := make(map[string]bool)
	for _, tenantKeyset := range sshKeyGroupInventory.TenantKeysets {
		identifier := tenantKeyset.GetKeysetIdentifier()
		if tenantKeyset == nil || identifier == nil {
			continue
		}
		sshKeyGroupIDStr := identifier.GetKeysetId()
		_, hasExistingAssociation := skgIDSkgsaMap[sshKeyGroupIDStr]
		if hasExistingAssociation || len(tenantKeyset.GetKeysetContent().GetPublicKeys()) > 0 {
			continue
		}
		key := getTenantKeysetKey(identifier)
		if recoverableWithoutKeys[key] {
			continue
		}
		slogger := logger.With().Str("SSH Key Group ID", sshKeyGroupIDStr).Str("Tenant Org", identifier.GetOrganizationId()).Logger()
		_, _, siteAssociation, recoverable, stateErr := mskg.getSSHKeyGroupRecoveryState(ctx, nil, slogger, site, tenantKeyset)
		if stateErr != nil {
			slogger.Error().Err(stateErr).Msg("failed to check SSH Key Group recovery from Site inventory")
			continue
		}
		if !recoverable {
			if siteAssociation != nil {
				skgIDSkgsaMap[sshKeyGroupIDStr] = siteAssociation
			}
			continue
		}
		recoverableWithoutKeys[key] = true
		identifiersToFetch = append(identifiersToFetch, identifier)
	}

	var detailedKeysets map[string]*corev1.TenantKeyset
	var detailErr error
	if len(identifiersToFetch) > 0 {
		detailedKeysets, detailErr = mskg.getTenantKeysetDetailsFromSite(ctx, site.ID, identifiersToFetch)
		if detailErr != nil {
			logger.Error().Err(detailErr).Int("SSH Key Group Count", len(identifiersToFetch)).
				Msg("failed to retrieve SSH Key Group details from Site")
		}
	}

	// Iterate through SSHKeyGroup Inventory and update DB
	for _, tenantKeyset := range sshKeyGroupInventory.TenantKeysets {
		if tenantKeyset == nil {
			logger.Error().Msg("nil TenantKeyset entry sent from Site Controller, skipping")
			continue
		}

		if tenantKeyset.KeysetIdentifier == nil {
			logger.Error().Msg("nil KeysetIdentifier entry sent from Site Controller, skipping")
			continue
		}

		sshKeyGroupIDStr := tenantKeyset.KeysetIdentifier.KeysetId
		slogger := logger.With().Str("SSH Key Group ID", sshKeyGroupIDStr).Logger()

		key := getTenantKeysetKey(tenantKeyset.KeysetIdentifier)
		detailedKeyset, hasDetailedKeyset := detailedKeysets[key]
		if hasDetailedKeyset {
			tenantKeyset = detailedKeyset
		}

		skgsa, ok := skgIDSkgsaMap[sshKeyGroupIDStr]
		if !ok {
			if len(tenantKeyset.GetKeysetContent().GetPublicKeys()) == 0 {
				if detailErr != nil || !recoverableWithoutKeys[key] {
					continue
				}
				if !hasDetailedKeyset {
					slogger.Warn().Msg("Site did not return details for recoverable SSH Key Group")
					continue
				}
			}
			skgsa, err = mskg.createOrUpdateSSHKeyGroupFromSite(ctx, site, tenantKeyset)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to recover SSH Key Group from Site inventory")
				continue
			}
			if skgsa == nil {
				continue
			}
			skgIDSkgsaMap[sshKeyGroupIDStr] = skgsa
			updatedSkgMap[sshKeyGroupIDStr] = true
			slogger.Info().Str("SSH Key Group Site Association ID", skgsa.ID.String()).
				Msg("created or undeleted SSH Key Group from Site inventory")
		}

		existingSkgsaTkMap[sshKeyGroupIDStr] = tenantKeyset

		reportedSSHKeyGroupIDMap[skgsa.SSHKeyGroupID.String()] = true

		if !skgsa.IsMissingOnSite {
			continue
		}

		// Update SSHKeyGroupSiteAssociation missing flag as it is now found on Site
		_, serr := skgsaDAO.Update(ctx, nil, cdbm.SSHKeyGroupSiteAssociationUpdateInput{
			ID:              skgsa.ID,
			IsMissingOnSite: cwutil.GetPtr(false),
		})
		if serr != nil {
			slogger.Error().Err(serr).Msg("failed to update SSH Key Group Site Association missing flag in DB")
			continue
		}
	}

	// Process all SSH Key Group Site Associations in DB
	for skgID, skgsa := range skgIDSkgsaMap {
		slogger := logger.With().Str("SSH Key Group Site Association ID", skgsa.ID.String()).Logger()

		syncRequired := false

		tenantKeyset, ok := existingSkgsaTkMap[skgID]
		if !ok {
			if reconcileDeletions && !reportedSSHKeyGroupIDMap[skgID] {
				// SSH Key Group was not found on Site
				if skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
					// If the SSHKeyGroupSiteAssociation was being deleted, we can proceed with removing it from the DB
					serr := skgsaDAO.Delete(ctx, nil, skgsa.ID)
					if serr != nil {
						slogger.Error().Err(serr).Msg("failed to delete SSH Key Group Site Association from DB")
						continue
					}
					// Trigger re-evaluation of SSH Key Group status
					err := mskg.UpdateSSHKeyGroupStatusInDB(ctx, skgID)
					if err != nil {
						slogger.Error().Err(err).Msg("failed to trigger SSH Key Group status update in DB")
					}
				} else {
					if skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusSynced {
						// Was this created within inventory receipt interval? If so, we may be processing an older inventory
						if site.IsTimeWithinStaleInventoryThreshold(skgsa.Created) {
							continue
						}

						// Set isMissingOnSite flag to true and update status, user can decide on deletion
						_, serr := skgsaDAO.Update(ctx, nil, cdbm.SSHKeyGroupSiteAssociationUpdateInput{ID: skgsa.ID, IsMissingOnSite: cwutil.GetPtr(true)})
						if serr != nil {
							slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB for SSH Key Group Site Association")
							continue
						}

						serr = mskg.updateSSHKeyGroupSiteAssociationStatusInDB(ctx, nil, skgsa.ID, cwutil.GetPtr(cdbm.SSHKeyGroupSiteAssociationStatusError), cwutil.GetPtr("SSHKeyGroup is missing on Site"))
						if serr != nil {
							slogger.Error().Err(serr).Msg("failed to update SSH Key Group Site Association status detail in DB")
						}

						updatedSkgMap[skgID] = true
					}

					// SSH Key Group is either missing or has been created on Site yet
					syncRequired = true
				}
			}
		} else if tenantKeyset.Version != *skgsa.Version {
			// If there's a version mismatch between Cloud and what is on Site, then sync is required
			syncRequired = true
		} else if tenantKeyset.Version == *skgsa.Version && (skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusSyncing || skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusError) {
			// If we reached this condition then SSH Key Group was found on Site and the versions match
			// but the status is not synced, so we need to sync it
			serr := mskg.updateSSHKeyGroupSiteAssociationStatusInDB(ctx, nil, skgsa.ID, cwutil.GetPtr(cdbm.SSHKeyGroupSiteAssociationStatusSynced), cwutil.GetPtr("SSH Key Group has successfully been synced with Site"))
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update SSH Key Group status detail in DB")
			}

			updatedSkgMap[skgID] = true
		}

		if syncRequired {
			// Sync is required
			if skgsa.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
				serr := mskg.DeleteSSHKeyGroupViaSiteAgent(ctx, siteID, skgsa.SSHKeyGroupID)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to trigger SSH Key Group deletion via Site Agent")
				}
			} else {
				serr := mskg.SyncSSHKeyGroupViaSiteAgent(ctx, siteID, skgsa.SSHKeyGroupID, *skgsa.Version)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to trigger SSH Key Group sync via Site Agent")
				}
			}
		}
	}

	updatedSkgStrIDs := []string{}
	for skgID := range updatedSkgMap {
		updatedSkgStrIDs = append(updatedSkgStrIDs, skgID)
	}

	return updatedSkgStrIDs, nil
}

// getTenantKeysetDetailsFromSite retrieves keysets with their public keys from Core
// when metadata-only inventory identifies keysets eligible for recovery.
func (mskg ManageSSHKeyGroup) getTenantKeysetDetailsFromSite(
	ctx context.Context,
	siteID uuid.UUID,
	identifiers []*corev1.TenantKeysetIdentifier,
) (map[string]*corev1.TenantKeyset, error) {
	detailedKeysets := make(map[string]*corev1.TenantKeyset, len(identifiers))
	if len(identifiers) == 0 {
		return detailedKeysets, nil
	}

	stc, err := mskg.siteClientPool.GetClientByID(siteID)
	if err != nil {
		return nil, fmt.Errorf("failed to retrieve Temporal client for Site: %w", err)
	}

	requestJSON, err := protojson.Marshal(&corev1.TenantKeysetsByIdsRequest{
		KeysetIds:      identifiers,
		IncludeKeyData: true,
	})
	if err != nil {
		return nil, fmt.Errorf("failed to encode Tenant keyset request: %w", err)
	}

	workflowContext, cancel := context.WithTimeout(ctx, cwutil.WorkflowContextTimeout)
	defer cancel()

	run, err := stc.ExecuteWorkflow(
		workflowContext,
		client.StartWorkflowOptions{
			ID:                       "site-ssh-key-group-get-" + uuid.NewString(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: grpcproxy.WorkflowExecutionTimeout,
			WorkflowIDReusePolicy:    temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
		},
		grpcproxy.Core.WorkflowName,
		grpcproxy.Request{
			FullMethod:  corev1.Forge_FindTenantKeysetsByIds_FullMethodName,
			RequestJSON: requestJSON,
		},
	)
	if err != nil {
		return nil, fmt.Errorf("failed to execute Core proxy workflow: %w", err)
	}

	var proxyResponse grpcproxy.Response
	err = run.Get(workflowContext, &proxyResponse)
	if err != nil {
		return nil, fmt.Errorf("failed to retrieve Tenant keyset details: %w", err)
	}

	var response corev1.TenantKeySetList
	err = (protojson.UnmarshalOptions{DiscardUnknown: true}).Unmarshal(proxyResponse.ResponseJSON, &response)
	if err != nil {
		return nil, fmt.Errorf("failed to decode Tenant keyset response: %w", err)
	}

	for _, detailedKeyset := range response.GetKeyset() {
		detailedIdentifier := detailedKeyset.GetKeysetIdentifier()
		if detailedKeyset == nil || detailedIdentifier == nil || detailedKeyset.GetVersion() == "" {
			log.Warn().Str("Site ID", siteID.String()).Msg("skipping incomplete Tenant keyset entry returned by Core")
			continue
		}
		detailedKeysets[getTenantKeysetKey(detailedIdentifier)] = detailedKeyset
	}

	return detailedKeysets, nil
}

// getSSHKeyGroupRecoveryState resolves the Tenant, Group and Site association state
// for reported and reports whether recovery may proceed. When tx is non-nil, it
// acquires the Tenant and Group recovery locks before reading Group state. When
// recoverable is false, a non-nil association means both the Group and Site
// association are already active.
func (mskg ManageSSHKeyGroup) getSSHKeyGroupRecoveryState(
	ctx context.Context,
	tx *cdb.Tx,
	logger zerolog.Logger,
	site *cdbm.Site,
	reported *corev1.TenantKeyset,
) (*cdbm.Tenant, *cdbm.SSHKeyGroup, *cdbm.SSHKeyGroupSiteAssociation, bool, error) {
	identifier := reported.GetKeysetIdentifier()
	if reported == nil || identifier == nil || identifier.GetKeysetId() == "" || identifier.GetOrganizationId() == "" || reported.GetVersion() == "" {
		logger.Warn().Msg("unable to recover SSH Key Group from Site: missing keyset identifier, Tenant org or version")
		return nil, nil, nil, false, nil
	}
	sshKeyGroupID, err := uuid.Parse(identifier.GetKeysetId())
	if err != nil || sshKeyGroupID.String() != identifier.GetKeysetId() {
		logger.Warn().Err(err).Msg("unable to recover SSH Key Group from Site: keyset ID is not a canonical UUID")
		return nil, nil, nil, false, nil
	}

	tenants, _, err := cdbm.NewTenantDAO(mskg.dbSession).GetAll(
		ctx, tx,
		cdbm.TenantFilterInput{Orgs: []string{identifier.GetOrganizationId()}},
		cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil,
	)
	if err != nil {
		return nil, nil, nil, false, fmt.Errorf("unable to recover SSH Key Group: failed to retrieve Tenant, DB error: %w", err)
	}
	if len(tenants) != 1 {
		logger.Warn().Int("Tenant Count", len(tenants)).Msg("unable to recover SSH Key Group: organization does not resolve to exactly one Tenant")
		return nil, nil, nil, false, nil
	}
	tenant := &tenants[0]

	if tx != nil {
		lockErr := tx.TryAcquireAdvisoryLock(ctx, cdbm.SSHKeyTenantLockID(tenant.ID), nil)
		if lockErr != nil {
			return nil, nil, nil, false, fmt.Errorf("unable to recover SSH Key Group: failed to acquire Tenant recovery lock, DB error: %w", lockErr)
		}
		lockErr = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(sshKeyGroupID.String()), nil)
		if lockErr != nil {
			return nil, nil, nil, false, fmt.Errorf("unable to recover SSH Key Group: failed to acquire Group recovery lock, DB error: %w", lockErr)
		}
	}

	groupMatches, _, err := cdbm.NewSSHKeyGroupDAO(mskg.dbSession).GetAll(
		ctx, tx,
		cdbm.SSHKeyGroupFilterInput{SSHKeyGroupIDs: []uuid.UUID{sshKeyGroupID}, IncludeDeleted: true},
		cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil,
	)
	if err != nil {
		return nil, nil, nil, false, fmt.Errorf("unable to recover SSH Key Group: failed to retrieve Group, DB error: %w", err)
	}

	var existingGroup *cdbm.SSHKeyGroup
	if len(groupMatches) > 0 {
		existingGroup = &groupMatches[0]
		if existingGroup.TenantID != tenant.ID || existingGroup.Org != tenant.Org {
			logger.Warn().Msg("unable to recover SSH Key Group: Group ownership differs from Site inventory")
			return nil, nil, nil, false, nil
		}
		if existingGroup.Deleted == nil && existingGroup.Status == cdbm.SSHKeyGroupStatusDeleting {
			logger.Info().Msg("skipping recovery of deleting SSH Key Group")
			return nil, nil, nil, false, nil
		}
		if existingGroup.Deleted != nil && site.IsTimeWithinStaleInventoryThreshold(*existingGroup.Deleted) {
			logger.Info().Msg("not undeleting SSH Key Group because it was deleted within the inventory staleness window")
			return nil, nil, nil, false, nil
		}
	}

	siteAssociations, _, err := cdbm.NewSSHKeyGroupSiteAssociationDAO(mskg.dbSession).GetAll(
		ctx, tx,
		cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{sshKeyGroupID}, SiteID: &site.ID, IncludeDeleted: true},
		cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil,
	)
	if err != nil {
		return nil, nil, nil, false, fmt.Errorf("unable to recover SSH Key Group: failed to retrieve Site association, DB error: %w", err)
	}

	var activeAssociation, deletedAssociation *cdbm.SSHKeyGroupSiteAssociation
	for i := range siteAssociations {
		association := &siteAssociations[i]
		if association.Deleted == nil {
			if activeAssociation != nil {
				logger.Warn().Msg("unable to recover SSH Key Group: multiple active Site associations were found")
				return nil, nil, nil, false, nil
			}
			activeAssociation = association
		} else if deletedAssociation == nil || association.Deleted.After(*deletedAssociation.Deleted) {
			deletedAssociation = association
		}
	}

	if activeAssociation != nil {
		if activeAssociation.Status == cdbm.SSHKeyGroupSiteAssociationStatusDeleting {
			logger.Info().Msg("skipping recovery of deleting SSH Key Group Site association")
			return nil, nil, nil, false, nil
		}
		if existingGroup != nil && existingGroup.Deleted == nil {
			return tenant, existingGroup, activeAssociation, false, nil
		}
		return tenant, existingGroup, activeAssociation, true, nil
	}
	if deletedAssociation != nil && site.IsTimeWithinStaleInventoryThreshold(*deletedAssociation.Deleted) {
		logger.Info().Msg("not undeleting SSH Key Group Site association because it was deleted within the inventory staleness window")
		return nil, nil, nil, false, nil
	}

	return tenant, existingGroup, deletedAssociation, true, nil
}

// createOrUpdateSSHKeyGroupFromSite recovers a Site keyset that has no active
// Site association in REST. Missing or stale-deleted Groups adopt the Site key
// data; an active Group is associated only when its existing keys match.
func (mskg ManageSSHKeyGroup) createOrUpdateSSHKeyGroupFromSite(
	ctx context.Context,
	site *cdbm.Site,
	reported *corev1.TenantKeyset,
) (*cdbm.SSHKeyGroupSiteAssociation, error) {
	logger := log.With().Str("Activity", "UpdateSSHKeyGroupsInDB").Str("Site ID", site.ID.String()).Logger()
	identifier := reported.GetKeysetIdentifier()
	if identifier != nil {
		logger = logger.With().Str("SSH Key Group ID", identifier.GetKeysetId()).Str("Tenant Org", identifier.GetOrganizationId()).Logger()
	}

	content := reported.GetKeysetContent()
	if content == nil {
		logger.Warn().Msg("unable to recover SSH Key Group from Site: keyset content is missing")
		return nil, nil
	}

	reportedKeys := make(map[string]string, len(content.PublicKeys))
	for i, reportedKey := range content.PublicKeys {
		if reportedKey == nil {
			logger.Warn().Int("Public Key Index", i).Msg("unable to recover SSH Key Group from Site: public key entry is nil")
			return nil, nil
		}

		publicKeyText := strings.TrimSpace(reportedKey.GetPublicKey())
		publicKey, _, options, rest, err := ssh.ParseAuthorizedKey([]byte(publicKeyText))
		if err != nil || len(options) > 0 || len(strings.TrimSpace(string(rest))) > 0 {
			logger.Warn().Err(err).Int("Public Key Index", i).Msg("unable to recover SSH Key Group from Site: public key is invalid")
			return nil, nil
		}

		switch publicKey.Type() {
		case ssh.KeyAlgoRSA, ssh.KeyAlgoECDSA256, ssh.KeyAlgoECDSA384, ssh.KeyAlgoECDSA521, ssh.KeyAlgoED25519:
		default:
			logger.Warn().Int("Public Key Index", i).Str("Key Type", publicKey.Type()).Msg("unable to recover SSH Key Group from Site: unsupported public key type")
			return nil, nil
		}

		fingerprint := strings.TrimPrefix(ssh.FingerprintSHA256(publicKey), "SHA256:")
		_, duplicate := reportedKeys[fingerprint]
		if duplicate {
			logger.Warn().Str("SSH Key Fingerprint", fingerprint).Msg("unable to recover SSH Key Group from Site: duplicate public key fingerprint")
			return nil, nil
		}
		reportedKeys[fingerprint] = publicKeyText
	}

	return cdb.WithTxResult(ctx, mskg.dbSession, func(tx *cdb.Tx) (*cdbm.SSHKeyGroupSiteAssociation, error) {
		tenant, existingGroup, siteAssociation, recoverable, txErr := mskg.getSSHKeyGroupRecoveryState(ctx, tx, logger, site, reported)
		if txErr != nil || !recoverable {
			return siteAssociation, txErr
		}
		sshKeyGroupID := uuid.MustParse(reported.GetKeysetIdentifier().GetKeysetId())

		sshKeyGroupDAO := cdbm.NewSSHKeyGroupDAO(mskg.dbSession)
		siteAssociationDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(mskg.dbSession)
		keyAssociationDAO := cdbm.NewSSHKeyAssociationDAO(mskg.dbSession)
		groupIsActive := existingGroup != nil && existingGroup.Deleted == nil
		recoveryVersion := reported.GetVersion()

		// Verify active key associations match Site key fingerprints when an existing
		// Group record is present (active or stale-deleted with stray active links).
		existingActiveKeyIDs := make(map[uuid.UUID]bool)
		if existingGroup != nil {
			activeAssociations, _, associationErr := keyAssociationDAO.GetAll(
				ctx, tx,
				cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{sshKeyGroupID}},
				cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
				[]string{cdbm.SSHKeyRelationName},
			)
			if associationErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to retrieve SSH Key associations, DB error: %w", associationErr)
			}
			if groupIsActive && len(activeAssociations) != len(reportedKeys) {
				logger.Warn().Msg("unable to recover SSH Key Group Site association: REST and Site key contents differ")
				return nil, nil
			}
			for i := range activeAssociations {
				if activeAssociations[i].SSHKey == nil || activeAssociations[i].SSHKey.Fingerprint == nil {
					logger.Warn().Msg("unable to recover SSH Key Group: active REST and Site key contents differ")
					return nil, nil
				}
				_, keyReported := reportedKeys[*activeAssociations[i].SSHKey.Fingerprint]
				if !keyReported {
					logger.Warn().Msg("unable to recover SSH Key Group: active REST and Site key contents differ")
					return nil, nil
				}
				existingActiveKeyIDs[activeAssociations[i].SSHKeyID] = true
			}
		}

		recoveredGroup := existingGroup
		groupChanged := false

		if !groupIsActive {
			sshKeyDAO := cdbm.NewSSHKeyDAO(mskg.dbSession)
			tenantKeys, _, keyErr := sshKeyDAO.GetAll(
				ctx, tx,
				cdbm.SSHKeyFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, IncludeDeleted: true},
				cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil,
			)
			if keyErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to retrieve Tenant SSH Keys, DB error: %w", keyErr)
			}

			usedKeyNames := make(map[string]bool, len(tenantKeys))
			activeKeyByFingerprint := make(map[string]*cdbm.SSHKey)
			deletedKeyByFingerprint := make(map[string]*cdbm.SSHKey)
			duplicateKeyFingerprints := make(map[string]bool)
			for i := range tenantKeys {
				key := &tenantKeys[i]
				if key.Deleted == nil {
					usedKeyNames[key.Name] = true
				}
				if key.Fingerprint == nil {
					continue
				}
				fingerprint := *key.Fingerprint
				if key.Deleted == nil {
					_, exists := activeKeyByFingerprint[fingerprint]
					if exists {
						duplicateKeyFingerprints[fingerprint] = true
					} else {
						activeKeyByFingerprint[fingerprint] = key
					}
				} else {
					previousKey := deletedKeyByFingerprint[fingerprint]
					if previousKey == nil || key.Deleted.After(*previousKey.Deleted) {
						deletedKeyByFingerprint[fingerprint] = key
					}
				}
			}

			sortedFingerprints := slices.Sorted(maps.Keys(reportedKeys))
			for _, fingerprint := range sortedFingerprints {
				if duplicateKeyFingerprints[fingerprint] {
					logger.Warn().Str("SSH Key Fingerprint", fingerprint).Msg("unable to recover SSH Key Group: multiple active SSH Keys have the same fingerprint")
					return nil, nil
				}
				if activeKeyByFingerprint[fingerprint] == nil && deletedKeyByFingerprint[fingerprint] != nil && site.IsTimeWithinStaleInventoryThreshold(*deletedKeyByFingerprint[fingerprint].Deleted) {
					logger.Info().Str("SSH Key ID", deletedKeyByFingerprint[fingerprint].ID.String()).
						Msg("not undeleting SSH Key because it was deleted within the inventory staleness window")
					return nil, nil
				}
			}

			activeGroups, _, groupNameErr := sshKeyGroupDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupFilterInput{TenantIDs: []uuid.UUID{tenant.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
			if groupNameErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to check Group names, DB error: %w", groupNameErr)
			}
			usedGroupNames := make(map[string]bool, len(activeGroups))
			for _, group := range activeGroups {
				usedGroupNames[group.Name] = true
			}
			baseGroupName := "recovered-" + sshKeyGroupID.String()[:8]
			if existingGroup != nil && !usedGroupNames[existingGroup.Name] {
				baseGroupName = existingGroup.Name
			}
			groupName := getUniqueName(baseGroupName, usedGroupNames)

			if existingGroup == nil {
				description := "Recovered from Site inventory"
				recoveredGroup, txErr = sshKeyGroupDAO.Create(ctx, tx, cdbm.SSHKeyGroupCreateInput{
					SSHKeyGroupID: &sshKeyGroupID,
					Name:          groupName,
					Description:   &description,
					TenantOrg:     tenant.Org,
					TenantID:      tenant.ID,
					Version:       &recoveryVersion,
					Status:        cdbm.SSHKeyGroupStatusSynced,
					CreatedBy:     tenant.CreatedBy,
				})
				if txErr != nil {
					return nil, fmt.Errorf("unable to recover SSH Key Group: failed to create Group, DB error: %w", txErr)
				}
			} else {
				_, txErr = sshKeyGroupDAO.Clear(ctx, tx, cdbm.SSHKeyGroupClearInput{SSHKeyGroupID: existingGroup.ID, Deleted: true})
				if txErr != nil {
					return nil, fmt.Errorf("unable to recover SSH Key Group: failed to undelete Group, DB error: %w", txErr)
				}
				recoveredGroup, txErr = sshKeyGroupDAO.Update(ctx, tx, cdbm.SSHKeyGroupUpdateInput{
					SSHKeyGroupID: existingGroup.ID,
					Name:          &groupName,
					Version:       &recoveryVersion,
					Status:        cwutil.GetPtr(cdbm.SSHKeyGroupStatusSynced),
				})
				if txErr != nil {
					return nil, fmt.Errorf("unable to recover SSH Key Group: failed to refresh Group, DB error: %w", txErr)
				}
			}
			groupChanged = true

			for _, fingerprint := range sortedFingerprints {
				keyFingerprint := fingerprint
				publicKeyText := reportedKeys[fingerprint]
				digest := sha256.Sum256([]byte(fingerprint))
				defaultKeyName := fmt.Sprintf("recovered-key-%x", digest[:6])

				sshKey := activeKeyByFingerprint[fingerprint]
				switch {
				case sshKey != nil:
				case deletedKeyByFingerprint[fingerprint] != nil:
					deleted := deletedKeyByFingerprint[fingerprint]
					baseKeyName := defaultKeyName
					if len(deleted.Name) >= 2 && len(deleted.Name) <= 256 && !usedKeyNames[deleted.Name] && strings.TrimSpace(deleted.Name) == deleted.Name {
						baseKeyName = deleted.Name
					}
					keyName := getUniqueName(baseKeyName, usedKeyNames)
					_, txErr = sshKeyDAO.Clear(ctx, tx, cdbm.SSHKeyClearInput{SSHKeyID: deleted.ID, Deleted: true})
					if txErr != nil {
						return nil, fmt.Errorf("unable to recover SSH Key Group: failed to undelete SSH Key, DB error: %w", txErr)
					}
					sshKey, txErr = sshKeyDAO.Update(ctx, tx, cdbm.SSHKeyUpdateInput{
						SSHKeyID:    deleted.ID,
						Name:        &keyName,
						PublicKey:   &publicKeyText,
						Fingerprint: &keyFingerprint,
					})
					if txErr != nil {
						return nil, fmt.Errorf("unable to recover SSH Key Group: failed to refresh SSH Key, DB error: %w", txErr)
					}
				default:
					sshKey, txErr = sshKeyDAO.Create(ctx, tx, cdbm.SSHKeyCreateInput{
						Name:        getUniqueName(defaultKeyName, usedKeyNames),
						TenantOrg:   tenant.Org,
						TenantID:    tenant.ID,
						PublicKey:   publicKeyText,
						Fingerprint: &keyFingerprint,
						CreatedBy:   tenant.CreatedBy,
					})
					if txErr != nil {
						return nil, fmt.Errorf("unable to recover SSH Key Group: failed to create SSH Key, DB error: %w", txErr)
					}
				}

				if !existingActiveKeyIDs[sshKey.ID] {
					_, txErr = keyAssociationDAO.Create(ctx, tx, cdbm.SSHKeyAssociationCreateInput{
						SSHKeyID:      sshKey.ID,
						SSHKeyGroupID: recoveredGroup.ID,
						CreatedBy:     tenant.CreatedBy,
					})
					if txErr != nil {
						return nil, fmt.Errorf("unable to recover SSH Key Group: failed to create SSH Key association, DB error: %w", txErr)
					}
				}
			}
		}

		recoveredAssociation := siteAssociation
		associationChanged := false
		if siteAssociation == nil {
			recoveredAssociation, txErr = siteAssociationDAO.Create(ctx, tx, cdbm.SSHKeyGroupSiteAssociationCreateInput{
				SSHKeyGroupID: recoveredGroup.ID,
				SiteID:        site.ID,
				Version:       &recoveryVersion,
				Status:        cdbm.SSHKeyGroupSiteAssociationStatusSynced,
				CreatedBy:     tenant.CreatedBy,
			})
			if txErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to create Site association, DB error: %w", txErr)
			}
			associationChanged = true
		} else if siteAssociation.Deleted != nil || groupChanged {
			if siteAssociation.Deleted != nil {
				_, txErr = siteAssociationDAO.Clear(ctx, tx, cdbm.SSHKeyGroupSiteAssociationClearInput{ID: siteAssociation.ID, Deleted: true})
				if txErr != nil {
					return nil, fmt.Errorf("unable to recover SSH Key Group: failed to undelete Site association, DB error: %w", txErr)
				}
			}
			recoveredAssociation, txErr = siteAssociationDAO.Update(ctx, tx, cdbm.SSHKeyGroupSiteAssociationUpdateInput{
				ID:              siteAssociation.ID,
				Version:         &recoveryVersion,
				Status:          cwutil.GetPtr(cdbm.SSHKeyGroupSiteAssociationStatusSynced),
				IsMissingOnSite: cwutil.GetPtr(false),
			})
			if txErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to refresh Site association, DB error: %w", txErr)
			}
			associationChanged = true
		}

		// Adding a Site association rotates only the active Group's optimistic
		// concurrency token; sibling Site association versions stay untouched.
		if groupIsActive && associationChanged {
			_, txErr = sshKeyGroupDAO.GenerateAndUpdateVersion(ctx, tx, recoveredGroup.ID, true)
			if txErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to rotate Group version, DB error: %w", txErr)
			}
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mskg.dbSession)
		if groupChanged {
			_, txErr = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
				EntityID: recoveredGroup.ID.String(),
				Status:   cdbm.SSHKeyGroupStatusSynced,
				Message:  cwutil.GetPtr(msgSSHKeyGroupRecovered),
			})
			if txErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to create Group status history, DB error: %w", txErr)
			}
		}
		if associationChanged {
			_, txErr = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{
				EntityID: recoveredAssociation.ID.String(),
				Status:   cdbm.SSHKeyGroupSiteAssociationStatusSynced,
				Message:  cwutil.GetPtr(msgSSHKeyGroupSiteRecovered),
			})
			if txErr != nil {
				return nil, fmt.Errorf("unable to recover SSH Key Group: failed to create Site association status history, DB error: %w", txErr)
			}
		}

		return recoveredAssociation, nil
	})
}

func getTenantKeysetKey(identifier *corev1.TenantKeysetIdentifier) string {
	return identifier.GetOrganizationId() + "/" + identifier.GetKeysetId()
}

func getUniqueName(base string, used map[string]bool) string {
	name := base
	// Start numeric disambiguators at -2 when base is already taken.
	for suffix := 2; used[name]; suffix++ {
		name = fmt.Sprintf("%s-%d", base, suffix)
	}
	used[name] = true
	return name
}

// DeleteSSHKeyGroupViaSiteAgent is a Temporal activity that delete a SSHKeyGroup in Site Controller via Site agent
func (mskg ManageSSHKeyGroup) DeleteSSHKeyGroupViaSiteAgent(ctx context.Context, siteID uuid.UUID, sshKeyGroupID uuid.UUID) error {
	logger := log.With().Str("Activity", "DeleteSSHKeyGroupViaSiteAgent").Str("SSH Key Group ID", sshKeyGroupID.String()).
		Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(mskg.dbSession)

	skgsa, err := skgsaDAO.GetBySSHKeyGroupIDAndSiteID(ctx, nil, sshKeyGroupID, siteID, []string{cdbm.SSHKeyGroupRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve SSH Key Group Site Association from DB by ID")
		return err
	}

	if skgsa.SiteID != siteID {
		logger.Error().Msg("SSH Key Group does not belong to specified Site")
		return fmt.Errorf("SSH Key Group does not belong to specified Site")
	}

	logger.Info().Msg("retrieved SSH Key Group Site Association from DB")

	// Get the temporal client for the site we are working with.
	stc, err := mskg.siteClientPool.GetClientByID(siteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return err
	}

	workflowOptions := client.StartWorkflowOptions{
		ID:                    "site-ssh-key-group-delete-" + sshKeyGroupID.String(),
		TaskQueue:             queue.SiteTaskQueue,
		WorkflowIDReusePolicy: temporalEnums.WORKFLOW_ID_REUSE_POLICY_ALLOW_DUPLICATE,
	}

	// skgsa.SSHKeyGroupID is authoritative here -- it was loaded above via
	// GetBySSHKeyGroupIDAndSiteID(sshKeyGroupID, siteID), so the deletion
	// request keys off the same group we just resolved.
	deleteSSHKeyGroupRequest := skgsa.ToDeletionRequestProto()

	we, err := stc.ExecuteWorkflow(ctx, workflowOptions, "DeleteSSHKeyGroupV2", deleteSSHKeyGroupRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to trigger site agent delete SSH Key Group workflow")
		status := cdbm.SSHKeyGroupSiteAssociationStatusError
		statusMessage := "failed to initiate SSH Key Group deletion via Site Agent"

		_ = mskg.updateSSHKeyGroupSiteAssociationStatusInDB(ctx, nil, skgsa.ID, &status, &statusMessage)

		return err
	}

	// TODO: Execute this synchronously

	logger.Info().Str("Workflow ID", we.GetID()).Msg("triggered Site agent workflow to delete SSH Key Group")

	logger.Info().Msg("completed activity")

	return nil
}

// updateSSHKeyGroupSiteAssociationStatusInDB is helper function to write SSHKeyGroupSiteAssociation updates to DB
func (mskg ManageSSHKeyGroup) updateSSHKeyGroupSiteAssociationStatusInDB(ctx context.Context, tx *cdb.Tx, skgsaID uuid.UUID, status *string, statusMessage *string) error {
	if status != nil {
		skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(mskg.dbSession)

		_, err := skgsaDAO.Update(ctx, tx, cdbm.SSHKeyGroupSiteAssociationUpdateInput{ID: skgsaID, Status: status})
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mskg.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: skgsaID.String(), Status: *status, Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// UpdateSSHKeyGroupStatusInDB is helper function to write SSH Key Group updates to DB
func (mskg ManageSSHKeyGroup) UpdateSSHKeyGroupStatusInDB(ctx context.Context, sshKeyGroupIDStr string) error {
	logger := log.With().Str("Activity", "UpdateSSHKeyGroupStatusInDB").Str("SSH Key Group ID", sshKeyGroupIDStr).Logger()

	logger.Info().Msg("starting activity")

	skgID, err := uuid.Parse(sshKeyGroupIDStr)
	if err != nil {
		logger.Error().Err(err).Msg("failed to parse SSHKey Group ID from string")
		return err
	}

	skgDAO := cdbm.NewSSHKeyGroupDAO(mskg.dbSession)

	skg, err := skgDAO.GetByID(ctx, nil, skgID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received request for unknown or deleted SSH Key Group")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve SSH Key Group from DB")
		}
		return nil
	}

	logger.Info().Msg("retrieved SSHKey Group from DB")

	var sgStatus *string
	var sgMessage *string

	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(mskg.dbSession)
	skgsas, skgsaTotal, err := skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skgID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get SSHKey Group Associations from DB for SSH Key Group")
		return err
	}

	skaDAO := cdbm.NewSSHKeyAssociationDAO(mskg.dbSession)
	skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(mskg.dbSession)

	// SSH Key Group is in deleting state
	if skg.Status == cdbm.SSHKeyGroupStatusDeleting {
		if skgsaTotal == 0 {
			// Start a db tx
			tx, err := cdb.BeginTx(ctx, mskg.dbSession, &sql.TxOptions{})
			if err != nil {
				logger.Error().Err(err).Msg("failed to start transaction")
				return err
			}

			// No more associations left, we can delete the Key Group
			serr := skgDAO.Delete(ctx, tx, skgID)
			if serr != nil {
				logger.Error().Err(serr).Msg("failed to delete SSH Key Group from DB")
				terr := tx.Rollback()
				if terr != nil {
					logger.Error().Err(terr).Msg("failed to rollback transaction")
				}
				return serr
			}

			// Remove all SSH Key associations
			skas, _, err := skaDAO.GetAll(ctx, tx, cdbm.SSHKeyAssociationFilterInput{SSHKeyGroupIDs: []uuid.UUID{skgID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("failed to retrieve SSH Key Assocications from DB by SSHKeyGroup ID")
				terr := tx.Rollback()
				if terr != nil {
					logger.Error().Err(terr).Msg("failed to rollback transaction")
				}
				return err
			}

			for _, ska := range skas {
				serr := skaDAO.Delete(ctx, tx, ska.ID)
				if serr != nil {
					logger.Error().Err(serr).Msg("failed to delete SSH Key Association from DB")
					terr := tx.Rollback()
					if terr != nil {
						logger.Error().Err(terr).Msg("failed to rollback transaction")
					}
					return serr
				}
			}

			// Remove all Instance associations
			skgias, _, err := skgiaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
				SSHKeyGroupIDs: []uuid.UUID{skgID},
			}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("failed to retrieve SSH Key Group Instance associations from DB")
				terr := tx.Rollback()
				if terr != nil {
					logger.Error().Err(terr).Msg("failed to rollback transaction")
				}
				return err
			}
			for _, skgia := range skgias {
				serr := skgiaDAO.Delete(ctx, tx, skgia.ID)
				if serr != nil {
					logger.Error().Err(serr).Msg("failed to delete SSH Key Group Instance association from DB")
					terr := tx.Rollback()
					if terr != nil {
						logger.Error().Err(terr).Msg("failed to rollback transaction")
					}
					return serr
				}
			}

			// Commit transaction
			err = tx.Commit()
			if err != nil {
				logger.Error().Err(err).Msg("error committing transaction to DB")
				return err
			}
		}

		// One or more associations left to delete from Sites
		return nil
	}

	if skgsaTotal == 0 {
		if skg.Status == cdbm.SSHKeyGroupStatusSynced {
			return nil
		}

		sgStatus = cwutil.GetPtr(cdbm.SSHKeyGroupStatusSynced)
		sgMessage = cwutil.GetPtr("SSH Key Group successfully synced to all Sites")
	} else {
		statusCountMap := map[string]int{}
		for _, dbskgsa := range skgsas {
			statusCountMap[dbskgsa.Status]++
		}

		if statusCountMap[cdbm.SSHKeyGroupSiteAssociationStatusError] > 0 {
			if skg.Status == cdbm.SSHKeyGroupStatusError {
				return nil
			}
			sgStatus = cwutil.GetPtr(cdbm.SSHKeyGroupStatusError)
			sgMessage = cwutil.GetPtr("Failed to sync SSH Key Group to one or more Sites")
		} else if statusCountMap[cdbm.SSHKeyGroupSiteAssociationStatusSyncing] > 0 {
			if skg.Status == cdbm.SSHKeyGroupStatusSyncing {
				return nil
			}
			sgStatus = cwutil.GetPtr(cdbm.SSHKeyGroupStatusSyncing)
			sgMessage = cwutil.GetPtr("SSH Key Group syncing to one or more Sites")
		} else {
			if skg.Status == cdbm.SSHKeyGroupStatusSynced {
				return nil
			}
			sgStatus = cwutil.GetPtr(cdbm.SSHKeyGroupStatusSynced)
			sgMessage = cwutil.GetPtr("SSH Key Group successfully synced to all Sites")
		}
	}

	// Update status
	_, err = skgDAO.Update(
		ctx,
		nil,
		cdbm.SSHKeyGroupUpdateInput{
			SSHKeyGroupID: skgID,
			Status:        sgStatus,
		},
	)
	if err != nil {
		return err
	}

	statusDetailDAO := cdbm.NewStatusDetailDAO(mskg.dbSession)
	_, err = statusDetailDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: skgID.String(), Status: *sgStatus, Message: sgMessage})
	if err != nil {
		return err
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// IsSSHKeyGroupCreated is helper function to get if sshkeygroup created or not
func (mskg ManageSSHKeyGroup) IsSSHKeyGroupCreatedOnSite(ctx context.Context, tx *cdb.Tx, sshKeyGroupSiteAssociationID uuid.UUID) (*bool, error) {
	sdDAO := cdbm.NewStatusDetailDAO(mskg.dbSession)
	skgsds, _, err := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{sshKeyGroupSiteAssociationID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)})
	if err != nil {
		return nil, err
	}
	for _, skgsd := range skgsds {
		// if it is synced, the tenantkeyset exists
		if skgsd.Status == cdbm.SSHKeyGroupSiteAssociationStatusSynced {
			return cwutil.GetPtr(true), nil
		}

		// if it is in error, however, statusmessage suggest that key DB error (duplicate)
		// only sync required in this case
		if skgsd.Status == cdbm.SSHKeyGroupSiteAssociationStatusError && strings.Contains(*skgsd.Message, util.ErrMsgSiteControllerDuplicateEntryFound) {
			return cwutil.GetPtr(true), nil
		}
	}
	return cwutil.GetPtr(false), nil
}

// isAlreadyExistsOnSite reports whether a Site workflow failed because the keyset already exists.
// Sites running an older Core report a duplicate as Internal with the raw database error text
// rather than AlreadyExists, so the text is still matched for them.
func isAlreadyExistsOnSite(err error) bool {
	var appErr *tsdk.ApplicationError
	if errors.As(err, &appErr) && slices.Contains(swe.AlreadyExistsErrTypes(), appErr.Type()) {
		return true
	}
	return strings.Contains(err.Error(), util.ErrMsgSiteControllerDuplicateEntryFound)
}

// NewManageSSHKeyGroup returns a new ManageSSHKeyGroup activity
func NewManageSSHKeyGroup(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageSSHKeyGroup {
	return ManageSSHKeyGroup{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
