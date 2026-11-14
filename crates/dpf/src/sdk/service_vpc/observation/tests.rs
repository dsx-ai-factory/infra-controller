// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::mem::discriminant;

use carbide_test_support::{Check, check_values};
use serde_json::json;

use super::*;
use crate::sdk::tests::service_vpc::observations::{child, node, registration, request, set};

/// Supplies canonical resources so each case isolates one identity, generation or withdrawal condition.
struct Snapshot {
    request: ServiceVpcChainRequest,
    registration: DPUServiceChain,
    node: Node,
    set: ServiceChainSet,
    children: Vec<ServiceChain>,
}

impl Snapshot {
    /// Builds one internally consistent snapshot so cases cannot pass because a prerequisite is missing.
    fn new(intent: ServiceVpcChainIntent) -> Self {
        Self::from_request(request(intent))
    }

    /// Builds current resources for a supplied request so history tests retain attachment identity.
    fn from_request(request: ServiceVpcChainRequest) -> Self {
        Self {
            registration: registration(&request),
            node: node(&request),
            set: set(&request),
            children: vec![child(&request)],
            request,
        }
    }
}

/// Exercises the pure classification steps used between ordered API reads without duplicating I/O tests.
fn inspect(snapshot: Snapshot) -> ServiceVpcChainObservation {
    let mut observed = ServiceVpcChainObservation::pending(&snapshot.request);
    observed.registration = resource_observation(&snapshot.registration.metadata);
    let expected = match prepare_registration(
        &snapshot.request,
        Some(&snapshot.registration),
        &snapshot.request.definition.namespace,
    ) {
        Ok((expected, _)) => expected,
        Err((state, reason)) => {
            observed.set_state(state, Some(reason));
            return observed;
        }
    };
    let nodes = index_nodes_by_dpu(vec![snapshot.node]);
    let sets = BTreeMap::from([(snapshot.request.definition.name.clone(), snapshot.set)]);
    if let Err((state, reason)) =
        validate_remote_snapshot(&snapshot.request, &expected, &nodes, &sets, &mut observed)
    {
        observed.set_state(state, Some(reason));
    } else {
        classify_children(
            &snapshot.request,
            &expected,
            snapshot.children.iter(),
            &mut observed,
        );
    }
    observed
}

/// Adds another slot's child on the same Node while retaining the shared service-interface port,
/// ensuring observations distinguish the slot's patch from interfaces used by the whole service.
fn add_sibling_slot_chain(snapshot: &mut Snapshot) {
    let mut sibling = snapshot.request.clone();
    let identity = DpfHelmChartIdentity::from_service_id(sibling.correlation.service_id);
    sibling.correlation.slot_index = 1;
    sibling.correlation.intent = ServiceVpcChainIntent::Active;
    sibling.definition.excluded_dpu_ids.clear();
    sibling.definition.name = identity.slot_chain_name(sibling.correlation.interface_ordinal, 1);
    sibling.definition.labels.insert(
        DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL.to_owned(),
        "1".to_owned(),
    );
    sibling.definition.template_labels = sibling.definition.labels.clone();
    sibling.definition.interface_selectors[1] = BTreeMap::from([(
        identity.slot_placement_label_key(sibling.correlation.interface_ordinal, 1),
        DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned(),
    )]);
    sibling.definition.node_selector_labels = sibling.definition.interface_selectors[1].clone();
    let mut sibling = child(&sibling);
    // Distinct management registrations and sets must not share their immutable ancestry stamps.
    sibling.metadata.name = Some("sibling-slot-child".to_owned());
    sibling.metadata.uid = Some("sibling-slot-child-uid".to_owned());
    sibling.metadata.labels.as_mut().expect("labels").insert(
        SERVICE_VPC_CHAIN_REGISTRATION_UID_LABEL.to_owned(),
        "sibling-registration-uid".to_owned(),
    );
    sibling
        .metadata
        .owner_references
        .as_mut()
        .expect("controller reference")[0]
        .uid = "sibling-set-uid".to_owned();
    snapshot.children.push(sibling);
}

