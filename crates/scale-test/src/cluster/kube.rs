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

//! [`Cluster`] on the Kubernetes API, with the Job's service account.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use http_body_util::{BodyExt, Limited};
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::api::core::v1::{ConfigMap, Pod, Service};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use k8s_openapi::jiff::Timestamp;
use kube::api::{GetParams, ListParams, Patch, PatchParams, PostParams};
use kube::core::{ObjectList, Request};
use kube::{Client, Resource};
use serde::de::DeserializeOwned;
use tokio::time::Instant;

use super::{
    CALL_TIMEOUT, Cluster, ClusterError, ConfigMapObject, LeaseObject, LeaseReplaced, MatService,
    PodInfo,
};

const MANAGED_BY_MAT: &str = "app.kubernetes.io/managed-by=mat-k8s-controller";
const LABEL_MACHINE_ID: &str = "nvidia-infra-controller/mat-machine-id";
const LABEL_MACHINE_TYPE: &str = "nvidia-infra-controller/mat-machine-type";
const LABEL_MAT_ID: &str = "nvidia-infra-controller/mat-id";
const ANNOTATION_BMC_IP: &str = "nvidia-infra-controller/mat-bmc-ip";
const ANNOTATION_HARDWARE_TYPE: &str = "nvidia-infra-controller/mat-hardware-type";
const FIELD_MANAGER: &str = "scale-test";
/// Objects asked for per list page.
const LIST_PAGE: u32 = 500;
/// Most objects one list may return over all its pages, well above a
/// site's MAT Services.
const MAX_LIST_ITEMS: usize = 100_000;
/// The time one list may take over all its pages: a second a page for a
/// list at [`MAX_LIST_ITEMS`]. Each page also has the call timeout.
const LIST_TIMEOUT: Duration = Duration::from_secs((MAX_LIST_ITEMS / LIST_PAGE as usize) as u64);
/// Largest reply body read, success or error: a list page of
/// [`LIST_PAGE`] objects fits well within it.
const MAX_REPLY_BYTES: usize = 16 << 20;
/// How long the body of a non-2xx reply is read for its reason. The status
/// alone decides the error after that.
const ERROR_BODY_WAIT: Duration = Duration::from_secs(1);

/// Every call is bounded by `call_timeout`, whoever calls it, a list by
/// `list_timeout` over its pages, and every reply by 16 MiB. Only a 2xx
/// reply is a success. The client never logs a reply body, and no server
/// text reaches an error.
pub struct KubeCluster {
    client: Client,
    namespace: String,
    call_timeout: Duration,
    list_timeout: Duration,
    max_reply_bytes: usize,
}

impl KubeCluster {
    /// In-cluster config, or the local kubeconfig. A kubeconfig that runs
    /// a credential plugin is refused: the client runs it with no deadline.
    pub async fn new(namespace: &str) -> Result<Self, ClusterError> {
        let config = kube::Config::infer()
            .await
            .map_err(|_| ClusterError::Invalid("no Kubernetes client config".to_string()))?;
        if unbounded_credentials(&config.auth_info) {
            return Err(ClusterError::Invalid(
                "credential plugins and token files are not supported".to_string(),
            ));
        }
        let client = Client::try_from(config).map_err(|e| failure(&e))?;
        Ok(Self {
            client,
            namespace: namespace.to_string(),
            call_timeout: CALL_TIMEOUT,
            list_timeout: LIST_TIMEOUT,
            max_reply_bytes: MAX_REPLY_BYTES,
        })
    }

    fn leases(&self) -> Request {
        Request::new(Lease::url_path(&(), Some(&self.namespace)))
    }

    fn config_maps(&self) -> Request {
        Request::new(ConfigMap::url_path(&(), Some(&self.namespace)))
    }

    /// When a call started now must have answered.
    fn deadline(&self) -> Instant {
        Instant::now() + self.call_timeout
    }

    fn timed_out(&self) -> ClusterError {
        ClusterError::Timeout(self.call_timeout.as_secs())
    }

    /// Sends `request` by `until` and decodes a 2xx reply as `T`. Any other
    /// status is an error that keeps only its code and a known reason, also
    /// when its body does not arrive by `until`.
    async fn call<T: DeserializeOwned>(
        &self,
        request: Result<http::Request<Vec<u8>>, kube::core::request::Error>,
        until: Instant,
    ) -> Result<T, ClusterError> {
        self.call_status(request, until)
            .await
            .and_then(|(_, object)| object)
    }

