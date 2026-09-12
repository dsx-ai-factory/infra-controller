// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package controller implements the Kubernetes Service reconciliation logic
// for machine-a-tron mock BMC endpoints.
package controller

import (
	"context"
	"errors"
	"fmt"
	"hash/fnv"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/rs/zerolog"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/util/intstr"
	"k8s.io/apimachinery/pkg/util/wait"
	"k8s.io/client-go/util/retry"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/matclient"
)

const (
	// LabelManagedBy identifies the controller managing the resource.
	LabelManagedBy = "app.kubernetes.io/managed-by"
	// LabelManagedByValue is the value for the managed-by label.
	LabelManagedByValue = "mat-k8s-controller"

	// LabelPodName is the label that identifies which machine-a-tron pod owns this service.
	LabelPodName = "nvidia-infra-controller/pod-name"

	// LabelMatID is the machine-a-tron ID label.
	LabelMatID = "nvidia-infra-controller/mat-id"
	// LabelMachineID is the NICo machine ID label.
	LabelMachineID = "nvidia-infra-controller/mat-machine-id"
	// LabelMachineType distinguishes host vs dpu.
	LabelMachineType = "nvidia-infra-controller/mat-machine-type"
	// LabelParentMatID links DPUs to their parent host.
	LabelParentMatID = "nvidia-infra-controller/mat-parent-id"

	// AnnotationBMCIP is the BMC IP address annotation.
	AnnotationBMCIP = "nvidia-infra-controller/mat-bmc-ip"
	// AnnotationAPIState is the API state annotation.
	AnnotationAPIState = "nvidia-infra-controller/mat-api-state"
	// AnnotationPowerState is the power state annotation.
	AnnotationPowerState = "nvidia-infra-controller/mat-power-state"
	// AnnotationHardwareType is the hardware type annotation.
	AnnotationHardwareType = "nvidia-infra-controller/mat-hardware-type"
	// AnnotationRedfishListenPort is the Redfish listen port annotation.
	AnnotationRedfishListenPort = "nvidia-infra-controller/mat-redfish-listen-port"
	// AnnotationIPMIListenPort is the IPMI listen port annotation.
	AnnotationIPMIListenPort = "nvidia-infra-controller/mat-ipmi-listen-port"
	// AnnotationSSHListenPort is the SSH listen port annotation.
	AnnotationSSHListenPort = "nvidia-infra-controller/mat-ssh-listen-port"

	// MachineTypeHost is the machine type for hosts.
	MachineTypeHost = "host"
	// MachineTypeDPU is the machine type for DPUs.
	MachineTypeDPU = "dpu"

	// PortNameRedfish is the name of the Redfish port.
	PortNameRedfish = "redfish"
	// PortNameIPMI is the name of the IPMI port.
	PortNameIPMI = "ipmi"
	// PortNameSSH is the name of the SSH port.
	PortNameSSH = "ssh"

	// DefaultConcurrency is the default number of concurrent workers for K8s API calls.
	DefaultConcurrency = 50
)

// DefaultCreateBackoff bounds the in-cycle retries of a Service create that the
// API server rejected because the requested ClusterIP was still held by another
// Service. Addresses released by deletes running in the same cycle become
// available within this window; anything still held is retried next cycle.
var DefaultCreateBackoff = wait.Backoff{
	Steps:    4,
	Duration: 250 * time.Millisecond,
	Factor:   2.0,
	Jitter:   0.1,
}

// ServiceBuilder builds Kubernetes Services from machine status.
type ServiceBuilder struct {
	Namespace    string
	BaseSelector map[string]string
	// OwnerRefs maps pod names to their Deployment's OwnerReference.
	// Services are owned by the machine-a-tron Deployment they route to.
	OwnerRefs map[string]metav1.OwnerReference
}

// BuildServiceName generates a consistent service name for a machine.
func BuildServiceName(machineType, matID string) string {
	shortID := matID
	if len(matID) > 12 {
		shortID = fmt.Sprintf("%s-%s", matID[:12], shortHash(matID))
	}
	return fmt.Sprintf("mat-bmc-%s-%s", machineType, shortID)
}

func shortHash(s string) string {
	h := fnv.New32a()
	_, _ = h.Write([]byte(s))
	return fmt.Sprintf("%08x", h.Sum32())
}

