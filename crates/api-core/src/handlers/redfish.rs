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
use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;

use arc_swap::ArcSwap;
use carbide_instrument::emit;
use carbide_secrets::credentials::CredentialReader;
use carbide_utils::HostPortPair;
use carbide_utils::redfish::{format_forwarded_host_parameter, parse_uri_host_ip};
use chrono::{DateTime, Local};
use db::redfish_actions::{
    approve_request, delete_request, fetch_request, find_serials, insert_request, list_requests,
    set_applied, update_response,
};
use db::{Transaction, TransactionVending};
use http::header::CONTENT_TYPE;
use http::uri::Authority;
use http::{HeaderMap, HeaderValue, Uri};
use model::redfish::BMCResponse;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::CarbideError;
use crate::api::log_request_data;
use crate::auth::external_user_info;

// TODO: put this in carbide config?
pub const NUM_REQUIRED_APPROVALS: usize = 2;

/// A detached Redfish action finished, but its result never made it into the
/// database. The `request_id` and `result_index` let an operator find the
/// missing slot without turning either unbounded value into a metric label.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "redfish_action_result_persistence_failed",
    metric_name = "carbide_redfish_action_result_persistence_failures_total",
    component = "nico-api",
    log = error,
    metric = counter,
    message = "Error applying redfish action",
    describe = "Number of detached Redfish action results that failed to persist"
)]
struct RedfishActionResultPersistenceFailed {
    #[context]
    request_id: i64,
    #[context]
    result_index: usize,
    #[context]
    error: String,
}

pub(crate) async fn redfish_browse(
    api: &crate::api::Api,
    request: tonic::Request<::rpc::forge::RedfishBrowseRequest>,
) -> Result<tonic::Response<::rpc::forge::RedfishBrowseResponse>, tonic::Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let uri: http::Uri = match request.uri.clone().parse() {
        Ok(uri) => uri,
        Err(err) => {
            return Err(CarbideError::internal(format!("parsing uri failed: {err}")).into());
        }
    };

    let (new_uri, headers, http_client, auth) = create_client(
        uri,
        &api.database_connection,
        api.credential_manager.as_ref(),
        &api.dynamic_settings.bmc_proxy,
        api.bmc_proxy_passthrough.as_deref(),
    )
    .await?;

    let request = auth.apply(http_client.request(http::Method::GET, new_uri.to_string()));
    let response = match request.headers(headers).send().await {
        Ok(response) => response,
        Err(e) => {
            return Err(CarbideError::internal(format!("http request failed: {e:?}")).into());
        }
    };

    let headers = response
        .headers()
        .iter()
        .map(|(x, y)| {
            (
                x.to_string(),
                String::from_utf8_lossy(y.as_bytes()).to_string(),
            )
        })
        .collect::<HashMap<String, String>>();

    let status = response.status();
    let text = response.text().await.map_err(|e| {
        CarbideError::internal(format!(
            "error reading response body: {e}, status: {status}"
        ))
    })?;

    Ok(tonic::Response::new(::rpc::forge::RedfishBrowseResponse {
        text,
        headers,
    }))
}
pub(crate) async fn redfish_list_actions(
    api: &crate::api::Api,
    request: tonic::Request<::rpc::forge::RedfishListActionsRequest>,
) -> Result<tonic::Response<::rpc::forge::RedfishListActionsResponse>, tonic::Status> {
    log_request_data(&request);

    let filter: model::redfish::RedfishListActionsFilter = request.into_inner().into();

    let result = list_requests(filter, &api.database_connection).await?;

    Ok(tonic::Response::new(
        rpc::forge::RedfishListActionsResponse {
            actions: result.into_iter().map(Into::into).collect(),
        },
    ))
}

pub(crate) async fn redfish_create_action(
    api: &crate::api::Api,
    request: tonic::Request<::rpc::forge::RedfishCreateActionRequest>,
) -> Result<tonic::Response<::rpc::forge::RedfishCreateActionResponse>, tonic::Status> {
    log_request_data(&request);

    let authored_by = external_user_info(&request)?.user.ok_or(
        CarbideError::ClientCertificateMissingInformation("external user name".to_string()),
    )?;

    let rpc_request = request.into_inner();
    let ips = rpc_request.ips.clone();
    let create_action: model::redfish::RedfishCreateAction = rpc_request.into();

    let mut txn = api.txn_begin().await?;

    let ip_to_serial = find_serials(&ips, &mut txn).await?;
    let machine_ips: Vec<_> = ip_to_serial.keys().cloned().collect();
    // this is the neatest way I could think of splitting the iterator/map into two vecs
    // explicitly in the same order. could be a for loop instead.
    let serials: Vec<_> = machine_ips
        .iter()
        .map(|ip| ip_to_serial.get(ip).unwrap())
        .collect();

    let request_id =
        insert_request(authored_by, create_action, &mut txn, machine_ips, serials).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(
        ::rpc::forge::RedfishCreateActionResponse { request_id },
    ))
}

