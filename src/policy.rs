use crate::{
    catalog::Operation,
    error::{Error, Result},
};
use serde_json::{Value, json};

pub const ENV_BUILD: &str = "CE_MCP_ALLOW_BUILD";
pub const ENV_PLC_CONNECTION: &str = "CE_MCP_ALLOW_PLC_CONNECTION";
pub const ENV_PLC_WRITE: &str = "CE_MCP_ALLOW_PLC_WRITE";
pub const ENV_SECURITY_ADMIN: &str = "CE_MCP_ALLOW_SECURITY_ADMIN";
pub const ENV_PRIVATE_APIS: &str = "CE_MCP_ALLOW_PRIVATE_APIS";

/// Immutable for the lifetime of a server process; no MCP call changes it.
#[derive(Clone, Copy, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag mirrors one independent CE_MCP_ALLOW_* switch"
)]
pub struct ExecutionPolicy {
    pub allow_build: bool,
    pub allow_plc_connection: bool,
    pub allow_plc_write: bool,
    pub allow_security_admin: bool,
    pub allow_private_apis: bool,
}

/// What an operation can do beyond editing the project held in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restriction {
    None,
    /// Display* entry points: only the read-only viewer is allowed.
    Viewer,
    Build,
    /// Opens an online connection. `simulator_exempt` operations name their
    /// target explicitly, so a simulator target needs no unlock.
    PlcConnection {
        simulator_exempt: bool,
    },
    /// Changes what runs on a controller. `simulator_exempt` operations are
    /// allowed after a live check that the owning application targets the simulator.
    PlcWrite {
        simulator_exempt: bool,
    },
    /// Terminates Control Expert processes; never allowed.
    ProcessTermination,
    SecurityAdmin,
    PrivateApi,
}

