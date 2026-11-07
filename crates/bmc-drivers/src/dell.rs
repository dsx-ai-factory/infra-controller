/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Dell iDRAC mechanics shared by several capabilities.

use std::collections::BTreeMap;

use bmc_platform::{
    BootInterfaceSelector, DriverOutcome, OpCx, OperationReference, PlatformError, VendorJobId,
};
use nv_redfish::core::{ActionError, Bmc, ModificationResponse, RedfishSettings};
use nv_redfish::oem::dell::DellManager;
use nv_redfish::oem::dell::attributes::{AttributesUpdate, DellAttributes, DellAttributesUpdate};
use nv_redfish::oem::dell::schema::dell_lc_service::GetRemoteServicesApiStatusResponseLcStatus as LcStatus;
use nv_redfish::oem::dell::schema::oem_manager::{
    ManagerImportSystemConfigurationAction, ShareParametersUpdate, ShutdownType,
};
pub(crate) use nv_redfish::oem::dell::schema::settings::ApplyTime as ManagerApplyTime;
use nv_redfish::oem::dell::schema::{
    ActionAnnotations as DellActionAnnotations,
    SettingsApplyTimeUpdate as DellSettingsApplyTimeUpdate,
};
use nv_redfish::oem::oem_value;
use nv_redfish::schema::SettingsApplyTimeUpdate;
use nv_redfish::schema::network_device_function::NetworkDeviceFunction as NetworkDeviceFunctionSchema;
use nv_redfish::schema::settings::ApplyTime;
use serde_json::Value;

use crate::resources::{RedfishResourcesExt as _, bios_update, dynamic_properties};

/// The iDRAC attribute that blocks configuration changes while enabled.
pub(crate) const SYSTEM_LOCKDOWN: &str = "Lockdown.1.SystemLockdown";

