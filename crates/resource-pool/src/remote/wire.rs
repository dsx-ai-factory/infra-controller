// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_resource_pool_api::v1;
use model::resource_pool::ResourcePoolValueDomain;
use model::resource_pool::allocation::{
    AllocationSource, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationFailure, AllocationFailureCode, AllocationResult,
    AllocationSelection, ProviderRouteTargetMode, ProviderRouteTargetPolicy,
};
use model::vpc::routing_profile::RouteTargetConfig;

use super::RemoteResourcePoolError;
use crate::value::{ValueConversionError, value_from_wire, value_to_wire};

pub(super) fn operation(
    record: &ResourcePoolAllocation,
) -> Result<v1::AllocationOperation, RemoteResourcePoolError> {
    let AllocationSource::Remote {
        binding_id,
        request,
        ..
    } = &record.source
    else {
        return Err(RemoteResourcePoolError::InvalidRecord(
            "allocation is not remote",
        ));
    };

    Ok(v1::AllocationOperation {
        pool: Some(v1::PoolBinding {
            site_id: request.site_id.to_string(),
            authority_id: request.authority_id.clone(),
            pool_id: request.pool_id.clone(),
            binding_id: binding_id.to_string(),
            logical_pool: record.pool_name.clone(),
        }),
        request_id: record.allocation_id.to_string(),
        owner: Some(v1::Owner {
            owner_type: record.owner_type.to_string(),
            owner_id: record.owner_id.clone(),
            slot: request.slot.clone(),
            generation: request.generation,
        }),
        eligibility_revision: request.eligibility_revision.clone(),
    })
}

pub(super) fn operation_id(
    record: &ResourcePoolAllocation,
) -> Result<v1::OperationIdentity, RemoteResourcePoolError> {
    let AllocationSource::Remote {
        binding_id,
        request,
        ..
    } = &record.source
    else {
        return Err(RemoteResourcePoolError::InvalidRecord(
            "allocation is not remote",
        ));
    };

    Ok(v1::OperationIdentity {
        pool: Some(v1::PoolBinding {
            site_id: request.site_id.to_string(),
            authority_id: request.authority_id.clone(),
            pool_id: request.pool_id.clone(),
            binding_id: binding_id.to_string(),
            logical_pool: record.pool_name.clone(),
        }),
        request_id: record.allocation_id.to_string(),
    })
}

pub(super) fn assignment(
    record: &ResourcePoolAllocation,
    allocation: v1::Allocation,
) -> Result<AllocationAssignment, RemoteResourcePoolError> {
    // Recover checks the queried pool and request ID. Its request omits the
    // owner and revision, so compare those with our saved intent before saving
    // an assignment that a foreground caller could attach.
    if allocation.operation.as_ref() != Some(&operation(record)?) {
        return Err(RemoteResourcePoolError::InvalidResponse(
            "allocation operation differs from the saved request",
        ));
    }
    let assignment_mode = match v1::AssignmentMode::try_from(allocation.assignment_mode) {
        Ok(v1::AssignmentMode::Automatic) => AssignmentMode::Automatic,
        Ok(v1::AssignmentMode::Manual) => AssignmentMode::Manual,
        _ => {
            return Err(RemoteResourcePoolError::InvalidResponse(
                "allocation assignment mode is missing or unknown",
            ));
        }
    };
    let allocated_value = allocation
        .value
        .ok_or(RemoteResourcePoolError::InvalidResponse(
            "allocated value is required",
        ))?;
    let assignment = AllocationAssignment {
        allocation_id: allocation.allocation_id,
        value: match allocated_value.kind.as_ref() {
            Some(v1::value::Kind::Integer(_)) => ResourcePoolValue::Integer(value_from_wire(
                ResourcePoolValueDomain::Integer,
                &allocated_value,
            )?),
            Some(v1::value::Kind::Ipv4(_) | v1::value::Kind::Ipv6(_)) => {
                ResourcePoolValue::IpAddress(value_from_wire(
                    ResourcePoolValueDomain::IpAddress,
                    &allocated_value,
                )?)
            }
            Some(v1::value::Kind::Ipv6Prefix(_)) => ResourcePoolValue::Ipv6Prefix(value_from_wire(
                ResourcePoolValueDomain::Ipv6Prefix,
                &allocated_value,
            )?),
            None => {
                return Err(RemoteResourcePoolError::InvalidResponse(
                    "allocated value kind is required",
                ));
            }
        },
        assignment_mode,
        route_target_policy: allocation
            .route_target_policy
            .map(route_target_policy)
            .transpose()?,
    };
    validate_selection(&super::parameters(record)?.selection, &assignment)?;
    Ok(assignment)
}

