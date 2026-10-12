// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflowtypes

import (
	"sync"
	"time"

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
	connectionTime time.Time
	// worker is the Temporal worker the latest connection attempt started. It is
	// nil before the first attempt and while an attempt is in progress.
	worker *WorkerStatus
}

// SetWorker records the Temporal worker the latest connection attempt started.
func (s *State) SetWorker(worker *WorkerStatus) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.worker = worker
}

// Worker returns the Temporal worker the latest connection attempt started.
func (s *State) Worker() *WorkerStatus {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.worker
}

// WorkerStatus is the outcome of one attempt to start the Temporal worker.
type WorkerStatus struct {
	// Clients are the Temporal clients the worker was started with, kept here so
	// health checks can use them while a reload replaces the Temporal clients.
	Clients []client.Client
	mu      sync.RWMutex
	err     error
}

// NewWorkerStatus returns the status of a worker started with the given clients.
func NewWorkerStatus(clients ...client.Client) *WorkerStatus {
	return &WorkerStatus{Clients: clients}
}

// SetErr records why the worker failed to start or stopped.
func (w *WorkerStatus) SetErr(err error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.err = err
}

// Err returns why the worker failed to start or stopped, or nil while it runs.
func (w *WorkerStatus) Err() error {
	w.mu.RLock()
	defer w.mu.RUnlock()
	return w.err
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
func (s *State) SetConnectionTime(connectionTime time.Time) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.connectionTime = connectionTime
}

// ConnectionTime returns the last Temporal connection attempt time, which is zero before
// the first attempt.
func (s *State) ConnectionTime() time.Time {
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
	ExpectedRackGroupState      *MgrState
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
		ExpectedRackGroupState:      &MgrState{},
		ExpectedSwitchState:         &MgrState{},
		SKUState:                    &MgrState{},
		DpuExtensionServiceState:    &MgrState{},
		NVLinkLogicalPartitionState: &MgrState{},
		VpcPeeringState:             &MgrState{},
		SpectrumXPartitionState:     &MgrState{},
		TenantIdentityState:         &MgrState{},
	}
}