// BuildService creates a Kubernetes Service for a machine's BMC.
// podName is used to create a pod-specific selector for multi-pod deployments.
func (b *ServiceBuilder) BuildService(machine *matclient.MachineStatus, machineType, parentMatID, podName string) *corev1.Service {
	name := BuildServiceName(machineType, machine.MatID)

	labels := map[string]string{
		LabelManagedBy:   LabelManagedByValue,
		LabelMatID:       machine.MatID,
		LabelMachineType: machineType,
	}
	if machine.MachineID != nil {
		labels[LabelMachineID] = *machine.MachineID
	}
	if parentMatID != "" {
		labels[LabelParentMatID] = parentMatID
	}

	annotations := map[string]string{
		AnnotationAPIState:          machine.APIState,
		AnnotationPowerState:        machine.PowerState,
		AnnotationRedfishListenPort: strconv.Itoa(int(machine.BMC.Redfish.ListenPort)),
	}
	if machine.BMC.IP != nil {
		annotations[AnnotationBMCIP] = *machine.BMC.IP
	}
	if machine.HardwareType != nil {
		annotations[AnnotationHardwareType] = *machine.HardwareType
	}

	ports := []corev1.ServicePort{
		{
			Name:       PortNameRedfish,
			Protocol:   corev1.ProtocolTCP,
			Port:       int32(machine.BMC.Redfish.ReachablePort),
			TargetPort: intstr.FromInt32(int32(machine.BMC.Redfish.ListenPort)),
		},
	}

	// Add IPMI port if available
	if machine.BMC.IPMI != nil {
		ports = append(ports, corev1.ServicePort{
			Name:       PortNameIPMI,
			Protocol:   corev1.ProtocolUDP,
			Port:       int32(machine.BMC.IPMI.ReachablePort),
			TargetPort: intstr.FromInt32(int32(machine.BMC.IPMI.ListenPort)),
		})
		annotations[AnnotationIPMIListenPort] = strconv.Itoa(int(machine.BMC.IPMI.ListenPort))
	}

	// Add SSH port if available
	if machine.BMC.SSH != nil {
		ports = append(ports, corev1.ServicePort{
			Name:       PortNameSSH,
			Protocol:   corev1.ProtocolTCP,
			Port:       int32(machine.BMC.SSH.ReachablePort),
			TargetPort: intstr.FromInt32(int32(machine.BMC.SSH.ListenPort)),
		})
		annotations[AnnotationSSHListenPort] = strconv.Itoa(int(machine.BMC.SSH.ListenPort))
	}

	// Build selector - include pod name for multi-pod deployments
	selector := make(map[string]string)
	for k, v := range b.BaseSelector {
		selector[k] = v
	}
	if podName != "" {
		selector[LabelPodName] = podName
	}

	svc := &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{
			Name:        name,
			Namespace:   b.Namespace,
			Labels:      labels,
			Annotations: annotations,
		},
		Spec: corev1.ServiceSpec{
			Type:     corev1.ServiceTypeClusterIP,
			Selector: selector,
			Ports:    ports,
		},
	}

	// Set owner reference to the machine-a-tron Deployment this service routes to.
	// Uses podName as key (empty string for single-pod mode).
	if b.OwnerRefs != nil {
		if ownerRef, ok := b.OwnerRefs[podName]; ok {
			svc.OwnerReferences = []metav1.OwnerReference{ownerRef}
		}
	}

	// Set ClusterIP to BMC IP for direct addressing
	if machine.BMC.IP != nil {
		svc.Spec.ClusterIP = *machine.BMC.IP
	}

	return svc
}

// BuildServicesFromStatus builds Services for all machines in the status response.
// podName is used to create pod-specific selectors for multi-pod deployments.
//
// Machines that have not reported a BMC IP yet are skipped. A Service created
// without an explicit ClusterIP is given an arbitrary address from the
// ServiceCIDR by the API server, and that address can collide with a BMC IP
// that NICo DHCP later leases to another device. The skipped machine gets its
// Service on the first cycle after its BMC IP is known.
func (b *ServiceBuilder) BuildServicesFromStatus(status *matclient.MachinesStatusResponse, podName string) []*corev1.Service {
	var services []*corev1.Service

	for _, machine := range status.Machines {
		// Build service for the host
		if hasBMCIP(&machine) {
			services = append(services, b.BuildService(&machine, MachineTypeHost, "", podName))
		}

		// Build services for DPUs
		for _, dpu := range machine.DPUs {
			if hasBMCIP(&dpu) {
				services = append(services, b.BuildService(&dpu, MachineTypeDPU, machine.MatID, podName))
			}
		}
	}

	return services
}

