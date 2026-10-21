// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ipxetemplate

import (
	"context"
	"errors"
	"fmt"
	"reflect"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
)

// ManageIpxeTemplate is an activity wrapper for managing iPXE template inventory that allows
// injecting DB access
type ManageIpxeTemplate struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// UpdateIpxeTemplatesInDB is a Temporal activity that takes a collection of iPXE template data
// pushed by the Site Agent and reconciles the DB.
//
// iPXE templates are global in REST (one row per stable template UUID), and per-site
// availability is tracked via IpxeTemplateSiteAssociation. For each reported template
// we ensure the global row exists with current fields and that an ITSA row exists for
// the reporting site. Templates no longer reported by this site have their ITSA
// removed; if no ITSA remains anywhere for a template, the global row is hard-deleted.
func (mit ManageIpxeTemplate) UpdateIpxeTemplatesInDB(ctx context.Context, siteID uuid.UUID, inventory *corev1.IpxeTemplateInventory) error {
	logger := log.With().Str("Activity", "UpdateIpxeTemplatesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("Starting activity")

	if inventory == nil {
		logger.Error().Msg("UpdateIpxeTemplatesInDB called with nil inventory")
		return errors.New("UpdateIpxeTemplatesInDB called with nil inventory")
	}

	if inventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("Received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	// Ensure site exists
	stDAO := cdbm.NewSiteDAO(mit.dbSession)
	_, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("Received inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("Failed to retrieve Site from DB")
		}
		return err
	}

	templateDAO := cdbm.NewIpxeTemplateDAO(mit.dbSession)
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(mit.dbSession)

	// Fetch existing ITSA rows for this site (with their template loaded) so we
	// can reconcile against this inventory snapshot.
	existingITSAs, _, err := itsaDAO.GetAll(ctx, nil,
		cdbm.IpxeTemplateSiteAssociationFilterInput{SiteIDs: []uuid.UUID{siteID}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.IpxeTemplateRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to get IpxeTemplateSiteAssociation rows for Site from DB")
		return err
	}

	// Map existing ITSAs by template ID for quick lookup.
	existingITSAByTemplateID := map[uuid.UUID]*cdbm.IpxeTemplateSiteAssociation{}
	for i := range existingITSAs {
		existingITSAByTemplateID[existingITSAs[i].IpxeTemplateID] = &existingITSAs[i]
	}

	// Track all template IDs reported by this inventory payload (for both
	// inline templates and the page item-id list, used during paginated runs).
	reportedTemplateIDs := map[uuid.UUID]bool{}

	if inventory.InventoryPage != nil {
		logger.Info().Msgf("Received iPXE template inventory page: %d of %d, page size: %d, total count: %d",
			inventory.InventoryPage.CurrentPage, inventory.InventoryPage.TotalPages,
			inventory.InventoryPage.PageSize, inventory.InventoryPage.TotalItems)

		for _, idStr := range inventory.InventoryPage.ItemIds {
			if tid, perr := uuid.Parse(idStr); perr == nil {
				reportedTemplateIDs[tid] = true
			}
		}
	}

	for _, reported := range inventory.GetTemplates() {
		if reported == nil {
			logger.Error().Msg("Received nil iPXE template entry, skipping")
			continue
		}
		if reported.GetId() == nil || reported.GetId().GetValue() == "" {
			logger.Error().Str("Name", reported.Name).Msg("Received iPXE template with empty id, skipping")
			continue
		}

		templateID, perr := uuid.Parse(reported.GetId().GetValue())
		if perr != nil {
			logger.Error().Err(perr).Str("Name", reported.Name).Msg("Received iPXE template with invalid id, skipping")
			continue
		}

		// Only propagate PUBLIC templates into REST.
		if reported.Visibility != corev1.IpxeTemplateVisibility_PUBLIC {
			logger.Debug().Str("Name", reported.Name).Str("Visibility", reported.Visibility.String()).Msg("Skipping non-public iPXE template")
			continue
		}

		reportedTemplateIDs[templateID] = true
		reportedVisibility := ipxeVisibilityToString(reported.Visibility)

		// Look up the global template row (if any).
		globalTmpl, getErr := templateDAO.Get(ctx, nil, templateID)
		if getErr != nil && !errors.Is(getErr, cdb.ErrDoesNotExist) {
			logger.Error().Err(getErr).Str("Name", reported.Name).Msg("Failed to look up global iPXE template")
			return fmt.Errorf("failed to look up iPXE template %q: %w", reported.Name, getErr)
		}

		if globalTmpl == nil {
			// First sighting of this template across all sites — create it.
			input := cdbm.IpxeTemplateCreateInput{
				ID:                templateID,
				Name:              reported.Name,
				Template:          reported.Template,
				RequiredParams:    reported.RequiredParams,
				ReservedParams:    reported.ReservedParams,
				RequiredArtifacts: reported.RequiredArtifacts,
				Visibility:        reportedVisibility,
			}
			if _, cerr := templateDAO.Create(ctx, nil, input); cerr != nil {
				logger.Error().Err(cerr).Str("Name", reported.Name).Msg("Failed to create iPXE template in DB")
				return fmt.Errorf("failed to create iPXE template %q: %w", reported.Name, cerr)
			}
		} else if globalTmpl.Name != reported.Name {
			// Cross-site name conflict: a template with the same ID is already
			// known under a different name. Keep the existing name (first
			// writer wins) and skip both the field update and the ITSA upsert.
			logger.Error().
				Str("TemplateID", templateID.String()).
				Str("ReportedName", reported.Name).
				Str("ExistingName", globalTmpl.Name).
				Msg("Template ID reused with different name, skipping")
			continue
		} else if globalTmpl.Visibility != reportedVisibility ||
			globalTmpl.Template != reported.Template ||
			!reflect.DeepEqual(globalTmpl.RequiredParams, reported.RequiredParams) ||
			!reflect.DeepEqual(globalTmpl.ReservedParams, reported.ReservedParams) ||
			!reflect.DeepEqual(globalTmpl.RequiredArtifacts, reported.RequiredArtifacts) {
			input := cdbm.IpxeTemplateUpdateInput{
				IpxeTemplateID:    globalTmpl.ID,
				Name:              cutil.GetPtr(reported.Name),
				Template:          cutil.GetPtr(reported.Template),
				RequiredParams:    cutil.GetPtr(reported.RequiredParams),
				ReservedParams:    cutil.GetPtr(reported.ReservedParams),
				RequiredArtifacts: cutil.GetPtr(reported.RequiredArtifacts),
				Visibility:        cutil.GetPtr(reportedVisibility),
			}
			if _, uerr := templateDAO.Update(ctx, nil, input); uerr != nil {
				logger.Error().Err(uerr).Str("Name", reported.Name).Msg("Failed to update iPXE template in DB")
				return fmt.Errorf("failed to update iPXE template %q: %w", reported.Name, uerr)
			}
		}

		// Ensure an ITSA exists for (template, site).
		if _, present := existingITSAByTemplateID[templateID]; !present {
			if _, cerr := itsaDAO.Create(ctx, nil, cdbm.IpxeTemplateSiteAssociationCreateInput{
				IpxeTemplateID: templateID,
				SiteID:         siteID,
			}); cerr != nil {
				logger.Error().Err(cerr).Str("Name", reported.Name).Msg("Failed to create iPXE template site association")
				return fmt.Errorf("failed to associate iPXE template %q with site: %w", reported.Name, cerr)
			}
		}
	}

	// Reconcile deletions only on the final page of an inventory run.
	if util.ShouldReconcileDeletions(inventory.GetInventoryPage()) {
		for _, existing := range existingITSAs {
			if reportedTemplateIDs[existing.IpxeTemplateID] {
				continue
			}
			templateName := ""
			if existing.IpxeTemplate != nil {
				templateName = existing.IpxeTemplate.Name
			}
			logger.Info().
				Str("Name", templateName).
				Str("TemplateID", existing.IpxeTemplateID.String()).
				Msg("Removing iPXE template site association since it is no longer reported by Site Controller")
			if derr := itsaDAO.Delete(ctx, nil, existing.ID); derr != nil {
				logger.Error().Err(derr).Str("Name", templateName).Msg("Failed to delete iPXE template site association from DB")
				return fmt.Errorf("failed to delete iPXE template site association for %q: %w", templateName, derr)
			}

			// If no other site references this template anymore, hard-delete
			// the global template row.
			_, count, gerr := itsaDAO.GetAll(ctx, nil,
				cdbm.IpxeTemplateSiteAssociationFilterInput{IpxeTemplateIDs: []uuid.UUID{existing.IpxeTemplateID}},
				cdbp.PageInput{Limit: cutil.GetPtr(1)},
				nil,
			)
			if gerr != nil {
				logger.Error().Err(gerr).Str("TemplateID", existing.IpxeTemplateID.String()).
					Msg("Failed to count remaining site associations for iPXE template")
				return fmt.Errorf("failed to count site associations for iPXE template %q: %w", templateName, gerr)
			}
			if count == 0 {
				if derr := templateDAO.Delete(ctx, nil, existing.IpxeTemplateID); derr != nil {
					logger.Error().Err(derr).Str("Name", templateName).Msg("Failed to delete global iPXE template from DB")
					return fmt.Errorf("failed to delete iPXE template %q: %w", templateName, derr)
				}
			}
		}
	}

	logger.Info().Msg("Completed activity")
	return nil
}

// NewManageIpxeTemplate returns a new ManageIpxeTemplate activity
func NewManageIpxeTemplate(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageIpxeTemplate {
	return ManageIpxeTemplate{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}

// ipxeVisibilityToString converts the IpxeTemplateVisibility enum from the gRPC
// proto to the visibility string representation stored in the database.
func ipxeVisibilityToString(visibility corev1.IpxeTemplateVisibility) string {
	if visibility == corev1.IpxeTemplateVisibility_PUBLIC {
		return cdbm.IpxeTemplateVisibilityPublic
	}
	return cdbm.IpxeTemplateVisibilityInternal
}