impl ExecutionPolicy {
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let enabled = |name| get(name).as_deref() == Some("1");
        Self {
            allow_build: enabled(ENV_BUILD),
            allow_plc_connection: enabled(ENV_PLC_CONNECTION),
            allow_plc_write: enabled(ENV_PLC_WRITE),
            allow_security_admin: enabled(ENV_SECURITY_ADMIN),
            allow_private_apis: enabled(ENV_PRIVATE_APIS),
        }
    }
    pub fn snapshot(self) -> Value {
        json!({
            "allow_build": self.allow_build,
            "allow_plc_connection": self.allow_plc_connection,
            "allow_plc_write": self.allow_plc_write,
            "allow_security_admin": self.allow_security_admin,
            "allow_private_apis": self.allow_private_apis,
            "analysis_allowed": true,
            "headless_start_required": true,
            "viewer": "Read-only, through ce_viewer_open on the same session. No editable UI and no multiple writers.",
            "simulator_connection": "TargetConnectionOpen with target=psrConnectToSimulator needs no unlock.",
            "simulator_write": "Transfers and PLC commands require a live check that the owning application has SelectTarget=psrConnectToSimulator and was not connected to a PLC by this server, immediately before the call.",
            "process_termination": "KillProcess and ExitPServer are always blocked.",
            "environment": {
                "build": format!("{ENV_BUILD}=1"),
                "plc_connection": format!("{ENV_PLC_CONNECTION}=1"),
                "plc_write": format!("{ENV_PLC_WRITE}=1"),
                "security_admin": format!("{ENV_SECURITY_ADMIN}=1"),
                "private_apis": format!("{ENV_PRIVATE_APIS}=1")
            },
            "loaded_at_startup": true
        })
    }
    /// Checks that need no live COM verification. Simulator-exempt
    /// restrictions are left to the runtime, which verifies the target.
    pub fn check_static(self, op: &Operation) -> Result<()> {
        match restriction(op) {
            Restriction::Build if !self.allow_build => Err(blocked(
                op,
                "build_disabled",
                ENV_BUILD,
                "Building is blocked, including in simulation. Analyze remains available.",
            )),
            Restriction::PlcConnection {
                simulator_exempt: false,
            } if !self.allow_plc_connection => Err(blocked(
                op,
                "plc_connection_disabled",
                ENV_PLC_CONNECTION,
                "Online connections to controllers and devices are blocked.",
            )),
            Restriction::PlcWrite {
                simulator_exempt: false,
            } if !self.allow_plc_write => Err(blocked(
                op,
                "plc_write_disabled",
                ENV_PLC_WRITE,
                "Operations that change what runs on a controller are blocked.",
            )),
            Restriction::ProcessTermination => {
                let mut error = Error::new(
                    "operation_blocked",
                    "Terminating Control Expert processes is not allowed through this server.",
                );
                error.details = Some(json!({"api_id": op.id, "reason": "process_termination"}));
                Err(error)
            }
            Restriction::SecurityAdmin if !self.allow_security_admin => Err(blocked(
                op,
                "security_admin_disabled",
                ENV_SECURITY_ADMIN,
                "Security server administration (users, profiles, policies, certificates) is blocked.",
            )),
            Restriction::PrivateApi if !self.allow_private_apis => Err(blocked(
                op,
                "private_api_disabled",
                ENV_PRIVATE_APIS,
                "Private, internal and test interfaces are blocked.",
            )),
            _ => Ok(()),
        }
    }
    /// Activation and `QueryInterface` follow the category rules of calls.
    pub fn check_category(self, kind: &str, id: &str, category: &str) -> Result<()> {
        let (allowed, reason, setting) = match category_restriction(category) {
            Restriction::SecurityAdmin => (
                self.allow_security_admin,
                "security_admin_disabled",
                ENV_SECURITY_ADMIN,
            ),
            Restriction::PrivateApi => (
                self.allow_private_apis,
                "private_api_disabled",
                ENV_PRIVATE_APIS,
            ),
            _ => return Ok(()),
        };
        if allowed {
            return Ok(());
        }
        let mut error = Error::new(
            "operation_blocked",
            format!("The {kind} {id} belongs to the restricted category {category}."),
        );
        let mut details = json!({
            "category": category, "reason": reason, "unlock_environment": format!("{setting}=1"),
            "restart_required": true
        });
        details[kind] = json!(id);
        error.details = Some(details);
        Err(error)
    }
    pub fn describe(self, op: &Operation) -> Value {
        match restriction(op) {
            Restriction::None => {
                json!({"restriction": null, "note": "No additional restriction; session and API preconditions still apply."})
            }
            Restriction::Viewer => {
                json!({"restriction": "read_only_viewer", "viewer_tool": "ce_viewer_open", "read_write_allowed": false})
            }
            Restriction::Build => {
                json!({"restriction": "build", "enabled": self.allow_build, "unlock_environment": format!("{ENV_BUILD}=1")})
            }
            Restriction::PlcConnection { simulator_exempt } => {
                json!({"restriction": "plc_connection", "enabled": self.allow_plc_connection, "simulator_target_allowed": simulator_exempt, "unlock_environment": format!("{ENV_PLC_CONNECTION}=1")})
            }
            Restriction::PlcWrite { simulator_exempt } => {
                json!({"restriction": "plc_write", "enabled": self.allow_plc_write, "simulator_requires_live_verification": simulator_exempt && !self.allow_plc_write, "unlock_environment": format!("{ENV_PLC_WRITE}=1")})
            }
            Restriction::ProcessTermination => {
                json!({"restriction": "process_termination", "enabled": false, "unlock_environment": null})
            }
            Restriction::SecurityAdmin => {
                json!({"restriction": "security_admin", "enabled": self.allow_security_admin, "unlock_environment": format!("{ENV_SECURITY_ADMIN}=1")})
            }
            Restriction::PrivateApi => {
                json!({"restriction": "private_api", "enabled": self.allow_private_apis, "unlock_environment": format!("{ENV_PRIVATE_APIS}=1")})
            }
        }
    }
}

pub fn restriction(op: &Operation) -> Restriction {
    let project = matches!(
        op.interface.as_str(),
        "UnityProServer.IProject" | "UnityProServer.IProject2"
    );
    let name = op.name.as_str();
    match (op.interface.as_str(), name) {
        _ if is_ui_entry(op) => Restriction::Viewer,
        _ if project && matches!(name, "Build" | "BuildAll") => Restriction::Build,
        _ if project && matches!(name, "TransferAllPCtoPLC" | "RestoreDataToPLC") => {
            Restriction::PlcWrite {
                simulator_exempt: true,
            }
        }
        ("UnityProServer.IApplication", "TargetSendCommand") => Restriction::PlcWrite {
            simulator_exempt: true,
        },
        ("UnityProServer.IApplication", "TargetConnectionOpen") => Restriction::PlcConnection {
            simulator_exempt: true,
        },
        ("APPLICATIONSAFETYLib.IApplicationSafety", "SafetyTargetConnectionOpen")
        | ("UnityProServer.IPServerDtm" | "UnityProServer.IPServerDtmRoot", "Connect") => {
            Restriction::PlcConnection {
                simulator_exempt: false,
            }
        }
        (
            "APPLICATIONSAFETYLib.IApplicationSafety",
            "SwitchToSafetyMode" | "SwitchToMaintenanceMode",
        ) => Restriction::PlcWrite {
            simulator_exempt: false,
        },
        ("PSExeLib.IHealthServerGenMulti", "KillProcess")
        | ("PSExeLib.IPSEActiveConnector", "ExitPServer") => Restriction::ProcessTermination,
        _ => category_restriction(&op.category),
    }
}
fn category_restriction(category: &str) -> Restriction {
    match category {
        "security" | "security_support" => Restriction::SecurityAdmin,
        "private" | "internal" | "tests" => Restriction::PrivateApi,
        _ => Restriction::None,
    }
}

