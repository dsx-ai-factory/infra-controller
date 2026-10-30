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

//! An in-memory [`Cluster`] for tests, with resource versions, label
//! selectors and scripted failures.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Notify, oneshot};

use super::{
    Cluster, ClusterError, ConfigMapObject, LeaseObject, LeaseReplaced, MatService, NamedConfigMap,
    PodInfo,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClusterOp {
    GetLease,
    CreateLease,
    ReplaceLease,
    GetConfigMap,
    PutConfigMapKey,
    GetConfigMapObject,
    CreateConfigMap,
    ReplaceConfigMap,
    ListMatServices,
    ListPods,
}

#[derive(Default)]
struct State {
    leases: BTreeMap<String, LeaseObject>,
    /// Keyed by namespace and name.
    config_maps: BTreeMap<(String, String), NamedConfigMap>,
    /// What each ConfigMap carries besides its labels and data.
    config_map_meta: BTreeMap<(String, String), MapMeta>,
    mat_services: Vec<MatService>,
    pods: BTreeMap<String, Vec<PodInfo>>,
    failures: BTreeMap<ClusterOp, VecDeque<ClusterError>>,
    /// Writes that commit, then fail with this error.
    lost_replies: BTreeMap<ClusterOp, VecDeque<ClusterError>>,
    /// The next Lease write of each op is parked.
    park: BTreeSet<ClusterOp>,
    parked: VecDeque<Parked>,
    delays: BTreeMap<ClusterOp, Duration>,
    calls: Vec<(ClusterOp, String)>,
    next_version: u64,
    /// Lease replaces skip the UID check.
    skip_uid_check: bool,
    /// The next replace that makes a Lease answers without it.
    unread_created: bool,
}

#[derive(Default)]
struct MapMeta {
    resource_version: String,
    uid: String,
    annotations: BTreeMap<String, String>,
}

impl State {
    fn version(&mut self) -> String {
        self.next_version += 1;
        self.next_version.to_string()
    }

    /// Stores `lease` with a new resource version, and a new UID when
    /// `uid` is `None`.
    fn store(&mut self, lease: &LeaseObject, uid: Option<String>) -> LeaseObject {
        let version = self.version();
        let mut stored = lease.clone();
        stored.uid = Some(uid.unwrap_or_else(|| format!("uid-{version}")));
        stored.resource_version = Some(version);
        self.leases.insert(stored.name.clone(), stored.clone());
        stored
    }

    /// A write to the ConfigMap `key`: a new resource version, and a UID
    /// when it is new.
    fn touch(&mut self, key: &(String, String)) -> &mut MapMeta {
        let version = self.version();
        let meta = self
            .config_map_meta
            .entry(key.clone())
            .or_insert_with(|| MapMeta {
                uid: format!("uid-{version}"),
                ..MapMeta::default()
            });
        meta.resource_version = version;
        meta
    }

    fn config_map_object(&self, key: &(String, String)) -> Option<ConfigMapObject> {
        let config_map = self.config_maps.get(key)?;
        let meta = self.config_map_meta.get(key);
        Some(ConfigMapObject {
            name: config_map.name.clone(),
            resource_version: meta.map(|m| m.resource_version.clone()),
            uid: meta.map(|m| m.uid.clone()),
            labels: config_map.labels.clone(),
            annotations: meta.map(|m| m.annotations.clone()).unwrap_or_default(),
            data: config_map.data.clone(),
        })
    }
}

type Hook = Box<dyn Fn(ClusterOp, &str) + Send + Sync>;

type Reply = Result<LeaseReplaced, ClusterError>;

/// A Lease write the server took but has not committed yet.
struct Parked {
    op: ClusterOp,
    lease: LeaseObject,
    reply: oneshot::Sender<Reply>,
}

/// Parks `lease` when the next write of `op` is to be parked. The receiver
/// answers once [`FakeCluster::commit_parked`] commits it.
fn park(state: &mut State, op: ClusterOp, lease: &LeaseObject) -> Option<oneshot::Receiver<Reply>> {
    if !state.park.remove(&op) {
        return None;
    }
    let (reply, answer) = oneshot::channel();
    state.parked.push_back(Parked {
        op,
        lease: lease.clone(),
        reply,
    });
    Some(answer)
}

/// Commits a create as the API server does, with a new UID.
fn create(state: &mut State, lease: &LeaseObject) -> Result<LeaseObject, ClusterError> {
    if state.leases.contains_key(&lease.name) {
        return Err(ClusterError::AlreadyExists);
    }
    Ok(state.store(lease, None))
}

/// Commits a replace as the API server does: the UID, when sent, and the
/// resource version are compared when the write commits. A replace with
/// no UID makes a missing Lease, with a new UID, whatever version it
/// sent, and answers 201.
fn replace(state: &mut State, lease: &LeaseObject) -> Reply {
    let uid_checked = lease.uid.is_some() && !state.skip_uid_check;
    let Some(current) = state.leases.get(&lease.name) else {
        if uid_checked {
            return Err(ClusterError::Conflict);
        }
        let made = state.store(lease, None);
        if std::mem::take(&mut state.unread_created) {
            return Ok(LeaseReplaced::CreatedUnread);
        }
        return Ok(LeaseReplaced::Created(made));
    };
    if current.resource_version != lease.resource_version
        || (uid_checked && current.uid != lease.uid)
    {
        return Err(ClusterError::Conflict);
    }
    let uid = current.uid.clone();
    Ok(LeaseReplaced::Replaced(state.store(lease, uid)))
}

/// `stored`, unless a lost reply is queued for `op`.
fn lost_or<T>(state: &mut State, op: ClusterOp, stored: T) -> Result<T, ClusterError> {
    match state
        .lost_replies
        .get_mut(&op)
        .and_then(VecDeque::pop_front)
    {
        Some(error) => Err(error),
        None => Ok(stored),
    }
}

/// The run's namespace holds its Leases, ConfigMaps and MAT Services.
pub struct FakeCluster {
    namespace: String,
    state: Mutex<State>,
    hooks: Mutex<Vec<Hook>>,
    /// Wakes [`wait_parked`](Self::wait_parked) when a write is parked.
    parked_changed: Notify,
}

impl FakeCluster {
    pub fn new(namespace: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            state: Mutex::default(),
            hooks: Mutex::default(),
            parked_changed: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records the call, runs the hooks, then returns a scripted failure if
    /// one is queued.
    fn enter(&self, op: ClusterOp, target: &str) -> Result<MutexGuard<'_, State>, ClusterError> {
        let failure = {
            let mut state = self.lock();
            state.calls.push((op, target.to_string()));
            state.failures.get_mut(&op).and_then(VecDeque::pop_front)
        };
        for hook in self.hooks.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            hook(op, target);
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(self.lock()),
        }
    }

    /// Runs `hook` at the start of every later call, before it reads or
    /// writes. The hook may change this cluster.
    pub fn on_call(&self, hook: impl Fn(ClusterOp, &str) + Send + Sync + 'static) {
        self.hooks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(hook));
    }

    /// Every later call of `op` waits `by` before it runs.
    /// `Duration::MAX` never completes.
    pub fn delay(&self, op: ClusterOp, by: Duration) {
        self.lock().delays.insert(op, by);
    }

    async fn pause(&self, op: ClusterOp) {
        let delay = self.lock().delays.get(&op).copied();
        match delay {
            Some(Duration::MAX) => std::future::pending().await,
            Some(by) => tokio::time::sleep(by).await,
            None => {}
        }
    }

    /// The next call of `op` fails with `error`. Queued in order.
    pub fn fail_next(&self, op: ClusterOp, error: ClusterError) {
        self.lock().failures.entry(op).or_default().push_back(error);
    }

    /// The next Lease or ConfigMap create or replace of `op` commits, then
    /// fails with `error`, as a write whose reply was lost. Queued in order.
    pub fn lose_reply(&self, op: ClusterOp, error: ClusterError) {
        self.lock()
            .lost_replies
            .entry(op)
            .or_default()
            .push_back(error);
    }

    /// The next Lease create or replace of `op` is parked at the server. It
    /// commits only on [`commit_parked`](Self::commit_parked), whether or not
    /// its caller still waits, and its caller waits until then.
    pub fn park_next(&self, op: ClusterOp) {
        self.lock().park.insert(op);
    }

    /// Waits until a write is parked.
    pub async fn wait_parked(&self) {
        loop {
            let changed = self.parked_changed.notified();
            if !self.lock().parked.is_empty() {
                return;
            }
            changed.await;
        }
    }

    /// Commits the oldest parked write, and answers its caller if it still
    /// waits. `None` when nothing is parked.
    pub fn commit_parked(&self) -> Option<Reply> {
        let mut state = self.lock();
        let parked = state.parked.pop_front()?;
        let committed = match parked.op {
            ClusterOp::CreateLease => create(&mut state, &parked.lease).map(LeaseReplaced::Created),
            _ => replace(&mut state, &parked.lease),
        };
        // The caller may have given up: the write stands anyway.
        let _ = parked.reply.send(committed.clone());
        Some(committed)
    }

    /// Answers a parked write once it commits.
    async fn parked(&self, answer: oneshot::Receiver<Reply>) -> Reply {
        self.parked_changed.notify_waiters();
        answer
            .await
            .unwrap_or_else(|_| Err(ClusterError::Transport("parked write dropped".to_string())))
    }

    pub fn calls(&self) -> Vec<(ClusterOp, String)> {
        self.lock().calls.clone()
    }

    pub fn set_mat_services(&self, services: Vec<MatService>) {
        self.lock().mat_services = services;
    }

    pub fn set_pods(&self, namespace: &str, pods: Vec<PodInfo>) {
        self.lock().pods.insert(namespace.to_string(), pods);
    }

    /// Adds the pod, or replaces the one with its name.
    pub fn upsert_pod(&self, namespace: &str, pod: PodInfo) {
        let mut state = self.lock();
        let pods = state.pods.entry(namespace.to_string()).or_default();
        pods.retain(|p| p.name != pod.name);
        pods.push(pod);
    }

    pub fn remove_pod(&self, namespace: &str, name: &str) {
        if let Some(pods) = self.lock().pods.get_mut(namespace) {
            pods.retain(|p| p.name != name);
        }
    }

    pub fn put_config_map(&self, namespace: &str, config_map: NamedConfigMap) {
        let mut state = self.lock();
        let key = (namespace.to_string(), config_map.name.clone());
        state.touch(&key);
        state.config_maps.insert(key, config_map);
    }

    /// A ConfigMap of the run's namespace with its resource version, UID
    /// and annotations.
    pub fn config_map_object(&self, name: &str) -> Option<ConfigMapObject> {
        self.lock()
            .config_map_object(&(self.namespace.clone(), name.to_string()))
    }

    /// Data of a ConfigMap in the run's namespace.
    pub fn config_map(&self, name: &str) -> Option<BTreeMap<String, String>> {
        self.lock()
            .config_maps
            .get(&(self.namespace.clone(), name.to_string()))
            .map(|cm| cm.data.clone())
    }

    pub fn lease(&self, name: &str) -> Option<LeaseObject> {
        self.lock().leases.get(name).cloned()
    }

    /// Stores `lease` as another writer would, with a new resource version.
    /// It keeps the stored Lease's UID, or gets a new one when no Lease is
    /// stored, whatever UID `lease` has.
    pub fn set_lease(&self, lease: LeaseObject) -> LeaseObject {
        let mut state = self.lock();
        let uid = state.leases.get(&lease.name).and_then(|l| l.uid.clone());
        state.store(&lease, uid)
    }

    /// Every later Lease replace skips the UID check, as an API that
    /// ignores it would: a replace of a missing Lease then makes it.
    pub fn skip_uid_check(&self) {
        self.lock().skip_uid_check = true;
    }

    /// The next replace that makes a Lease answers 201 with no Lease in
    /// it, as a reply whose body broke.
    pub fn unread_created(&self) {
        self.lock().unread_created = true;
    }

    /// Deletes the Lease as an operator would, with a precondition on its
    /// resource version: `Conflict` when it changed, 404 when it is gone.
    pub fn delete_lease(&self, name: &str, resource_version: &str) -> Result<(), ClusterError> {
        let mut state = self.lock();
        let Some(current) = state.leases.get(name) else {
            return Err(not_found(name));
        };
        if current.resource_version.as_deref() != Some(resource_version) {
            return Err(ClusterError::Conflict);
        }
        state.leases.remove(name);
        Ok(())
    }
}