pub(super) fn validate_selection(
    selection: &AllocationSelection,
    assignment: &AllocationAssignment,
) -> Result<(), RemoteResourcePoolError> {
    let requested_value = match selection {
        AllocationSelection::Automatic => {
            if assignment.assignment_mode != AssignmentMode::Automatic {
                return Err(RemoteResourcePoolError::InvalidResponse(
                    "automatic allocation returned a manual assignment",
                ));
            }
            None
        }
        AllocationSelection::Requested(value) => {
            if assignment.assignment_mode != AssignmentMode::Manual {
                return Err(RemoteResourcePoolError::InvalidResponse(
                    "requested allocation returned an automatic assignment",
                ));
            }
            Some(value)
        }
        AllocationSelection::Exact(value) => Some(value),
    };
    if let Some(requested_value) = requested_value
        && requested_value != &assignment.value
    {
        return Err(RemoteResourcePoolError::InvalidResponse(
            "allocated value differs from the saved selection",
        ));
    }
    Ok(())
}

pub(super) fn failure(failure: v1::Failure) -> Result<AllocationFailure, RemoteResourcePoolError> {
    let code = match v1::FailureCode::try_from(failure.code) {
        Ok(v1::FailureCode::InvalidArgument) => AllocationFailureCode::InvalidArgument,
        Ok(v1::FailureCode::ResourceExhausted) => AllocationFailureCode::ResourceExhausted,
        Ok(v1::FailureCode::FailedPrecondition) => AllocationFailureCode::FailedPrecondition,
        _ => {
            return Err(RemoteResourcePoolError::InvalidResponse(
                "saved failure code is missing or unknown",
            ));
        }
    };
    Ok(AllocationFailure {
        code,
        message: failure.message,
    })
}

pub(super) fn release(
    record: &ResourcePoolAllocation,
) -> Result<v1::ReleaseRequest, RemoteResourcePoolError> {
    let operation = operation(record)?;
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &record.source
    else {
        return Err(RemoteResourcePoolError::InvalidRecord(
            "release requires a saved assignment",
        ));
    };
    Ok(v1::ReleaseRequest {
        pool: operation.pool,
        allocation_id: assignment.allocation_id.clone(),
        owner: operation.owner,
    })
}

pub(super) fn value(value: &ResourcePoolValue) -> Result<v1::Value, ValueConversionError> {
    match value {
        ResourcePoolValue::Integer(value) => value_to_wire(ResourcePoolValueDomain::Integer, value),
        ResourcePoolValue::IpAddress(value) => {
            value_to_wire(ResourcePoolValueDomain::IpAddress, value)
        }
        ResourcePoolValue::Ipv6Prefix(value) => {
            value_to_wire(ResourcePoolValueDomain::Ipv6Prefix, value)
        }
    }
}