pub(crate) async fn redfish_approve_action(
    api: &crate::api::Api,
    request: tonic::Request<::rpc::forge::RedfishActionId>,
) -> Result<tonic::Response<::rpc::forge::RedfishApproveActionResponse>, tonic::Status> {
    log_request_data(&request);

    let approver = external_user_info(&request)?.user.ok_or(
        CarbideError::ClientCertificateMissingInformation("external user name".to_string()),
    )?;

    let request: model::redfish::RedfishActionId = request.into_inner().into();

    let mut txn = api.txn_begin().await?;
    let action_request = fetch_request(request, &mut txn).await?;
    if action_request.approvers.contains(&approver) {
        return Err(
            CarbideError::InvalidArgument("user already approved request".to_owned()).into(),
        );
    }

    let is_approved = approve_request(approver, request, &mut txn).await?;
    if !is_approved {
        return Err(
            CarbideError::InvalidArgument("user already approved request".to_owned()).into(),
        );
    }
    txn.commit().await?;

    Ok(tonic::Response::new(
        ::rpc::forge::RedfishApproveActionResponse {},
    ))
}

pub(crate) async fn redfish_apply_action(
    api: &crate::api::Api,
    request: tonic::Request<::rpc::forge::RedfishActionId>,
) -> Result<tonic::Response<::rpc::forge::RedfishApplyActionResponse>, tonic::Status> {
    log_request_data(&request);

    let applier = external_user_info(&request)?.user.ok_or(
        CarbideError::ClientCertificateMissingInformation("external user name".to_string()),
    )?;

    let request: model::redfish::RedfishActionId = request.into_inner().into();

    let mut txn = api.txn_begin().await?;

    let action_request = fetch_request(request, &mut txn).await?;
    if action_request.applied_at.is_some() {
        return Err(CarbideError::InvalidArgument("action already applied".to_owned()).into());
    }

    if action_request.approvers.len() < NUM_REQUIRED_APPROVALS {
        return Err(CarbideError::InvalidArgument("insufficient approvals".to_owned()).into());
    }

    let ip_to_serial = find_serials(&action_request.machine_ips, &mut txn).await?;

    let is_applied = set_applied(applier, request, &mut txn).await?;
    if !is_applied {
        return Err(CarbideError::InvalidArgument("request was already applied".to_owned()).into());
    }

    let mut uris: Vec<(Uri, usize)> = Vec::with_capacity(action_request.machine_ips.len());

    // Do preflight checks in the foreground while the transaction is open, so it can be rolled back
    // on any error
    for (index, (machine_ip, original_serial)) in action_request
        .machine_ips
        .into_iter()
        .zip(action_request.board_serials)
        .enumerate()
    {
        // check that serial is the same.
        if ip_to_serial.get(&machine_ip) != Some(&original_serial) {
            update_response(request, &mut txn, BMCResponse {
                headers: HashMap::new(),
                status: "not executed".to_owned(),
                body: "machine serial did not match original serial at time of request creation. IP address was reused".to_owned(),
                completed_at: DateTime::from(Local::now()),
            }, index).await?;
        } else {
            uris.push((
                redfish_action_uri(&machine_ip, &action_request.target)?,
                index,
            ));
        }
    }

    for (uri, index) in uris {
        // Spawn off the task to send the request, open a transaction, and store the result.
        tokio::spawn({
            let pool = api.database_connection.clone();
            let credential_reader = api.credential_manager.clone();
            let bmc_proxy = api.dynamic_settings.bmc_proxy.clone();
            let core_proxy = api.bmc_proxy_passthrough.clone();
            let mut parameters = action_request.parameters.clone();
            async move {
                // Allow tests to trigger mock behavior by inserting a `"__TEST_BEHAVIOR__": "..."`
                // into the parameters list. Only supported in cfg(test), and not done in production.
                let test_behavior = TestBehavior::from_parameters_if_testing(&mut parameters);

                let response = handle_request(
                    parameters,
                    uri,
                    &pool,
                    credential_reader.as_ref(),
                    bmc_proxy.as_ref(),
                    core_proxy.as_deref(),
                    test_behavior,
                )
                .await;

                // Enclosing function may have returned. Nowhere to return error to.
                update_response_in_tx(&pool, request, index, response)
                    .await
                    .inspect_err(|e| {
                        emit(RedfishActionResultPersistenceFailed {
                            request_id: request.request_id,
                            result_index: index,
                            error: e.to_string(),
                        });
                    })
                    .ok();
            }
        });
    }

    txn.commit().await?;

    Ok(tonic::Response::new(
        ::rpc::forge::RedfishApplyActionResponse {},
    ))
}