pub fn is_ui_entry(op: &Operation) -> bool {
    op.name.starts_with("Display") || op.name == "SetDisplayPosition"
}
#[cfg(windows)]
pub(crate) fn viewer_blocked(op: &Operation) -> Error {
    let mut error = Error::new(
        "operation_blocked",
        "The agent session starts headless. Use ce_viewer_open to observe it; only DisplayStart in psrHmiReadOnly mode without command-line arguments is allowed.",
    );
    error.details = Some(
        json!({"api_id": op.id, "reason": "read_only_viewer_required", "viewer_tool": "ce_viewer_open", "read_write_allowed": false}),
    );
    error
}

pub(crate) fn blocked(op: &Operation, reason: &str, setting: &str, message: &str) -> Error {
    let mut error = Error::new("operation_blocked", message);
    error.details = Some(json!({
        "api_id": op.id, "reason": reason, "unlock_environment": format!("{setting}=1"),
        "restart_required": true,
        "note": "The operator must set the unlock in the MCP server's startup environment. It cannot be changed through ce_api_call."
    }));
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::Catalog, versions::Profile};
    use std::collections::HashMap;

    fn policy(values: &[(&str, &str)]) -> ExecutionPolicy {
        let values: HashMap<_, _> = values.iter().copied().collect();
        ExecutionPolicy::from_lookup(|name| values.get(name).map(ToString::to_string))
    }
    fn catalog() -> Catalog {
        Catalog::load(Profile::select("15.1").expect("15.1 is supported"))
            .expect("the bundled catalog loads")
    }

    #[test]
    fn only_explicit_one_enables_each_independent_capability() {
        let names = [
            ENV_BUILD,
            ENV_PLC_CONNECTION,
            ENV_PLC_WRITE,
            ENV_SECURITY_ADMIN,
            ENV_PRIVATE_APIS,
        ];
        for value in ["", "0", "true", "yes", " 1"] {
            let p = policy(&names.map(|n| (n, value)));
            assert!(
                !p.allow_build
                    && !p.allow_plc_connection
                    && !p.allow_plc_write
                    && !p.allow_security_admin
                    && !p.allow_private_apis
            );
        }
        let p = policy(&[(ENV_BUILD, "1")]);
        assert!(p.allow_build && !p.allow_plc_write && !p.allow_plc_connection);
        let p = policy(&[(ENV_PLC_WRITE, "1")]);
        assert!(!p.allow_build && p.allow_plc_write && !p.allow_plc_connection);
        let p = policy(&[(ENV_PLC_CONNECTION, "1")]);
        assert!(p.allow_plc_connection && !p.allow_plc_write);
        // Legacy variable names no longer unlock anything.
        let p = policy(&[("SCE_ALLOW_BUILD", "1"), ("SCE_ALLOW_PLC_TRANSFER", "1")]);
        assert!(!p.allow_build && !p.allow_plc_write);
    }

    #[test]
    fn both_project_interfaces_are_covered_without_blocking_analysis_or_status() {
        let catalog = catalog();
        for interface in ["IProject", "IProject2"] {
            for method in ["Build", "BuildAll"] {
                let op = catalog
                    .operation(&format!("UnityProServer.{interface}.{method}:method"))
                    .unwrap();
                assert_eq!(
                    ExecutionPolicy::default()
                        .check_static(op)
                        .unwrap_err()
                        .code,
                    "operation_blocked"
                );
                assert!(
                    ExecutionPolicy {
                        allow_build: true,
                        ..Default::default()
                    }
                    .check_static(op)
                    .is_ok()
                );
            }
            for method in ["TransferAllPCtoPLC", "RestoreDataToPLC"] {
                let op = catalog
                    .operation(&format!("UnityProServer.{interface}.{method}:method"))
                    .unwrap();
                assert_eq!(
                    restriction(op),
                    Restriction::PlcWrite {
                        simulator_exempt: true
                    }
                );
                // Simulator verification happens live in the runtime.
                assert!(ExecutionPolicy::default().check_static(op).is_ok());
            }
            for suffix in [
                "Analyze:method",
                "InfoBuildState:get",
                "SaveDataFromPLC:method",
            ] {
                let op = catalog
                    .operation(&format!("UnityProServer.{interface}.{suffix}"))
                    .unwrap();
                assert_eq!(restriction(op), Restriction::None);
            }
        }
    }

    #[test]
    fn controller_connections_writes_and_process_termination_are_classified() {
        let catalog = catalog();
        let class = |id: &str| restriction(catalog.operation(id).unwrap());
        assert_eq!(
            class("UnityProServer.IApplication.TargetConnectionOpen:method"),
            Restriction::PlcConnection {
                simulator_exempt: true
            }
        );
        assert_eq!(
            class("UnityProServer.IApplication.TargetSendCommand:method"),
            Restriction::PlcWrite {
                simulator_exempt: true
            }
        );
        for id in [
            "APPLICATIONSAFETYLib.IApplicationSafety.SafetyTargetConnectionOpen:method",
            "UnityProServer.IPServerDtm.Connect:method",
            "UnityProServer.IPServerDtmRoot.Connect:method",
        ] {
            assert_eq!(
                class(id),
                Restriction::PlcConnection {
                    simulator_exempt: false
                },
                "{id}"
            );
        }
        for id in [
            "APPLICATIONSAFETYLib.IApplicationSafety.SwitchToSafetyMode:method",
            "APPLICATIONSAFETYLib.IApplicationSafety.SwitchToMaintenanceMode:method",
        ] {
            assert_eq!(
                class(id),
                Restriction::PlcWrite {
                    simulator_exempt: false
                }
            );
        }
        let everything = ExecutionPolicy {
            allow_build: true,
            allow_plc_connection: true,
            allow_plc_write: true,
            allow_security_admin: true,
            allow_private_apis: true,
        };
        for id in [
            "PSExeLib.IHealthServerGenMulti.KillProcess:method",
            "PSExeLib.IPSEActiveConnector.ExitPServer:method",
        ] {
            assert_eq!(class(id), Restriction::ProcessTermination);
            let op = catalog.operation(id).unwrap();
            assert!(everything.check_static(op).is_err(), "{id}");
        }
        for id in [
            "UnityProServer.IApplication.TransferAllPLCToPC:method",
            "UnityProServer.IApplication.TargetConnectionClose:method",
            "UnityProServer.IApplication.SetPlcConnectionAddress:method",
            "UnityProServer.IVariables.Add:method",
        ] {
            assert_eq!(class(id), Restriction::None, "{id}");
        }
    }

    #[test]
    fn restricted_categories_require_their_own_unlock() {
        let catalog = catalog();
        let security = catalog
            .operation("UnitySecurityServer.IUBServ.UBSDeleteUser:method")
            .unwrap();
        let private = catalog
            .operations
            .values()
            .find(|op| op.category == "private" && op.kind != "event")
            .unwrap();
        let test = catalog
            .operation("APPLICATIONFORUNITTESTLib.IUnitTest.RunTest:method")
            .unwrap();
        for op in [security, private, test] {
            assert!(ExecutionPolicy::default().check_static(op).is_err());
        }
        let p = ExecutionPolicy {
            allow_security_admin: true,
            ..Default::default()
        };
        assert!(p.check_static(security).is_ok());
        assert!(p.check_static(private).is_err() && p.check_static(test).is_err());
        let p = ExecutionPolicy {
            allow_private_apis: true,
            ..Default::default()
        };
        assert!(p.check_static(security).is_err());
        assert!(p.check_static(private).is_ok() && p.check_static(test).is_ok());
        // Every operation of a restricted category is covered.
        for op in catalog.operations.values() {
            if matches!(
                op.category.as_str(),
                "security" | "security_support" | "private" | "internal" | "tests"
            ) {
                assert!(
                    ExecutionPolicy::default().check_static(op).is_err(),
                    "{}",
                    op.id
                );
            }
        }
        assert!(
            ExecutionPolicy::default()
                .check_category("coclass", "UnitySecurityServer.Security", "security")
                .is_err()
        );
        assert!(
            ExecutionPolicy::default()
                .check_category("coclass", "UnityProBroker.PServerBroker", "ude")
                .is_ok()
        );
    }
}
