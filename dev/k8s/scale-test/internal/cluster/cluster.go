// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package cluster holds the Kubernetes reads and writes the runner needs. v1
// uses the Lease in the run's own namespace for the site lock; ConfigMap
// calls arrive with their first user in a later change. MAT inventory reads
// the Pods and the MAT Services.
package cluster

import (
	"context"
	"fmt"
	"net/netip"
	"time"
)

// MatService is a Service the MAT controller made for one simulated device.
type MatService struct {
	Name        string
	MachineType string
	// MachineID is label nvidia-infra-controller/mat-machine-id. Missing on
	// per-pod Services.
	MachineID    string
	MatID        string
	BmcIP        netip.Addr
	HardwareType string
	ExternalIPs  []netip.Addr
}

// PodInfo is one Pod as the readers need it.
type PodInfo struct {
	Name        string
	UID         string // a pod recreated under its name has a new one
	Labels      map[string]string
	Annotations map[string]string
	Phase       string
	Ready       bool
	// CreatedAt is metadata.creationTimestamp, else the Unix epoch.
	CreatedAt time.Time
	// Restarts sums every container and init container, saturating.
	Restarts uint32
	IP       string
}

// Lease is a Lease as the site lock sees it. Empty strings and zero times
// are unset.
type Lease struct {
	Name            string
	ResourceVersion string
	// UID is set on every Lease the API returns. A replace sends it, so the
	// API refuses the write once the Lease was deleted or made again.
	UID                  string
	HolderIdentity       string
	LeaseDurationSeconds int32
	AcquireTime          time.Time
	RenewTime            time.Time
	Annotations          map[string]string
}

// LeaseReplaced is what a Lease replace did. Created means the API answered
// 201: no Lease was there and the replace made one, so the Lease the write
// meant was gone. Lease is nil when a 201 reply did not carry it whole.
type LeaseReplaced struct {
	Created bool
	Lease   *Lease
}

type Kind string

const (
	Conflict      Kind = "conflict"
	AlreadyExists Kind = "alreadyExists"
	Forbidden     Kind = "forbidden"
	API           Kind = "api"
	// Transport means no answer came: the request may have reached the API,
	// and the same call may work when tried again.
	Transport Kind = "transport"
	// Invalid means refused before it was sent, or an answer that is not
	// valid. A write answered this way may have landed.
	Invalid Kind = "invalid"
	Timeout Kind = "timeout"
)

// Error keeps a status code and a known reason or a fixed text. Server and
// client text never reach it.
type Error struct {
	Kind Kind
	Code int
	// Text is a known Kubernetes reason for Forbidden and API, and a fixed
	// text for Transport and Invalid.
	Text string
	Secs int64
}

func (e Error) Error() string {
	switch e.Kind {
	case Conflict:
		return "conflict: the object changed since it was read"
	case AlreadyExists:
		return "the object already exists"
	case Forbidden:
		return "forbidden: " + e.Text
	case API:
		return fmt.Sprintf("kubernetes API error %d: %s", e.Code, e.Text)
	case Transport:
		return "kubernetes API unreachable: " + e.Text
	case Invalid:
		return "kubernetes API call invalid: " + e.Text
	}
	return fmt.Sprintf("kubernetes API call timed out after %d s", e.Secs)
}

func (e Error) KindText() string { return string(e.Kind) }

// CallTimeout bounds any single Kubernetes call.
const CallTimeout = 10 * time.Second

// Cluster bounds every call itself, so callers such as the site lock add no
// timeout of their own.
type Cluster interface {
	// GetLease returns nil when the Lease is missing.
	GetLease(ctx context.Context, name string) (*Lease, error)
	// CreateLease fails with AlreadyExists when another process made it first.
	CreateLease(ctx context.Context, l Lease) (Lease, error)
	// ReplaceLease fails with Conflict on a stale ResourceVersion, or on a UID
	// the stored Lease does not have, also when no Lease is stored.
	ReplaceLease(ctx context.Context, l Lease) (LeaseReplaced, error)
}

// ListMatServices and ListPods are the MAT inventory reads. They are methods
// of the concrete *Kube, not of the Cluster interface: the inventory package
// depends on its own narrow interface, so adding them here would force every
// Cluster implementation (such as a test fake) to carry them.
