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
use std::error;
use std::path::{Path, PathBuf};

use carbide_authn::middleware::Principal;
use casbin::{CoreApi, DefaultModel, Enforcer, FileAdapter};

use crate::auth::{Authorization, AuthorizationError, PolicyEngine, Predicate};

pub(super) enum ModelType {
    // Basic ACL with three arguments (subject, action, object)
    _BasicAcl,

    // A custom model that does RBAC on (subject, action) with glob matching
    // on the action.
    Rbac,
}

pub(super) struct CasbinEngine {
    inner: Enforcer,
}

impl CasbinEngine {
    pub(super) async fn new(
        model_type: ModelType,
        policy_path: &Path,
    ) -> Result<Self, Box<dyn error::Error>> {
        let model = build_model(model_type).await;
        let policy_path = PathBuf::from(policy_path);
        let adapter = FileAdapter::new(policy_path);
        let enforcer = Enforcer::new(model, adapter).await?;
        Ok(CasbinEngine { inner: enforcer })
    }
}

impl PolicyEngine for CasbinEngine {
    fn authorize(
        &self,
        principals: &[Principal],
        predicate: Predicate,
    ) -> Result<Authorization, AuthorizationError> {
        let enforcer = &self.inner;

        // We move the predicate into the Authorization later, so let's record a
        // printable version of it up front for our logging needs.
        let dbg_predicate = format!("{:?}", predicate);

        let auth_result = principals
            .iter()
            .find(|principal| {
                let cas_subject = principal.as_identifier();
                // Casbin is pretty stringly-typed under the hood. Be careful
                // that what we're passing in here matches the order that the
                // model and policy use.
                let enforce_result = match &predicate {
                    Predicate::ForgeCall(method) => {
                        let forge_call = format!("forge/{method}");
                        enforcer.enforce((cas_subject, forge_call))
                    }
                };
                match enforce_result {
                    Ok(true) => true,
                    Ok(false) => {
                        tracing::debug!(?principal, ?dbg_predicate, "CasbinEngine: denied");
                        false
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "CasbinEngine: error from enforcer");
                        false
                    }
                }
            })
            .map(|principal| Authorization {
                _principal: principal.clone(),
                _predicate: predicate,
            })
            .ok_or(AuthorizationError::Unauthorized);

        if let Ok(authorization) = auth_result.as_ref() {
            tracing::debug!(?authorization, "CasbinEngine: authorized");
        }

        auth_result
    }
}

async fn build_model(model_type: ModelType) -> DefaultModel {
    // TODO: Is it possible to build this using the inscrutable .add_def()
    // method of DefaultModel? That seems to be what from_str() is implemented
    // on top of.
    let policy_config = match model_type {
        ModelType::_BasicAcl => MODEL_CONFIG_ACL,
        ModelType::Rbac => MODEL_CONFIG_RBAC,
    };
    DefaultModel::from_str(policy_config)
        .await
        .expect("Could not load ACL model")
}

// This is the "basic model" from the supported models, aka "ACL without superuser".
const MODEL_CONFIG_ACL: &str = r#"
[request_definition]
r = sub, obj, act

[policy_definition]
p = sub, obj, act

[policy_effect]
e = some(where (p.eft == allow))

[matchers]
m = r.sub == p.sub && r.obj == p.obj && r.act == p.act
"#;

const MODEL_CONFIG_RBAC: &str = r#"
[request_definition]
r = sub, act

[policy_definition]
p = sub, act

[role_definition]
g = _, _

[policy_effect]
e = some(where (p.eft == allow))

[matchers]
m = g(r.sub, p.sub) && globMatch(r.act, p.act)
"#;