/// The Dell resources the selected Manager advertises.
async fn dell_manager<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DellManager<B>, PlatformError> {
    cx.manager()
        .await?
        .oem_dell()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

/// The selected Manager's iDRAC attributes.
pub(crate) async fn manager_attributes<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<DellAttributes<B>, PlatformError> {
    cx.manager()
        .await?
        .oem_dell_attributes()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

/// Escapes XML character data.
fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// `@Redfish.SettingsApplyTime` for settings iDRAC applies on the next reset.
pub(crate) fn on_reset() -> SettingsApplyTimeUpdate {
    SettingsApplyTimeUpdate::builder()
        .with_apply_time(ApplyTime::OnReset)
        .build()
}

/// Whether iDRAC rejected a write because an attribute is read-only
/// (MessageId `IDRAC.*.SYS410`).
pub(crate) fn is_read_only_attribute(error: &PlatformError) -> bool {
    matches!(
        error,
        PlatformError::Bmc {
            status: 400,
            message_id,
            message,
        } if message_id.as_deref().is_some_and(|id| id.ends_with("SYS410"))
            || message.contains("SYS410")
            || message.contains("read-only")
    )
}

fn function_matches(
    function: &NetworkDeviceFunctionSchema,
    selector: &BootInterfaceSelector,
) -> bool {
    match selector {
        BootInterfaceSelector::Mac(mac) => function
            .ethernet
            .as_ref()
            .and_then(|ethernet| ethernet.mac_address.as_ref().and_then(Option::as_ref))
            .is_some_and(|reported| reported.eq_ignore_ascii_case(&mac.to_string())),
        BootInterfaceSelector::InterfaceId(interface_id)
        | BootInterfaceSelector::Pair { interface_id, .. } => {
            *interface_id == function.id || interface_id.starts_with(&format!("{}-", function.id))
        }
    }
}

/// Maps a mutation response, reporting iDRAC jobs as vendor jobs.
///
/// iDRAC may locate a new job under the manager's `Jobs`, its `Oem/Dell/Jobs`
/// or the TaskService, but only the selected manager's `Oem/Dell/Jobs/{id}`
/// reports the Dell `JobState`, so jobs are polled there.
pub(crate) async fn job_outcome<B: Bmc, T>(
    cx: &OpCx<'_, B>,
    response: ModificationResponse<T>,
) -> Result<DriverOutcome, PlatformError> {
    let task = match response {
        ModificationResponse::Task(task) => task,
        other => return Ok(DriverOutcome::from(other)),
    };
    let retry_after_seconds = task.retry_after.map(|duration| duration.as_secs());
    let body_task = task.task;
    let location = task.location.0;
    let job_id = location
        .last_segment()
        .filter(|id| id.starts_with("JID_") || location.to_string().contains("/Oem/Dell/Jobs/"))
        .and_then(|id| id.parse::<VendorJobId>().ok());
    Ok(DriverOutcome::accepted(match job_id {
        Some(job_id) => {
            let jobs = dell_manager(cx)
                .await?
                .configuration_jobs()
                .ok_or(PlatformError::Unsupported)?;
            OperationReference::VendorJob {
                uri: format!("{}/{job_id}", jobs.odata_id()).into(),
                job_id,
                retry_after_seconds,
            }
        }
        None => OperationReference::RedfishTask {
            uri: location,
            retry_after_seconds,
            task: body_task,
        },
    }))
}

/// Deletes every queued iDRAC job.
///
/// iDRAC rejects new configuration jobs (`SYS011`) while any job is queued,
/// so every BIOS writer clears the queue first. The queue cannot be cleared
/// while system lockdown is enabled, which is reported as `LockedDown`.
pub(crate) async fn clear_job_queue<B: Bmc>(cx: &OpCx<'_, B>) -> Result<(), PlatformError>
where
    B::Error: ActionError,
{
    let system_lockdown = manager_attributes(cx)
        .await?
        .attribute(SYSTEM_LOCKDOWN)
        .and_then(|value| value.str_value().map(str::to_owned))
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("iDRAC attributes do not report {SYSTEM_LOCKDOWN}"),
        })?;
    if system_lockdown == "Enabled" {
        return Err(PlatformError::LockedDown);
    }
    dell_manager(cx)
        .await?
        .job_service()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .delete_job_queue("JID_CLEARALL")
        .await
        .map(drop)
        .map_err(|error| cx.map_redfish_error(error))
}

/// Fails unless the Lifecycle Controller is ready to accept provisioning
/// requests, which storage configuration needs.
pub(crate) async fn require_lifecycle_controller_ready<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<(), PlatformError>
where
    B::Error: ActionError,
{
    let response = dell_manager(cx)
        .await?
        .lc_service()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .remote_services_api_status()
        .await
        .map_err(|error| cx.map_redfish_error(error))?;
    let lc_status = match response {
        ModificationResponse::Entity(status) => status.lc_status,
        ModificationResponse::Task(_)
        | ModificationResponse::Empty
        | ModificationResponse::Message(_) => None,
    };
    match lc_status {
        Some(LcStatus::Ready) => Ok(()),
        other => Err(PlatformError::NotReady {
            message: format!(
                "the Lifecycle Controller is not ready to accept provisioning requests: {other:?}"
            ),
        }),
    }
}

/// Clears the UEFI setup password by importing a BIOS Server Configuration
/// Profile, for iDRAC firmware whose `Bios.ChangePassword` cannot clear a
/// password once set (dell/iDRAC-Redfish-Scripting#308).
pub(crate) async fn clear_uefi_password_via_import<B: Bmc>(
    cx: &OpCx<'_, B>,
    current_password: &str,
) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    let import_buffer = format!(
        r#"<SystemConfiguration><Component FQDD="BIOS.Setup.1-1"><Attribute Name="OldSetupPassword">{}</Attribute><Attribute Name="NewSetupPassword"></Attribute></Component></SystemConfiguration>"#,
        xml_escape(current_password)
    );
    let response = dell_manager(cx)
        .await?
        .import_system_configuration(&ManagerImportSystemConfigurationAction {
            redfish_annotations: DellActionAnnotations::default(),
            share_parameters: ShareParametersUpdate::builder()
                .with_target(vec!["BIOS".to_string()])
                .build(),
            import_buffer: Some(import_buffer),
            shutdown_type: Some(ShutdownType::Forced),
            host_power_state: None,
        })
        .await
        .map_err(|error| cx.map_redfish_error(error))?;
    job_outcome(cx, response).await
}

/// Creates the configuration job that applies staged BIOS settings on the next reset.
pub(crate) async fn create_bios_config_job<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<DriverOutcome, PlatformError> {
    let bios = cx.bios().await?;
    let settings = bios
        .raw()
        .settings_object()
        .ok_or(PlatformError::Unsupported)?;
    let response = dell_manager(cx)
        .await?
        .configuration_jobs()
        .ok_or(PlatformError::Unsupported)?
        .create_configuration_job(settings.id())
        .await
        .map_err(|error| cx.map_redfish_error(error))?;
    job_outcome(cx, response).await
}

/// Stages BIOS attributes as an iDRAC configuration job applied on the next reset.
pub(crate) async fn stage_bios_attributes<B: Bmc>(
    cx: &OpCx<'_, B>,
    attributes: &BTreeMap<String, Value>,
) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    clear_job_queue(cx).await?;
    let body = bios_update(attributes)?.with_settings_apply_time(on_reset());
    let response = cx.update_bios_settings(&body).await?;
    job_outcome(cx, response).await
}