// hasBMCIP reports whether the machine has a BMC IP to pin the ClusterIP to.
func hasBMCIP(machine *matclient.MachineStatus) bool {
	return machine.BMC.IP != nil && *machine.BMC.IP != ""
}

// countMachines returns the number of hosts and DPUs in a status response.
func countMachines(status *matclient.MachinesStatusResponse) int {
	n := 0
	for _, machine := range status.Machines {
		n += 1 + len(machine.DPUs)
	}
	return n
}

// ServiceDiff represents the differences between desired and existing services.
type ServiceDiff struct {
	Create   []*corev1.Service
	Update   []*corev1.Service
	Recreate []*corev1.Service // Services that need delete+create due to immutable field changes
	Delete   []string
}

// ComputeServiceDiff calculates the differences between desired and existing services.
func ComputeServiceDiff(desired []*corev1.Service, existing []*corev1.Service) ServiceDiff {
	diff := ServiceDiff{}

	existingMap := make(map[string]*corev1.Service)
	for _, svc := range existing {
		existingMap[svc.Name] = svc
	}

	desiredMap := make(map[string]*corev1.Service)
	deduped := make([]*corev1.Service, 0, len(desired))
	for _, svc := range desired {
		if _, exists := desiredMap[svc.Name]; exists {
			continue
		}
		desiredMap[svc.Name] = svc
		deduped = append(deduped, svc)
	}

	// Find services to create or update
	for _, svc := range deduped {
		existingSvc, exists := existingMap[svc.Name]
		if !exists {
			diff.Create = append(diff.Create, svc)
		} else if needsUpdate(svc, existingSvc) {
			svc.ResourceVersion = existingSvc.ResourceVersion
			// Check if ClusterIP is changing (immutable field)
			if svc.Spec.ClusterIP != "" && existingSvc.Spec.ClusterIP != "" &&
				svc.Spec.ClusterIP != existingSvc.Spec.ClusterIP {
				// ClusterIP changed - need to delete and recreate
				diff.Recreate = append(diff.Recreate, svc)
			} else {
				// Preserve existing ClusterIP if not explicitly set
				if svc.Spec.ClusterIP == "" {
					svc.Spec.ClusterIP = existingSvc.Spec.ClusterIP
				}
				preserveForeignMetadata(svc, existingSvc)
				diff.Update = append(diff.Update, svc)
			}
		}
	}

	// Find services to delete (managed by us but no longer desired)
	for _, existing := range existing {
		if _, wanted := desiredMap[existing.Name]; !wanted {
			// Only delete if we manage this service
			if existing.Labels[LabelManagedBy] == LabelManagedByValue {
				diff.Delete = append(diff.Delete, existing.Name)
			}
		}
	}

	return diff
}

// needsUpdate checks if a service needs to be updated.
func needsUpdate(desired, existing *corev1.Service) bool {
	// Check ports
	if len(desired.Spec.Ports) != len(existing.Spec.Ports) {
		return true
	}
	for i, port := range desired.Spec.Ports {
		if i >= len(existing.Spec.Ports) {
			return true
		}
		existingPort := existing.Spec.Ports[i]
		if port.Name != existingPort.Name ||
			port.Port != existingPort.Port ||
			port.Protocol != existingPort.Protocol ||
			port.TargetPort.IntValue() != existingPort.TargetPort.IntValue() {
			return true
		}
	}

	// Check selector
	if len(desired.Spec.Selector) != len(existing.Spec.Selector) {
		return true
	}
	for k, v := range desired.Spec.Selector {
		if existing.Spec.Selector[k] != v {
			return true
		}
	}

	// Check labels owned by this controller, preserving foreign labels.
	for k, v := range desired.Labels {
		if existing.Labels[k] != v {
			return true
		}
	}
	for k := range existing.Labels {
		if isControllerLabel(k) {
			if _, exists := desired.Labels[k]; !exists {
				return true
			}
		}
	}

	// Check annotations owned by this controller, preserving foreign annotations.
	for k, v := range desired.Annotations {
		if existing.Annotations[k] != v {
			return true
		}
	}
	for k := range existing.Annotations {
		if isControllerAnnotation(k) {
			if _, exists := desired.Annotations[k]; !exists {
				return true
			}
		}
	}

	// Check ClusterIP change
	if desired.Spec.ClusterIP != "" && existing.Spec.ClusterIP != "" &&
		desired.Spec.ClusterIP != existing.Spec.ClusterIP {
		return true
	}

	// Check OwnerReferences - ensures services get properly owned by the machine-a-tron Deployment
	if len(desired.OwnerReferences) != len(existing.OwnerReferences) {
		return true
	}
	for i, ref := range desired.OwnerReferences {
		if i >= len(existing.OwnerReferences) {
			return true
		}
		existingRef := existing.OwnerReferences[i]
		if ref.APIVersion != existingRef.APIVersion ||
			ref.Kind != existingRef.Kind ||
			ref.Name != existingRef.Name ||
			ref.UID != existingRef.UID {
			return true
		}
	}

	return false
}

