// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"
	"maps"
	"net/netip"

	"github.com/google/uuid"
	"go.opentelemetry.io/otel/attribute"
	"google.golang.org/protobuf/proto"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ErrVpcCreationConflict means a Core object does not match the saved creation.
var ErrVpcCreationConflict = errors.New("reported VPC does not match the saved creation inputs")

// CompleteCreation owns a short transaction that saves the original result and
// history without changing caller fields. It validates ID and tenant even when
// returning a non-Pending or positively rejected row unchanged. Completions lock the row.
func (vsd VpcSQLDAO) CompleteCreation(ctx context.Context, id uuid.UUID, controllerVpc *corev1.Vpc) (_ *Vpc, retErr error) {
	ctx, span := cotel.StartSpan(ctx, "VpcDAO.CompleteCreation")
	defer func() { cotel.EndSpan(span, retErr) }()
	cotel.SetAttribute(span, attribute.String("id", id.String()))
	return db.WithTxResult(ctx, vsd.dbSession, func(tx *db.Tx) (*Vpc, error) {
		vpc := &Vpc{}
		err := tx.GetBunTx().NewSelect().Model(vpc).Where("v.id = ?", id).For("UPDATE").Scan(ctx)
		if err != nil {
			if err == sql.ErrNoRows {
				return nil, db.ErrDoesNotExist
			}
			return nil, err
		}
		if controllerVpc.GetId().GetValue() != vpc.GetSiteID().String() || controllerVpc.GetConfig().GetTenantOrganizationId() != vpc.Org {
			return nil, ErrVpcCreationConflict
		}
		if vpc.Status != VpcStatusPending || vpc.CreationRejectedAt != nil {
			return vpc, nil
		}
		if !vpc.matchesCreation(controllerVpc) {
			return nil, ErrVpcCreationConflict
		}
		reported := &Vpc{}
		reported.FromProto(controllerVpc)
		status := VpcStatusProvisioning
		message := "VPC provisioning has been initiated on Site"
		if reported.ActiveVni != nil {
			status = VpcStatusReady
			message = "VPC is ready for use"
		}
		updated, err := vsd.Update(ctx, tx, VpcUpdateInput{
			VpcID: id, RoutingProfile: reported.RoutingProfile,
			ActiveVni: reported.ActiveVni, EffectiveRoutingProfile: reported.EffectiveRoutingProfile,
			Status: &status, IsMissingOnSite: cutil.GetPtr(false),
		})
		if err != nil {
			return nil, err
		}
		_, err = NewStatusDetailDAO(vsd.dbSession).Create(ctx, tx, StatusDetailCreateInput{
			EntityID: id.String(), Status: status, Message: &message,
		})
		return updated, err
	})
}

