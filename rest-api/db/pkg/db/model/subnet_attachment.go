// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/google/uuid"
)

// SubnetAttachIntent holds the immutable Site operation identity. The VPC IDs
// are both REST (authorization) and Site-facing (fenced RPC); neither may be
// inferred from a later inventory observation.
type SubnetAttachIntent struct {
	ID                    uuid.UUID
	SubnetID              uuid.UUID
	TenantID              uuid.UUID
	SiteID                uuid.UUID
	ControllerSegmentID   uuid.UUID
	SourceVpcID           uuid.UUID
	TargetVpcID           uuid.UUID
	SourceControllerVpcID uuid.UUID
	TargetControllerVpcID uuid.UUID
	SegmentVersion        string
	RecoveryToken         *uuid.UUID
}

// ReserveAttachment serializes with inventory on the Subnet row. A failed CAS
// never creates an operation, and callers must not send a Site RPC for it.
func (ssd SubnetSQLDAO) ReserveAttachment(ctx context.Context, tx *db.Tx, intent SubnetAttachIntent) (bool, error) {
	if tx == nil || intent.ID == uuid.Nil || intent.SubnetID == uuid.Nil || intent.SiteID == uuid.Nil || intent.TenantID == uuid.Nil ||
		intent.ControllerSegmentID == uuid.Nil || intent.SourceVpcID == uuid.Nil || intent.TargetVpcID == uuid.Nil ||
		intent.SourceControllerVpcID == uuid.Nil || intent.TargetControllerVpcID == uuid.Nil || intent.SegmentVersion == "" ||
		intent.SourceVpcID == intent.TargetVpcID {
		return false, fmt.Errorf("invalid Subnet attachment reservation")
	}
	result, err := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(&Subnet{}).
		Set("attach_intent_id = ?", intent.ID).
		Set("attach_source_vpc_id = ?", intent.SourceVpcID).
		Set("attach_target_vpc_id = ?", intent.TargetVpcID).
		Set("attach_source_controller_vpc_id = ?", intent.SourceControllerVpcID).
		Set("attach_target_controller_vpc_id = ?", intent.TargetControllerVpcID).
		Set("attach_segment_version = ?", intent.SegmentVersion).
		Set("attach_next_at = current_timestamp + interval '70 seconds'").Set("attach_attempts = 0").
		Where("id = ? AND site_id = ? AND tenant_id = ? AND vpc_id = ? AND controller_network_segment_id = ? AND deleted IS NULL AND status = ? AND attach_intent_id IS NULL", intent.SubnetID, intent.SiteID, intent.TenantID, intent.SourceVpcID, intent.ControllerSegmentID, SubnetStatusReady).
		Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count == 1, err
}

// CompleteAttachment commits only a previously reserved source-to-target
// transition. Inventory and another handler cannot win the row while the intent
// remains set. Clear the intent only after the exact Core target is confirmed.
func (ssd SubnetSQLDAO) CompleteAttachment(ctx context.Context, tx *db.Tx, intent SubnetAttachIntent) (bool, error) {
	if tx == nil || intent.ID == uuid.Nil || intent.SubnetID == uuid.Nil || intent.TargetVpcID == uuid.Nil || intent.SourceVpcID == uuid.Nil {
		return false, fmt.Errorf("invalid Subnet attachment completion")
	}
	q := db.GetIDB(tx, ssd.dbSession).NewUpdate().Model(&Subnet{}).
		Set("vpc_id = ?", intent.TargetVpcID).Set("updated = current_timestamp").
		Set("attach_intent_id = NULL").Set("attach_source_vpc_id = NULL").Set("attach_target_vpc_id = NULL").
		Set("attach_source_controller_vpc_id = NULL").Set("attach_target_controller_vpc_id = NULL").
		Set("attach_segment_version = NULL").Set("attach_recovery_token = NULL").
		Set("attach_lease_until = NULL").Set("attach_next_at = NULL").
		Where("id = ? AND site_id = ? AND tenant_id = ? AND vpc_id = ? AND controller_network_segment_id = ? AND attach_intent_id = ? AND deleted IS NULL", intent.SubnetID, intent.SiteID, intent.TenantID, intent.SourceVpcID, intent.ControllerSegmentID, intent.ID)
	if intent.RecoveryToken == nil {
		q = q.Where("attach_recovery_token IS NULL")
	} else {
		q = q.Where("attach_recovery_token = ? AND attach_lease_until > current_timestamp", *intent.RecoveryToken)
	}
	result, err := q.Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count == 1, err
}

// ClaimAttachmentRecovery uses SKIP LOCKED and a durable lease to coordinate
// multiple workflow replicas. A lease only fences DB completion; delayed Site
// requests are fenced by Core's expected segment version.
func (ssd SubnetSQLDAO) ClaimAttachmentRecovery(ctx context.Context, maxRows int, lease time.Duration) ([]Subnet, error) {
	if maxRows < 1 || maxRows > 32 || lease < time.Second || lease > 5*time.Minute {
		return nil, fmt.Errorf("invalid Subnet attachment recovery claim bounds")
	}
	claimed := []Subnet{}
	err := ssd.dbSession.DB.NewRaw(`
        WITH due AS (SELECT id FROM subnet WHERE deleted IS NULL AND attach_intent_id IS NOT NULL
            AND (attach_next_at IS NULL OR attach_next_at <= current_timestamp)
            AND (attach_lease_until IS NULL OR attach_lease_until <= current_timestamp)
            ORDER BY updated, id LIMIT ? FOR UPDATE SKIP LOCKED)
        UPDATE subnet AS su SET attach_recovery_token = gen_random_uuid(),
            attach_lease_until = current_timestamp + (? * interval '1 second'),
            attach_attempts = attach_attempts + 1
        FROM due WHERE su.id = due.id RETURNING su.*`, maxRows, lease.Seconds()).Scan(ctx, &claimed)
	return claimed, err
}

// DeferAttachmentRecovery releases one live claim while retaining the immutable
// intent for the next bounded pass (including after worker restart).
func (ssd SubnetSQLDAO) DeferAttachmentRecovery(ctx context.Context, subnetID, intentID, token uuid.UUID, delay time.Duration) (bool, error) {
	if subnetID == uuid.Nil || intentID == uuid.Nil || token == uuid.Nil || delay < time.Second || delay > time.Hour {
		return false, fmt.Errorf("invalid Subnet attachment recovery delay")
	}
	result, err := ssd.dbSession.DB.NewUpdate().Model(&Subnet{}).
		Set("attach_recovery_token = NULL").Set("attach_lease_until = NULL").
		Set("attach_next_at = current_timestamp + (? * interval '1 second')", delay.Seconds()).
		Where("id = ? AND attach_intent_id = ? AND attach_recovery_token = ? AND attach_lease_until > current_timestamp AND deleted IS NULL", subnetID, intentID, token).Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count == 1, err
}