/// The `Oem/Dell/DellNIC` object of the network device function `selector`
/// names, found under the chassis that shares the selected system's id.
///
/// A MAC matches the function's `Ethernet.MACAddress`; an interface id
/// matches the function's id or a partition of it (`NIC.Slot.7-1` for
/// `NIC.Slot.7-1-1`), which still resolves when the MAC has been stripped.
pub(crate) async fn nic<B: Bmc>(
    cx: &OpCx<'_, B>,
    selector: &BootInterfaceSelector,
) -> Result<serde_json::Map<String, Value>, PlatformError> {
    let system_id = cx.system().await?.raw().id.clone();
    let chassis =
        cx.chassis_by_id(&system_id)
            .await?
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: format!("no Chassis {system_id}"),
            })?;
    let adapters = chassis
        .network_adapters()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("Chassis {system_id} reports no NetworkAdapters"),
        })?;
    for adapter in adapters {
        let Some(functions) = adapter
            .network_device_functions()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
        else {
            continue;
        };
        let functions = functions
            .members()
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        if let Some(function) = functions
            .iter()
            .map(|function| function.raw())
            .find(|function| function_matches(function, selector))
        {
            return function
                .oem
                .as_ref()
                .and_then(|oem| oem_value(oem, "Dell"))
                .and_then(|dell| dell.get("DellNIC"))
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| PlatformError::InvalidResponse {
                    message: format!("network device function {} reports no DellNIC", function.id),
                });
        }
    }
    Err(PlatformError::InvalidResponse {
        message: format!("no network device function matches {selector:?}"),
    })
}

/// The NIC slot iDRAC's `HttpDev1Interface` names: the selector's interface
/// id, the slot of the NIC with its MAC, or empty on a host without a boot
/// interface.
pub(crate) async fn nic_slot<B: Bmc>(
    cx: &OpCx<'_, B>,
    selector: Option<&BootInterfaceSelector>,
) -> Result<String, PlatformError> {
    match selector {
        None => Ok(String::new()),
        Some(
            BootInterfaceSelector::InterfaceId(interface_id)
            | BootInterfaceSelector::Pair { interface_id, .. },
        ) => Ok(interface_id.clone()),
        Some(selector) => nic(cx, selector)
            .await?
            .get("Id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: "DellNIC reports no Id".to_string(),
            }),
    }
}

/// The values of `names` the selected Manager's iDRAC attributes report;
/// unreported attributes are omitted and null ones are `Value::Null`.
pub(crate) async fn manager_attribute_values<B: Bmc>(
    cx: &OpCx<'_, B>,
    names: &[&str],
) -> Result<BTreeMap<String, Value>, PlatformError> {
    let attributes = manager_attributes(cx).await?;
    Ok(names
        .iter()
        .filter_map(|name| {
            let attribute = attributes.attribute(name)?;
            let value = attribute
                .str_value()
                .map(Value::from)
                .or_else(|| attribute.bool_value().map(Value::from))
                .or_else(|| attribute.integer_value().map(Value::from))
                .or_else(|| attribute.decimal_value().map(Value::from))
                .unwrap_or(Value::Null);
            Some((name.to_string(), value))
        })
        .collect())
}