// matchesCreation compares the creation fields that Core persists. An omitted
// profile may resolve through tenant or Site configuration. Core does not persist
// the default NVLink partition, so its absence cannot establish a conflict.
func (vpc *Vpc) matchesCreation(reported *corev1.Vpc) bool {
	expected := vpc.ToProto()
	savedConfig, reportedConfig := expected.GetConfig(), reported.GetConfig()
	if reportedConfig == nil || savedConfig.GetNetworkVirtualizationType() != reportedConfig.GetNetworkVirtualizationType() ||
		savedConfig.GetSlaacEnabled() != reportedConfig.GetSlaacEnabled() ||
		savedConfig.GetNetworkSecurityGroupId() != reportedConfig.GetNetworkSecurityGroupId() ||
		savedConfig.GetPowerResourceGroup() != reportedConfig.GetPowerResourceGroup() ||
		(savedConfig.Vni == nil) != (reportedConfig.Vni == nil) || savedConfig.GetVni() != reportedConfig.GetVni() ||
		(savedConfig.RoutingProfileType != nil && savedConfig.GetRoutingProfileType() != reportedConfig.GetRoutingProfileType()) {
		return false
	}
	metadata := reported.GetMetadata()
	if metadata == nil {
		return false
	}
	var labels Labels
	labels.FromProto(metadata.GetLabels())
	if expected.Metadata.Name != metadata.GetName() ||
		expected.Metadata.Description != metadata.GetDescription() || !maps.Equal(vpc.Labels, labels) {
		return false
	}
	// Core parses policy prefixes before returning them. Normalize their spelling
	// while preserving presence, order, host bits and symbolic route targets.
	savedOverrides, reportedOverrides := savedConfig.GetRoutingProfileOverrides(), reportedConfig.GetRoutingProfileOverrides()
	if savedOverrides == nil || reportedOverrides == nil {
		return savedOverrides == nil && reportedOverrides == nil
	}
	savedOverrides = proto.Clone(savedOverrides).(*corev1.VpcRoutingProfileOverrides)
	reportedOverrides = proto.Clone(reportedOverrides).(*corev1.VpcRoutingProfileOverrides)
	for _, overrides := range []*corev1.VpcRoutingProfileOverrides{savedOverrides, reportedOverrides} {
		for _, entries := range []*corev1.PrefixFilterPolicyEntries{overrides.AcceptedLeaksFromUnderlay, overrides.AllowedAnycastPrefixes} {
			for _, entry := range entries.GetValues() {
				prefix, err := netip.ParsePrefix(entry.GetPrefix())
				if err != nil {
					return false
				}
				entry.Prefix = prefix.String()
			}
		}
	}
	return proto.Equal(savedOverrides, reportedOverrides)
}

// RejectCreation atomically records Error, its proof timestamp and history.
// The caller must prove either original precommit rejection or that it did not
// dispatch creation. Other failures must remain Pending. Non-Pending rows are
// returned unchanged, so repeated decisions do not add history.
func (vsd VpcSQLDAO) RejectCreation(ctx context.Context, id uuid.UUID) (_ *Vpc, retErr error) {
	ctx, span := cotel.StartSpan(ctx, "VpcDAO.RejectCreation")
	defer func() { cotel.EndSpan(span, retErr) }()
	cotel.SetAttribute(span, attribute.String("id", id.String()))
	return db.WithTxResult(ctx, vsd.dbSession, func(tx *db.Tx) (*Vpc, error) {
		vpc := &Vpc{}
		err := tx.GetBunTx().NewSelect().Model(vpc).Where("v.id = ?", id).For("UPDATE").Scan(ctx)
		if err != nil {
			if err == sql.ErrNoRows {
				return nil, db.ErrDoesNotExist
			}
			return nil, err
		}
		if vpc.Status != VpcStatusPending {
			return vpc, nil
		}
		rejectedAt := db.GetCurTime()
		updated, err := vsd.Update(ctx, tx, VpcUpdateInput{
			VpcID: id, Status: cutil.GetPtr(VpcStatusError), creationRejectedAt: &rejectedAt,
		})
		if err != nil {
			return nil, err
		}
		_, err = NewStatusDetailDAO(vsd.dbSession).Create(ctx, tx, StatusDetailCreateInput{
			EntityID: id.String(), Status: VpcStatusError, Message: cutil.GetPtr("VPC creation stopped before Core commit"),
		})
		return updated, err
	})
}

// DeleteRejectedCreation commits local deletion before the caller attempts DPS cleanup.
// Only the call that changes an undeleted row may make that best-effort attempt.
func (vsd VpcSQLDAO) DeleteRejectedCreation(ctx context.Context, id uuid.UUID) (_ bool, retErr error) {
	ctx, span := cotel.StartSpan(ctx, "VpcDAO.DeleteRejectedCreation")
	defer func() { cotel.EndSpan(span, retErr) }()
	cotel.SetAttribute(span, attribute.String("id", id.String()))
	result, err := vsd.dbSession.DB.NewDelete().Model((*Vpc)(nil)).
		Where("id = ?", id).Where("creation_rejected_at IS NOT NULL").Where("deleted IS NULL").Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count > 0, err
}
