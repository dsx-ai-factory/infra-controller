use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use ::rpc::forge::{InterfaceFunctionType, ManagedHostNetworkConfigResponse};
use eyre::WrapErr;
use itertools::Itertools;

mod parse_hbn_conf;

#[derive(Debug)]
/// Keeps HBN-owned host VF representors aligned with active tenant VFs.
pub(crate) struct HostVfLinkManager {
    interface_map: VfInterfaceMap,
    reconciler: LinkStateReconciler,
    controller: Box<dyn LinkStateController>,
}

impl HostVfLinkManager {
    /// Creates a manager from the DPU OS host VF propagation configuration.
    pub(crate) async fn init_for_dpu_os() -> eyre::Result<Self> {
        let interface_map = VfInterfaceMap::load_from(parse_hbn_conf::HBN_CONF_PATH).await?;
        Ok(Self::new(
            interface_map,
            Box::new(IpLinkStateController::default()),
        ))
    }

    fn new(interface_map: VfInterfaceMap, controller: Box<dyn LinkStateController>) -> Self {
        let reconciler = LinkStateReconciler::new(interface_map.managed_host_representors());
        Self {
            interface_map,
            reconciler,
            controller,
        }
    }

    /// Applies the desired administrative state to every managed host VF representor.
    pub(crate) async fn reconcile(
        &mut self,
        config: &ManagedHostNetworkConfigResponse,
    ) -> eyre::Result<()> {
        reconcile_active_vfs(
            &self.interface_map,
            &mut self.reconciler,
            config,
            self.controller.as_mut(),
        )
        .await
    }

    /// Invalidates every cached link-state estimate.
    pub(crate) fn invalidate_cached_state(&mut self) {
        self.reconciler.invalidate_cached_state();
    }
}

/// Returns the virtual function IDs attached to tenant network interfaces.
fn active_vf_ids(config: &ManagedHostNetworkConfigResponse) -> HashSet<u32> {
    config
        .tenant_interfaces
        .iter()
        .filter(|interface| interface.function_type() == InterfaceFunctionType::Virtual)
        .filter_map(|interface| interface.virtual_function_id)
        .collect()
}

async fn reconcile_active_vfs(
    interface_map: &VfInterfaceMap,
    reconciler: &mut LinkStateReconciler,
    network_config: &ManagedHostNetworkConfigResponse,
    controller: &mut dyn LinkStateController,
) -> eyre::Result<()> {
    let active_vf_ids = active_vf_ids(network_config);
    let valid_vf_ids = interface_map.valid_vf_ids();
    let mut missing_vf_ids = active_vf_ids
        .difference(&valid_vf_ids)
        .copied()
        .collect::<Vec<_>>();
    missing_vf_ids.sort_unstable();

    if !missing_vf_ids.is_empty() {
        eyre::bail!(
            "active virtual functions are missing from [{}]: {}",
            parse_hbn_conf::LINK_PROPAGATION_SECTION,
            missing_vf_ids.iter().join(", ")
        );
    }

    let active_representors = active_vf_ids
        .into_iter()
        .filter_map(|vf_id| interface_map.representor_name(vf_id));
    reconciler
        .reconcile_links(active_representors, controller)
        .await
        .map_err(eyre::Report::new)
}
#[derive(Debug, Eq, PartialEq)]
/// Maps tenant VF IDs to their HBN-owned host representors.
struct VfInterfaceMap {
    representor_names: HashMap<u32, String>,
}

impl VfInterfaceMap {
    /// Loads a validated host VF ownership mapping from an HBN configuration file.
    async fn load_from(path: impl AsRef<Path>) -> eyre::Result<Self> {
        let path = path.as_ref();
        let contents = tokio::fs::read_to_string(path)
            .await
            .wrap_err_with(|| format!("reading {}", path.display()))?;
        Self::parse(&contents).wrap_err_with(|| format!("parsing {}", path.display()))
    }

    /// Parses host VF ownership entries from an HBN configuration file.
    ///
    /// An example minimal valid configuration is:
    ///
    /// ```text
    /// [LINK_PROPAGATION]
    /// pf0vf7:pf0vf7_if_r
    /// ```
    fn parse(contents: &str) -> eyre::Result<Self> {
        Ok(Self {
            representor_names: parse_hbn_conf::get_hbn_vf_mapping(contents)?,
        })
    }

