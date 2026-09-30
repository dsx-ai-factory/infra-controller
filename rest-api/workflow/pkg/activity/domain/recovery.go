// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package domain

import (
	"context"
	"fmt"
	"net/http"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
)

type ManageDomain struct {
	DB    *cdb.Session
	Sites *sc.ClientPool
}

// ReconcileReservedDomains executes only durable, previously authorized
// reserved-ID intents, claiming one row per tick so its lease cannot expire
// while an earlier row waits on a Site RPC. No arbitrary legacy DNS name is
// claimed as tenant-owned. Each batch and Site request has a finite bound.
func (m ManageDomain) ReconcileReservedDomains(ctx context.Context) error {
	dao := cdbm.NewDomainDAO(m.DB)
	due, err := dao.ClaimRecovery(ctx, 1, 120*time.Second)
	if err != nil {
		return err
	}
	for i := range due {
		opCtx, cancel := context.WithTimeout(ctx, 65*time.Second)
		err := m.reconcileOne(opCtx, dao, &due[i])
		cancel()
		if err != nil {
			log.Warn().Err(err).Str("domainID", due[i].ID.String()).Msg("reserved Domain remains pending")
		}
	}
	return nil
}

func (m ManageDomain) reconcileOne(ctx context.Context, dao cdbm.DomainDAO, d *cdbm.Domain) error {
	if d.RecoveryToken == nil || d.ControllerDomainID == nil || d.TenantID == nil || d.SiteID == nil {
		return fmt.Errorf("incomplete reserved Domain recovery identity")
	}
	defer func() {
		release, cancel := context.WithTimeout(context.WithoutCancel(ctx), 3*time.Second)
		defer cancel()
		delay := time.Duration(30+min(d.RecoveryAttempts*10, 270)) * time.Second
		if _, err := dao.DeferRecovery(release, d.ID, *d.RecoveryToken, delay); err != nil {
			log.Error().Err(err).Str("domainID", d.ID.String()).Msg("failed to defer Domain intent")
		}
	}()
	// Recheck authorization from the persisted row before initiating any new
	// Core write. Revocation/deletion leaves an explicit unresolved record.
	if _, err := cdbm.NewTenantSiteDAO(m.DB).GetByTenantIDAndSiteID(ctx, nil, *d.TenantID, *d.SiteID, nil); err != nil {
		return fmt.Errorf("Domain owner lost Site access: %w", err)
	}
	site, err := cdbm.NewSiteDAO(m.DB).GetByID(ctx, nil, *d.SiteID, nil, false)
	if err != nil {
		return err
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return fmt.Errorf("Domain Site is not registered")
	}
	stc, err := m.Sites.GetClientByID(*d.SiteID)
	if err != nil {
		return err
	}
	switch d.Status {
	case cdbm.DomainStatusPending:
		// The Core create is idempotent only for this exact ID/name/intent;
		// a late first RPC cannot create a different owner or another ID.
		resource := &corev1.Domain{}
		req := &corev1.CreateDomainRequest{Name: d.Hostname, ReservedId: &corev1.DomainId{Value: d.ControllerDomainID.String()}}
		if apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_CreateDomain_FullMethodName, req, resource, d.SiteID.String()); apiErr != nil {
			// Only a definitive Core validation/conflict response can terminate
			// this intent. A proxy timeout or transport failure leaves the same
			// reserved ID Pending for safe replay on a later sweep.
			if apiErr.Code == http.StatusBadRequest || apiErr.Code == http.StatusConflict || apiErr.Code == http.StatusPreconditionFailed {
				changed, err := dao.CompleteRecovery(ctx, d.ID, *d.ControllerDomainID, *d.RecoveryToken, cdbm.DomainStatusPending, cdbm.DomainStatusError, false)
				if err != nil || !changed {
					return fmt.Errorf("Domain rejection CAS failed: changed=%t err=%v", changed, err)
				}
				return nil
			}
			return fmt.Errorf("reserved Core create unconfirmed: %s", apiErr.Message)
		}
		if resource.GetId().GetValue() != d.ControllerDomainID.String() || cdbm.NormalizeForwardDomainName(resource.GetName()) != d.Hostname {
			return fmt.Errorf("Core returned a different Domain for reserved ID")
		}
		changed, err := dao.CompleteRecovery(ctx, d.ID, *d.ControllerDomainID, *d.RecoveryToken, cdbm.DomainStatusPending, cdbm.DomainStatusReady, false)
		if err != nil || !changed {
			return fmt.Errorf("Domain Ready CAS failed: changed=%t err=%v", changed, err)
		}
	case cdbm.DomainStatusDeleting:
		// Even an absent Core row must leave a durable cancellation tombstone
		// before REST may soft-delete its projection (late create race).
		req := &corev1.DomainDeletionRequest{Id: &corev1.DomainId{Value: d.ControllerDomainID.String()}, CancelReservedId: true}
		if apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteDomain_FullMethodName, req, nil, d.SiteID.String()); apiErr != nil {
			return fmt.Errorf("Core deletion/cancellation unconfirmed: %s", apiErr.Message)
		}
		changed, err := dao.CompleteRecovery(ctx, d.ID, *d.ControllerDomainID, *d.RecoveryToken, cdbm.DomainStatusDeleting, cdbm.DomainStatusDeleting, true)
		if err != nil || !changed {
			return fmt.Errorf("Domain delete CAS failed: changed=%t err=%v", changed, err)
		}
	default:
		return fmt.Errorf("unexpected Domain recovery status %q", d.Status)
	}
	return nil
}
