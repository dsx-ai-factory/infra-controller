// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package controller implements the Kubernetes Service reconciliation logic
// for machine-a-tron mock BMC endpoints.
package controller

import (
	"context"
	"fmt"
	"hash/fnv"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/rs/zerolog"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/util/intstr"

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
func (b *ServiceBuilder) BuildServicesFromStatus(status *matclient.MachinesStatusResponse, podName string) []*corev1.Service {
	var services []*corev1.Service

	for _, machine := range status.Machines {
		// Build service for the host only after DHCP has assigned its BMC IP.
		// BuildService sets spec.clusterIP from the BMC IP, so a Service built
		// without one would be given an arbitrary ClusterIP by the API server.
		if machine.BMC.IP != nil && *machine.BMC.IP != "" {
			services = append(services, b.BuildService(&machine, MachineTypeHost, "", podName))
		}

		// Build services for DPUs under the same BMC IP gate.
		for _, dpu := range machine.DPUs {
			if dpu.BMC.IP != nil && *dpu.BMC.IP != "" {
				services = append(services, b.BuildService(&dpu, MachineTypeDPU, machine.MatID, podName))
			}
		}
	}

	return services
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
	Errors    []error
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

	// Process recreates (delete then create for immutable field changes like ClusterIP)
	// Skip recreates if any fetch failed to prevent spurious Service removal
	if !fetchFailed && len(diff.Recreate) > 0 {
		recreated := r.processRecreatesConcurrently(ctx, diff.Recreate, &result)
		result.Recreated = recreated
	}

	// Process creates concurrently
	if len(diff.Create) > 0 {
		created := r.processCreatesConcurrently(ctx, diff.Create, &result)
		result.Created = created
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

			if err := r.k8sClient.Delete(ctx, r.serviceBuilder.Namespace, svc.Name); err != nil {
				r.logger.Error().Err(err).Str("service", svc.Name).Msg("failed to delete service for recreate")
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("deleting service %s for recreate: %w", svc.Name, err))
				errMu.Unlock()
				return
			}
			// Clear ResourceVersion for create
			svc.ResourceVersion = ""
			if err := r.k8sClient.Create(ctx, svc); err != nil {
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

// processCreatesConcurrently creates services using a worker pool.
func (r *Reconciler) processCreatesConcurrently(ctx context.Context, services []*corev1.Service, result *ReconcileResult) int {
	var created int64
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

			if err := r.k8sClient.Create(ctx, svc); err != nil {
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("creating service %s: %w", svc.Name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&created, 1)
			}
		}(svc)
	}

	wg.Wait()
	return int(created)
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
