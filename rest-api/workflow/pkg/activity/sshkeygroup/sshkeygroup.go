// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshkeygroup

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"go.temporal.io/sdk/client"

	temporalEnums "go.temporal.io/api/enums/v1"

	tsdk "go.temporal.io/sdk/temporal"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

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
		logger.Warn().Msg(fmt.Sprintf("SSHKeyGroup %s is in deleting state, cannot sync to the Site %s", sshKeyGroupID.String(), siteID.String()))
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
		logger.Info().Str("Workflow ID", wid).Msg(fmt.Sprintf("executed synchronous %s SSHKeyGroup workflow", workflowMethod))

		// Block until the workflow has completed and returned success/error.
		err = we.Get(ctx, nil)
		if err != nil {
			var timeoutErr *tsdk.TimeoutError
			// Check for timeout errors
			if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded {
				logger.Error().Err(err).Msg(fmt.Sprintf("failed to %s SSHKeyGroup, timeout occurred executing workflow on Site.", workflowMethod))

				// Create a new context deadlines
				newctx, newcancel := context.WithTimeout(context.Background(), cwutil.WorkflowContextNewAfterTimeout)
				defer newcancel()

				// Initiate termination workflow
				serr := stc.TerminateWorkflow(newctx, wid, "", fmt.Sprintf("timeout occurred executing %s SSHKeyGroup workflow", workflowMethod))
				if serr != nil {
					logger.Error().Err(serr).Msg(fmt.Sprintf("failed to execute terminate Temporal workflow for %s SSHKeyGroup", workflowMethod))
				}
				logger.Info().Str("Workflow ID", wid).Msg(fmt.Sprintf("initiated terminate synchronous %s SSHKeyGroup workflow successfully", workflowMethod))

				status = cdbm.SSHKeyGroupSiteAssociationStatusError
				statusMessage = fmt.Sprintf("failed to %s SSHKeyGroup, timeout occurred executing workflow on Site.", workflowMethod)
				err = nil // Clear error so function returns nil after updating status
			} else if strings.Contains(err.Error(), util.ErrMsgSiteControllerDuplicateEntryFound) {
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
		logger.Info().Str("Workflow ID", we.GetID()).Msg(fmt.Sprintf("successfully executed %s SSHKeyGroup workflow on Site", workflowMethod))
	}

	logger.Info().Msg("completed activity")

	return nil
}

// UpdateSSHKeyGroupsInDB takes information pushed by Site Agent for a collection of SSH Key Groups associated with the Site and updates the DB
func (mskg ManageSSHKeyGroup) UpdateSSHKeyGroupsInDB(ctx context.Context, siteID uuid.UUID, sshKeyGroupInventory *corev1.SSHKeyGroupInventory) ([]string, error) {
	logger := log.With().Str("Activity", "UpdateSSHKeyGroupsInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mskg.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
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

	existingSkgsaTkMap := map[string]*corev1.TenantKeyset{}

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

		skgsa, ok := skgIDSkgsaMap[sshKeyGroupIDStr]
		if !ok {
			slogger.Error().Str("Controller Tenant Keyset ID", tenantKeyset.KeysetIdentifier.KeysetId).Msg("Tenant Keyset does not have a Site Association record in DB, possibly created directly on Site")
			continue
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

	updatedSkgMap := map[string]bool{}

	// Process all SSH Key Group Site Associations in DB
	for skgID, skgsa := range skgIDSkgsaMap {
		slogger := logger.With().Str("SSH Key Group Site Association ID", skgsa.ID.String()).Logger()

		syncRequired := false

		tenantKeyset, ok := existingSkgsaTkMap[skgID]
		if !ok {
			if !reportedSSHKeyGroupIDMap[skgID] {
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
	for skgID, _ := range updatedSkgMap {
		updatedSkgStrIDs = append(updatedSkgStrIDs, skgID)
	}

	return updatedSkgStrIDs, nil
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

// NewManageSSHKeyGroup returns a new ManageSSHKeyGroup activity
func NewManageSSHKeyGroup(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageSSHKeyGroup {
	return ManageSSHKeyGroup{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