/// Writes iDRAC manager attributes, applied at `apply_time` when given.
pub(crate) async fn patch_manager_attributes<B: Bmc>(
    cx: &OpCx<'_, B>,
    attributes: &BTreeMap<String, Value>,
    apply_time: Option<ManagerApplyTime>,
) -> Result<DriverOutcome, PlatformError> {
    let mut body = DellAttributesUpdate::builder()
        .with_attributes(
            AttributesUpdate::builder()
                .with_dynamic_properties(dynamic_properties(attributes)?)
                .build(),
        )
        .build();
    if let Some(apply_time) = apply_time {
        body = body.with_settings_apply_time(
            DellSettingsApplyTimeUpdate::builder()
                .with_apply_time(apply_time)
                .build(),
        );
    }
    let response = manager_attributes(cx)
        .await?
        .update(&body)
        .await
        .map_err(|error| cx.map_redfish_error(error))?;
    job_outcome(cx, response).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nv_redfish::core::{AsyncTask, AsyncTaskLocation};
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body};

    #[test]
    fn a_function_matches_by_mac_or_by_interface_id_and_its_partitions() {
        let function: NetworkDeviceFunctionSchema = serde_json::from_value(json!({
            "@odata.id": "/redfish/v1/Chassis/System.Embedded.1/NetworkAdapters/NIC.Slot.7/NetworkDeviceFunctions/NIC.Slot.7-1",
            "Id": "NIC.Slot.7-1",
            "Name": "NIC.Slot.7-1",
            "Ethernet": {"MACAddress": "b8:e9:24:17:6d:72"},
        }))
        .expect("network device function");
        let interface = |id: &str| BootInterfaceSelector::InterfaceId(id.to_string());
        let mac = |mac: &str| BootInterfaceSelector::Mac(mac.parse().expect("MAC"));
        let cases = [
            ("same MAC in another case", mac("B8:E9:24:17:6D:72"), true),
            ("other MAC", mac("B8:E9:24:17:6D:73"), false),
            ("the function itself", interface("NIC.Slot.7-1"), true),
            ("one of its partitions", interface("NIC.Slot.7-1-1"), true),
            ("a longer slot number", interface("NIC.Slot.7-10"), false),
        ];
        for (name, selector, expected) in cases {
            assert_eq!(function_matches(&function, &selector), expected, "{name}");
        }
    }

    fn task(location: &str) -> ModificationResponse<Value> {
        ModificationResponse::Task(AsyncTask {
            location: AsyncTaskLocation(location.to_string().into()),
            retry_after: Some(Duration::from_secs(7)),
            task: None,
        })
    }

    #[tokio::test]
    async fn job_ids_are_polled_in_the_managers_dell_job_queue() {
        const MANAGER: &str = "/redfish/v1/Managers/iDRAC.Embedded.1";
        let bmc = Fixture::new(
            "Dell",
            "Integrated Dell Remote Access Controller",
            "System.Embedded.1",
            "iDRAC.Embedded.1",
        )
        .document(
            MANAGER,
            json!({
                "@odata.id": MANAGER,
                "Id": "iDRAC.Embedded.1",
                "Name": "Manager",
                "Links": {"Oem": {"Dell": {
                    "Jobs": {"@odata.id": format!("{MANAGER}/Oem/Dell/Jobs")},
                }}},
            }),
        )
        .build()
        .await;
        let cx = bmc.cx().await;

        assert_eq!(
            job_outcome(
                &cx,
                task("/redfish/v1/Managers/iDRAC.Embedded.1/Jobs/JID_42")
            )
            .await,
            Ok(DriverOutcome::accepted(OperationReference::VendorJob {
                uri: "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/JID_42"
                    .to_string()
                    .into(),
                job_id: VendorJobId::new("JID_42".to_string()).expect("nonempty"),
                retry_after_seconds: Some(7),
            }))
        );
        assert_eq!(
            job_outcome(&cx, task("/redfish/v1/TaskService/Tasks/42")).await,
            Ok(DriverOutcome::accepted(OperationReference::RedfishTask {
                uri: "/redfish/v1/TaskService/Tasks/42".to_string().into(),
                retry_after_seconds: Some(7),
                task: None,
            }))
        );
    }

    #[tokio::test]
    async fn job_queue_is_cleared_through_the_advertised_action_unless_locked_down() {
        const MANAGER: &str = "/redfish/v1/Managers/iDRAC.Embedded.1";
        const ATTRIBUTES: &str =
            "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1";
        const JOB_SERVICE: &str = "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService";
        const DELETE_JOB_QUEUE: &str = "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService/Actions/DellJobService.DeleteJobQueue";

        for (system_lockdown, expected) in [
            ("Enabled", Err(PlatformError::LockedDown)),
            ("Disabled", Ok(())),
        ] {
            let bmc = Fixture::new(
                "Dell",
                "Integrated Dell Remote Access Controller",
                "System.Embedded.1",
                "iDRAC.Embedded.1",
            )
            .document(
                MANAGER,
                json!({
                    "@odata.id": MANAGER,
                    "Id": "iDRAC.Embedded.1",
                    "Name": "Manager",
                    "Links": {"Oem": {"Dell": {
                        "DellAttributes": [{"@odata.id": ATTRIBUTES}],
                        "DellJobService": {"@odata.id": JOB_SERVICE},
                    }}},
                }),
            )
            .document(
                ATTRIBUTES,
                json!({
                    "@odata.id": ATTRIBUTES,
                    "Id": "iDRAC.Embedded.1",
                    "Name": "Manager Attributes",
                    "Attributes": {"Lockdown.1.SystemLockdown": system_lockdown},
                }),
            )
            .document(
                JOB_SERVICE,
                json!({
                    "@odata.id": JOB_SERVICE,
                    "Id": "Job Service",
                    "Name": "DellJobService",
                    "Actions": {"#DellJobService.DeleteJobQueue": {"target": DELETE_JOB_QUEUE}},
                }),
            )
            .build()
            .await;
            let cx = bmc.cx().await;

            assert_eq!(clear_job_queue(&cx).await, expected, "{system_lockdown}");
            let writes = bmc.writes();
            match expected {
                Err(_) => assert!(writes.is_empty(), "{system_lockdown}"),
                Ok(()) => {
                    assert_eq!(writes.len(), 1);
                    assert!(
                        writes[0].uri.ends_with(DELETE_JOB_QUEUE),
                        "{}",
                        writes[0].uri
                    );
                    assert_eq!(body(&writes[0]), json!({"JobID": "JID_CLEARALL"}));
                }
            }
        }
    }
}