fn not_found(name: &str) -> ClusterError {
    ClusterError::Api {
        code: 404,
        message: format!("lease {name} not found"),
    }
}

#[async_trait]
impl Cluster for FakeCluster {
    async fn get_lease(&self, name: &str) -> Result<Option<LeaseObject>, ClusterError> {
        self.pause(ClusterOp::GetLease).await;
        let state = self.enter(ClusterOp::GetLease, name)?;
        Ok(state.leases.get(name).cloned())
    }

    async fn create_lease(&self, lease: &LeaseObject) -> Result<LeaseObject, ClusterError> {
        self.pause(ClusterOp::CreateLease).await;
        let answer = {
            let mut state = self.enter(ClusterOp::CreateLease, &lease.name)?;
            match park(&mut state, ClusterOp::CreateLease, lease) {
                Some(answer) => answer,
                None => {
                    let stored = create(&mut state, lease)?;
                    return lost_or(&mut state, ClusterOp::CreateLease, stored);
                }
            }
        };
        self.parked(answer).await.and_then(|written| {
            written
                .into_lease()
                .ok_or_else(|| ClusterError::Invalid("the reply did not decode".to_string()))
        })
    }

    async fn replace_lease(&self, lease: &LeaseObject) -> Result<LeaseReplaced, ClusterError> {
        self.pause(ClusterOp::ReplaceLease).await;
        let answer = {
            let mut state = self.enter(ClusterOp::ReplaceLease, &lease.name)?;
            match park(&mut state, ClusterOp::ReplaceLease, lease) {
                Some(answer) => answer,
                None => {
                    let stored = replace(&mut state, lease)?;
                    return lost_or(&mut state, ClusterOp::ReplaceLease, stored);
                }
            }
        };
        self.parked(answer).await
    }

