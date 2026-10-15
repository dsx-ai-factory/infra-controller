/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Json, Path, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use lazy_static::lazy_static;
use rand::RngExt;
use rand::distr::StandardUniform;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch, json_patch};
use crate::{Callbacks, http, redfish};

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    r.route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Attributes",
        get(get_managers_oem_dell_attributes::<C>).patch(patch_managers_oem_dell_attributes::<C>),
    ).route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1",
        get(get_managers_oem_dell_attributes::<C>).patch(patch_managers_oem_dell_attributes::<C>),
    ).route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Jobs",
        post(post_dell_create_bios_job::<C>),
    ).route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs",
        post(post_dell_create_bios_job::<C>),
    ).route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Jobs/{job_id}",
        get(get_dell_job::<C>),
    ).route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/{job_id}",
        get(get_dell_job::<C>),
    ).route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService/Actions/DellJobService.DeleteJobQueue",
        post(post_delete_job_queue)
    ).route(
        "/redfish/v1/Managers/iDRAC.Embedded.1/Actions/Oem/EID_674_Manager.ImportSystemConfiguration",
        post(post_import_sys_configuration::<C>)
    )
}

fn attributes_resource() -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Borrowed(
            "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1",
        ),
        odata_type: Cow::Borrowed("#DellAttributes.v1_0_0.DellAttributes"),
        name: Cow::Borrowed("OEMAttributeRegistry"),
        id: Cow::Borrowed("iDRACAttributes"),
    }
}

async fn get_managers_oem_dell_attributes<C: Callbacks>(
    State(state): State<BmcState<C>>,
) -> Response {
    let redfish::oem::State::DellIdrac(state) = state.oem_state else {
        return http::not_found();
    };
    lazy_static! {
        // Only attributes required by libredfish:
        static ref base: serde_json::Value = attributes_resource().json_patch().patch(json!({
            "Attributes": {
                "IPMILan.1.Enable": "Enabled",
                "IPMISOL.1.BaudRate": "115200",
                "IPMISOL.1.Enable": "Enabled",
                "IPMISOL.1.MinPrivilege": "Administrator",
                "Lockdown.1.SystemLockdown": "Disabled",
                "OS-BMC.1.AdminState": "Disabled",
                "Racadm.1.Enable": "Enabled",
                "SSH.1.Enable": "Enabled",
                "SerialRedirection.1.Enable": "Enabled",
                "WebServer.1.HostHeaderCheck": "Disabled",
            }
        }));
    }
    state.get_attrs(base.clone()).into_ok_response()
}

async fn patch_managers_oem_dell_attributes<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Json(attrs): Json<serde_json::Value>,
) -> Response {
    let redfish::oem::State::DellIdrac(state) = state.oem_state else {
        return http::not_found();
    };
    state.update_attrs(attrs);
    json!({}).into_ok_response()
}

#[derive(Debug, Clone)]
pub(crate) enum JobState {
    Scheduled,
    Completed,
}

async fn get_dell_job<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(job_id): Path<String>,
) -> Response {
    let redfish::oem::State::DellIdrac(state) = state.oem_state else {
        return http::not_found();
    };
    let Some(job) = state.get_job(&job_id) else {
        return json!(format!("could not find iDRAC job: {job_id}"))
            .into_response(StatusCode::NOT_FOUND);
    };

    let job_state = match job.job_state {
        JobState::Scheduled => "Scheduled".to_string(),
        JobState::Completed => "Completed".to_string(),
    };

    serde_json::json!({
        "@odata.context": "/redfish/v1/$metadata#DellJob.DellJob",
        "@odata.id": format!("/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/{job_id}"),
        "@odata.type": "#DellJob.v1_5_0.DellJob",
        "ActualRunningStartTime": format!("{}", job.start_time),
        "ActualRunningStopTime": null,
        "CompletionTime": null,
        "Description": "Job Instance",
        "EndTime": "TIME_NA",
        "Id": job_id,
        "JobState": job_state,
        "JobType": job.job_type,
        "Message": job_state,
        "MessageArgs": [],
        "MessageArgs@odata.count": 0,
        "MessageId": "PR19",
        "Name": job.job_type,
        "PercentComplete": job.percent_complete(),
        "StartTime": format!("{}", job.start_time),
        "TargetSettingsURI": null
    })
    .into_ok_response()
}