/// Requires current per-child conditions and ancestry for activation, and a current exclusion
/// acknowledgement plus absence for withdrawal, preventing stale readiness or labels from proving completion.
#[test]
fn chain_evidence_filters_identity_generation_and_withdrawal_conditions() {
    type Change = fn(&mut Snapshot);
    let cases: Vec<(&'static str, Change, ServiceVpcChainState)> = vec![
        // Both child acknowledgements and immutable ancestry establish this Node's configuration.
        ("current child", |_| {}, ServiceVpcChainState::Converged),
        // Sharing the service-side interface does not connect another slot to this slot's VPC.
        (
            "sibling slot on the same Node",
            add_sibling_slot_chain,
            ServiceVpcChainState::Converged,
        ),
        // Withdrawal requires only this slot's chain absence, allowing a different slot to remain active.
        (
            "withdrawal with another slot still active",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children.clear();
                add_sibling_slot_chain(snapshot);
            },
            ServiceVpcChainState::Isolated,
        ),
        // Other Nodes may still be reconciling even after this child's configuration is current.
        (
            "set-wide acknowledgement pending",
            |snapshot| {
                snapshot
                    .set
                    .status
                    .as_mut()
                    .expect("status")
                    .conditions
                    .as_mut()
                    .expect("conditions")[0]
                    .status = "False".to_owned();
            },
            ServiceVpcChainState::Converged,
        ),
        // Ready alone cannot replace the SFC controller's condition-specific acknowledgement.
        (
            "stale reconciliation condition",
            |snapshot| {
                snapshot.children[0]
                    .status
                    .as_mut()
                    .expect("status")
                    .conditions
                    .as_mut()
                    .expect("conditions")[0]
                    .observed_generation = Some(1);
            },
            ServiceVpcChainState::PendingCreation,
        ),
        // Top-level observedGeneration cannot fill in a missing Ready acknowledgement generation.
        (
            "missing Ready generation",
            |snapshot| {
                snapshot.children[0]
                    .status
                    .as_mut()
                    .expect("status")
                    .conditions
                    .as_mut()
                    .expect("conditions")[1]
                    .observed_generation = None;
            },
            ServiceVpcChainState::PendingCreation,
        ),
        // A same-label child left by an earlier set cannot establish current ancestry.
        (
            "old set owner UID",
            |snapshot| {
                snapshot.children[0]
                    .metadata
                    .owner_references
                    .as_mut()
                    .expect("owner")[0]
                    .uid = "old-set-uid".to_owned();
            },
            ServiceVpcChainState::StaleIdentity,
        ),
        // A matching owner alone cannot replace the current management registration's template stamp.
        (
            "old registration stamp",
            |snapshot| {
                snapshot.children[0]
                    .metadata
                    .labels
                    .as_mut()
                    .expect("labels")
                    .insert(
                        SERVICE_VPC_CHAIN_REGISTRATION_UID_LABEL.to_owned(),
                        "old-registration-uid".to_owned(),
                    );
            },
            ServiceVpcChainState::StaleIdentity,
        ),
        // A ready child with an older selector annotation cannot establish the current registration's activation.
        (
            "old child selector annotation",
            |snapshot| {
                // Advance committed desire and its registration/set, retaining only the child's old annotation.
                snapshot.request.correlation.selector_version =
                    snapshot.request.correlation.selector_version.increment();
                snapshot.request.definition.selector_version =
                    Some(snapshot.request.correlation.selector_version);
                snapshot.registration = registration(&snapshot.request);
                snapshot.set = set(&snapshot.request);
            },
            ServiceVpcChainState::StaleIdentity,
        ),
        // Current readiness cannot establish convergence for a child with a different switch MTU.
        (
            "different child switches",
            |snapshot| {
                let mut changed = serde_json::to_value(&snapshot.children[0]).expect("child JSON");
                changed["spec"]["switches"][0]["serviceMTU"] = json!(9100);
                snapshot.children[0] = serde_json::from_value(changed).expect("changed child");
            },
            ServiceVpcChainState::StaleIdentity,
        ),
        // Deletion takes precedence over old positive conditions on the same child.
        (
            "deleting ready child",
            |snapshot| {
                snapshot.children[0].metadata.deletion_timestamp =
                    Some(serde_json::from_value(json!("2026-11-06T00:00:00Z")).expect("timestamp"));
            },
            ServiceVpcChainState::Deleting,
        ),
        // Cancellation before child creation needs no prior Ready observation after the exclusion ack.
        (
            "retirement before child creation",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children.clear();
            },
            ServiceVpcChainState::Isolated,
        ),
        // Aggregate generation and Ready cannot acknowledge the set controller's exclusion work.
        (
            "stale exclusion acknowledgement",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children.clear();
                snapshot
                    .set
                    .status
                    .as_mut()
                    .expect("status")
                    .conditions
                    .as_mut()
                    .expect("conditions")[0]
                    .observed_generation = Some(2);
            },
            ServiceVpcChainState::ExclusionPending,
        ),
        // A current acknowledgement for a different selector is not acknowledgement of this exclusion.
        (
            "remote exclusion missing",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children.clear();
                let mut changed = serde_json::to_value(&snapshot.set).expect("set JSON");
                changed["spec"]["nodeSelector"]["matchExpressions"]
                    .as_array_mut()
                    .expect("expressions")
                    .pop();
                snapshot.set = serde_json::from_value(changed).expect("set without exclusion");
            },
            ServiceVpcChainState::ExclusionPending,
        ),
        // The actual set must carry the current registration stamp even when no child is present.
        (
            "old set registration stamp",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children.clear();
                snapshot
                    .set
                    .metadata
                    .labels
                    .as_mut()
                    .expect("labels")
                    .insert(
                        SERVICE_VPC_CHAIN_REGISTRATION_UID_LABEL.to_owned(),
                        "old-registration-uid".to_owned(),
                    );
            },
            ServiceVpcChainState::StaleIdentity,
        ),
        // A wrong-owner leftover still blocks absence when interface selectors connect it to this endpoint.
        (
            "foreign withdrawal leftover",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                // Remove all ancestry clues while retaining the target slot's patch selector.
                snapshot.children[0].metadata.labels = None;
                snapshot.children[0].metadata.owner_references = None;
            },
            ServiceVpcChainState::StaleIdentity,
        ),
        // A still-present child drains independently of old selector stamps and readiness.
        (
            "pre-exclusion child still draining",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children[0].metadata.annotations = None;
                for condition in snapshot.children[0]
                    .status
                    .as_mut()
                    .expect("status")
                    .conditions
                    .as_mut()
                    .expect("conditions")
                {
                    condition.status = "False".to_owned();
                }
            },
            ServiceVpcChainState::Deleting,
        ),
        // Finalizer removal on an already deleting child can be normal completed cleanup.
        (
            "withdrawal deletion in progress",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children[0].metadata.deletion_timestamp =
                    Some(serde_json::from_value(json!("2026-11-06T00:00:00Z")).expect("timestamp"));
                snapshot.children[0].metadata.finalizers = None;
            },
            ServiceVpcChainState::Deleting,
        ),
        // API absence on a replacement Node cannot prove cleanup for the original Node UID.
        (
            "replacement Node with no children",
            |snapshot| {
                *snapshot = Snapshot::new(ServiceVpcChainIntent::Retiring);
                snapshot.children.clear();
                snapshot.node.metadata.uid = Some("replacement-node-uid".to_owned());
            },
            ServiceVpcChainState::StaleIdentity,
        ),
        // Selector correlation must match the exact definition before any remote evidence is consumed.
        (
            "stale selector correlation",
            |snapshot| {
                snapshot.request.correlation.selector_version =
                    snapshot.request.correlation.selector_version.increment();
            },
            ServiceVpcChainState::StaleIdentity,
        ),
    ];
    check_values(
        cases.into_iter().map(|(scenario, change, expected)| Check {
            scenario,
            input: change,
            expect: (discriminant(&expected), false),
        }),
        |change| {
            // Vary identity, generation or selector desire while untouched fields provide a valid current activation.
            let mut snapshot = Snapshot::new(ServiceVpcChainIntent::Active);
            change(&mut snapshot);

            // Classify through the same checks used between the SDK's ordered reads.
            let observed = inspect(snapshot);
            (
                discriminant(&observed.state),
                observed.cleanup_bypass_observed,
            )
        },
    );
}

