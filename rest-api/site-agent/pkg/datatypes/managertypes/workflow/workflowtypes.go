// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflowtypes

import (
	"sync"

	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/worker"
	"go.uber.org/atomic"
)

// State - temporal state
type State struct {
	mu sync.RWMutex
	// ConnectionAttempted the number of times the connection has been attempted
	ConnectionAttempted atomic.Uint64
	// ConnectionSucc the number of times the connection has succeded
	ConnectionSucc atomic.Uint64
	// HealthStatus current health state
	HealthStatus atomic.Uint64
	// Err is error message
	err string
	// ConnectionTime time when attempted to connect
	connectionTime string
}

// SetErr records the last Temporal connection error.
func (s *State) SetErr(err string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.err = err
}

// Err returns the last Temporal connection error.
func (s *State) Err() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.err
}

// SetConnectionTime records the last Temporal connection attempt time.
func (s *State) SetConnectionTime(connectionTime string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.connectionTime = connectionTime
}

// ConnectionTime returns the last Temporal connection attempt time.
func (s *State) ConnectionTime() string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.connectionTime
}

// MgrState - Mgr state
type MgrState struct {
	// WflowStarted the number of times the Wflow has started
	WflowStarted atomic.Uint64
	// WflowActFail the number of times the Wflow Activity has failed
	WflowActFail atomic.Uint64
	// WflowActSucc the number of times the Wflow Activity has succeded
	WflowActSucc atomic.Uint64
	// WflowPubFail the number of times the Wflow Publishing has failed
	WflowPubFail atomic.Uint64
	// WflowPubSucc the number of times the Wflow Publishing has succeded
	WflowPubSucc atomic.Uint64
}

// Workflow - workflow data
type Workflow struct {
	ID                          string
	Name                        string
	Namespace                   string
	Temporal                    Temporal
	WorkflowFunctions           []interface{}
	State                       *State
	VpcState                    *MgrState
	VpcPrefixState              *MgrState
	SubnetState                 *MgrState
	InstanceState               *MgrState
	MachineState                *MgrState
	TenantState                 *MgrState
	SSHKeyGroupState            *MgrState
	InfiniBandPartitionState    *MgrState
	OperatingSystemState        *MgrState
	MachineValidationState      *MgrState
	InstanceTypeState           *MgrState
	NetworkSecurityGroupState   *MgrState
	ExpectedMachineState        *MgrState
	ExpectedPowerShelfState     *MgrState
	ExpectedRackState           *MgrState
	ExpectedSwitchState         *MgrState
	SKUState                    *MgrState
	DpuExtensionServiceState    *MgrState
	NVLinkLogicalPartitionState *MgrState
	VpcPeeringState             *MgrState
	SpectrumXPartitionState     *MgrState
	TenantIdentityState         *MgrState
}

// Temporal datastructure
type Temporal struct {
	Publisher  client.Client
	Subscriber client.Client
	Worker     worker.Worker
}

// NewWorkflowInstance - new instance
func NewWorkflowInstance() *Workflow {
	// Initialize the necessary values and return
	return &Workflow{
		State:                       &State{},
		VpcState:                    &MgrState{},
		VpcPrefixState:              &MgrState{},
		SubnetState:                 &MgrState{},
		InstanceState:               &MgrState{},
		SSHKeyGroupState:            &MgrState{},
		MachineState:                &MgrState{},
		TenantState:                 &MgrState{},
		InfiniBandPartitionState:    &MgrState{},
		OperatingSystemState:        &MgrState{},
		MachineValidationState:      &MgrState{},
		InstanceTypeState:           &MgrState{},
		NetworkSecurityGroupState:   &MgrState{},
		ExpectedMachineState:        &MgrState{},
		ExpectedPowerShelfState:     &MgrState{},
		ExpectedRackState:           &MgrState{},
		ExpectedSwitchState:         &MgrState{},
		SKUState:                    &MgrState{},
		DpuExtensionServiceState:    &MgrState{},
		NVLinkLogicalPartitionState: &MgrState{},
		VpcPeeringState:             &MgrState{},
		SpectrumXPartitionState:     &MgrState{},
		TenantIdentityState:         &MgrState{},
	}
}