func preserveForeignMetadata(desired, existing *corev1.Service) {
	desired.Labels = mergeMetadata(existing.Labels, desired.Labels, isControllerLabel)
	desired.Annotations = mergeMetadata(existing.Annotations, desired.Annotations, isControllerAnnotation)
}

func mergeMetadata(existing, desired map[string]string, isControllerKey func(string) bool) map[string]string {
	merged := make(map[string]string, len(existing)+len(desired))
	for k, v := range existing {
		if !isControllerKey(k) {
			merged[k] = v
		}
	}
	for k, v := range desired {
		merged[k] = v
	}
	return merged
}

func isControllerLabel(k string) bool {
	switch k {
	case LabelManagedBy, LabelPodName, LabelMatID, LabelMachineID, LabelMachineType, LabelParentMatID:
		return true
	default:
		return false
	}
}

func isControllerAnnotation(k string) bool {
	switch k {
	case AnnotationBMCIP, AnnotationAPIState, AnnotationPowerState, AnnotationHardwareType, AnnotationRedfishListenPort, AnnotationIPMIListenPort, AnnotationSSHListenPort:
		return true
	default:
		return false
	}
}

// K8sServiceClient defines the interface for Kubernetes service operations.
type K8sServiceClient interface {
	List(ctx context.Context, namespace string, labelSelector string) ([]*corev1.Service, error)
	Get(ctx context.Context, namespace, name string) (*corev1.Service, error)
	Create(ctx context.Context, svc *corev1.Service) error
	Update(ctx context.Context, svc *corev1.Service) error
	Delete(ctx context.Context, namespace, name string) error
}

// ReconcileResult holds the results of a reconciliation cycle.
type ReconcileResult struct {
	Created   int
	Updated   int
	Deleted   int
	Recreated int
	// Adopted counts Services that already existed under the desired name but
	// were not in the managed List, and were brought under management in place.
	Adopted int
	Errors  []error
}

// Discovery is an interface for discovering machine-a-tron instances.
type Discovery interface {
	Discover(ctx context.Context) ([]DiscoveredInstance, error)
}

// StatusFetcher fetches machine status from a machine-a-tron instance.
// Used for testing; production code uses matclient.Client.
type StatusFetcher interface {
	GetMachinesStatus(ctx context.Context) (*matclient.MachinesStatusResponse, error)
}

// StatusFetcherFunc is a function type for creating StatusFetchers per URL.
type StatusFetcherFunc func(url string) (StatusFetcher, error)

// Closeable is an optional interface for StatusFetchers that need cleanup.
type Closeable interface {
	Close() error
}

// Reconciler reconciles Kubernetes Services with machine-a-tron machine status.
type Reconciler struct {
	discovery        Discovery
	serviceBuilder   *ServiceBuilder
	k8sClient        K8sServiceClient
	deploymentClient DeploymentClient
	clientOpts       []matclient.Option
	statusFetcher    StatusFetcherFunc // Optional, for testing. If nil, uses matclient.
	logger           zerolog.Logger
	concurrency      int
	// createBackoff bounds in-cycle retries of creates rejected for a held ClusterIP.
	createBackoff wait.Backoff

	// clientCache caches StatusFetcher instances by URL for connection reuse.
	// Entries are evicted when their URLs are absent from discovery.
	clientCache map[string]StatusFetcher
}

// DeploymentClient is an interface for fetching Deployments.
type DeploymentClient interface {
	Get(ctx context.Context, namespace, name string) (*metav1.OwnerReference, error)
}