/// Retains detected finalizer bypass when supplied again after disappearance, preventing a later
/// empty list from clearing a known unsupported cleanup path.
#[test]
fn known_cleanup_bypass_is_not_cleared_by_later_absence() {
    enum RemovedProtection {
        Set,
        Child,
        ChildWithDeletingSibling,
    }
    let cases = [
        // A historically acknowledged set cannot legitimately lose protection while non-deleting.
        ("set finalizer removed", RemovedProtection::Set),
        // Old child acknowledgement still proves protection existed even after its generation changed.
        (
            "child finalizer removed after reconciliation",
            RemovedProtection::Child,
        ),
        // Deletion of one leftover must not hide another relevant child's unsupported cleanup.
        (
            "child bypass alongside deletion",
            RemovedProtection::ChildWithDeletingSibling,
        ),
    ];
    check_values(
        cases.map(|(scenario, removed)| Check {
            scenario,
            input: removed,
            expect: (true, true, true, true),
        }),
        |removed| {
            // Remove cleanup protection after historical acknowledgement while the resource is non-deleting.
            let mut snapshot = Snapshot::new(ServiceVpcChainIntent::Active);
            if matches!(removed, RemovedProtection::Set) {
                // A newer set spec must not hide proof that finalizer protection previously existed.
                snapshot.set.metadata.finalizers = None;
                snapshot.set.metadata.generation = Some(4);
            } else {
                // A newer child generation also retains the earlier successful reconciliation history.
                snapshot.children[0].metadata.finalizers = None;
                snapshot.children[0].metadata.generation = Some(3);
            }

            if matches!(removed, RemovedProtection::ChildWithDeletingSibling) {
                // Add a separate deleting leftover while the original bypassed child remains non-deleting.
                let mut deleting = snapshot.children[0].clone();
                deleting.metadata.name = Some("deleting-leftover".to_owned());
                deleting.metadata.uid = Some("deleting-leftover-uid".to_owned());
                deleting.metadata.deletion_timestamp =
                    Some(serde_json::from_value(json!("2026-11-06T00:00:00Z")).expect("timestamp"));
                deleting.metadata.finalizers = Some(vec![CHAIN_FINALIZER.to_owned()]);
                snapshot.children.push(deleting);
            }

            // Retain this attachment's identity and inspect the unsupported path before disappearance.
            let mut retirement = snapshot.request.clone();
            let prior_set_generation = snapshot.set.metadata.generation.expect("set generation");
            let bypassed = inspect(snapshot);

            // Advance committed desire while retaining the same attachment, endpoint and Node baseline.
            retirement.correlation.intent = ServiceVpcChainIntent::Retiring;
            retirement.correlation.extension_services_config_version = retirement
                .correlation
                .extension_services_config_version
                .increment();
            retirement.correlation.network_config_version =
                retirement.correlation.network_config_version.increment();
            retirement.correlation.selector_version =
                retirement.correlation.selector_version.increment();
            retirement.definition.selector_version = Some(retirement.correlation.selector_version);
            retirement
                .definition
                .excluded_dpu_ids
                .push(retirement.correlation.dpu_id);
            retirement.cleanup_bypass_observed = bypassed.cleanup_bypass_observed;
            let mut later = Snapshot::from_request(retirement);
            later.registration.metadata.generation = Some(5);
            later.registration.metadata.resource_version = Some("100".to_owned());
            later.set.metadata.generation = Some(prior_set_generation + 1);
            later.set.metadata.resource_version = Some("101".to_owned());
            for condition in later
                .set
                .status
                .as_mut()
                .expect("status")
                .conditions
                .as_mut()
                .expect("conditions")
            {
                condition.observed_generation = Some(prior_set_generation + 1);
            }

            // Supply acknowledged absence after disappearance without clearing the retained bypass fact.
            later.children.clear();
            let later = inspect(later);

            // Absence cannot erase retained cleanup history or become isolation evidence.
            (
                matches!(bypassed.state, ServiceVpcChainState::StaleIdentity),
                bypassed.cleanup_bypass_observed,
                matches!(later.state, ServiceVpcChainState::StaleIdentity),
                later.cleanup_bypass_observed,
            )
        },
    );
}
