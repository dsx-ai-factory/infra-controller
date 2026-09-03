// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

func TestSpectrumXPartition_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "east-west"

	t.Run("populates id, tenant org, and metadata", func(t *testing.T) {
		sxp := &SpectrumXPartition{
			ID:          id,
			Name:        "sxp-a",
			Org:         "org-1",
			Description: &desc,
			Labels:      map[string]string{"env": "prod"},
			VNI:         cutil.GetPtr(10200),
		}
		got := sxp.ToProto()
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		assert.Equal(t, "org-1", got.TenantOrganizationId)
		assert.Equal(t, uint32(10200), got.Vni)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "sxp-a", got.Metadata.Name)
		assert.Equal(t, "east-west", got.Metadata.Description)
		require.Len(t, got.Metadata.Labels, 1)
		assert.Equal(t, "env", got.Metadata.Labels[0].Key)
	})

	// A nil VNI is the not-yet-allocated case, and the wire field cannot express it,
	// so it has to serialize as 0 rather than panicking on the dereference.
	t.Run("nil VNI, description, and labels yield zero values", func(t *testing.T) {
		sxp := &SpectrumXPartition{ID: id, Org: "org-1", Name: "sxp-a"}
		got := sxp.ToProto()
		assert.Equal(t, uint32(0), got.Vni)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "", got.Metadata.Description)
		assert.Nil(t, got.Metadata.Labels)
	})
}

func TestSpectrumXPartition_ToDeletionRequestProto(t *testing.T) {
	id := uuid.New()
	sxp := &SpectrumXPartition{ID: id}

	got := sxp.ToDeletionRequestProto()
	require.NotNil(t, got)
	require.NotNil(t, got.Id)
	assert.Equal(t, id.String(), got.Id.Value)
}

func TestSpectrumXPartitionStatus_Message(t *testing.T) {
	tests := []struct {
		name   string
		status SpectrumXPartitionStatus
		want   string
	}{
		{name: "Pending", status: SpectrumXPartitionStatusPending, want: "SpectrumX Partition request received, pending creation on Site"},
		{name: "Ready", status: SpectrumXPartitionStatusReady, want: "SpectrumX Partition is ready for use"},
		{name: "Error", status: SpectrumXPartitionStatusError, want: "SpectrumX Partition is in error state"},
		{name: "unrecognized status has no message", status: SpectrumXPartitionStatus("Bogus"), want: ""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.status.Message())
		})
	}
}

func TestSpectrumXPartition_Validate(t *testing.T) {
	tests := []struct {
		name    string
		sxp     SpectrumXPartition
		wantErr bool
	}{
		{
			name: "valid",
			sxp:  SpectrumXPartition{Name: "sxp-a", Status: SpectrumXPartitionStatusPending},
		},
		{
			name:    "missing name",
			sxp:     SpectrumXPartition{Status: SpectrumXPartitionStatusPending},
			wantErr: true,
		},
		{
			name:    "name with surrounding whitespace",
			sxp:     SpectrumXPartition{Name: " sxp-a ", Status: SpectrumXPartitionStatusPending},
			wantErr: true,
		},
		{
			name:    "unrecognized status",
			sxp:     SpectrumXPartition{Name: "sxp-a", Status: SpectrumXPartitionStatus("Bogus")},
			wantErr: true,
		},
		{
			name:    "missing status",
			sxp:     SpectrumXPartition{Name: "sxp-a"},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.sxp.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