    /// [`call`](Self::call), with the 2xx status the reply had. Once that
    /// status is known, a body that does not arrive or decode is an error
    /// next to it: a 201 keeps its meaning whatever its body holds.
    async fn call_status<T: DeserializeOwned>(
        &self,
        request: Result<http::Request<Vec<u8>>, kube::core::request::Error>,
        until: Instant,
    ) -> Result<(http::StatusCode, Result<T, ClusterError>), ClusterError> {
        let request = request.map_err(|_| unbuilt())?;
        // The headers must leave the error body wait before `until`, so a
        // refusal keeps its status inside the call's own time.
        let headers_until = until
            .checked_sub(ERROR_BODY_WAIT)
            .filter(|t| *t > Instant::now())
            .unwrap_or(until);
        let sent = tokio::time::timeout_at(
            headers_until,
            self.client.send(request.map(kube::client::Body::from)),
        );
        let reply = sent
            .await
            .map_err(|_| self.timed_out())?
            .map_err(|e| failure(&e))?;
        let status = reply.status();
        let collect = Limited::new(reply.into_body(), self.max_reply_bytes).collect();
        if !status.is_success() {
            // The status already says what happened: a 404 or a 403 keeps
            // its meaning when its body is too big, broken or never ends.
            let body_until = until.min(Instant::now() + ERROR_BODY_WAIT);
            let reason = match tokio::time::timeout_at(body_until, collect).await {
                Ok(Ok(body)) => serde_json::from_slice::<kube::core::Status>(&body.to_bytes())
                    .map(|s| s.reason)
                    .unwrap_or_default(),
                Ok(Err(_)) | Err(_) => String::new(),
            };
            return Err(status_error(status.as_u16(), &reason));
        }
        let object = match tokio::time::timeout_at(until, collect).await {
            Err(_) => Err(self.timed_out()),
            Ok(Err(e)) if e.is::<http_body_util::LengthLimitError>() => Err(ClusterError::Invalid(
                format!("the reply is over {} bytes", self.max_reply_bytes),
            )),
            Ok(Err(_)) => Err(ClusterError::Transport(
                "the reply body did not arrive whole".to_string(),
            )),
            Ok(Ok(body)) => serde_json::from_slice(&body.to_bytes())
                .map_err(|_| ClusterError::Invalid("the reply did not decode".to_string())),
        };
        Ok((status, object))
    }