async fn update_response_in_tx(
    pool: &PgPool,
    request: model::redfish::RedfishActionId,
    index: usize,
    response: BMCResponse,
) -> Result<(), tonic::Status> {
    let mut txn = Transaction::begin(pool).await?;
    update_response(request, &mut txn, response, index).await?;
    txn.commit().await?;
    Ok(())
}

async fn handle_request(
    parameters: String,
    uri: Uri,
    pool: &PgPool,
    credential_reader: &dyn CredentialReader,
    bmc_proxy: &ArcSwap<Option<HostPortPair>>,
    core_proxy: Option<&crate::bmc_proxy::PassthroughClient>,
    test_behavior: Option<TestBehavior>,
) -> BMCResponse {
    // Allow test mocks for returning errors at defined points
    let (new_uri, mut headers, http_client, auth) = match (
        create_client(uri, pool, credential_reader, bmc_proxy, core_proxy).await,
        test_behavior.and_then(TestBehavior::into_client_creation_error),
    ) {
        (Ok(tuple), None) => tuple,
        (Err(error), _) | (_, Some(error)) => {
            // Make a UUID for easy log correlation
            let failure_uuid = Uuid::new_v4();
            tracing::error!(
                failure_uuid = %failure_uuid,
                error = %error,
                "Redfish client creation failure",
            );

            // Set the "response" to indicate we couldn't get a redfish client. Don't
            // leak error string in case of credentials/etc.
            return BMCResponse {
                headers: HashMap::new(),
                status: "not executed".to_string(),
                body: format!("error creating redfish client, see logs: {failure_uuid}"),
                completed_at: DateTime::from(Local::now()),
            };
        }
    };

    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    // Don't perform the request if we're mocking the response
    let result = if let Some(e) = test_behavior.and_then(TestBehavior::into_request_error) {
        Err(e)
    } else if let Some(mock_response) = test_behavior.and_then(TestBehavior::into_mock_success) {
        Ok(mock_response)
    } else {
        let request = auth.apply(http_client.request(http::Method::POST, new_uri.to_string()));
        match request.body(parameters).headers(headers).send().await {
            Ok(response) => {
                let headers = response
                    .headers()
                    .iter()
                    .map(|(x, y)| {
                        (
                            x.to_string(),
                            String::from_utf8_lossy(y.as_bytes()).to_string(),
                        )
                    })
                    .collect::<HashMap<String, String>>();
                let status = response.status().to_string();
                let body = response
                    .text()
                    .await
                    .unwrap_or("could not decode body as text".to_owned());
                Ok(BMCResponse {
                    status,
                    headers,
                    body,
                    completed_at: DateTime::from(Local::now()),
                })
            }
            Err(e) => Err(e.into()),
        }
    };

    match result {
        Ok(response) => response,
        Err(e) => BMCResponse {
            headers: HashMap::new(),
            status: e
                .status_code
                .map(|s| s.to_string())
                .unwrap_or("missing status".to_owned()),
            body: e.description,
            completed_at: DateTime::from(Local::now()),
        },
    }
}

/// `metadata_host` extracts the port-free host key used for BMC metadata lookup.
///
/// `Uri::host` retains IPv6 brackets, which are URI syntax rather than part of
/// the address. Normalize IP literals to their bare form while leaving hostnames
/// unchanged; a URI without a host produces an empty lookup key.
fn metadata_host(uri: &Uri) -> String {
    uri.host()
        .map(|host| parse_uri_host_ip(host).map_or_else(|| host.to_string(), |ip| ip.to_string()))
        .unwrap_or_default()
}