// NewReconciler creates a new Reconciler.
func NewReconciler(
	discovery Discovery,
	serviceBuilder *ServiceBuilder,
	k8sClient K8sServiceClient,
	deploymentClient DeploymentClient,
	clientOpts []matclient.Option,
	logger zerolog.Logger,
) *Reconciler {
	return &Reconciler{
		discovery:        discovery,
		serviceBuilder:   serviceBuilder,
		k8sClient:        k8sClient,
		deploymentClient: deploymentClient,
		clientOpts:       clientOpts,
		logger:           logger,
		concurrency:      DefaultConcurrency,
		createBackoff:    DefaultCreateBackoff,
		clientCache:      make(map[string]StatusFetcher),
	}
}

// SetConcurrency sets the number of concurrent workers for K8s API calls.
func (r *Reconciler) SetConcurrency(n int) {
	if n > 0 {
		r.concurrency = n
	}
}

// getOrCreateClient returns a cached StatusFetcher or creates a new one.
func (r *Reconciler) getOrCreateClient(url string) (StatusFetcher, error) {
	if fetcher, ok := r.clientCache[url]; ok {
		return fetcher, nil
	}

	var fetcher StatusFetcher
	var err error
	if r.statusFetcher != nil {
		fetcher, err = r.statusFetcher(url)
	} else {
		fetcher, err = matclient.NewClient(url, r.clientOpts...)
	}
	if err != nil {
		return nil, err
	}

	r.clientCache[url] = fetcher
	return fetcher, nil
}

// Reconcile performs a full reconciliation cycle.
func (r *Reconciler) Reconcile(ctx context.Context) ReconcileResult {
	result := ReconcileResult{}

	// Discover machine-a-tron instances
	instances, err := r.discovery.Discover(ctx)
	if err != nil {
		result.Errors = append(result.Errors, fmt.Errorf("discovering instances: %w", err))
		return result
	}

	// Build set of discovered URLs for cache eviction (even if empty)
	discoveredURLs := make(map[string]struct{}, len(instances))
	for _, instance := range instances {
		discoveredURLs[instance.URL] = struct{}{}
	}

	// Evict cached clients whose URLs are no longer discovered
	for url, fetcher := range r.clientCache {
		if _, found := discoveredURLs[url]; !found {
			if c, ok := fetcher.(Closeable); ok {
				_ = c.Close()
			}
			delete(r.clientCache, url)
		}
	}

	if len(instances) == 0 {
		r.logger.Warn().Msg("no machine-a-tron instances discovered")
		return result
	}

	r.logger.Debug().
		Int("count", len(instances)).
		Msg("discovered machine-a-tron instances")

	// Look up owner references for each discovered machine-a-tron Deployment.
	// Service name is "<deployment>-bmc-mock", so Deployment name is Service name minus "-bmc-mock".
	// For single-pod mode (no pod-name label), we use empty string as the key.
	if r.deploymentClient != nil {
		r.serviceBuilder.OwnerRefs = make(map[string]metav1.OwnerReference)
		for _, instance := range instances {
			// Derive Deployment name from Service name (strip "-bmc-mock" suffix)
			deployName := strings.TrimSuffix(instance.ServiceName, "-bmc-mock")
			ownerRef, err := r.deploymentClient.Get(ctx, r.serviceBuilder.Namespace, deployName)
			if err != nil {
				r.logger.Warn().Err(err).
					Str("deployment", deployName).
					Str("pod", instance.PodName).
					Msg("failed to fetch owner Deployment, Services will not have owner reference")
			} else if ownerRef != nil {
				// Use PodName as key (empty string for single-pod mode)
				r.serviceBuilder.OwnerRefs[instance.PodName] = *ownerRef
				r.logger.Debug().
					Str("deployment", deployName).
					Str("pod", instance.PodName).
					Msg("using owner reference for garbage collection")
			}
		}
	}

	// Collect all desired services from all instances
	var allDesired []*corev1.Service
	fetchFailed := false

	for _, instance := range instances {
		fetcher, err := r.getOrCreateClient(instance.URL)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Errorf("creating client for %s: %w", instance.URL, err))
			fetchFailed = true
			continue
		}

		r.logger.Debug().
			Str("url", instance.URL).
			Msg("fetching machine status")

		status, err := fetcher.GetMachinesStatus(ctx)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Errorf("fetching status from %s: %w", instance.URL, err))
			fetchFailed = true
			continue
		}

		r.logger.Debug().
			Str("url", instance.URL).
			Str("pod", instance.PodName).
			Int("machines", len(status.Machines)).
			Msg("fetched machine status")

		services := r.serviceBuilder.BuildServicesFromStatus(status, instance.PodName)
		if skipped := countMachines(status) - len(services); skipped > 0 {
			r.logger.Info().
				Str("pod", instance.PodName).
				Int("skipped", skipped).
				Msg("machines without a BMC IP yet, no Service built for them")
		}
		allDesired = append(allDesired, services...)
	}

	r.logger.Info().
		Int("total_services", len(allDesired)).
		Msg("built desired services from all instances")

	// List existing services
	existing, err := r.k8sClient.List(ctx, r.serviceBuilder.Namespace,
		fmt.Sprintf("%s=%s", LabelManagedBy, LabelManagedByValue))
	if err != nil {
		result.Errors = append(result.Errors, fmt.Errorf("listing existing services: %w", err))
		return result
	}

	// Compute and apply diff
	diff := ComputeServiceDiff(allDesired, existing)

	r.logger.Info().
		Int("create", len(diff.Create)).
		Int("update", len(diff.Update)).
		Int("delete", len(diff.Delete)).
		Int("recreate", len(diff.Recreate)).
		Msg("computed service diff")

	// Process deletes first (needed for recreate to work)
	// Skip deletions if any fetch failed to prevent spurious Service removal
	if fetchFailed {
		r.logger.Warn().Msg("skipping deletions due to partial status-fetch failures")
	}

	// Process deletes concurrently
	if !fetchFailed && len(diff.Delete) > 0 {
		deleted := r.processDeletesConcurrently(ctx, diff.Delete, &result)
		result.Deleted = deleted
	}

	// Process recreates (delete then create for immutable field changes like ClusterIP).
	// A recreate is only computed for a device whose status was fetched this
	// cycle, so it runs even when another instance's fetch failed. Holding it
	// back would keep the Service on an address the device no longer has and
	// block that address for whichever device now holds it.
	if len(diff.Recreate) > 0 {
		recreated := r.processRecreatesConcurrently(ctx, diff.Recreate, &result)
		result.Recreated = recreated
	}

	// Process creates concurrently. The pre-cycle listing tells a create which
	// managed Service holds an address the API server refuses to allocate.
	if len(diff.Create) > 0 {
		created, adopted := r.processCreatesConcurrently(ctx, diff.Create, servicesByClusterIP(existing), &result)
		result.Created = created
		result.Adopted = adopted
	}

	// Process updates concurrently
	if len(diff.Update) > 0 {
		updated := r.processUpdatesConcurrently(ctx, diff.Update, &result)
		result.Updated = updated
	}

	return result
}