    /// The object `name` of `kind`, or `None` on a 404.
    async fn get<T: Resource + DeserializeOwned>(
        &self,
        kind: Request,
        name: &str,
        until: Instant,
    ) -> Result<Option<T>, ClusterError> {
        match self
            .call(kind.get(name, &GetParams::default()), until)
            .await
        {
            Ok(object) => stored(object, name).map(Some),
            Err(ClusterError::Api { code: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Every object of `kind` with `labels`, read in pages of
    /// [`LIST_PAGE`] up to `max_items` in all. Each page has the call
    /// timeout, and the whole list `list_timeout`. A page without a list
    /// resource version, a continue token that does not move on, or more
    /// objects than `max_items` is an error.
    async fn list_all<T: DeserializeOwned + Clone>(
        &self,
        kind: Request,
        labels: &str,
        max_items: usize,
    ) -> Result<Vec<T>, ClusterError> {
        let list_until = Instant::now() + self.list_timeout;
        let mut items = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut params = ListParams::default().labels(labels).limit(LIST_PAGE);
            if let Some(token) = &token {
                params = params.continue_token(token);
            }
            let until = self.deadline().min(list_until);
            let page: ObjectList<T> = match self.call(kind.list(&params), until).await {
                Err(ClusterError::Timeout(_)) if until == list_until => {
                    return Err(ClusterError::Timeout(self.list_timeout.as_secs()));
                }
                page => page?,
            };
            if page.metadata.resource_version.is_none() {
                return Err(ClusterError::Invalid(
                    "the reply is not a list of the API".to_string(),
                ));
            }
            if items.len() + page.items.len() > max_items {
                return Err(ClusterError::Invalid(format!(
                    "the list holds over {max_items} objects"
                )));
            }
            items.extend(page.items);
            match page.metadata.continue_.filter(|next| !next.is_empty()) {
                None => return Ok(items),
                Some(next) if token.as_deref() == Some(next.as_str()) => {
                    return Err(ClusterError::Invalid(
                        "the list continue token did not move on".to_string(),
                    ));
                }
                Some(next) => token = Some(next),
            }
        }
    }
}

/// A non-2xx reply as an error: its code and a known reason, never the
/// server's message.
fn status_error(code: u16, reason: &str) -> ClusterError {
    match (code, reason) {
        (409, "AlreadyExists") => ClusterError::AlreadyExists,
        (409, _) => ClusterError::Conflict,
        (403, _) => ClusterError::Forbidden(known_reason(reason).to_string()),
        (code, _) => ClusterError::Api {
            code,
            message: known_reason(reason).to_string(),
        },
    }
}

/// The API's reason when it is one Kubernetes defines, else a fixed text.
fn known_reason(reason: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "BadRequest",
        "Unauthorized",
        "Forbidden",
        "NotFound",
        "AlreadyExists",
        "Conflict",
        "Gone",
        "Invalid",
        "ServerTimeout",
        "Timeout",
        "TooManyRequests",
        "MethodNotAllowed",
        "NotAcceptable",
        "RequestEntityTooLarge",
        "UnsupportedMediaType",
        "InternalError",
        "Expired",
        "ServiceUnavailable",
    ];
    KNOWN
        .iter()
        .find(|known| **known == reason)
        .copied()
        .unwrap_or("no known reason")
}

/// `object` when it is the stored object the API returns: its own name, a
/// resource version and a UID.
fn stored<K: Resource>(object: K, name: &str) -> Result<K, ClusterError> {
    let meta = object.meta();
    if meta.name.as_deref() == Some(name)
        && meta.resource_version.is_some()
        && meta.uid.as_deref().is_some_and(|uid| !uid.is_empty())
    {
        Ok(object)
    } else {
        Err(ClusterError::Invalid(
            "the reply is not the stored object".to_string(),
        ))
    }
}

/// A version a replace can compare and swap on. The API replaces an
/// object with no version, an empty one or `0` unconditionally.
fn swappable(version: Option<&str>) -> Result<(), ClusterError> {
    match version {
        Some(v) if !v.is_empty() && v != "0" => Ok(()),
        _ => Err(ClusterError::Conflict),
    }
}

/// A client failure as an error with fixed text. A connection failure, or
/// one this does not know, may have reached the API. The others are refused
/// before the request is sent, or a reply that is not valid.
fn failure(e: &kube::Error) -> ClusterError {
    let invalid = |why: &str| ClusterError::Invalid(why.to_string());
    match e {
        kube::Error::HyperError(_) | kube::Error::Service(_) => {
            ClusterError::Transport("the connection failed".to_string())
        }
        kube::Error::SerdeError(_) => invalid("the reply did not decode"),
        kube::Error::RustlsTls(_) | kube::Error::TlsRequired => invalid("TLS failed"),
        kube::Error::Auth(_) => invalid("authentication failed"),
        kube::Error::InferConfig(_) | kube::Error::InferKubeconfig(_) => {
            invalid("no Kubernetes client config")
        }
        _ => ClusterError::Transport("the request failed".to_string()),
    }
}

/// Pre-epoch times with a fraction have negative nanoseconds in jiff, so
/// the time is split again into whole seconds and a nonnegative fraction.
fn to_chrono(t: Timestamp) -> Option<DateTime<Utc>> {
    let nanos = t.as_nanosecond();
    let seconds = i64::try_from(nanos.div_euclid(1_000_000_000)).ok()?;
    let fraction = u32::try_from(nanos.rem_euclid(1_000_000_000)).ok()?;
    DateTime::from_timestamp(seconds, fraction)
}

/// The service account token the kubelet mounts in every pod.
const SERVICE_TOKEN_FILE: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// True when connecting runs a credential plugin or reads a token file
/// other than the pod's service account token: the client reads either
/// with no deadline.
fn unbounded_credentials(auth: &kube::config::AuthInfo) -> bool {
    auth.exec.is_some()
        || auth.auth_provider.is_some()
        || auth
            .token_file
            .as_deref()
            .is_some_and(|file| file != SERVICE_TOKEN_FILE)
}

fn micro(t: Option<DateTime<Utc>>) -> Option<MicroTime> {
    let t = t?;
    let nanos = i32::try_from(t.timestamp_subsec_nanos()).ok()?;
    Timestamp::new(t.timestamp(), nanos).ok().map(MicroTime)
}

fn lease_from(lease: Lease) -> LeaseObject {
    let spec = lease.spec.unwrap_or_default();
    LeaseObject {
        name: lease.metadata.name.unwrap_or_default(),
        resource_version: lease.metadata.resource_version,
        uid: lease.metadata.uid,
        holder_identity: spec.holder_identity,
        lease_duration_seconds: spec.lease_duration_seconds.unwrap_or_default(),
        acquire_time: spec.acquire_time.and_then(|t| to_chrono(t.0)),
        renew_time: spec.renew_time.and_then(|t| to_chrono(t.0)),
        annotations: lease.metadata.annotations.unwrap_or_default(),
    }
}

/// The resource version and UID go along: the API refuses a replace when
/// either changed, and a replace with the UID never makes a missing Lease.
fn lease_to(lease: &LeaseObject) -> Lease {
    Lease {
        metadata: ObjectMeta {
            name: Some(lease.name.clone()),
            resource_version: lease.resource_version.clone(),
            uid: lease.uid.clone(),
            annotations: Some(lease.annotations.clone()),
            ..ObjectMeta::default()
        },
        spec: Some(LeaseSpec {
            holder_identity: lease.holder_identity.clone(),
            lease_duration_seconds: Some(lease.lease_duration_seconds),
            acquire_time: micro(lease.acquire_time),
            renew_time: micro(lease.renew_time),
            ..LeaseSpec::default()
        }),
    }
}

fn mat_service_from(service: Service) -> MatService {
    let labels = service.metadata.labels.unwrap_or_default();
    let annotations = service.metadata.annotations.unwrap_or_default();
    let ip = |raw: &str| raw.parse::<IpAddr>().ok();
    MatService {
        name: service.metadata.name.unwrap_or_default(),
        machine_type: labels.get(LABEL_MACHINE_TYPE).cloned(),
        machine_id: labels.get(LABEL_MACHINE_ID).cloned(),
        mat_id: labels.get(LABEL_MAT_ID).cloned(),
        bmc_ip: annotations.get(ANNOTATION_BMC_IP).and_then(|raw| ip(raw)),
        hardware_type: annotations.get(ANNOTATION_HARDWARE_TYPE).cloned(),
        external_ips: service
            .spec
            .and_then(|s| s.external_ips)
            .unwrap_or_default()
            .iter()
            .filter_map(|raw| ip(raw))
            .collect(),
    }
}

fn pod_from(pod: Pod) -> PodInfo {
    let status = pod.status.unwrap_or_default();
    let ready = status
        .conditions
        .unwrap_or_default()
        .iter()
        .any(|c| c.type_ == "Ready" && c.status == "True");
    // A restartable init container (a native sidecar) restarts on its own,
    // so its count goes in with the main containers.
    let restarts = status
        .container_statuses
        .unwrap_or_default()
        .iter()
        .chain(status.init_container_statuses.unwrap_or_default().iter())
        .map(|c| u32::try_from(c.restart_count).unwrap_or_default())
        .fold(0u32, u32::saturating_add);
    PodInfo {
        name: pod.metadata.name.unwrap_or_default(),
        labels: pod.metadata.labels.unwrap_or_default(),
        annotations: pod.metadata.annotations.unwrap_or_default(),
        phase: status.phase.unwrap_or_default(),
        ready,
        created_at: pod
            .metadata
            .creation_timestamp
            .and_then(|t| to_chrono(t.0))
            .unwrap_or_default(),
        restarts,
        ip: status.pod_ip,
    }
}

fn config_map_object_from(config_map: ConfigMap) -> ConfigMapObject {
    let meta = config_map.metadata;
    ConfigMapObject {
        name: meta.name.unwrap_or_default(),
        resource_version: meta.resource_version,
        uid: meta.uid,
        labels: meta.labels.unwrap_or_default(),
        annotations: meta.annotations.unwrap_or_default(),
        data: config_map.data.unwrap_or_default(),
    }
}

/// The resource version and UID go along: the server refuses the replace
/// when either changed.
fn config_map_object_to(config_map: &ConfigMapObject) -> ConfigMap {
    ConfigMap {
        metadata: ObjectMeta {
            name: Some(config_map.name.clone()),
            resource_version: config_map.resource_version.clone(),
            uid: config_map.uid.clone(),
            labels: Some(config_map.labels.clone()),
            annotations: Some(config_map.annotations.clone()),
            ..ObjectMeta::default()
        },
        data: Some(config_map.data.clone()),
        ..ConfigMap::default()
    }
}

fn post_params() -> PostParams {
    PostParams {
        field_manager: Some(FIELD_MANAGER.to_string()),
        ..PostParams::default()
    }
}

#[async_trait]
impl Cluster for KubeCluster {
    async fn get_lease(&self, name: &str) -> Result<Option<LeaseObject>, ClusterError> {
        let until = self.deadline();
        let lease: Option<Lease> = self.get(self.leases(), name, until).await?;
        Ok(lease.map(lease_from))
    }

    async fn create_lease(&self, lease: &LeaseObject) -> Result<LeaseObject, ClusterError> {
        let until = self.deadline();
        let mut object = lease_to(lease);
        object.metadata.resource_version = None;
        object.metadata.uid = None;
        let body = serde_json::to_vec(&object).map_err(|_| unbuilt())?;
        let created: Lease = self
            .call(self.leases().create(&PostParams::default(), body), until)
            .await?;
        Ok(lease_from(stored(created, &lease.name)?))
    }

    async fn replace_lease(&self, lease: &LeaseObject) -> Result<LeaseReplaced, ClusterError> {
        swappable(lease.resource_version.as_deref())?;
        // Without the UID the API makes the Lease when it is missing.
        if lease.uid.as_deref().is_none_or(str::is_empty) {
            return Err(ClusterError::Invalid(
                "a Lease replace needs the Lease UID".to_string(),
            ));
        }
        let until = self.deadline();
        let body = serde_json::to_vec(&lease_to(lease)).map_err(|_| unbuilt())?;
        let (status, replaced) = self
            .call_status::<Lease>(
                self.leases()
                    .replace(&lease.name, &PostParams::default(), body),
                until,
            )
            .await?;
        let replaced = replaced
            .and_then(|object| stored(object, &lease.name))
            .map(lease_from);
        if status == http::StatusCode::CREATED {
            // The replace made the Lease, whatever its body holds.
            return Ok(replaced.map_or(LeaseReplaced::CreatedUnread, LeaseReplaced::Created));
        }
        replaced.map(LeaseReplaced::Replaced)
    }

    async fn get_config_map(
        &self,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, ClusterError> {
        let until = self.deadline();
        let config_map: Option<ConfigMap> = self.get(self.config_maps(), name, until).await?;
        Ok(config_map.map(|cm| cm.data.unwrap_or_default()))
    }

    async fn put_config_map_key(
        &self,
        name: &str,
        key: &str,
        value: &str,
    ) -> Result<(), ClusterError> {
        let until = self.deadline();
        let patch = serde_json::json!({ "data": { key: value } });
        let params = PatchParams {
            field_manager: Some(FIELD_MANAGER.to_string()),
            ..PatchParams::default()
        };
        let patch_it = || async {
            let patched: ConfigMap = self
                .call(
                    self.config_maps()
                        .patch(name, &params, &Patch::Merge(&patch)),
                    until,
                )
                .await?;
            stored(patched, name).map(|_| ())
        };
        match patch_it().await {
            Err(ClusterError::Api { code: 404, .. }) => {
                let config_map = ConfigMap {
                    metadata: ObjectMeta {
                        name: Some(name.to_string()),
                        ..ObjectMeta::default()
                    },
                    data: Some(BTreeMap::from([(key.to_string(), value.to_string())])),
                    ..ConfigMap::default()
                };
                let body = serde_json::to_vec(&config_map).map_err(|_| unbuilt())?;
                let created: Result<ConfigMap, _> = self
                    .call(
                        self.config_maps().create(&PostParams::default(), body),
                        until,
                    )
                    .await;
                match created {
                    Ok(created) => stored(created, name).map(|_| ()),
                    // Created by someone else in between: patch that one.
                    Err(ClusterError::AlreadyExists | ClusterError::Conflict) => patch_it().await,
                    Err(e) => Err(e),
                }
            }
            other => other,
        }
    }

    async fn get_config_map_object(
        &self,
        name: &str,
    ) -> Result<Option<ConfigMapObject>, ClusterError> {
        let until = self.deadline();
        let config_map: Option<ConfigMap> = self.get(self.config_maps(), name, until).await?;
        Ok(config_map.map(config_map_object_from))
    }

    async fn create_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError> {
        let until = self.deadline();
        let mut object = config_map_object_to(config_map);
        object.metadata.resource_version = None;
        object.metadata.uid = None;
        let body = serde_json::to_vec(&object).map_err(|_| unbuilt())?;
        let created: ConfigMap = self
            .call(self.config_maps().create(&post_params(), body), until)
            .await?;
        Ok(config_map_object_from(stored(created, &config_map.name)?))
    }

    async fn replace_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError> {
        swappable(config_map.resource_version.as_deref())?;
        let until = self.deadline();
        let body = serde_json::to_vec(&config_map_object_to(config_map)).map_err(|_| unbuilt())?;
        let replaced: ConfigMap = self
            .call(
                self.config_maps()
                    .replace(&config_map.name, &post_params(), body),
                until,
            )
            .await?;
        Ok(config_map_object_from(stored(replaced, &config_map.name)?))
    }

    async fn list_mat_services(&self) -> Result<Vec<MatService>, ClusterError> {
        let kind = Request::new(Service::url_path(&(), Some(&self.namespace)));
        let list: Vec<Service> = self.list_all(kind, MANAGED_BY_MAT, MAX_LIST_ITEMS).await?;
        Ok(list.into_iter().map(mat_service_from).collect())
    }

    async fn list_pods(
        &self,
        namespace: &str,
        label_selector: &str,
    ) -> Result<Vec<PodInfo>, ClusterError> {
        let kind = Request::new(Pod::url_path(&(), Some(namespace)));
        let list: Vec<Pod> = self.list_all(kind, label_selector, MAX_LIST_ITEMS).await?;
        Ok(list.into_iter().map(pod_from).collect())
    }
}

fn unbuilt() -> ClusterError {
    ClusterError::Invalid("the request did not build".to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use futures::StreamExt;
    use k8s_openapi::api::core::v1::{ContainerStatus, PodStatus};
    use serde_json::{Value, json};

    use super::*;

    const SECRET: &str = "token=s3cret https://user:pw@api.example";

    /// A cluster whose API is `router`, served on 127.0.0.1.
    async fn served(router: Router) -> KubeCluster {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let config = kube::Config::new(format!("http://{addr}").parse().unwrap());
        KubeCluster {
            client: Client::try_from(config).unwrap(),
            namespace: "ns".to_string(),
            call_timeout: Duration::from_secs(1),
            list_timeout: Duration::from_secs(5),
            max_reply_bytes: 4_096,
        }
    }

    fn pod(name: &str) -> Value {
        json!({"metadata": {"name": name, "resourceVersion": "1"}})
    }

    fn lease_object(version: Option<&str>) -> LeaseObject {
        LeaseObject {
            name: "l".to_string(),
            resource_version: version.map(str::to_string),
            uid: Some("u1".to_string()),
            holder_identity: None,
            lease_duration_seconds: 60,
            acquire_time: None,
            renew_time: None,
            annotations: BTreeMap::new(),
        }
    }

    type Asked = Arc<Mutex<Vec<String>>>;

    /// Three pages: a, b, then c, d, then e.
    async fn pages(
        State(asked): State<Asked>,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> Json<Value> {
        let token = query.get("continue").cloned().unwrap_or_default();
        asked.lock().unwrap().push(format!(
            "limit={} continue={token}",
            query.get("limit").cloned().unwrap_or_default()
        ));
        let (items, next) = match token.as_str() {
            "" => (vec![pod("a"), pod("b")], Some("c1")),
            "c1" => (vec![pod("c"), pod("d")], Some("c2")),
            _ => (vec![pod("e")], None),
        };
        Json(json!({
            "apiVersion": "v1",
            "kind": "PodList",
            "metadata": {"resourceVersion": "9", "continue": next},
            "items": items,
        }))
    }

    #[tokio::test]
    async fn lists_are_read_in_pages_and_capped() {
        let asked = Asked::default();
        let router = Router::new()
            .route("/api/v1/namespaces/ns/pods", get(pages))
            .with_state(asked.clone());
        let cluster = served(router).await;

        let pods = cluster.list_pods("ns", "app=x").await.unwrap();
        let names: Vec<String> = pods.into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e"]);
        assert_eq!(
            *asked.lock().unwrap(),
            [
                "limit=500 continue=",
                "limit=500 continue=c1",
                "limit=500 continue=c2"
            ]
        );

        let kind = Request::new(Pod::url_path(&(), Some("ns")));
        let capped: Result<Vec<Pod>, _> = cluster.list_all(kind, "app=x", 3).await;
        assert_eq!(
            capped.map(|items| items.len()),
            Err(ClusterError::Invalid(
                "the list holds over 3 objects".to_string()
            ))
        );
    }

    /// Pages that each answer in 400 ms, every one with a next page.
    async fn slow_pages(Query(query): Query<BTreeMap<String, String>>) -> Json<Value> {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let page: u32 = query
            .get("continue")
            .and_then(|token| token.parse().ok())
            .unwrap_or(0);
        Json(json!({
            "apiVersion": "v1",
            "kind": "PodList",
            "metadata": {"resourceVersion": "9", "continue": (page + 1).to_string()},
            "items": [pod(&format!("p{page}"))],
        }))
    }

    #[tokio::test]
    async fn each_list_page_has_its_own_deadline_inside_the_lists() {
        let router = Router::new().route("/api/v1/namespaces/ns/pods", get(slow_pages));
        let mut cluster = served(router).await;
        cluster.list_timeout = Duration::from_secs(2);
        // Four pages take 1.6 s, past the 1 s call timeout.
        let kind = || Request::new(Pod::url_path(&(), Some("ns")));
        let capped: Result<Vec<Pod>, _> = cluster.list_all(kind(), "app=x", 3).await;
        assert_eq!(
            capped.map(|items| items.len()),
            Err(ClusterError::Invalid(
                "the list holds over 3 objects".to_string()
            )),
            "the fourth page is read"
        );
        // A list that never ends stops at the list's own time.
        let started = std::time::Instant::now();
        let endless: Result<Vec<Pod>, _> = cluster.list_all(kind(), "app=x", 1_000).await;
        assert_eq!(
            endless.map(|items| items.len()),
            Err(ClusterError::Timeout(2))
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(LIST_TIMEOUT, Duration::from_secs(200), "a second a page");
    }

    #[test]
    fn a_lease_converts_both_ways() {
        let at = |raw: &str| DateTime::parse_from_rfc3339(raw).ok().map(|t| t.to_utc());
        let lease = LeaseObject {
            name: "l".to_string(),
            resource_version: Some("7".to_string()),
            uid: Some("u1".to_string()),
            holder_identity: Some("test/r1/r1-x".to_string()),
            lease_duration_seconds: 60,
            acquire_time: at("1969-12-31T23:59:59.250001Z"),
            renew_time: at("2026-10-23T10:00:00.123456Z"),
            annotations: BTreeMap::from([("k".to_string(), "v".to_string())]),
        };
        assert_eq!(lease_from(lease_to(&lease)), lease);
        // On the wire too: the API keeps microseconds.
        let wire: Lease =
            serde_json::from_value(serde_json::to_value(lease_to(&lease)).unwrap()).unwrap();
        assert_eq!(lease_from(wire), lease);
        assert_eq!(micro(None), None);
    }

    #[tokio::test]
    async fn only_a_2xx_reply_is_a_success() {
        // A redirect is no success, even with a whole stored object in it.
        let stored_lease = || async {
            let body = json!({
                "apiVersion": "coordination.k8s.io/v1",
                "kind": "Lease",
                "metadata": {"name": "l", "resourceVersion": "7"},
            });
            (StatusCode::FOUND, Json(body))
        };
        let stored_config_map = || async {
            let body = json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {"name": "c", "resourceVersion": "7"},
            });
            (StatusCode::FOUND, Json(body))
        };
        let list = || async {
            let body = json!({"apiVersion": "v1", "kind": "PodList", "metadata": {"resourceVersion": "9"}, "items": []});
            (StatusCode::FOUND, Json(body))
        };
        let router = Router::new()
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
                get(stored_lease).put(stored_lease),
            )
            .route(
                "/api/v1/namespaces/ns/configmaps/c",
                get(stored_config_map).patch(stored_config_map),
            )
            .route("/api/v1/namespaces/ns/pods", get(list));
        let cluster = served(router).await;
        let redirect = || ClusterError::Api {
            code: 302,
            message: "no known reason".to_string(),
        };

        assert_eq!(cluster.get_lease("l").await, Err(redirect()));
        assert_eq!(
            cluster.replace_lease(&lease_object(Some("7"))).await,
            Err(redirect())
        );
        assert_eq!(cluster.get_config_map("c").await, Err(redirect()));
        assert_eq!(
            cluster.put_config_map_key("c", "k", "v").await,
            Err(redirect())
        );
        assert_eq!(cluster.list_pods("ns", "app=x").await, Err(redirect()));
    }

    #[tokio::test]
    async fn a_2xx_reply_that_is_not_the_stored_object_is_an_error() {
        let named_other =
            || async { Json(json!({"apiVersion": "coordination.k8s.io/v1", "kind": "Lease"})) };
        let no_uid = || async {
            Json(json!({
                "apiVersion": "coordination.k8s.io/v1",
                "kind": "Lease",
                "metadata": {"name": "n", "resourceVersion": "7"},
            }))
        };
        let router = Router::new()
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
                get(named_other),
            )
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/n",
                get(no_uid),
            );
        let cluster = served(router).await;
        for name in ["l", "n"] {
            assert_eq!(
                cluster.get_lease(name).await,
                Err(ClusterError::Invalid(
                    "the reply is not the stored object".to_string()
                )),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn a_lease_replace_sends_the_uid_and_reports_a_201_as_created() {
        // The API answers 201 when the replace made the Lease, else 200.
        let sent = Asked::default();
        let put = |State(sent): State<Asked>, Json(body): Json<Value>| async move {
            sent.lock()
                .unwrap()
                .push(body["metadata"]["uid"].to_string());
            let status = if body["metadata"]["resourceVersion"] == "7" {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            let stored = json!({
                "apiVersion": "coordination.k8s.io/v1",
                "kind": "Lease",
                "metadata": {"name": "l", "resourceVersion": "8", "uid": "u2"},
            });
            (status, Json(stored))
        };
        let router = Router::new()
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
                axum::routing::put(put),
            )
            .with_state(sent.clone());
        let cluster = served(router).await;
        let stored = LeaseObject {
            resource_version: Some("8".to_string()),
            uid: Some("u2".to_string()),
            lease_duration_seconds: 0,
            ..lease_object(None)
        };
        assert_eq!(
            cluster.replace_lease(&lease_object(Some("7"))).await,
            Ok(LeaseReplaced::Replaced(stored.clone()))
        );
        assert_eq!(
            cluster.replace_lease(&lease_object(Some("6"))).await,
            Ok(LeaseReplaced::Created(stored))
        );
        assert_eq!(*sent.lock().unwrap(), ["\"u1\"", "\"u1\""]);
    }

    #[tokio::test]
    async fn a_201_lease_replace_with_a_bad_body_is_still_created() {
        // The version picks the status, and every body is broken.
        let put = |Json(body): Json<Value>| async move {
            let status = if body["metadata"]["resourceVersion"] == "7" {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (status, "{\"kind\":")
        };
        let router = Router::new().route(
            "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
            axum::routing::put(put),
        );
        let cluster = served(router).await;
        assert_eq!(
            cluster.replace_lease(&lease_object(Some("6"))).await,
            Ok(LeaseReplaced::CreatedUnread)
        );
        assert_eq!(
            cluster.replace_lease(&lease_object(Some("7"))).await,
            Err(ClusterError::Invalid(
                "the reply did not decode".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn errors_carry_no_server_or_client_text() {
        let reply = |code: u16, reason: &'static str| {
            move || async move {
                let body =
                    json!({"kind": "Status", "code": code, "reason": reason, "message": SECRET});
                (StatusCode::from_u16(code).unwrap(), Json(body)).into_response()
            }
        };
        let router = Router::new()
            .route(
                "/api/v1/namespaces/ns/configmaps/forbidden",
                get(reply(403, "Forbidden")),
            )
            .route(
                "/api/v1/namespaces/ns/configmaps/known",
                get(reply(500, "InternalError")),
            )
            .route(
                "/api/v1/namespaces/ns/configmaps/unknown",
                get(reply(500, "SecretToken123")),
            )
            .route(
                "/api/v1/namespaces/ns/configmaps/text",
                get(|| async { (StatusCode::BAD_GATEWAY, SECRET).into_response() }),
            )
            .route(
                "/api/v1/namespaces/ns/configmaps/gone",
                get(reply(404, "NotFound")),
            );
        let cluster = served(router).await;
        let cases = [
            ("forbidden", "forbidden: Forbidden"),
            ("known", "kubernetes API error 500: InternalError"),
            ("unknown", "kubernetes API error 500: no known reason"),
            ("text", "kubernetes API error 502: no known reason"),
        ];
        for (name, want) in cases {
            let error = cluster.get_config_map(name).await.unwrap_err();
            assert_eq!(error.to_string(), want, "{name}");
        }
        assert_eq!(cluster.get_config_map("gone").await, Ok(None));
        let transport = failure(&kube::Error::Service(SECRET.into()));
        assert_eq!(
            transport.to_string(),
            "kubernetes API unreachable: the connection failed"
        );
        assert_eq!(
            status_error(409, "AlreadyExists"),
            ClusterError::AlreadyExists
        );
        assert_eq!(status_error(409, "Conflict"), ClusterError::Conflict);
    }

    #[tokio::test]
    async fn a_reply_over_its_size_is_an_error_that_keeps_its_status() {
        let big = || async { (StatusCode::OK, "x".repeat(10_000)) };
        let gone = || async { (StatusCode::NOT_FOUND, "x".repeat(10_000)) };
        let denied = || async { (StatusCode::FORBIDDEN, "x".repeat(10_000)) };
        let router = Router::new()
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
                get(big),
            )
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/gone",
                get(gone),
            )
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/denied",
                get(denied),
            );
        let cluster = served(router).await;
        assert_eq!(
            cluster.get_lease("l").await,
            Err(ClusterError::Invalid(
                "the reply is over 4096 bytes".to_string()
            ))
        );
        assert_eq!(cluster.get_lease("gone").await, Ok(None));
        assert_eq!(
            cluster.get_lease("denied").await,
            Err(ClusterError::Forbidden("no known reason".to_string()))
        );
    }

    #[tokio::test]
    async fn an_error_whose_body_never_ends_keeps_its_status() {
        let endless = || async {
            let first = futures::stream::once(async {
                Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{"))
            });
            let body = axum::body::Body::from_stream(first.chain(futures::stream::pending()));
            (StatusCode::FORBIDDEN, body)
        };
        let router = Router::new().route(
            "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
            get(endless),
        );
        let mut cluster = served(router).await;
        cluster.call_timeout = Duration::from_secs(5);
        assert_eq!(
            cluster.get_lease("l").await,
            Err(ClusterError::Forbidden("no known reason".to_string()))
        );
    }

    #[tokio::test]
    async fn an_error_near_the_deadline_keeps_its_status() {
        // The 403 headers come at 0.5 s of the 1 s call, then a body that
        // never ends.
        let late = || async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let first = futures::stream::once(async {
                Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{"))
            });
            let body = axum::body::Body::from_stream(first.chain(futures::stream::pending()));
            (StatusCode::FORBIDDEN, body)
        };
        let router = Router::new().route(
            "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
            get(late),
        );
        let cluster = served(router).await;
        assert_eq!(
            cluster.get_lease("l").await,
            Err(ClusterError::Forbidden("no known reason".to_string()))
        );
    }

    #[test]
    fn credential_plugins_are_refused() {
        let mut auth = kube::config::AuthInfo::default();
        assert!(!unbounded_credentials(&auth));
        auth.token_file = Some(SERVICE_TOKEN_FILE.to_string());
        assert!(!unbounded_credentials(&auth), "the pod's own token");
        auth.token_file = Some("/tmp/fifo".to_string());
        assert!(unbounded_credentials(&auth), "any other token file");
        auth.token_file = None;
        auth.exec = Some(serde_json::from_value(json!({"command": "helper"})).unwrap());
        assert!(unbounded_credentials(&auth));
    }

    #[tokio::test]
    async fn every_call_is_bounded_and_a_replace_needs_a_version() {
        let asked = Asked::default();
        let hang = |State(asked): State<Asked>| async move {
            asked.lock().unwrap().push("asked".to_string());
            std::future::pending::<Json<Value>>().await
        };
        let router = Router::new()
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases",
                axum::routing::post(hang),
            )
            .route(
                "/apis/coordination.k8s.io/v1/namespaces/ns/leases/l",
                get(hang).put(hang),
            )
            .route(
                "/api/v1/namespaces/ns/configmaps",
                axum::routing::post(hang),
            )
            .route(
                "/api/v1/namespaces/ns/configmaps/c",
                get(hang).put(hang).patch(hang),
            )
            .route("/api/v1/namespaces/ns/services", get(hang))
            .route("/api/v1/namespaces/ns/pods", get(hang))
            .with_state(asked.clone());
        let cluster = served(router).await;

        // Each of the ten methods against an API that never answers.
        type Call<'a> =
            std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ClusterError>> + 'a>>;
        let config_map = ConfigMapObject {
            name: "c".to_string(),
            resource_version: Some("7".to_string()),
            ..ConfigMapObject::default()
        };
        let calls: Vec<(&str, Call<'_>)> = vec![
            (
                "get_lease",
                Box::pin(async { cluster.get_lease("l").await.map(|_| ()) }),
            ),
            (
                "create_lease",
                Box::pin(async { cluster.create_lease(&lease_object(None)).await.map(|_| ()) }),
            ),
            (
                "replace_lease",
                Box::pin(async {
                    let lease = lease_object(Some("7"));
                    cluster.replace_lease(&lease).await.map(|_| ())
                }),
            ),
            (
                "get_config_map",
                Box::pin(async { cluster.get_config_map("c").await.map(|_| ()) }),
            ),
            (
                "put_config_map_key",
                Box::pin(cluster.put_config_map_key("c", "k", "v")),
            ),
            (
                "get_config_map_object",
                Box::pin(async { cluster.get_config_map_object("c").await.map(|_| ()) }),
            ),
            (
                "create_config_map",
                Box::pin(async { cluster.create_config_map(&config_map).await.map(|_| ()) }),
            ),
            (
                "replace_config_map",
                Box::pin(async { cluster.replace_config_map(&config_map).await.map(|_| ()) }),
            ),
            (
                "list_mat_services",
                Box::pin(async { cluster.list_mat_services().await.map(|_| ()) }),
            ),
            (
                "list_pods",
                Box::pin(async { cluster.list_pods("ns", "app=x").await.map(|_| ()) }),
            ),
        ];
        let (names, calls): (Vec<_>, Vec<_>) = calls.into_iter().unzip();
        let started = std::time::Instant::now();
        let results = futures::future::join_all(calls).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        for (name, result) in names.iter().zip(results) {
            assert_eq!(result, Err(ClusterError::Timeout(1)), "{name}");
        }
        assert_eq!(asked.lock().unwrap().len(), 10, "each sent once");

        // Without the UID the API would make a missing Lease.
        for uid in [None, Some(String::new())] {
            let lease = LeaseObject {
                uid,
                ..lease_object(Some("7"))
            };
            assert_eq!(
                cluster.replace_lease(&lease).await,
                Err(ClusterError::Invalid(
                    "a Lease replace needs the Lease UID".to_string()
                ))
            );
        }
        // The API would replace these unconditionally.
        for version in [None, Some(""), Some("0")] {
            assert_eq!(
                cluster.replace_lease(&lease_object(version)).await,
                Err(ClusterError::Conflict),
                "{version:?}"
            );
            let config_map = ConfigMapObject {
                name: "c".to_string(),
                resource_version: version.map(str::to_string),
                ..ConfigMapObject::default()
            };
            assert_eq!(
                cluster.replace_config_map(&config_map).await,
                Err(ClusterError::Conflict),
                "{version:?}"
            );
        }
        assert_eq!(asked.lock().unwrap().len(), 10, "no replace was sent");
    }

    #[test]
    fn times_before_the_epoch_and_restart_sums_convert() {
        for at in ["1969-12-31T23:59:59.5Z", "2300-01-01T00:00:00.25Z"] {
            let stamp: Timestamp = at.parse().unwrap();
            assert_eq!(
                to_chrono(stamp),
                DateTime::parse_from_rfc3339(at).ok().map(|t| t.to_utc()),
                "{at}"
            );
        }
        let status = |restarts| ContainerStatus {
            restart_count: restarts,
            ..ContainerStatus::default()
        };
        let busy = Pod {
            status: Some(PodStatus {
                container_statuses: Some(vec![status(i32::MAX); 3]),
                ..PodStatus::default()
            }),
            ..Pod::default()
        };
        assert_eq!(pod_from(busy).restarts, u32::MAX);
    }

    #[test]
    fn init_sidecar_restarts_count_towards_the_pod() {
        let statuses = |counts: &[i32]| {
            Some(
                counts
                    .iter()
                    .map(|&restart_count| ContainerStatus {
                        restart_count,
                        ..ContainerStatus::default()
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let pod_with = |main: &[i32], init: &[i32]| Pod {
            status: Some(PodStatus {
                container_statuses: statuses(main),
                init_container_statuses: statuses(init),
                ..PodStatus::default()
            }),
            ..Pod::default()
        };
        assert_eq!(pod_from(pod_with(&[0], &[1])).restarts, 1);
        assert_eq!(pod_from(pod_with(&[2], &[3])).restarts, 5);
        // Two main containers sum to u32::MAX - 1, the sidecar pushes past it.
        let full = [i32::MAX, i32::MAX];
        assert_eq!(pod_from(pod_with(&full, &[])).restarts, u32::MAX - 1);
        assert_eq!(pod_from(pod_with(&full, &[2])).restarts, u32::MAX);
        // A negative count reads as zero.
        assert_eq!(pod_from(pod_with(&[-1], &[1])).restarts, 1);
    }
}
