// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"google.golang.org/protobuf/types/known/emptypb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APISpectrumXMultiplaneRange selects inclusive, ascending rail or plane bounds.
type APISpectrumXMultiplaneRange struct {
	Start *int `json:"start"`
	End   *int `json:"end"`
}

// APIMachineSpectrumXMultiplaneUpdateRequest changes the Cartesian product of
// exactly one rail selector and one plane selector. True disables the links.
type APIMachineSpectrumXMultiplaneUpdateRequest struct {
	MachineID       string                       `json:"-"`
	Rail            *int                         `json:"rail,omitempty"`
	AllRails        *bool                        `json:"allRails,omitempty"`
	RailRange       *APISpectrumXMultiplaneRange `json:"railRange,omitempty"`
	Plane           *int                         `json:"plane,omitempty"`
	AllPlanes       *bool                        `json:"allPlanes,omitempty"`
	PlaneRange      *APISpectrumXMultiplaneRange `json:"planeRange,omitempty"`
	MaintenanceMode *bool                        `json:"maintenanceMode"`
	// Reference omission or null preserves the reference; an empty string clears it.
	Reference *string `json:"reference,omitempty"`
}

// Validate rejects missing, ambiguous, or out-of-bounds selections before proxying.
func (r *APIMachineSpectrumXMultiplaneUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Rail, validation.By(r.validateRailSelector)),
		validation.Field(&r.Plane, validation.By(r.validatePlaneSelector)),
		validation.Field(&r.MaintenanceMode, validation.NotNil.Error("maintenanceMode is required")),
	)
}

func (r *APIMachineSpectrumXMultiplaneUpdateRequest) validateRailSelector(_ interface{}) error {
	return validateMultiplaneSelection(r.Rail, r.AllRails, r.RailRange, 3)
}

func (r *APIMachineSpectrumXMultiplaneUpdateRequest) validatePlaneSelector(_ interface{}) error {
	return validateMultiplaneSelection(r.Plane, r.AllPlanes, r.PlaneRange, 7)
}

func validateMultiplaneSelection(index *int, all *bool, bounds *APISpectrumXMultiplaneRange, maximum int) error {
	count := 0
	for _, present := range []bool{index != nil, all != nil, bounds != nil} {
		if present {
			count++
		}
	}
	if count != 1 {
		return fmt.Errorf("exactly one individual, all, or range selector is required")
	}
	if all != nil && !*all {
		return fmt.Errorf("all selector must be true")
	}
	if index != nil {
		return validation.Validate(index, validation.Min(0), validation.Max(maximum))
	}
	if bounds != nil {
		err := validation.ValidateStruct(bounds,
			validation.Field(&bounds.Start, validation.NotNil, validation.Min(0), validation.Max(maximum)),
			validation.Field(&bounds.End, validation.NotNil, validation.Min(0), validation.Max(maximum)),
		)
		if err != nil {
			return err
		}
		if *bounds.Start > *bounds.End {
			return validation.Errors{"end": fmt.Errorf("must be greater than or equal to start")}
		}
	}
	return nil
}

// ToProto preserves explicit zero selections, false maintenance mode, and reference presence.
func (r APIMachineSpectrumXMultiplaneUpdateRequest) ToProto() *corev1.MachineSpxMultiplaneMaintenanceUpdateRequest {
	req := &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest{
		MachineId: &corev1.MachineId{Id: r.MachineID}, MaintenanceMode: r.MaintenanceMode, Reference: r.Reference,
	}
	switch {
	case r.Rail != nil:
		req.RailSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_Rail{Rail: uint32(*r.Rail)}
	case r.AllRails != nil:
		req.RailSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_AllRails{AllRails: &emptypb.Empty{}}
	case r.RailRange != nil:
		req.RailSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_RailRange{RailRange: r.RailRange.ToProto()}
	}
	switch {
	case r.Plane != nil:
		req.PlaneSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_Plane{Plane: uint32(*r.Plane)}
	case r.AllPlanes != nil:
		req.PlaneSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_AllPlanes{AllPlanes: &emptypb.Empty{}}
	case r.PlaneRange != nil:
		req.PlaneSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_PlaneRange{PlaneRange: r.PlaneRange.ToProto()}
	}
	return req
}

// ToProto converts already-validated inclusive bounds.
func (r APISpectrumXMultiplaneRange) ToProto() *corev1.SpxMultiplaneRange {
	start, end := uint32(*r.Start), uint32(*r.End)
	return &corev1.SpxMultiplaneRange{Start: &start, End: &end}
}

