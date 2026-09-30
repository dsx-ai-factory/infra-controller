// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package subnet

import (
	"context"
	"fmt"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ReconcileAttachmentIntents processes a bounded number of authorized, durable
// operations per cloud workflow tick. A claimed row remains pending if Core is
// unreachable, the Site loses its tenant association or Core reports a third
// VPC; neither inventory nor elapsed time is proof of a successful attach.
func (ms ManageSubnet) ReconcileAttachmentIntents(ctx context.Context) error {
	dao := cdbm.NewSubnetDAO(ms.dbSession)
	intents, err := dao.ClaimAttachmentRecovery(ctx, 8, 120*time.Second)
	if err != nil {
		return err
	}
	for i := range intents {
		// A timeout on one Site must not monopolize this bounded activity.
		opCtx, cancel := context.WithTimeout(ctx, 90*time.Second)
		err := ms.reconcileAttachment(opCtx, dao, &intents[i])
		cancel()
		if err != nil {
			log.Warn().Err(err).Str("subnetID", intents[i].ID.String()).Msg("Subnet attachment remains pending")
		}
	}
	return nil
}

func (ms ManageSubnet) reconcileAttachment(ctx context.Context, dao cdbm.SubnetDAO, s *cdbm.Subnet) error {
	if s.AttachIntentID == nil || s.AttachRecoveryToken == nil {
		return fmt.Errorf("unclaimed Subnet attachment")
	}
	// Bound retries and release the cross-replica claim even if every remote
	// read fails. No worker may finalize using an expired/replaced token.
	defer func() {
		delay := time.Duration(30+min(s.AttachAttempts*10, 270)) * time.Second
		releaseCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 3*time.Second)
		defer cancel()
		if _, err := dao.DeferAttachmentRecovery(releaseCtx, s.ID, *s.AttachIntentID, *s.AttachRecoveryToken, delay); err != nil {
			log.Error().Err(err).Str("subnetID", s.ID.String()).Msg("failed to defer Subnet attachment recovery")
		}
	}()
	if s.AttachSourceVpcID == nil || s.AttachTargetVpcID == nil || s.AttachSourceControllerVpcID == nil ||
		s.AttachTargetControllerVpcID == nil || s.AttachSegmentVersion == nil || s.ControllerNetworkSegmentID == nil {
		return fmt.Errorf("incomplete durable Subnet attachment identity")
	}
	// Previously authorized operation only: fail closed on revoked tenant-Site
	// association or Site deletion before any new Site-facing RPC.
	if _, err := cdbm.NewTenantSiteDAO(ms.dbSession).GetByTenantIDAndSiteID(ctx, nil, s.TenantID, s.SiteID, nil); err != nil {
		return fmt.Errorf("tenant-Site association not current: %w", err)
	}
	site, err := cdbm.NewSiteDAO(ms.dbSession).GetByID(ctx, nil, s.SiteID, nil, false)
	if err != nil || site.Status != cdbm.SiteStatusRegistered {
		return fmt.Errorf("Site is unavailable for Subnet recovery: %v", err)
	}
	stc, err := ms.siteClientPool.GetClientByID(s.SiteID)
	if err != nil {
		return err
	}
	read := func() (*corev1.NetworkSegment, error) {
		result := &corev1.NetworkSegmentList{}
		apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindNetworkSegmentsByIds_FullMethodName,
			&corev1.NetworkSegmentsByIdsRequest{NetworkSegmentsIds: []*corev1.NetworkSegmentId{{Value: s.ControllerNetworkSegmentID.String()}}}, result, s.SiteID.String())
		if apiErr != nil {
			return nil, fmt.Errorf("Core segment read failed: %s", apiErr.Message)
		}
		if len(result.GetNetworkSegments()) != 1 || result.GetNetworkSegments()[0].GetId().GetValue() != s.ControllerNetworkSegmentID.String() {
			return nil, fmt.Errorf("Core segment identity not confirmed")
		}
		segment := result.GetNetworkSegments()[0]
		if segment.GetConfig().GetSegmentType() != corev1.NetworkSegmentType_TENANT || segment.GetStatus().GetLifecycle().GetVersion() == "" {
			return nil, fmt.Errorf("Core tenant segment version not confirmed")
		}
		return segment, nil
	}
	segment, err := read()
	if err != nil {
		return err
	}
	vpc := segment.GetConfig().GetVpcId().GetValue()
	if vpc == s.AttachTargetControllerVpcID.String() {
		intent := cdbm.SubnetAttachIntent{
			ID: *s.AttachIntentID, SubnetID: s.ID, TenantID: s.TenantID, SiteID: s.SiteID,
			ControllerSegmentID: *s.ControllerNetworkSegmentID,
			SourceVpcID:         *s.AttachSourceVpcID, TargetVpcID: *s.AttachTargetVpcID,
			RecoveryToken: s.AttachRecoveryToken,
		}
		changed, err := cdb.WithTxResult(ctx, ms.dbSession, func(tx *cdb.Tx) (bool, error) {
			return dao.CompleteAttachment(ctx, tx, intent)
		})
		if err != nil || !changed {
			return fmt.Errorf("cannot finalize Core-confirmed attachment: changed=%t err=%v", changed, err)
		}
		return nil
	}
	if vpc != s.AttachSourceControllerVpcID.String() {
		return fmt.Errorf("Core segment moved to a third VPC; manual reconciliation required")
	}
	if segment.GetStatus().GetLifecycle().GetVersion() != *s.AttachSegmentVersion {
		// A different Core version fences this intent's late RPC. Do not
		// reverse someone else's write; operator investigation is required.
		return fmt.Errorf("Core segment version changed without expected attachment")
	}
	request := &corev1.AttachNetworkSegmentToVpcRequest{
		NetworkSegmentId:       &corev1.NetworkSegmentId{Value: s.ControllerNetworkSegmentID.String()},
		VpcId:                  &corev1.VpcId{Value: s.AttachTargetControllerVpcID.String()},
		AllowReplace:           true,
		ExpectedSourceVpcId:    &corev1.VpcId{Value: s.AttachSourceControllerVpcID.String()},
		ExpectedSegmentVersion: s.AttachSegmentVersion,
	}
	response := &corev1.NetworkSegment{}
	if apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_AttachNetworkSegmentToVpc_FullMethodName, request, response, s.SiteID.String()); apiErr != nil {
		return fmt.Errorf("Core attach unconfirmed: %s", apiErr.Message)
	}
	if response.GetId().GetValue() != s.ControllerNetworkSegmentID.String() || response.GetConfig().GetVpcId().GetValue() != s.AttachTargetControllerVpcID.String() {
		return fmt.Errorf("Core attach returned unexpected identity")
	}
	// A Core response can be replayed or delayed. Always re-read confirmed
	// state before CAS completion. If the lease expired, completion fails.
	segment, err = read()
	if err != nil {
		return err
	}
	if segment.GetConfig().GetVpcId().GetValue() != s.AttachTargetControllerVpcID.String() {
		return fmt.Errorf("Core target not confirmed after attach")
	}
	intent := cdbm.SubnetAttachIntent{ID: *s.AttachIntentID, SubnetID: s.ID, TenantID: s.TenantID, SiteID: s.SiteID,
		ControllerSegmentID: *s.ControllerNetworkSegmentID, SourceVpcID: *s.AttachSourceVpcID,
		TargetVpcID: *s.AttachTargetVpcID, RecoveryToken: s.AttachRecoveryToken}
	changed, err := cdb.WithTxResult(ctx, ms.dbSession, func(tx *cdb.Tx) (bool, error) { return dao.CompleteAttachment(ctx, tx, intent) })
	if err != nil || !changed {
		return fmt.Errorf("cannot commit verified Core attachment: changed=%t err=%v", changed, err)
	}
	return nil
}
