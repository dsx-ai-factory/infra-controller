// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"encoding/json"
	"errors"
	"maps"
	"slices"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var errFirmwareUnauthorized = errors.New("firmware client not authorized")

func firmwareCatalogRequest(c probeapi.ConfigResponse, vendor, model string, s FirmwareSettings) (*corev1.UpsertHostFirmwareConfigRequest, bool, error) {
	kind := corev1.HostFirmwareComponentType(corev1.HostFirmwareComponentType_value["HOST_FIRMWARE_COMPONENT_TYPE_"+strings.ToUpper(string(s.Firmware.Component))])
	ordering := map[corev1.HostFirmwareComponentType]bool{kind: true}
	current := false
	seen := map[corev1.HostFirmwareComponentType]bool{}
	for _, target := range c.NicoAPI.FirmwareTargets {
		if target.Vendor != vendor || target.Model != model {
			continue
		}
		if !target.ExplicitStart || target.ComponentName == nil {
			return nil, false, instanceIssue("firmware catalog requires known explicit start on every model component")
		}
		name := strings.ToUpper(*target.ComponentName)
		if alias := map[string]string{"HGXBMC": "HGX_BMC", "CPLDMB": "CPLD_MB", "CPLDPDB": "CPLD_PDB", "COMBINEDBMCUEFI": "COMBINED_BMC_UEFI"}[name]; alias != "" {
			name = alias
		}
		component := corev1.HostFirmwareComponentType(corev1.HostFirmwareComponentType_value["HOST_FIRMWARE_COMPONENT_TYPE_"+name])
		if component == 0 || seen[component] {
			return nil, false, instanceIssue("firmware catalog component ordering is unknown or ambiguous")
		}
		ordering[component] = true
		seen[component] = true
		if component == kind {
			current = target.Version == s.Firmware.TargetVersion
		}
	}
	var refs []*corev1.HostFirmwareComponentRef
	for _, component := range slices.Sorted(maps.Keys(ordering)) {
		refs = append(refs, &corev1.HostFirmwareComponentRef{Type: component})
	}
	body := &corev1.UpsertHostFirmwareConfigRequest{Vendor: vendor, Model: model, ExplicitStartNeeded: new(true), ComponentOrdering: refs, Components: []*corev1.UpsertHostFirmwareComponentConfig{{Type: kind, Firmware: []*corev1.HostFirmwareVersionConfig{{Version: s.Firmware.TargetVersion, Default: true, InstallOnlySpecified: true, Artifacts: []*corev1.HostFirmwareArtifact{{Url: s.Firmware.ArtifactURL, Sha256: new(s.Firmware.ArtifactSHA256)}}}}}}}
	return body, !current, nil
}

// Only effective targets are proved. No ownership of artifacts or catalog rows is claimed.
func firmwareOtherTargets(c probeapi.ConfigResponse, body *corev1.UpsertHostFirmwareConfigRequest) string {
	rows := slices.DeleteFunc(slices.Clone(c.NicoAPI.FirmwareTargets), func(t probeapi.FirmwareTarget) bool {
		return t.Vendor == body.Vendor && t.Model == body.Model && t.ComponentName != nil && strings.EqualFold(*t.ComponentName, strings.TrimPrefix(body.Components[0].Type.String(), "HOST_FIRMWARE_COMPONENT_TYPE_"))
	})
	b, _ := json.Marshal([]any{rows, c.NicoAPI.Runtime})
	return digest(b)
}