/// `uri_authority` combines a host and separate port into URI authority syntax.
///
/// IP literals can arrive bare or bracketed, so normalize them and bracket IPv6
/// exactly once before appending the port. `Authority::try_from` rejects any
/// remaining invalid authority syntax.
fn uri_authority(host: &str, port: Option<u32>) -> Result<Authority, http::uri::InvalidUri> {
    let host = match parse_uri_host_ip(host) {
        Some(IpAddr::V4(ip)) => ip.to_string(),
        Some(IpAddr::V6(ip)) => format!("[{ip}]"),
        None => host.to_string(),
    };

    match port {
        Some(port) => Authority::try_from(format!("{host}:{port}")),
        None => Authority::try_from(host),
    }
}

/// `client_authority` applies a proxy override to the BMC metadata authority.
///
/// The boolean marks that the proxy variant supplied a host. This tells
/// `create_client` to include the original metadata IP in `Forwarded`.
/// `HostOnly` inherits the metadata port, while `PortOnly` keeps the metadata
/// host and leaves the boolean false.
fn client_authority(
    metadata_host: &str,
    metadata_port: Option<u32>,
    proxy_address: Option<&HostPortPair>,
) -> Result<(Authority, bool), http::uri::InvalidUri> {
    let (host, port, add_custom_header) = match proxy_address {
        None => (metadata_host, metadata_port, false),
        Some(HostPortPair::HostAndPort(host, port)) => {
            (host.as_str(), Some(u32::from(*port)), true)
        }
        Some(HostPortPair::HostOnly(host)) => (host.as_str(), metadata_port, true),
        Some(HostPortPair::PortOnly(port)) => (metadata_host, Some(u32::from(*port)), false),
    };

    uri_authority(host, port).map(|authority| (authority, add_custom_header))
}

/// `redfish_action_uri` builds the initial HTTPS URI for a Redfish action.
///
/// `machine_ip` is stored without URI brackets, so route it through
/// `uri_authority` before attaching `target`. `create_client` can replace this
/// authority later with the resolved BMC or proxy endpoint.
fn redfish_action_uri(machine_ip: &str, target: &str) -> Result<Uri, CarbideError> {
    let authority = uri_authority(machine_ip, None)
        .map_err(|error| CarbideError::internal(format!("invalid uri from machine_ip: {error}")))?;

    Uri::builder()
        .scheme("https")
        .authority(authority)
        .path_and_query(target)
        .build()
        .map_err(|error| CarbideError::internal(format!("invalid uri from machine_ip: {error}")))
}

/// How the raw Redfish passthrough authenticates to what it dials. Carrying
/// the credentials inside the variant makes "Basic auth without credentials"
/// unrepresentable.
enum PassthroughAuth {
    /// Direct to the BMC (possibly via the dev bmc-mock redirect): send the
    /// stored BMC credentials as Basic auth.
    BmcBasic(Box<rpc::forge::BmcMetaDataGetResponse>),
    /// Via nico-bmc-proxy, which authenticates upstream itself: send no
    /// credentials at all.
    CoreProxy,
}

impl PassthroughAuth {
    fn apply(
        &self,
        request: reqwest_middleware::RequestBuilder,
    ) -> reqwest_middleware::RequestBuilder {
        match self {
            Self::BmcBasic(metadata) => {
                request.basic_auth(metadata.user.clone(), Some(metadata.password.clone()))
            }
            Self::CoreProxy => request,
        }
    }
}

async fn create_client(
    uri: http::Uri,
    pool: &PgPool,
    credential_reader: &dyn CredentialReader,
    bmc_proxy: &ArcSwap<Option<HostPortPair>>,
    core_proxy: Option<&crate::bmc_proxy::PassthroughClient>,
) -> Result<
    (
        http::Uri,
        HeaderMap,
        reqwest_middleware::ClientWithMiddleware,
        PassthroughAuth,
    ),
    CarbideError,