    async fn get_config_map(
        &self,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, ClusterError> {
        self.pause(ClusterOp::GetConfigMap).await;
        let state = self.enter(ClusterOp::GetConfigMap, name)?;
        Ok(state
            .config_maps
            .get(&(self.namespace.clone(), name.to_string()))
            .map(|cm| cm.data.clone()))
    }

    async fn put_config_map_key(
        &self,
        name: &str,
        key: &str,
        value: &str,
    ) -> Result<(), ClusterError> {
        self.pause(ClusterOp::PutConfigMapKey).await;
        let mut state = self.enter(ClusterOp::PutConfigMapKey, name)?;
        let map = (self.namespace.clone(), name.to_string());
        state.touch(&map);
        let config_map = state
            .config_maps
            .entry(map)
            .or_insert_with(|| NamedConfigMap {
                name: name.to_string(),
                labels: BTreeMap::new(),
                data: BTreeMap::new(),
            });
        config_map.data.insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn get_config_map_object(
        &self,
        name: &str,
    ) -> Result<Option<ConfigMapObject>, ClusterError> {
        self.pause(ClusterOp::GetConfigMapObject).await;
        let state = self.enter(ClusterOp::GetConfigMapObject, name)?;
        Ok(state.config_map_object(&(self.namespace.clone(), name.to_string())))
    }

    async fn create_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError> {
        self.pause(ClusterOp::CreateConfigMap).await;
        let mut state = self.enter(ClusterOp::CreateConfigMap, &config_map.name)?;
        let key = (self.namespace.clone(), config_map.name.clone());
        if state.config_maps.contains_key(&key) {
            return Err(ClusterError::AlreadyExists);
        }
        state.config_map_meta.remove(&key);
        state.touch(&key).annotations = config_map.annotations.clone();
        state.config_maps.insert(
            key.clone(),
            NamedConfigMap {
                name: config_map.name.clone(),
                labels: config_map.labels.clone(),
                data: config_map.data.clone(),
            },
        );
        let stored = state.config_map_object(&key).expect("just created");
        lost_or(&mut state, ClusterOp::CreateConfigMap, stored)
    }

    async fn replace_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError> {
        self.pause(ClusterOp::ReplaceConfigMap).await;
        let mut state = self.enter(ClusterOp::ReplaceConfigMap, &config_map.name)?;
        let key = (self.namespace.clone(), config_map.name.clone());
        let Some(current) = state.config_map_object(&key) else {
            return Err(ClusterError::Api {
                code: 404,
                message: format!("configmap {} not found", config_map.name),
            });
        };
        let other_uid = config_map.uid.is_some() && config_map.uid != current.uid;
        if current.resource_version != config_map.resource_version || other_uid {
            return Err(ClusterError::Conflict);
        }
        state.touch(&key).annotations = config_map.annotations.clone();
        if let Some(stored) = state.config_maps.get_mut(&key) {
            stored.labels = config_map.labels.clone();
            stored.data = config_map.data.clone();
        }
        let stored = state.config_map_object(&key).expect("just replaced");
        lost_or(&mut state, ClusterOp::ReplaceConfigMap, stored)
    }

    async fn list_mat_services(&self) -> Result<Vec<MatService>, ClusterError> {
        self.pause(ClusterOp::ListMatServices).await;
        let state = self.enter(ClusterOp::ListMatServices, &self.namespace)?;
        Ok(state.mat_services.clone())
    }

    async fn list_pods(
        &self,
        namespace: &str,
        label_selector: &str,
    ) -> Result<Vec<PodInfo>, ClusterError> {
        self.pause(ClusterOp::ListPods).await;
        let target = format!("{namespace}/{label_selector}");
        let state = self.enter(ClusterOp::ListPods, &target)?;
        let selector = Selector::parse(label_selector)?;
        Ok(state
            .pods
            .get(namespace)
            .into_iter()
            .flatten()
            .filter(|p| selector.matches(&p.labels))
            .cloned()
            .collect())
    }
}

/// Equality-based label selectors: `k=v`, `k==v`, `k!=v`, `k`, `!k`.
struct Selector(Vec<Requirement>);

enum Requirement {
    Equals(String, String),
    NotEquals(String, String),
    Exists(String),
    NotExists(String),
}

impl Selector {
    fn parse(raw: &str) -> Result<Self, ClusterError> {
        let mut requirements = Vec::new();
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let requirement = if let Some((k, v)) = part.split_once("!=") {
                Requirement::NotEquals(k.trim().to_string(), v.trim().to_string())
            } else if let Some((k, v)) = part.split_once("==").or_else(|| part.split_once('=')) {
                Requirement::Equals(k.trim().to_string(), v.trim().to_string())
            } else if let Some(k) = part.strip_prefix('!') {
                Requirement::NotExists(k.trim().to_string())
            } else if part.contains([' ', '(']) {
                return Err(ClusterError::Api {
                    code: 400,
                    message: format!("fake cluster does not support selector {part:?}"),
                });
            } else {
                Requirement::Exists(part.to_string())
            };
            requirements.push(requirement);
        }
        Ok(Self(requirements))
    }

    fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.0.iter().all(|r| match r {
            Requirement::Equals(k, v) => labels.get(k) == Some(v),
            Requirement::NotEquals(k, v) => labels.get(k) != Some(v),
            Requirement::Exists(k) => labels.contains_key(k),
            Requirement::NotExists(k) => !labels.contains_key(k),
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;

    fn pod(name: &str, labels: &[(&str, &str)]) -> PodInfo {
        PodInfo {
            name: name.to_string(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            annotations: BTreeMap::new(),
            phase: "Running".to_string(),
            ready: true,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            restarts: 0,
            ip: None,
        }
    }

    fn lease(rv: Option<&str>) -> LeaseObject {
        LeaseObject {
            name: "lock".to_string(),
            resource_version: rv.map(str::to_string),
            uid: None,
            holder_identity: Some("test/r1/x".to_string()),
            lease_duration_seconds: 60,
            acquire_time: None,
            renew_time: None,
            annotations: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn fake_cluster_selects_pods_by_labels() {
        let cluster = FakeCluster::new("mat");
        cluster.set_pods(
            "nico",
            vec![
                pod("api-0", &[("app", "nico-api"), ("tier", "core")]),
                pod("api-1", &[("app", "nico-api")]),
                pod("dhcp-0", &[("app", "nico-dhcp"), ("tier", "core")]),
            ],
        );
        let names = |pods: Vec<PodInfo>| pods.into_iter().map(|p| p.name).collect::<Vec<_>>();
        assert_eq!(
            names(cluster.list_pods("nico", "app=nico-api").await.unwrap()),
            ["api-0", "api-1"]
        );
        assert_eq!(
            names(
                cluster
                    .list_pods("nico", "tier,app!=nico-api")
                    .await
                    .unwrap()
            ),
            ["dhcp-0"]
        );
        assert_eq!(
            names(cluster.list_pods("nico", "!tier").await.unwrap()),
            ["api-1"]
        );
        assert!(
            cluster
                .list_pods("other", "app=nico-api")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(cluster.list_pods("nico", "app in (a,b)").await.is_err());
    }

    #[tokio::test]
    async fn fake_cluster_leases_use_resource_versions() {
        let cluster = FakeCluster::new("mat");
        let created = cluster.create_lease(&lease(None)).await.unwrap();
        assert_eq!(
            cluster.create_lease(&lease(None)).await,
            Err(ClusterError::AlreadyExists)
        );
        let Ok(LeaseReplaced::Replaced(replaced)) = cluster.replace_lease(&created).await else {
            panic!("the stored Lease is replaced");
        };
        assert_ne!(replaced.resource_version, created.resource_version);
        assert_eq!(replaced.uid, created.uid, "a replace keeps the UID");
        assert_eq!(
            cluster.replace_lease(&created).await,
            Err(ClusterError::Conflict),
            "stale resource version"
        );
        let other_uid = LeaseObject {
            uid: Some("other".to_string()),
            ..replaced.clone()
        };
        assert_eq!(
            cluster.replace_lease(&other_uid).await,
            Err(ClusterError::Conflict),
            "another UID"
        );

        cluster.fail_next(
            ClusterOp::GetLease,
            ClusterError::Transport("down".to_string()),
        );
        assert!(cluster.get_lease("lock").await.is_err());
        assert_eq!(cluster.get_lease("lock").await.unwrap(), Some(replaced));
    }

    #[tokio::test]
    async fn parked_write_commits_without_its_caller() {
        let cluster = std::sync::Arc::new(FakeCluster::new("mat"));
        let created = cluster.create_lease(&lease(None)).await.unwrap();
        let send = |lease: LeaseObject| {
            let cluster = cluster.clone();
            tokio::spawn(async move { cluster.replace_lease(&lease).await })
        };

        // The caller gives up. The server still holds its write.
        cluster.park_next(ClusterOp::ReplaceLease);
        let call = send(created.clone());
        cluster.wait_parked().await;
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        assert_eq!(cluster.lease("lock"), Some(created.clone()), "not yet");
        let committed = cluster
            .commit_parked()
            .unwrap()
            .unwrap()
            .into_lease()
            .unwrap();
        assert_ne!(committed.resource_version, created.resource_version);
        assert_eq!(cluster.lease("lock"), Some(committed.clone()));

        // The version is compared at commit: another writer got in first.
        cluster.park_next(ClusterOp::ReplaceLease);
        let call = send(committed.clone());
        cluster.wait_parked().await;
        cluster.set_lease(committed);
        assert_eq!(cluster.commit_parked(), Some(Err(ClusterError::Conflict)));
        assert_eq!(
            call.await.unwrap(),
            Err(ClusterError::Conflict),
            "a caller that still waits gets the answer"
        );
        assert_eq!(cluster.commit_parked(), None);
    }

    #[tokio::test]
    async fn a_replace_of_a_missing_lease_makes_it_unless_the_uid_is_checked() {
        // As the API server: the UID refuses the write, and without it the
        // replace makes the Lease with a new UID, whatever version it sent.
        let cluster = FakeCluster::new("mat");
        let created = cluster.create_lease(&lease(None)).await.unwrap();
        cluster
            .delete_lease("lock", created.resource_version.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            cluster.replace_lease(&created).await,
            Err(ClusterError::Conflict)
        );
        assert_eq!(cluster.lease("lock"), None, "nothing made");

        let no_uid = LeaseObject {
            uid: None,
            ..created.clone()
        };
        let Ok(LeaseReplaced::Created(made)) = cluster.replace_lease(&no_uid).await else {
            panic!("a replace with no UID makes the Lease");
        };
        assert_ne!(made.uid, created.uid);
        assert_eq!(cluster.lease("lock"), Some(made.clone()));

        // An API that skips the UID check makes it too.
        cluster
            .delete_lease("lock", made.resource_version.as_deref().unwrap())
            .unwrap();
        cluster.skip_uid_check();
        let Ok(LeaseReplaced::Created(again)) = cluster.replace_lease(&made).await else {
            panic!("the UID check is skipped");
        };
        assert_ne!(again.uid, made.uid);
    }

    #[tokio::test]
    async fn fake_cluster_config_map_keys() {
        let cluster = FakeCluster::new("mat");
        cluster
            .put_config_map_key("r1-run", "run.json", "{}")
            .await
            .unwrap();
        cluster
            .put_config_map_key("r1-run", "other", "x")
            .await
            .unwrap();
        let data = cluster.get_config_map("r1-run").await.unwrap().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(cluster.config_map("r1-run").unwrap()["run.json"], "{}");
        assert_eq!(cluster.get_config_map("missing").await.unwrap(), None);
    }
    #[tokio::test]
    async fn fake_config_maps_compare_and_swap() {
        let cluster = FakeCluster::new("mat");
        let wanted = ConfigMapObject {
            name: "r1-result".to_string(),
            annotations: BTreeMap::from([("owner".to_string(), "a".to_string())]),
            ..ConfigMapObject::default()
        };
        let created = cluster.create_config_map(&wanted).await.unwrap();
        assert_eq!(
            cluster.create_config_map(&wanted).await,
            Err(ClusterError::AlreadyExists)
        );
        assert_eq!(created.annotations, wanted.annotations);
        assert!(created.uid.is_some());

        // Any write moves the version on: a replace from an older read fails.
        cluster
            .put_config_map_key("r1-result", "result.json", "{}")
            .await
            .unwrap();
        assert_eq!(
            cluster.replace_config_map(&created).await,
            Err(ClusterError::Conflict)
        );
        let current = cluster
            .get_config_map_object("r1-result")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.data["result.json"], "{}");
        let other_uid = ConfigMapObject {
            uid: Some("another".to_string()),
            ..current.clone()
        };
        assert_eq!(
            cluster.replace_config_map(&other_uid).await,
            Err(ClusterError::Conflict)
        );

        // A replace whose reply is lost still commits.
        cluster.lose_reply(
            ClusterOp::ReplaceConfigMap,
            ClusterError::Transport("reset".to_string()),
        );
        let owned = ConfigMapObject {
            annotations: BTreeMap::from([("owner".to_string(), "b".to_string())]),
            ..current.clone()
        };
        assert!(cluster.replace_config_map(&owned).await.is_err());
        let stored = cluster.config_map_object("r1-result").unwrap();
        assert_eq!(stored.annotations["owner"], "b");
        assert_ne!(stored.resource_version, current.resource_version);
        assert_eq!(stored.uid, current.uid);
    }
}