    /// Returns the names of every host VF representor owned by this mapping.
    fn managed_host_representors(&self) -> impl Iterator<Item = &str> + '_ {
        self.representor_names.values().map(String::as_str)
    }

    /// Returns the owned host representor associated with a VF ID.
    fn representor_name(&self, vf_id: u32) -> Option<&str> {
        self.representor_names.get(&vf_id).map(String::as_str)
    }

    /// Returns the set of VF IDs accepted by this ownership mapping.
    fn valid_vf_ids(&self) -> HashSet<u32> {
        self.representor_names.keys().copied().collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Describes a concrete administrative state for a managed link.
enum LinkAdminState {
    Up,
    Down,
}

impl LinkAdminState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

impl fmt::Display for LinkAdminState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Records the reconciler's estimate of a managed link's administrative state.
enum LinkAdminStateEstimate {
    #[default]
    Unknown,
    Assumed(LinkAdminState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Describes the desired administrative state change for one managed link.
struct LinkStateOperation {
    interface: String,
    desired_state: LinkAdminState,
}

impl LinkStateOperation {
    fn new(interface: impl Into<String>, desired_state: LinkAdminState) -> Self {
        Self {
            interface: interface.into(),
            desired_state,
        }
    }
}

/// Reports whether a failed link operation may have changed the link state.
trait LinkStateControllerError: Error + Send + Sync + 'static {
    /// Returns whether the cached administrative state can no longer be trusted.
    fn maybe_obscured_state(&self) -> bool;
}

type BoxedLinkStateControllerError = Box<dyn LinkStateControllerError>;

/// Applies administrative state changes selected by the reconciler.
#[async_trait::async_trait]
trait LinkStateController: fmt::Debug + Send {
    /// Applies one requested administrative state change.
    async fn apply_state_operation(
        &mut self,
        operation: &LinkStateOperation,
    ) -> Result<(), BoxedLinkStateControllerError>;
}

#[derive(Debug)]
/// Applies administrative state changes to Linux network interfaces.
struct IpLinkStateController {
    executable: OsString,
    timeout: Duration,
}

impl Default for IpLinkStateController {
    fn default() -> Self {
        const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

        Self {
            executable: OsString::from("ip"),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl IpLinkStateController {
    fn build_command(&self, operation: &LinkStateOperation) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args([
                "link",
                "set",
                "dev",
                operation.interface.as_str(),
                operation.desired_state.as_str(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    #[cfg(test)]
    fn with_executable(mut self, executable: impl Into<OsString>) -> Self {
        self.executable = executable.into();
        self
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[derive(Debug)]
/// Classifies failures from an administrative link-state command.
enum IpLinkStateControllerError {
    NotStarted(eyre::Report),
    Failed(eyre::Report),
}

impl LinkStateControllerError for IpLinkStateControllerError {
    fn maybe_obscured_state(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

impl fmt::Display for IpLinkStateControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStarted(error) => {
                write!(formatter, "link command did not start: {error:#}")
            }
            Self::Failed(error) => write!(formatter, "link command failed: {error:#}"),
        }
    }
}

impl Error for IpLinkStateControllerError {}

#[async_trait::async_trait]
impl LinkStateController for IpLinkStateController {
    async fn apply_state_operation(
        &mut self,
        operation: &LinkStateOperation,
    ) -> Result<(), BoxedLinkStateControllerError> {
        let mut command = self.build_command(operation);
        let command_description = crate::pretty_cmd(command.as_std());
        let child = command.spawn().map_err(|error| {
            Box::new(IpLinkStateControllerError::NotStarted(eyre::eyre!(
                "starting command {command_description:?}: {error}"
            ))) as BoxedLinkStateControllerError
        })?;

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                Box::new(IpLinkStateControllerError::Failed(eyre::eyre!(
                    "timed out after {:?} waiting for command {command_description:?}",
                    self.timeout
                ))) as BoxedLinkStateControllerError
            })?
            .map_err(|error| {
                Box::new(IpLinkStateControllerError::Failed(eyre::eyre!(
                    "waiting for command {command_description:?}: {error}"
                ))) as BoxedLinkStateControllerError
            })?;

        if !output.status.success() {
            return Err(Box::new(IpLinkStateControllerError::Failed(eyre::eyre!(
                "command {command_description:?} exited with status {}, stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))));
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
/// Tracks link-state estimates and selects representors that still need updates.
struct LinkStateReconciler {
    state_estimates: HashMap<String, LinkAdminStateEstimate>,
}

impl LinkStateReconciler {
    fn new<I, S>(managed_links: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            state_estimates: managed_links
                .into_iter()
                .map(|interface| (interface.into(), LinkAdminStateEstimate::Unknown))
                .collect(),
        }
    }

    fn invalidate_cached_state(&mut self) {
        for estimate in self.state_estimates.values_mut() {
            *estimate = LinkAdminStateEstimate::Unknown;
        }
    }

    fn pending_operations<I, S>(&self, active_links: I) -> Vec<LinkStateOperation>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let active_links = active_links
            .into_iter()
            .map(Into::into)
            .collect::<HashSet<_>>();

        self.state_estimates
            .iter()
            .filter_map(|(interface, estimate)| {
                match (*estimate, active_links.contains(interface)) {
                    (LinkAdminStateEstimate::Assumed(LinkAdminState::Up), true)
                    | (LinkAdminStateEstimate::Assumed(LinkAdminState::Down), false) => None,
                    (_, true) => Some(LinkStateOperation::new(interface, LinkAdminState::Up)),
                    (_, false) => Some(LinkStateOperation::new(interface, LinkAdminState::Down)),
                }
            })
            .collect()
    }

    async fn reconcile_links<I, S>(
        &mut self,
        active_links: I,
        controller: &mut dyn LinkStateController,
    ) -> Result<(), LinkStateReconciliationError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let pending_operations = self.pending_operations(active_links);
        let mut failures = Vec::new();

        for operation in pending_operations {
            match controller.apply_state_operation(&operation).await {
                Ok(()) => {
                    *self
                        .state_estimates
                        .get_mut(&operation.interface)
                        .expect("operation must reference a managed link") =
                        LinkAdminStateEstimate::Assumed(operation.desired_state);
                }
                Err(error) => {
                    if error.maybe_obscured_state() {
                        *self
                            .state_estimates
                            .get_mut(&operation.interface)
                            .expect("operation must reference a managed link") =
                            LinkAdminStateEstimate::Unknown;
                    }
                    failures.push(LinkStateOperationFailure { operation, error });
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(LinkStateReconciliationError { failures })
        }
    }
}

#[derive(Debug)]
/// Associates one failed administrative operation with its controller error.
struct LinkStateOperationFailure {
    operation: LinkStateOperation,
    error: BoxedLinkStateControllerError,
}

impl fmt::Display for LinkStateOperationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "interface={} desired_state={} error={}",
            self.operation.interface.as_str(),
            self.operation.desired_state.as_str(),
            self.error
        )
    }
}

#[derive(Debug)]
/// Reports every host VF link operation that failed during one reconciliation.
struct LinkStateReconciliationError {
    failures: Vec<LinkStateOperationFailure>,
}

impl fmt::Display for LinkStateReconciliationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut failures = self.failures.iter().collect::<Vec<_>>();
        failures.sort_by_key(|failure| &failure.operation.interface);
        write!(
            formatter,
            "failed to reconcile host VF links: {}",
            failures.into_iter().join("; ")
        )
    }
}

impl Error for LinkStateReconciliationError {}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use carbide_test_support::{Check, check_values};

    use super::*;

    fn vf_map(entries: &[(u32, &str)]) -> VfInterfaceMap {
        VfInterfaceMap {
            representor_names: entries
                .iter()
                .map(|(vf_id, interface)| (*vf_id, (*interface).to_string()))
                .collect(),
        }
    }

    #[test]
    fn retains_configured_host_representor_names() {
        let interface_map = vf_map(&[(7, "pf0vf07"), (1, "pf0vf1")]);
        let mut representors = interface_map
            .managed_host_representors()
            .collect::<Vec<_>>();
        representors.sort();

        assert_eq!(representors, ["pf0vf07", "pf0vf1"]);
    }

    #[test]
    fn extracts_active_virtual_function_ids() {
        check_values(
            [
                Check {
                    scenario: "virtual IDs are selected and deduplicated",
                    input: vec![
                        (InterfaceFunctionType::Virtual, Some(3)),
                        (InterfaceFunctionType::Virtual, Some(3)),
                        (InterfaceFunctionType::Virtual, Some(5)),
                    ],
                    expect: HashSet::from([3, 5]),
                },
                Check {
                    scenario: "physical and missing IDs are ignored",
                    input: vec![
                        (InterfaceFunctionType::Physical, Some(1)),
                        (InterfaceFunctionType::Virtual, None),
                    ],
                    expect: HashSet::new(),
                },
            ],
            |interfaces| {
                active_vf_ids(&ManagedHostNetworkConfigResponse {
                    tenant_interfaces: interfaces
                        .into_iter()
                        .map(|(function_type, virtual_function_id)| {
                            ::rpc::forge::FlatInterfaceConfig {
                                function_type: function_type.into(),
                                virtual_function_id,
                                ..Default::default()
                            }
                        })
                        .collect(),
                    ..Default::default()
                })
            },
        );
    }

    #[derive(Clone, Debug)]
    struct FakeControllerError {
        obscures_state: bool,
        message: &'static str,
    }

    impl fmt::Display for FakeControllerError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl Error for FakeControllerError {}

    impl LinkStateControllerError for FakeControllerError {
        fn maybe_obscured_state(&self) -> bool {
            self.obscures_state
        }
    }

    #[derive(Debug, Default)]
    struct FakeController {
        failures: HashMap<String, VecDeque<FakeControllerError>>,
        operations: Vec<LinkStateOperation>,
    }

    impl FakeController {
        fn fail_once(
            mut self,
            interface: &str,
            obscures_state: bool,
            message: &'static str,
        ) -> Self {
            self.failures
                .entry(interface.to_string())
                .or_default()
                .push_back(FakeControllerError {
                    obscures_state,
                    message,
                });
            self
        }
    }

    #[async_trait::async_trait]
    impl LinkStateController for FakeController {
        async fn apply_state_operation(
            &mut self,
            operation: &LinkStateOperation,
        ) -> Result<(), BoxedLinkStateControllerError> {
            self.operations.push(operation.clone());
            match self.failures.get_mut(&operation.interface) {
                Some(failures) if !failures.is_empty() => {
                    Err(Box::new(failures.pop_front().unwrap()))
                }
                _ => Ok(()),
            }
        }
    }

    fn sorted_operations(operations: &[LinkStateOperation]) -> Vec<LinkStateOperation> {
        let mut operations = operations.to_vec();
        operations.sort_by(|left, right| left.interface.cmp(&right.interface));
        operations
    }

    #[tokio::test]
    async fn reconciles_startup_attach_detach_and_cached_noop() {
        let mut reconciler = LinkStateReconciler::new(["pf0vf0", "pf0vf1"]);
        let mut controller = FakeController::default();

        reconciler
            .reconcile_links(["pf0vf0"], &mut controller)
            .await
            .unwrap();
        assert_eq!(
            sorted_operations(&controller.operations),
            vec![
                LinkStateOperation::new("pf0vf0", LinkAdminState::Up),
                LinkStateOperation::new("pf0vf1", LinkAdminState::Down),
            ]
        );

        controller.operations.clear();
        reconciler
            .reconcile_links(["pf0vf0"], &mut controller)
            .await
            .unwrap();
        assert!(controller.operations.is_empty());

        reconciler.invalidate_cached_state();
        reconciler
            .reconcile_links(["pf0vf0"], &mut controller)
            .await
            .unwrap();
        assert_eq!(
            sorted_operations(&controller.operations),
            vec![
                LinkStateOperation::new("pf0vf0", LinkAdminState::Up),
                LinkStateOperation::new("pf0vf1", LinkAdminState::Down),
            ]
        );

        controller.operations.clear();
        reconciler
            .reconcile_links(["pf0vf1"], &mut controller)
            .await
            .unwrap();
        assert_eq!(
            sorted_operations(&controller.operations),
            vec![
                LinkStateOperation::new("pf0vf0", LinkAdminState::Down),
                LinkStateOperation::new("pf0vf1", LinkAdminState::Up),
            ]
        );
    }

    #[tokio::test]
    async fn retains_successes_and_retries_only_failed_links() {
        let mut reconciler = LinkStateReconciler::new(["pf0vf0", "pf0vf1"]);
        let mut controller = FakeController::default().fail_once("pf0vf1", true, "denied");

        let error = reconciler
            .reconcile_links(["pf0vf0", "pf0vf1"], &mut controller)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("interface=pf0vf1"));

        controller.operations.clear();
        reconciler
            .reconcile_links(["pf0vf0", "pf0vf1"], &mut controller)
            .await
            .unwrap();
        assert_eq!(
            controller.operations,
            vec![LinkStateOperation::new("pf0vf1", LinkAdminState::Up)]
        );
    }

    #[tokio::test]
    async fn failure_classification_controls_state_estimate() {
        for (obscures_state, expect_retry) in [(false, false), (true, true)] {
            let mut reconciler = LinkStateReconciler::new(["pf0vf0"]);
            let mut controller = FakeController::default();
            reconciler
                .reconcile_links(["pf0vf0"], &mut controller)
                .await
                .unwrap();

            controller =
                FakeController::default().fail_once("pf0vf0", obscures_state, "operation failed");
            reconciler
                .reconcile_links(Vec::<&str>::new(), &mut controller)
                .await
                .unwrap_err();

            controller.operations.clear();
            reconciler
                .reconcile_links(["pf0vf0"], &mut controller)
                .await
                .unwrap();
            assert_eq!(
                !controller.operations.is_empty(),
                expect_retry,
                "obscures_state={obscures_state}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_unmapped_active_vfs_before_link_operations() {
        let interface_map = vf_map(&[(0, "pf0vf0")]);
        let mut reconciler = LinkStateReconciler::new(["pf0vf0"]);
        let mut controller = FakeController::default();
        let config = ManagedHostNetworkConfigResponse {
            tenant_interfaces: [7, 2]
                .into_iter()
                .map(|virtual_function_id| ::rpc::forge::FlatInterfaceConfig {
                    function_type: InterfaceFunctionType::Virtual.into(),
                    virtual_function_id: Some(virtual_function_id),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        let error = reconcile_active_vfs(&interface_map, &mut reconciler, &config, &mut controller)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("2, 7"));
        assert!(controller.operations.is_empty());
    }

    #[tokio::test]
    async fn formats_aggregated_failures_in_interface_order() {
        let mut reconciler = LinkStateReconciler::new(["pf0vf2", "pf0vf10"]);
        let mut controller = FakeController::default()
            .fail_once("pf0vf2", true, "second")
            .fail_once("pf0vf10", true, "first");

        let error = reconciler
            .reconcile_links(["pf0vf2", "pf0vf10"], &mut controller)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.find("pf0vf10").unwrap() < error.find("pf0vf2").unwrap());
        assert!(error.contains("desired_state=up"));
    }

    fn write_executable(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn ip_controller_builds_exact_command() {
        let controller = IpLinkStateController::default();
        let command =
            controller.build_command(&LinkStateOperation::new("pf0vf7", LinkAdminState::Down));

        assert_eq!(command.as_std().get_program(), OsStr::new("ip"));
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            ["link", "set", "dev", "pf0vf7", "down"]
                .into_iter()
                .map(OsStr::new)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn ip_controller_classifies_spawn_failure_as_unchanged() {
        let mut controller =
            IpLinkStateController::default().with_executable("/path/that/does/not/exist/ip-test");
        let error = controller
            .apply_state_operation(&LinkStateOperation::new("pf0vf0", LinkAdminState::Up))
            .await
            .unwrap_err();
        assert!(!error.maybe_obscured_state());
    }

    #[tokio::test]
    async fn ip_controller_reports_nonzero_exit_and_stderr() {
        let directory = tempfile::tempdir().unwrap();
        let executable = write_executable(
            directory.path(),
            "ip-test",
            "#!/bin/sh\necho permission-denied >&2\nexit 7\n",
        );
        let mut controller = IpLinkStateController::default().with_executable(executable);
        let error = controller
            .apply_state_operation(&LinkStateOperation::new("pf0vf0", LinkAdminState::Up))
            .await
            .unwrap_err();
        assert!(error.maybe_obscured_state(), "{error}");
        assert!(error.to_string().contains("permission-denied"));
    }

    #[tokio::test]
    async fn ip_controller_classifies_timeout_as_state_obscuring() {
        let directory = tempfile::tempdir().unwrap();
        let executable = write_executable(directory.path(), "ip-test", "#!/bin/sh\nsleep 1\n");
        let mut controller = IpLinkStateController::default()
            .with_executable(executable)
            .with_timeout(Duration::from_millis(10));
        let error = controller
            .apply_state_operation(&LinkStateOperation::new("pf0vf0", LinkAdminState::Up))
            .await
            .unwrap_err();
        assert!(error.maybe_obscured_state());
        assert!(error.to_string().contains("timed out"));
    }
}