> {
    // Via nico-bmc-proxy: the proxy resolves the BMC's credentials itself,
    // so no credential lookup happens here (a BMC with no stored root
    // credential fails at the proxy instead). The proxy dials the BMC's standard
    // https port; this handler never had per-machine port knowledge (the
    // metadata lookup on the direct path returns no port either). The
    // static [bmc_proxy] routing also wins over the dev-only dynamic
    // bmc-mock redirect.
    if let Some(passthrough) = core_proxy {
        let bmc_ip = metadata_host(&uri);
        // Keep the direct path's inventory check: the proxy forwards IP
        // targets without any lookup of its own, so skipping this would
        // let the admin passthrough relay requests to arbitrary addresses
        // from the proxy's network position.
        let mut txn = pool.txn_begin().await?;
        // Errors on an unparseable address or an IP with no inventoried BMC
        // interface -- that `Err` is the gate.
        crate::handlers::bmc_endpoint_explorer::validate_and_complete_bmc_endpoint_request(
            txn.as_mut(),
            Some(rpc::forge::BmcEndpointRequest {
                ip_address: bmc_ip.clone(),
                mac_address: None,
            }),
            None,
        )
        .await?;
        txn.commit().await?;

        let (new_authority, _) = client_authority(&bmc_ip, None, Some(passthrough.target()))
            .map_err(|error| CarbideError::internal(format!("creating url {error}")))?;
        let mut parts = uri.into_parts();
        parts.authority = Some(new_authority);
        let new_uri = http::Uri::from_parts(parts)
            .map_err(|e| CarbideError::internal(format!("invalid url parts {e}")))?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "forwarded",
            format_forwarded_host_parameter(&bmc_ip)
                .parse()
                .map_err(|e| CarbideError::internal(format!("invalid forwarded header: {e}")))?,
        );
        return Ok((
            new_uri,
            headers,
            passthrough.client().await,
            PassthroughAuth::CoreProxy,
        ));
    }

    let bmc_metadata_request = rpc::forge::BmcMetaDataGetRequest {
        machine_id: None,
        bmc_endpoint_request: Some(rpc::forge::BmcEndpointRequest {
            ip_address: metadata_host(&uri),
            mac_address: None,
        }),
        role: rpc::forge::UserRoles::Administrator.into(),
        request_type: rpc::forge::BmcRequestType::Ipmi.into(),
    };

    let metadata =
        crate::handlers::bmc_metadata::get_inner(bmc_metadata_request, pool, credential_reader)
            .await?;

    let proxy_address = bmc_proxy.load();
    let (new_authority, add_custom_header) =
        client_authority(&metadata.ip, metadata.port, proxy_address.as_ref().as_ref())
            .map_err(|error| CarbideError::internal(format!("creating url {error}")))?;
    let mut parts = uri.into_parts();
    parts.authority = Some(new_authority);
    let new_uri = http::Uri::from_parts(parts)
        .map_err(|e| CarbideError::internal(format!("invalid url parts {e}")))?;
    let mut headers = HeaderMap::new();
    if add_custom_header {
        headers.insert(
            "forwarded",
            format_forwarded_host_parameter(&metadata.ip)
                .parse()
                .unwrap(),
        );
    };
    let http_client = {
        let builder = reqwest::Client::builder();
        let builder = builder
            .danger_accept_invalid_certs(true)
            .redirect(reqwest::redirect::Policy::limited(5))
            .connect_timeout(std::time::Duration::from_secs(5)) // Limit connections to 5 seconds
            .timeout(std::time::Duration::from_secs(60)); // Limit the overall request to 60 seconds

        let client = match builder.build() {
            Ok(client) => client,
            Err(err) => {
                tracing::error!(error = %err, "build_http_client");
                return Err(CarbideError::internal(format!(
                    "http building failed: {err}"
                )));
            }
        };
        // The `reqwest-tracing` middleware injects the current span's W3C trace context into every
        // outgoing request (#2438).
        reqwest_middleware::ClientBuilder::new(client)
            .with(reqwest_tracing::TracingMiddleware::default())
            .build()
    };
    Ok((
        new_uri,
        headers,
        http_client,
        PassthroughAuth::BmcBasic(Box::new(metadata)),
    ))
}

pub(crate) async fn redfish_cancel_action(
    api: &crate::api::Api,
    request: tonic::Request<::rpc::forge::RedfishActionId>,
) -> Result<tonic::Response<::rpc::forge::RedfishCancelActionResponse>, tonic::Status> {
    log_request_data(&request);

    let request: model::redfish::RedfishActionId = request.into_inner().into();

    let mut txn = api.txn_begin().await?;

    delete_request(request, &mut txn).await?;

    txn.commit().await?;

    Ok(tonic::Response::new(
        ::rpc::forge::RedfishCancelActionResponse {},
    ))
}