// processDeletesConcurrently deletes services using a worker pool.
func (r *Reconciler) processDeletesConcurrently(ctx context.Context, names []string, result *ReconcileResult) int {
	var deleted int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, name := range names {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(names)).
				Msg("delete progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(name string) {
			defer wg.Done()
			defer func() { <-sem }()

			if err := r.k8sClient.Delete(ctx, r.serviceBuilder.Namespace, name); err != nil {
				r.logger.Error().Err(err).Str("service", name).Msg("failed to delete service")
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("deleting service %s: %w", name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&deleted, 1)
			}
		}(name)
	}

	wg.Wait()
	return int(deleted)
}

// processRecreatesConcurrently handles services that need delete+create.
func (r *Reconciler) processRecreatesConcurrently(ctx context.Context, services []*corev1.Service, result *ReconcileResult) int {
	var recreated int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, svc := range services {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(services)).
				Msg("recreate progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(svc *corev1.Service) {
			defer wg.Done()
			defer func() { <-sem }()

			if err := r.k8sClient.Delete(ctx, r.serviceBuilder.Namespace, svc.Name); err != nil && !apierrors.IsNotFound(err) {
				r.logger.Error().Err(err).Str("service", svc.Name).Msg("failed to delete service for recreate")
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("deleting service %s for recreate: %w", svc.Name, err))
				errMu.Unlock()
				return
			}
			// Clear ResourceVersion for create. Recreates in this batch run
			// concurrently and may briefly hold each other's addresses, which
			// the retry inside createService absorbs. No holder index is passed
			// because those Services are legitimately mid-move.
			svc.ResourceVersion = ""
			if _, err := r.createService(ctx, svc, nil); err != nil {
				r.logger.Error().Err(err).Str("service", svc.Name).Msg("failed to create service after delete")
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("creating service %s after recreate delete: %w", svc.Name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&recreated, 1)
			}
		}(svc)
	}

	wg.Wait()
	return int(recreated)
}