// APIMachineSpectrumXMultiplane contains desired host configuration and separate DPU observations.
type APIMachineSpectrumXMultiplane struct {
	MachineID   string                                   `json:"machineId"`
	Interfaces  []APIMachineSpectrumXMultiplaneInterface `json:"interfaces"`
	DPUStatuses []APIMachineSpectrumXMultiplaneDPUStatus `json:"dpuStatuses"`
}

// APIMachineSpectrumXMultiplaneInterface is one declared rail/plane's desired configuration.
type APIMachineSpectrumXMultiplaneInterface struct {
	LogicalIfname             string  `json:"logicalIfname"`
	Rail                      uint32  `json:"rail"`
	Plane                     uint32  `json:"plane"`
	ConfiguredMaintenanceMode *bool   `json:"configuredMaintenanceMode"`
	Reference                 *string `json:"reference"`
	Revision                  string  `json:"revision"`
}

// APIMachineSpectrumXMultiplaneDPUStatus keeps readback separate for each associated BF4 DPU.
type APIMachineSpectrumXMultiplaneDPUStatus struct {
	DPUMachineID string                                     `json:"dpuMachineId"`
	Interfaces   []APIMachineSpectrumXMultiplaneObservation `json:"interfaces"`
}

// APIMachineSpectrumXMultiplaneObservation describes an attempt, which may precede the desired revision.
type APIMachineSpectrumXMultiplaneObservation struct {
	LogicalIfname   string  `json:"logicalIfname"`
	MaintenanceMode *bool   `json:"maintenanceMode"`
	Revision        string  `json:"revision"`
	Netdev          *string `json:"netdev"`
	Error           *string `json:"error"`
	ObservedAt      *string `json:"observedAt"`
}

// FromProto copies the authoritative response, retaining null values and empty arrays.
func (m *APIMachineSpectrumXMultiplane) FromProto(in *corev1.MachineSpxMultiplane) {
	m.MachineID = in.GetMachineId().GetId()
	m.Interfaces = make([]APIMachineSpectrumXMultiplaneInterface, 0, len(in.GetInterfaces()))
	for _, item := range in.GetInterfaces() {
		var iface APIMachineSpectrumXMultiplaneInterface
		iface.FromProto(item)
		m.Interfaces = append(m.Interfaces, iface)
	}
	m.DPUStatuses = make([]APIMachineSpectrumXMultiplaneDPUStatus, 0, len(in.GetDpuStatuses()))
	for _, item := range in.GetDpuStatuses() {
		var status APIMachineSpectrumXMultiplaneDPUStatus
		status.FromProto(item)
		m.DPUStatuses = append(m.DPUStatuses, status)
	}
}

// FromProto copies a declared interface without inferring a configured mode from observations.
func (m *APIMachineSpectrumXMultiplaneInterface) FromProto(in *corev1.MachineSpxMultiplaneInterface) {
	*m = APIMachineSpectrumXMultiplaneInterface{}
	if in == nil {
		return
	}
	m.LogicalIfname, m.Rail, m.Plane = in.GetLogicalIfname(), in.GetRail(), in.GetPlane()
	m.ConfiguredMaintenanceMode, m.Reference, m.Revision = in.CfgMaintenanceMode, in.Reference, in.GetRevision()
}

// FromProto copies each DPU's observations in the order supplied by the site.
func (m *APIMachineSpectrumXMultiplaneDPUStatus) FromProto(in *corev1.MachineSpxMultiplaneDpuStatus) {
	m.DPUMachineID = in.GetDpuMachineId().GetId()
	m.Interfaces = make([]APIMachineSpectrumXMultiplaneObservation, 0, len(in.GetInterfaces()))
	for _, item := range in.GetInterfaces() {
		var observation APIMachineSpectrumXMultiplaneObservation
		observation.FromProto(item)
		m.Interfaces = append(m.Interfaces, observation)
	}
}

// FromProto preserves unknown readback as null rather than treating it as maintenance off.
func (m *APIMachineSpectrumXMultiplaneObservation) FromProto(in *corev1.AstraMultiplaneInterfaceStatus) {
	*m = APIMachineSpectrumXMultiplaneObservation{}
	if in == nil {
		return
	}
	m.LogicalIfname, m.Revision = in.GetLogicalIfname(), in.GetRevision()
	m.MaintenanceMode, m.Netdev, m.Error = in.MaintenanceMode, in.Netdev, in.Error
	m.ObservedAt = cutil.ProtoTimePtrToStrPtr(in.GetObservedAt())
}