func (f *firmwareRun) catalog(ctx context.Context, w *writer.Writer, end time.Time, inv inventory.SiteInventory, dry bool) error {
	owners := firmwareOwnMappings(f.latest, inv, f.mat, f.t.cfg.Namespace)
	ids := map[string]string{}
	vendor, model := "", ""
	count := uint64(0)
	for _, id := range slices.Sorted(maps.Keys(f.latest.Rows[probeapi.FamilyHost])) {
		u := f.latest.Rows[probeapi.FamilyHost][id]
		if !firmwareStructural(f.latest, u, f.t.s) || owners[id] == "" || owners[id] == "ambiguous" {
			continue
		}
		if vendor != "" && (vendor != *u.Host.Vendor || model != *u.Host.Model) {
			return instanceIssue("firmware structural census contains multiple vendor/models")
		}
		vendor, model = *u.Host.Vendor, *u.Host.Model
		count++
		if uint64(len(ids)) < f.t.s.Firmware.N {
			ids[id] = firmwareIdentity(f.latest, u)
		}
	}
	f.detail["structuralEligibleCount"] = count
	if uint64(len(ids)) != f.t.s.Firmware.N {
		return instanceIssue("firmware N exceeds the structural eligible census")
	}
	body, needed, err := firmwareCatalogRequest(f.latest.Config, vendor, model, f.t.s)
	if err != nil {
		return err
	}
	f.detail["catalogWriteNeeded"] = needed
	if !needed {
		f.detail["catalogTargetPersists"] = true
	}
	baseline := f.latest.Config
	// Slow preparation precedes the side-effect-free final dispatch guard.
	if err = f.prepare(ctx, end, false); err != nil {
		return err
	}
	guard := func() error {
		if e := f.evidenceGuard(false); e != nil {
			return e
		}
		if len(f.latest.Rows[probeapi.FamilyHostFirmwareRequests]) != 0 {
			return instanceIssue("firmware requests appeared before catalog dispatch")
		}
		for _, u := range f.latest.Rows[probeapi.FamilyHost] {
			if u.Host == nil || !closedFirmwareWindow(u.Host.FirmwareWindow) {
				return instanceIssue("unknown or open firmware window before catalog dispatch")
			}
		}
		for id, identity := range ids {
			u := f.latest.Rows[probeapi.FamilyHost][id]
			if !firmwareStructural(f.latest, u, f.t.s) || firmwareIdentity(f.latest, u) != identity {
				return instanceIssue("firmware catalog cohort eligibility changed")
			}
		}
		return nil
	}
	if err = guard(); err != nil {
		return err
	}
	if dry || !needed {
		return nil
	}
	b, _ := json.Marshal(body)
	if err = w.Record(ctx, "firmwareCatalogPlan", map[string]any{"baseline": baseline.NicoAPI.FirmwareTargets, "component": f.t.s.Firmware.Component, "target": f.t.s.Firmware.TargetVersion, "requestDigest": digest(b), "identity": ids}); err != nil {
		return err
	}
	var receipt writer.Outcome
	key := "firmware-catalog-persistent"
	for attempt := 0; attempt < 2; attempt++ {
		if attempt > 0 {
			if err = f.prepare(ctx, end, false); err != nil {
				return err
			}
		}
		b, _ := json.Marshal(body)
		receipt, err = writer.Send(ctx, w, writer.Call{WriteCall: journal.WriteCall{Scope: "firmware", Kind: "firmware-catalog-persistent", Key: key, RequestDigest: digest(b)}, Retry: writer.RetryPolicy{MaxAttempts: 1, AttemptTimeout: time.Minute}, Cutoff: end, BeforeDispatch: guard}, func(ctx context.Context) writer.Attempt {
			return firmwareRPC(f.client.UpsertHostFirmwareConfig(ctx, body))
		})
		if err != nil {
			if errors.Is(err, writer.ErrCutoff) {
				return instanceObservationError(err)
			}
			return err
		}
		if err = w.Record(ctx, "firmwareCatalogReceipt", map[string]any{"op": receipt.Op, "outcome": map[writer.Kind]string{writer.Accepted: "accepted", writer.Unclear: "unclear", writer.Failed: "failed"}[receipt.Kind], "sentAt": jsonx.Time{Time: receipt.SentAt}}); err != nil {
			return err
		}
		if receipt.Kind != writer.Failed || receipt.Reason != journal.Grpc("InvalidArgument") {
			break
		}
		if attempt == 1 {
			return instanceIssue("firmware catalog ordering rejected: model has runtime components the probe cannot see; set the runtime ordering manually")
		}
		body.ComponentOrdering = []*corev1.HostFirmwareComponentRef{{Type: body.Components[0].Type}}
		key += "-selected-order"
	}
	if receipt.Kind == writer.Failed {
		if firmwareUnauthorized(receipt) {
			return instanceObservationError(errFirmwareUnauthorized)
		}
		return instanceIssue("firmware catalog request rejected: fixture catalog is incompatible")
	}
	f.detail["catalogOutcome"] = "unresolved"
	end = minTime(f.end, receipt.SentAt.Add(time.Duration(3*f.t.s.Observation.IntervalSeconds+f.t.cfg.Probe.PollSeconds)*time.Second))
	proofCtx, cancel := context.WithDeadline(ctx, end)
	defer cancel()
	var last error
	unresolved := func() error {
		cause := context.DeadlineExceeded
		if ctx.Err() != nil {
			cause = ctx.Err()
		}
		return instanceObservationError(errors.Join(instanceIssue("unresolved firmware catalog: reconcile before selecting a different target"), cause, last))
	}
	for {
		if err = w.Check(ctx); err != nil {
			if err == writer.ErrSoftStop && (errors.Is(ctx.Err(), context.DeadlineExceeded) || !time.Now().Before(end)) {
				return unresolved()
			}
			return err
		}
		if !time.Now().Before(end) {
			return unresolved()
		}
		c, e := f.t.p.Config(proofCtx, probe.ConfigFilter{})
		if e == nil && firmwareConfig(c, f.t.s.Observation.IntervalSeconds) == nil && c.Metrics.ProcessStartSeconds == baseline.Metrics.ProcessStartSeconds && c.NicoAPI.ReadStart.After(receipt.SentAt) {
			if firmwareOtherTargets(c, body) != firmwareOtherTargets(baseline, body) {
				return instanceIssue("unresolved firmware catalog: other effective targets changed")
			}
			_, stillNeeded, validation := firmwareCatalogRequest(c, vendor, model, f.t.s)
			if validation != nil {
				return validation
			}
			if !stillNeeded {
				var next *firmwareSnapshot
				next, e = readFirmware(proofCtx, f.t.p, end, f.t.s.Observation.IntervalSeconds, false, f.latest)
				if e == nil && firmwareConfigIdentity(next.Config) == firmwareConfigIdentity(c) {
					proof := map[string]any{"revision": c.NicoAPI.Revision, "readStart": c.NicoAPI.ReadStart, "readAt": c.NicoAPI.ReadAt, "target": f.t.s.Firmware.TargetVersion}
					if receipt.Kind == writer.Unclear {
						if err = w.SettleFirmwareCatalog(ctx, receipt.Op, proof); err != nil {
							return err
						}
					}
					if err = w.Record(ctx, "firmwareCatalogProof", proof); err != nil {
						return err
					}
					f.detail["catalogOutcome"] = "effective-target-proven"
					f.detail["catalogTargetPersists"] = true
					f.latest = next
					f.plan.Config = next.Config
					return nil
				}
			}
		}
		if errors.Is(proofCtx.Err(), context.DeadlineExceeded) {
			last = errors.Join(last, e)
			return unresolved()
		}
		if e != nil {
			var pe *probe.Error
			if !errors.Is(e, errFirmwareProvenance) && (!errors.As(e, &pe) || !pe.Transient()) {
				return instanceObservationError(e)
			}
			last = e
		}
		if waitInstance(proofCtx, f.t.cfg.Probe.PollSeconds) != nil {
			return unresolved()
		}
	}
}

func firmwareUnauthorized(r writer.Outcome) bool {
	return r.Kind == writer.Failed && (r.Reason == journal.Grpc("PermissionDenied") || r.Reason == journal.Grpc("Unauthenticated"))
}