// processCreatesConcurrently creates services using a worker pool. holders
// indexes the managed Services listed at the start of the cycle by ClusterIP,
// see createService. It returns the number of Services created and adopted.
func (r *Reconciler) processCreatesConcurrently(ctx context.Context, services []*corev1.Service, holders map[string]*corev1.Service, result *ReconcileResult) (int, int) {
	var created, adopted int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, svc := range services {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(services)).
				Int64("created", atomic.LoadInt64(&created)).
				Msg("create progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(svc *corev1.Service) {
			defer wg.Done()
			defer func() { <-sem }()

			wasAdopted, err := r.createService(ctx, svc, holders)
			switch {
			case err != nil:
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("creating service %s: %w", svc.Name, err))
				errMu.Unlock()
			case wasAdopted:
				r.logger.Info().Str("service", svc.Name).Msg("adopted existing service")
				atomic.AddInt64(&adopted, 1)
			default:
				atomic.AddInt64(&created, 1)
			}
		}(svc)
	}

	wg.Wait()
	return int(created), int(adopted)
}

// createService creates svc and resolves the two API server rejections that
// otherwise leave a device without a Service until a later cycle:
//
//   - AlreadyExists: a Service with this name exists but was not returned by
//     the managed List (it lacks the managed-by label, or it was created between
//     the List and this call). It is brought under management by adoptService.
//   - Invalid on spec.clusterIPs, "provided IP is already allocated": another
//     Service holds the requested BMC IP. If holders shows a managed Service
//     occupying the address without owning it, that Service is deleted (see
//     evictClusterIPHolder), and the create is retried with backoff so an
//     address released during this cycle can be taken.
//
// The returned bool reports whether an existing Service was adopted rather than
// created. Any other error is returned unchanged for the caller to record.
func (r *Reconciler) createService(ctx context.Context, svc *corev1.Service, holders map[string]*corev1.Service) (bool, error) {
	backoff := r.createBackoff
	if backoff.Steps < 1 {
		backoff.Steps = 1
	}

	adopted := false
	evicted := false
	err := retry.OnError(backoff, isRetriableCreateError, func() error {
		err := r.k8sClient.Create(ctx, svc)
		switch {
		case err == nil:
			adopted = false
			return nil
		case apierrors.IsAlreadyExists(err):
			adopted = true
			return r.adoptService(ctx, svc)
		case isClusterIPConflict(err) && !evicted:
			evicted = true
			r.evictClusterIPHolder(ctx, svc, holders)
			return err
		default:
			return err
		}
	})
	return adopted && err == nil, err
}

// adoptService brings a Service that already exists under the desired name
// under management. ClusterIP is immutable, so when the existing address
// differs from the desired BMC IP the Service is deleted and created again.
// Otherwise the desired spec and controller-owned metadata are written over
// it, keeping foreign labels and annotations, so the Service ends up carrying
// the same annotations a freshly created one would.
func (r *Reconciler) adoptService(ctx context.Context, desired *corev1.Service) error {
	return retry.RetryOnConflict(retry.DefaultRetry, func() error {
		existing, err := r.k8sClient.Get(ctx, desired.Namespace, desired.Name)
		if err != nil {
			return err
		}

		if desired.Spec.ClusterIP != "" && existing.Spec.ClusterIP != "" &&
			desired.Spec.ClusterIP != existing.Spec.ClusterIP {
			r.logger.Info().
				Str("service", desired.Name).
				Str("existing_cluster_ip", existing.Spec.ClusterIP).
				Str("cluster_ip", desired.Spec.ClusterIP).
				Msg("existing service has a different ClusterIP, replacing it")
			if err := r.k8sClient.Delete(ctx, desired.Namespace, desired.Name); err != nil && !apierrors.IsNotFound(err) {
				return err
			}
			desired.ResourceVersion = ""
			return r.k8sClient.Create(ctx, desired)
		}

		if desired.Spec.ClusterIP == "" {
			desired.Spec.ClusterIP = existing.Spec.ClusterIP
		}
		desired.ResourceVersion = existing.ResourceVersion
		preserveForeignMetadata(desired, existing)
		return r.k8sClient.Update(ctx, desired)
	})
}