#[derive(Serialize, Copy, Clone)]
enum TestBehavior {
    FailureAtClientCreation,
    FailureAtRequest,
    Success,
}

impl FromStr for TestBehavior {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "FailureAtClientCreation" => Ok(TestBehavior::FailureAtClientCreation),
            "FailureAtRequest" => Ok(TestBehavior::FailureAtRequest),
            "Success" => Ok(TestBehavior::Success),
            _ => Err(()),
        }
    }
}

impl TestBehavior {
    fn into_client_creation_error(self) -> Option<CarbideError> {
        if let TestBehavior::FailureAtClientCreation = self {
            Some(CarbideError::internal(
                "mock failure at client creation".to_owned(),
            ))
        } else {
            None
        }
    }

    fn into_request_error(self) -> Option<RequestErrorInfo> {
        if let TestBehavior::FailureAtRequest = self {
            Some(RequestErrorInfo {
                status_code: Some(http::status::StatusCode::INTERNAL_SERVER_ERROR),
                description: "Mock request error".to_string(),
            })
        } else {
            None
        }
    }

    fn into_mock_success(self) -> Option<BMCResponse> {
        if let TestBehavior::Success = self {
            Some(BMCResponse {
                headers: Default::default(),
                status: "OK".to_string(),
                body: "Mock success".to_string(),
                completed_at: Default::default(),
            })
        } else {
            None
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn from_parameters_if_testing(parameters: &mut String) -> Option<TestBehavior> {
        let mut param_obj: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(parameters).expect("invalid parameters");
        if let Some(serde_json::Value::String(test_behavior)) =
            param_obj.remove("__TEST_BEHAVIOR__")
        {
            // Recreate the original params object without __TEST_BEHAVIOR__ (since we removed it.)
            *parameters = serde_json::to_string(&param_obj).unwrap();
            Some(test_behavior.parse().unwrap())
        } else {
            None
        }
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn from_parameters_if_testing(_parameters: &mut String) -> Option<TestBehavior> {
        None
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) mod test_behavior {
    use super::TestBehavior;

    pub(crate) fn success() -> serde_json::Value {
        serde_json::to_value(TestBehavior::Success).expect("test behavior must serialize")
    }

    pub(crate) fn failure_at_request() -> serde_json::Value {
        serde_json::to_value(TestBehavior::FailureAtRequest).expect("test behavior must serialize")
    }

    pub(crate) fn failure_at_client_creation() -> serde_json::Value {
        serde_json::to_value(TestBehavior::FailureAtClientCreation)
            .expect("test behavior must serialize")
    }

    pub(crate) fn request_failure_description() -> String {
        TestBehavior::FailureAtRequest
            .into_request_error()
            .expect("request failure behavior must produce an error")
            .description
    }
}

// Subset of the data we care about from the HTTP error, so that we can mock it (we can't build our
// own reqwest error as its constructors are all private.)
struct RequestErrorInfo {
    status_code: Option<http::status::StatusCode>,
    description: String,
}

impl From<reqwest_middleware::Error> for RequestErrorInfo {
    fn from(e: reqwest_middleware::Error) -> Self {
        Self {
            status_code: e.status(),
            description: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use arc_swap::ArcSwap;
    use carbide_instrument::emit;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::value_scenarios;
    use carbide_utils::HostPortPair;

    use super::{
        PassthroughAuth, RedfishActionResultPersistenceFailed, client_authority, create_client,
        metadata_host, redfish_action_uri,
    };
    use crate::bmc_proxy::{PassthroughClient, test_config_with_generated_pems};

    #[test]
    fn redfish_action_result_persistence_failure_logs_and_counts() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(RedfishActionResultPersistenceFailed {
                request_id: 42,
                result_index: 3,
                error: "database unavailable".to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].metadata_name,
            "redfish_action_result_persistence_failed"
        );
        assert_eq!(logs[0].level, tracing::Level::ERROR);
        assert_eq!(logs[0].message, "Error applying redfish action");
        assert_eq!(
            logs[0].field("event_name"),
            Some("redfish_action_result_persistence_failed")
        );
        assert_eq!(
            logs[0].field("metric_name"),
            Some("carbide_redfish_action_result_persistence_failures_total")
        );
        assert_eq!(logs[0].field("request_id"), Some("42"));
        assert_eq!(logs[0].field("result_index"), Some("3"));
        assert_eq!(logs[0].field("error"), Some("database unavailable"));

        assert_eq!(
            metrics.counter_delta(
                "carbide_redfish_action_result_persistence_failures_total",
                &[],
            ),
            1.0
        );
    }

    #[test]
    fn metadata_host_is_bare_for_ip_literals() {
        value_scenarios!(run = |raw_uri| {
            let uri: http::Uri = raw_uri.parse().unwrap();
            metadata_host(&uri)
        };
            "IPv4" {
                "https://192.0.2.10/redfish/v1" => "192.0.2.10".to_string(),
                "https://192.0.2.10:8443/redfish/v1" => "192.0.2.10".to_string(),
            }

            "bracketed IPv6" {
                "https://[2001:db8::10]/redfish/v1" => "2001:db8::10".to_string(),
                "https://[2001:db8::10]:8443/redfish/v1" => "2001:db8::10".to_string(),
            }

            "hostname" {
                "https://bmc.example.com/redfish/v1" => "bmc.example.com".to_string(),
                "https://bmc.example.com:8443/redfish/v1" => "bmc.example.com".to_string(),
            }
        );
    }

    struct ClientAuthorityCase {
        metadata_host: &'static str,
        metadata_port: Option<u32>,
        proxy: Option<HostPortPair>,
    }

    #[test]
    fn client_authority_brackets_ipv6_for_proxy_variants() {
        value_scenarios!(run = |ClientAuthorityCase { metadata_host, metadata_port, proxy }| {
            client_authority(metadata_host, metadata_port, proxy.as_ref())
                .map(|(authority, forwarded)| (authority.to_string(), forwarded))
                .unwrap()
        };
            "direct BMC" {
                ClientAuthorityCase {
                    metadata_host: "192.0.2.10",
                    metadata_port: None,
                    proxy: None,
                } => ("192.0.2.10".to_string(), false),
                ClientAuthorityCase {
                    metadata_host: "2001:db8::10",
                    metadata_port: None,
                    proxy: None,
                } => ("[2001:db8::10]".to_string(), false),
                ClientAuthorityCase {
                    metadata_host: "2001:db8::10",
                    metadata_port: Some(8443),
                    proxy: None,
                } => ("[2001:db8::10]:8443".to_string(), false),
                ClientAuthorityCase {
                    metadata_host: "bmc.example.com",
                    metadata_port: Some(8443),
                    proxy: None,
                } => ("bmc.example.com:8443".to_string(), false),
            }

            "proxy host and port" {
                ClientAuthorityCase {
                    metadata_host: "2001:db8::10",
                    metadata_port: None,
                    proxy: Some(HostPortPair::HostAndPort(
                        "2001:db8::20".to_string(),
                        9443,
                    )),
                } => ("[2001:db8::20]:9443".to_string(), true),
                ClientAuthorityCase {
                    metadata_host: "192.0.2.10",
                    metadata_port: None,
                    proxy: Some(HostPortPair::HostAndPort(
                        "proxy.example.com".to_string(),
                        9443,
                    )),
                } => ("proxy.example.com:9443".to_string(), true),
            }

            "proxy host only" {
                ClientAuthorityCase {
                    metadata_host: "2001:db8::10",
                    metadata_port: Some(8443),
                    proxy: Some(HostPortPair::HostOnly("[2001:db8::20]".to_string())),
                } => ("[2001:db8::20]:8443".to_string(), true),
            }

            "proxy port only" {
                ClientAuthorityCase {
                    metadata_host: "2001:db8::10",
                    metadata_port: None,
                    proxy: Some(HostPortPair::PortOnly(9443)),
                } => ("[2001:db8::10]:9443".to_string(), false),
            }
        );
    }

    #[test]
    fn action_uri_brackets_ipv6_authorities() {
        value_scenarios!(run = |machine_ip| {
            redfish_action_uri(machine_ip, "/redfish/v1/Systems/1/Actions/Reset")
                .unwrap()
                .to_string()
        };
            "IPv4" {
                "192.0.2.10" =>
                    "https://192.0.2.10/redfish/v1/Systems/1/Actions/Reset".to_string(),
            }

            "IPv6" {
                "2001:db8::10" =>
                    "https://[2001:db8::10]/redfish/v1/Systems/1/Actions/Reset".to_string(),
            }
        );
    }

    // --- passthrough via nico-bmc-proxy ------------------------------------

    /// The proxied passthrough keeps the direct path's inventory gate: a
    /// target that is not an IP, or an IP with no inventoried BMC interface,
    /// is rejected before any request is built -- the proxy itself forwards
    /// IP targets with no lookup of its own, so this gate is what stops the
    /// admin passthrough from relaying to arbitrary addresses.
    #[crate::sqlx_test]
    async fn passthrough_via_core_proxy_rejects_uninventoried_targets(pool: sqlx::PgPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let passthrough = PassthroughClient::new(&test_config_with_generated_pems(&dir))
            .expect("passthrough client builds from generated PEMs");
        let reader = TestCredentialManager::default();
        let dynamic = ArcSwap::from_pointee(None);

        for (scenario, target) in [
            ("an uninventoried IP", "https://192.0.2.99/redfish/v1"),
            ("a non-IP host", "https://bmc.example/redfish/v1"),
        ] {
            let result = create_client(
                target.parse().unwrap(),
                &pool,
                &reader,
                &dynamic,
                Some(&passthrough),
            )
            .await;
            assert!(result.is_err(), "{scenario} must be rejected by the gate");
        }
    }

    /// Via the core proxy, an inventoried BMC target is rewritten to the
    /// static proxy authority -- winning over the dynamic dev redirect --
    /// with the original path and query preserved, the Forwarded header
    /// naming the BMC, and no BMC credentials attached.
    #[crate::sqlx_test]
    async fn passthrough_via_core_proxy_targets_the_proxy_for_inventoried_bmcs(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.expect("acquire");
        let segment_id: carbide_uuid::network::NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version, network_segment_type) \
             VALUES ('passthrough-admin', 'V1-T0', 'admin') RETURNING id",
        )
        .fetch_one(conn.as_mut())
        .await
        .expect("segment seeds");
        let machine_id = carbide_uuid::machine::MachineId::new(
            carbide_uuid::machine::MachineIdSource::ProductBoardChassisSerial,
            [0x77; 32],
            carbide_uuid::machine::MachineType::Host,
        );
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(conn.as_mut())
            .await
            .expect("machine seeds");
        let interface_id: carbide_uuid::machine::MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces \
                    (segment_id, mac_address, primary_interface, hostname, machine_id, \
                     interface_type, association_type) \
                 VALUES ($1, '00:11:22:33:44:55', false, 'passthrough-bmc', $2, 'Bmc', 'Machine') \
                 RETURNING id",
        )
        .bind(segment_id)
        .bind(machine_id)
        .fetch_one(conn.as_mut())
        .await
        .expect("interface seeds");
        sqlx::query(
            "INSERT INTO machine_interface_addresses (interface_id, address, allocation_type) \
             VALUES ($1, '192.0.2.10'::inet, 'static')",
        )
        .bind(interface_id)
        .execute(conn.as_mut())
        .await
        .expect("address seeds");

        let dir = tempfile::tempdir().expect("tempdir");
        let passthrough = PassthroughClient::new(&test_config_with_generated_pems(&dir))
            .expect("passthrough client builds from generated PEMs");
        let reader = TestCredentialManager::default();
        // The dynamic dev redirect is set; the static proxy must still win.
        let dynamic = ArcSwap::from_pointee(Some(HostPortPair::HostAndPort(
            "bmc-mock.example".to_string(),
            8000,
        )));

        let (new_uri, headers, _client, auth) = create_client(
            "https://192.0.2.10/redfish/v1/Systems?only=self"
                .parse()
                .unwrap(),
            &pool,
            &reader,
            &dynamic,
            Some(&passthrough),
        )
        .await
        .expect("an inventoried BMC should pass the gate");

        assert_eq!(
            new_uri.authority().map(|a| a.as_str()),
            Some("bmc-proxy.example:1079"),
            "the request must target the static proxy, not the BMC or the dynamic redirect"
        );
        assert_eq!(
            new_uri.path_and_query().map(|pq| pq.as_str()),
            Some("/redfish/v1/Systems?only=self"),
            "the original path and query must survive the authority rewrite"
        );
        assert_eq!(
            headers.get("forwarded").and_then(|v| v.to_str().ok()),
            Some("host=192.0.2.10"),
            "the Forwarded header routes the proxy to the BMC"
        );
        assert!(
            matches!(auth, PassthroughAuth::CoreProxy),
            "the proxy authenticates upstream itself; no BMC credentials are attached"
        );
    }
}