fn route_target_policy(
    policy: v1::RouteTargetPolicy,
) -> Result<ProviderRouteTargetPolicy, RemoteResourcePoolError> {
    let mode = match v1::RouteTargetMode::try_from(policy.mode) {
        Ok(v1::RouteTargetMode::PreserveGenerated) => ProviderRouteTargetMode::Preserve,
        Ok(v1::RouteTargetMode::ReplaceGenerated) => ProviderRouteTargetMode::Replace,
        Err(_) => {
            return Err(RemoteResourcePoolError::InvalidResponse(
                "route target mode is unknown",
            ));
        }
    };
    let targets = |targets: Vec<v1::RouteTarget>| {
        targets
            .into_iter()
            .map(|target| RouteTargetConfig {
                asn: target.asn,
                vni: target.vni,
            })
            .collect()
    };
    Ok(ProviderRouteTargetPolicy {
        mode,
        imports: policy.imports.map(|imports| targets(imports.values)),
        exports: policy.exports.map(|exports| targets(exports.values)),
        peer_identity: targets(policy.peer_identity),
    })
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{check_cases, scenarios};
    use chrono::Utc;
    use model::resource_pool::OwnerType;
    use model::resource_pool::allocation::AllocationState;
    use model::resource_pool::remote::{CompletionAuthority, RemoteAllocationParameters};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn requests_preserve_saved_identity() {
        let record = ResourcePoolAllocation {
            allocation_id: Uuid::from_u128(1),
            pool_name: "segment-vni".into(),
            owner_type: OwnerType::NetworkSegment,
            owner_id: "segment-42".into(),
            state: AllocationState::ReleaseRequested,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source: AllocationSource::Remote {
                binding_id: Uuid::from_u128(2),
                request: Box::new(RemoteAllocationParameters {
                    site_id: Uuid::from_u128(3),
                    authority_id: "fabric-authority".into(),
                    pool_id: "fabric-vnis".into(),
                    slot: "service-vni".into(),
                    generation: u64::MAX,
                    eligibility_revision: "revision-7".into(),
                    selection: AllocationSelection::Automatic,
                    completion_authority: CompletionAuthority::Controller {
                        desire_id: Uuid::from_u128(4),
                    },
                }),
                result: Some(AllocationResult::Allocated(AllocationAssignment {
                    allocation_id: "provider-grant-17".into(),
                    value: ResourcePoolValue::Integer(4096),
                    assignment_mode: AssignmentMode::Automatic,
                    route_target_policy: None,
                })),
            },
        };

        // Keep the expected wire values independent of the conversion helpers,
        // so omitted or swapped fields cannot pass by matching the same mistake.
        let expected_pool = v1::PoolBinding {
            site_id: "00000000-0000-0000-0000-000000000003".into(),
            authority_id: "fabric-authority".into(),
            pool_id: "fabric-vnis".into(),
            binding_id: "00000000-0000-0000-0000-000000000002".into(),
            logical_pool: "segment-vni".into(),
        };
        let expected_owner = v1::Owner {
            owner_type: "network_segment".into(),
            owner_id: "segment-42".into(),
            slot: "service-vni".into(),
            generation: u64::MAX,
        };

        assert_eq!(
            operation(&record).unwrap(),
            v1::AllocationOperation {
                pool: Some(expected_pool.clone()),
                request_id: "00000000-0000-0000-0000-000000000001".into(),
                owner: Some(expected_owner.clone()),
                eligibility_revision: "revision-7".into(),
            }
        );
        assert_eq!(
            operation_id(&record).unwrap(),
            v1::OperationIdentity {
                pool: Some(expected_pool.clone()),
                request_id: "00000000-0000-0000-0000-000000000001".into(),
            }
        );
        assert_eq!(
            release(&record).unwrap(),
            v1::ReleaseRequest {
                pool: Some(expected_pool),
                allocation_id: "provider-grant-17".into(),
                owner: Some(expected_owner),
            }
        );
    }

    #[test]
    fn assignment_checks_saved_selection_and_assignment_mode() {
        use AllocationSelection::{Automatic, Exact, Requested};
        use ResourcePoolValue::{Integer, IpAddress, Ipv6Prefix};

        let convert = |(selection, value, assignment_mode): (
            AllocationSelection,
            v1::Value,
            v1::AssignmentMode,
        )| {
            let record = ResourcePoolAllocation {
                allocation_id: Uuid::new_v4(),
                pool_name: "vpc-vni".into(),
                owner_type: OwnerType::Vpc,
                owner_id: "vpc-owner".into(),
                state: AllocationState::Pending,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                source: AllocationSource::Remote {
                    binding_id: Uuid::new_v4(),
                    request: Box::new(RemoteAllocationParameters {
                        site_id: Uuid::new_v4(),
                        authority_id: "fabric".into(),
                        pool_id: "vni".into(),
                        slot: "vni".into(),
                        generation: 0,
                        eligibility_revision: "initial".into(),
                        selection,
                        completion_authority: CompletionAuthority::Controller {
                            desire_id: Uuid::new_v4(),
                        },
                    }),
                    result: None,
                },
            };
            assignment(
                &record,
                v1::Allocation {
                    operation: Some(operation(&record).unwrap()),
                    allocation_id: "provider-allocation".into(),
                    value: Some(value),
                    assignment_mode: assignment_mode as i32,
                    route_target_policy: None,
                },
            )
            .map(|assignment| {
                (
                    assignment.allocation_id,
                    assignment.value,
                    assignment.assignment_mode,
                )
            })
            .map_err(|error| match error {
                RemoteResourcePoolError::InvalidResponse(reason) => reason,
                error => panic!("unexpected assignment error: {error:?}"),
            })
        };
        let address = Ipv4Addr::new(192, 0, 2, 7);
        let prefix = v1::Value {
            kind: Some(v1::value::Kind::Ipv6Prefix(v1::Ipv6Prefix {
                network: "2001:db8::".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
                prefix_len: 64,
            })),
        };

        scenarios!(run = convert;
            "matching selection accepts the assigned value and mode" {
                (Requested(Integer(7)), 7.into(), v1::AssignmentMode::Manual)
                    => Yields(("provider-allocation".into(), Integer(7), AssignmentMode::Manual)),
                (Exact(Integer(7)), 7.into(), v1::AssignmentMode::Manual)
                    => Yields(("provider-allocation".into(), Integer(7), AssignmentMode::Manual)),
            }
            "address and prefix assignments retain their value types" {
                (Automatic, address.into(), v1::AssignmentMode::Automatic)
                    => Yields(("provider-allocation".into(), IpAddress(address.into()), AssignmentMode::Automatic)),
                (Automatic, prefix, v1::AssignmentMode::Automatic)
                    => Yields(("provider-allocation".into(), Ipv6Prefix("2001:db8::/64".parse().unwrap()), AssignmentMode::Automatic)),
            }
            "selection requires its assignment mode" {
                (Automatic, 7.into(), v1::AssignmentMode::Manual)
                    => FailsWith("automatic allocation returned a manual assignment"),
                (Requested(Integer(7)), 7.into(), v1::AssignmentMode::Automatic)
                    => FailsWith("requested allocation returned an automatic assignment"),
            }
            "allocated value must match the saved selection" {
                (Requested(Integer(8)), 7.into(), v1::AssignmentMode::Manual)
                    => FailsWith("allocated value differs from the saved selection"),
                (Exact(Integer(8)), 7.into(), v1::AssignmentMode::Automatic)
                    => FailsWith("allocated value differs from the saved selection"),
            }
        );
    }

    #[test]
    fn routing_policy_conversion_preserves_presence_order_and_duplicates() {
        let targets = |values: &[u32]| v1::RouteTargets {
            values: values
                .iter()
                .map(|&vni| v1::RouteTarget { asn: 64512, vni })
                .collect(),
        };
        let expected = |values: &[u32]| {
            values
                .iter()
                .map(|&vni| RouteTargetConfig { asn: 64512, vni })
                .collect()
        };
        check_cases(
            [
                carbide_test_support::Case {
                    scenario: "omitted imports differ from empty exports",
                    input: v1::RouteTargetPolicy {
                        mode: v1::RouteTargetMode::PreserveGenerated as i32,
                        imports: None,
                        exports: Some(targets(&[])),
                        peer_identity: vec![],
                    },
                    expect: Yields(ProviderRouteTargetPolicy {
                        mode: ProviderRouteTargetMode::Preserve,
                        imports: None,
                        exports: Some(vec![]),
                        peer_identity: vec![],
                    }),
                },
                carbide_test_support::Case {
                    scenario: "replacement retains direction and peer target order",
                    input: v1::RouteTargetPolicy {
                        mode: v1::RouteTargetMode::ReplaceGenerated as i32,
                        imports: Some(targets(&[2, 1, 2])),
                        exports: Some(targets(&[1, 2, 1])),
                        peer_identity: targets(&[2, 2, 1]).values,
                    },
                    expect: Yields(ProviderRouteTargetPolicy {
                        mode: ProviderRouteTargetMode::Replace,
                        imports: Some(expected(&[2, 1, 2])),
                        exports: Some(expected(&[1, 2, 1])),
                        peer_identity: expected(&[2, 2, 1]),
                    }),
                },
            ],
            |policy| route_target_policy(policy).map_err(drop),
        );
    }
}