// evictClusterIPHolder deletes the managed Service that holds svc's requested
// ClusterIP when that Service does not own the address: its recorded BMC IP
// annotation differs from its ClusterIP, which is the signature of a Service
// created before its device had reported a BMC IP and given an arbitrary
// address by the API server. The holder is re-read first so a Service that
// has since moved to another address is never touched. A holder whose BMC IP
// is the address is left alone: two devices reporting the same BMC IP is a
// data problem that must surface as an error rather than be resolved here.
func (r *Reconciler) evictClusterIPHolder(ctx context.Context, svc *corev1.Service, holders map[string]*corev1.Service) {
	holder, ok := holders[svc.Spec.ClusterIP]
	if !ok || holder.Name == svc.Name {
		return
	}

	current, err := r.k8sClient.Get(ctx, r.serviceBuilder.Namespace, holder.Name)
	if err != nil {
		if !apierrors.IsNotFound(err) {
			r.logger.Error().Err(err).Str("service", holder.Name).Msg("failed to read service holding a requested ClusterIP")
		}
		return
	}
	if current.Spec.ClusterIP != svc.Spec.ClusterIP {
		return
	}
	if current.Annotations[AnnotationBMCIP] == current.Spec.ClusterIP {
		r.logger.Error().
			Str("service", svc.Name).
			Str("cluster_ip", svc.Spec.ClusterIP).
			Str("held_by", current.Name).
			Msg("requested ClusterIP is the BMC IP of another managed service")
		return
	}

	if err := r.k8sClient.Delete(ctx, r.serviceBuilder.Namespace, current.Name); err != nil && !apierrors.IsNotFound(err) {
		r.logger.Error().Err(err).Str("service", current.Name).Msg("failed to delete service holding a ClusterIP it does not own")
		return
	}
	r.logger.Info().
		Str("service", svc.Name).
		Str("cluster_ip", svc.Spec.ClusterIP).
		Str("deleted", current.Name).
		Msg("deleted service holding a ClusterIP it does not own")
}

// isRetriableCreateError reports whether a failed create should be attempted
// again within the cycle: the requested ClusterIP was still held by another
// Service, or the Service under the same name changed between our calls.
func isRetriableCreateError(err error) bool {
	return isClusterIPConflict(err) || apierrors.IsAlreadyExists(err) || apierrors.IsNotFound(err)
}

// isClusterIPConflict reports whether err is the API server rejecting a create
// because the requested ClusterIP is held by another Service. The allocator
// reports this as an Invalid error whose cause is on spec.clusterIPs with the
// message "failed to allocate IP <ip>: provided IP is already allocated".
// Other spec.clusterIPs rejections, such as an address outside the
// ServiceCIDR, are configuration errors and are not treated as conflicts.
func isClusterIPConflict(err error) bool {
	var statusErr *apierrors.StatusError
	if !apierrors.IsInvalid(err) || !errors.As(err, &statusErr) {
		return false
	}
	details := statusErr.ErrStatus.Details
	if details == nil {
		return false
	}
	for _, cause := range details.Causes {
		if (cause.Field == "spec.clusterIPs" || cause.Field == "spec.clusterIP") &&
			strings.Contains(cause.Message, "already allocated") {
			return true
		}
	}
	return false
}

// servicesByClusterIP indexes Services by the address they hold.
func servicesByClusterIP(services []*corev1.Service) map[string]*corev1.Service {
	byIP := make(map[string]*corev1.Service, len(services))
	for _, svc := range services {
		if svc.Spec.ClusterIP != "" && svc.Spec.ClusterIP != corev1.ClusterIPNone {
			byIP[svc.Spec.ClusterIP] = svc
		}
	}
	return byIP
}

// processUpdatesConcurrently updates services using a worker pool.
func (r *Reconciler) processUpdatesConcurrently(ctx context.Context, services []*corev1.Service, result *ReconcileResult) int {
	var updated int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, svc := range services {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(services)).
				Int64("updated", atomic.LoadInt64(&updated)).
				Msg("update progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(svc *corev1.Service) {
			defer wg.Done()
			defer func() { <-sem }()

			if err := r.k8sClient.Update(ctx, svc); err != nil {
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("updating service %s: %w", svc.Name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&updated, 1)
			}
		}(svc)
	}

	wg.Wait()
	return int(updated)
}