pub(in crate::redfish) fn create_job_with_location<C: Callbacks>(state: BmcState<C>) -> Response {
    let redfish::oem::State::DellIdrac(state) = state.oem_state else {
        return http::not_found();
    };
    match state.add_job() {
        Ok(job_id) => json!({}).into_ok_response_with_location(
            HeaderValue::try_from(format!(
                "/redfish/v1/Managers/iDRAC.Embedded.1/Jobs/{job_id}"
            ))
            .expect("This must be valid header value"),
        ),
        Err(e) => json!(e.to_string()).into_response(StatusCode::BAD_REQUEST),
    }
}

async fn post_dell_create_bios_job<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    create_job_with_location(state)
}

async fn post_delete_job_queue() -> Response {
    json!({}).into_ok_response()
}

async fn post_import_sys_configuration<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    create_job_with_location(state)
}

const DELL_JOB_TYPE: &str = "DellConfiguration";

#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub(crate) job_id: String,
    pub(crate) job_state: JobState,
    pub(crate) job_type: String,
    pub(crate) start_time: chrono::DateTime<chrono::Utc>,
    pub(crate) end_time: Option<chrono::DateTime<chrono::Utc>>,
}

impl Job {
    pub(crate) fn is_dell_job(&self) -> bool {
        matches!(self.job_type.as_str(), DELL_JOB_TYPE)
    }

    pub(crate) fn percent_complete(&self) -> i32 {
        match &self.job_state {
            JobState::Completed => 100,
            _ => 0,
        }
    }
}

#[derive(Clone)]
pub(crate) struct IdracState {
    pub(crate) jobs: Arc<Mutex<HashMap<String, Job>>>,
    pub(crate) dell_attrs: Arc<Mutex<serde_json::Value>>,
}

impl Default for IdracState {
    fn default() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            dell_attrs: Arc::new(Mutex::new(serde_json::json!({}))),
        }
    }
}

impl IdracState {
    pub(crate) fn get_job(&self, job_id: &String) -> Option<Job> {
        self.jobs.lock().unwrap().get(job_id).cloned()
    }

    pub(crate) fn add_job(&self) -> Result<String, Box<dyn std::error::Error>> {
        let mut jobs = self.jobs.lock().unwrap();

        let job_id = rand::rng()
            .sample_iter::<u64, _>(StandardUniform)
            .map(|r| format!("JID_{r}"))
            .find(|id| !jobs.contains_key(id))
            .unwrap();

        let job = Job {
            job_id: job_id.clone(),
            job_state: JobState::Scheduled,
            job_type: DELL_JOB_TYPE.to_string(),
            start_time: chrono::offset::Utc::now(),
            end_time: None,
        };

        jobs.insert(job_id.clone(), job);
        Ok(job_id)
    }

    pub(crate) fn complete_all_bios_jobs(&self) {
        let mut jobs = self.jobs.lock().unwrap();

        let bios_jobs: Vec<Job> = jobs
            .values()
            .filter(|job| job.is_dell_job())
            .cloned()
            .collect();
        for mut job in bios_jobs {
            job.job_state = JobState::Completed;
            job.end_time = Some(chrono::offset::Utc::now());
            jobs.insert(job.job_id.clone(), job);
        }
    }

    pub(crate) fn update_attrs(&self, v: serde_json::Value) {
        let mut dell_attrs = self.dell_attrs.lock().unwrap();
        json_patch(&mut dell_attrs, v);
    }

    pub(crate) fn get_attrs(&self, mut base: serde_json::Value) -> serde_json::Value {
        let dell_attrs = self.dell_attrs.lock().unwrap();
        json_patch(&mut base, dell_attrs.clone());
        base
    }
}
