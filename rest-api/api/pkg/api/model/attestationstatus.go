// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// AttestationStatusOrderByMachineIDAsc is the only supported, and the default,
// order for listing fleet attestation statuses.
const AttestationStatusOrderByMachineIDAsc = "MACHINE_ID_ASC"

// AttestationMachineType selects which machines a fleet listing covers.
type AttestationMachineType string

// Fleet machine types.
const (
	AttestationMachineTypeHost AttestationMachineType = "Host"
	AttestationMachineTypeDpu  AttestationMachineType = "Dpu"
)

// attestationMachineTypes lists every accepted machine type filter value.
var attestationMachineTypes = []AttestationMachineType{AttestationMachineTypeHost, AttestationMachineTypeDpu}

// APIAttestationFleetRequest lists a Site's predicted attestation status per
// machine, one cursor page at a time.
type APIAttestationFleetRequest struct {
	// SiteID is the Site to read from
	SiteID string `query:"siteId" json:"siteId"`
	// Status keeps machines whose latest controller job has this status
	Status *AttestationStatus `query:"status" json:"status"`
	// MachineType keeps hosts or DPUs; every type when omitted
	MachineType *AttestationMachineType `query:"machineType" json:"machineType"`
	// PageSize caps the page; the Site's limit (default 100) when omitted
	PageSize *uint32 `query:"pageSize" json:"pageSize"`
	// PageToken continues from a previous page's nextPageToken
	PageToken string `query:"pageToken" json:"pageToken"`
	// OrderBy accepts only MACHINE_ID_ASC, the default
	OrderBy string `query:"orderBy" json:"orderBy"`
}

// Validate checks the request before any lookup.
func (r *APIAttestationFleetRequest) Validate() error {
	return validation.Errors{
		"siteId":      validation.Validate(r.SiteID, requiredUUIDRules...),
		"status":      validation.Validate(r.Status, oneOf(attestationStatuses...)),
		"machineType": validation.Validate(r.MachineType, oneOf(attestationMachineTypes...)),
		"pageSize":    validation.Validate(r.PageSize, validation.By(uint32PtrRange(1, pagination.MaxPageSize))),
		"orderBy":     validation.Validate(r.OrderBy, validation.In(AttestationStatusOrderByMachineIDAsc).Error(fmt.Sprintf("must be %s", AttestationStatusOrderByMachineIDAsc))),
	}.Filter()
}

// ToProto converts a validated request.
func (r *APIAttestationFleetRequest) ToProto() *corev1.ListMachineAttestationStatusesRequest {
	fleet := &corev1.SpdmMachineAttestationFleetRequest{PageToken: r.PageToken}
	if r.PageSize != nil {
		fleet.PageSize = *r.PageSize
	}
	if r.Status != nil {
		status := r.Status.ToProto()
		fleet.AttestationStatus = &status
	}
	if r.MachineType != nil {
		machineType := corev1.MachineType_HOST
		if *r.MachineType == AttestationMachineTypeDpu {
			machineType = corev1.MachineType_DPU
		}
		fleet.MachineType = &machineType
	}
	return &corev1.ListMachineAttestationStatusesRequest{
		Target: &corev1.ListMachineAttestationStatusesRequest_Fleet{Fleet: fleet},
	}
}

// APIAttestationFleetPage is one cursor page of fleet attestation statuses.
type APIAttestationFleetPage struct {
	// Statuses are ordered by ascending machine ID
	Statuses []APIMachineAttestationStatus `json:"statuses"`
	// NextPageToken continues the listing; empty on the last page
	NextPageToken string `json:"nextPageToken"`
}

// FromProto sets the page from Core. Fleet rows are predictions, so a
// selection reason must be absent.
func (m *APIAttestationFleetPage) FromProto(p *corev1.ListMachineAttestationStatusesResponse) error {
	out := APIAttestationFleetPage{Statuses: make([]APIMachineAttestationStatus, 0, len(p.GetStatuses())), NextPageToken: p.GetNextPageToken()}
	for _, ps := range p.GetStatuses() {
		var row APIMachineAttestationStatus
		err := row.fromProto(ps)
		if err != nil {
			return err
		}
		if row.AttesterSelectionStatus != nil {
			return fmt.Errorf("fleet attestation status for %s has a live selection status", row.MachineID)
		}
		out.Statuses = append(out.Statuses, row)
	}
	*m = out
	return nil
}
