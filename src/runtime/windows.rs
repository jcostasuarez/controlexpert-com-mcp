use super::{Action, diagnostics::Trace};
use crate::{
    catalog::{Catalog, Operation, Type},
    error::{Error, Result},
    policy::{self, ExecutionPolicy, Restriction},
};
use ::windows::{
    Win32::{
        Foundation::DISP_E_PARAMNOTFOUND,
        Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
        },
        System::{Com::*, Ole::*, Variant::*},
        UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        },
    },
    core::{BSTR, GUID, IUnknown, Interface as ComInterface, PCWSTR},
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::c_void,
    mem::ManuallyDrop,
    path::PathBuf,
    ptr,
    time::Instant,
    time::{SystemTime, UNIX_EPOCH},
};
#[cfg(test)]
mod tests;

pub struct Apartment;
impl Apartment {
    pub fn initialize() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
pub fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
struct Object {
    value: IUnknown,
    interface: Option<String>,
    serial: u64,
}
fn bootstrap_interface(interface: &str) -> bool {
    matches!(
        interface,
        "UnityProBroker.IPServerBroker"
            | "UnityProBroker.IPServerBrokerContext"
            | "UnityProBroker.IPSBBrokerInfo"
            | "UnityProBroker.IHealthServerGen"
    )
}
pub struct State {
    catalog: Arc<Catalog>,
    objects: HashMap<String, Object>,
    next_id: u64,
    nonce: String,
    policy: ExecutionPolicy,
    /// COM identities of applications that this server connected to a
    /// controller other than the simulator, until TargetConnectionClose.
    plc_connections: HashSet<usize>,
}
impl State {
    #[cfg(test)]
    pub fn new(catalog: Arc<Catalog>) -> Self {
        Self::with_policy(catalog, ExecutionPolicy::default())
    }
    pub fn with_policy(catalog: Arc<Catalog>, policy: ExecutionPolicy) -> Self {
        Self {
            policy,
            catalog,
            objects: HashMap::new(),
            plc_connections: HashSet::new(),
            next_id: 0,
            nonce: format!(
                "{:x}-{:x}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
        }
    }
    fn object(&self, handle: &str) -> Result<&Object> {
        self.objects.get(handle).ok_or_else(|| {
            Error::new(
                "invalid_handle",
                "Unknown or released object; handles are only valid in this MCP process",
            )
        })
    }
    fn store(&mut self, value: IUnknown, interface: Option<String>) -> Result<Value> {
        if self.objects.len() >= 10_000 {
            return Err(Error::new(
                "object_limit",
                "Object limit reached; release handles",
            ));
        }
        if interface.as_deref() == Some("UnityProServer.IApplication") {
            self.validate_application(&value)?;
        }
        self.next_id += 1;
        let handle = format!("ce:{}:{}", self.nonce, self.next_id);
        self.objects.insert(
            handle.clone(),
            Object {
                value,
                interface: interface.clone(),
                serial: self.next_id,
            },
        );
        Ok(json!({"$object":handle,"interface":interface}))
    }
    pub fn checkpoint(&self) -> u64 {
        self.next_id
    }
    pub fn application_ready(&self) -> bool {
        self.objects
            .values()
            .any(|object| object.interface.as_deref() == Some("UnityProServer.IApplication"))
    }
    fn require_application(&self) -> Result<()> {
        if self.application_ready() {
            return Ok(());
        }
        let mut error = Error::new(
            "application_not_ready",
            "First open Control Expert with ce_session_start and keep the IApplication handle. An external window or process does not enable this MCP session.",
        );
        error.details = Some(json!({
            "application_ready": false,
            "required_tool": "ce_session_start",
            "note": "An IApplication reference held by this client and validated against Control Expert 15.1 is required. It works with or without a visible UI."
        }));
        Err(error)
    }
    pub fn handles_since(&self, checkpoint: u64) -> Vec<String> {
        self.objects
            .iter()
            .filter(|(_, o)| o.serial > checkpoint)
            .map(|(h, _)| h.clone())
            .collect()
    }
    pub fn release_handles(&mut self, handles: &[String]) -> usize {
        let mut ordered: Vec<_> = handles
            .iter()
            .filter_map(|h| self.objects.get(h).map(|o| (o.serial, h.clone())))
            .collect();
        ordered.sort_unstable_by_key(|item| std::cmp::Reverse(item.0));
        let count = ordered.len();
        for (_, handle) in ordered {
            self.objects.remove(&handle);
        }
        count
    }
    pub fn run(&mut self, action: Action, trace: &Trace) -> Result<Value> {
        match action {
            Action::Probe => self.probe(),
            Action::SessionStart { file_path } => self
                .session_start(file_path, trace)
                .map_err(Error::with_activation_guidance),
            Action::ViewerOpen { handle } => self.viewer_open(&handle),
            Action::Create { class_id } => {
                if !matches!(
                    class_id.as_str(),
                    "UnityProBroker.PServerBroker" | "UnityProBroker.PServerBrokerContext"
                ) {
                    self.require_application()?;
                }
                self.create(&class_id)
            }
            Action::Call {
                handle,
                operation,
                arguments,
            } => {
                if !bootstrap_interface(&operation.interface) {
                    self.require_application()?;
                }
                self.call(&handle, &operation, &arguments)
            }
            Action::Query { handle, interface } => {
                if !bootstrap_interface(&interface) {
                    self.require_application()?;
                }
                let i = self.catalog.interface(&interface)?;
                self.policy
                    .check_category("interface", &interface, &i.category)?;
                if !i.dispatch || i.events {
                    return Err(Error::unsupported(
                        "QueryInterface only returns business IDispatch interfaces in this version",
                    ));
                }
                let value = query_dispatch(&self.object(&handle)?.value, &i.guid)?;
                self.store(value.cast()?, Some(interface))
            }
            Action::Release { handle } => {
                self.objects.remove(&handle).ok_or_else(|| {
                    Error::new("invalid_handle", "Object already released or unknown")
                })?;
                Ok(json!({"released":handle}))
            }
            Action::Objects => Ok(
                json!({"application_ready":self.application_ready(),"objects":self.objects.iter().map(|(h,o)|json!({"$object":h,"interface":o.interface})).collect::<Vec<_>>()}),
            ),
            Action::Reset => {
                let n = self.release_handles(&self.handles_since(0));
                Ok(
                    json!({"released_objects":n,"note":"COM references were released; no project was saved or closed."}),
                )
            }
        }
    }
    fn session_start(&mut self, file_path: Option<String>, trace: &Trace) -> Result<Value> {
        let started = Instant::now();
        let broker = trace.run_phase("broker_activation", || {
            self.create("UnityProBroker.PServerBroker")
        })?;
        let broker_ms = started.elapsed().as_millis();
        let handle = broker["$object"]
            .as_str()
            .ok_or_else(|| Error::new("invalid_result", "Broker without handle"))?
            .to_owned();
        let (id, arguments, phase) = match file_path {
            Some(path) => (
                "UnityProBroker.IPServerBroker.OpenApplication:method",
                BTreeMap::from([("StuFileName".into(), json!(path))]),
                "application_open",
            ),
            None => (
                "UnityProBroker.IPServerBroker.NewApplication:method",
                BTreeMap::new(),
                "application_create",
            ),
        };
        let operation = self.catalog.prepare(id, &arguments)?;
        let opening = Instant::now();
        let app = trace.run_phase(phase, || self.call(&handle, &operation, &arguments))?;
        let opening_ms = opening.elapsed().as_millis();
        if app["return"]["$object"].as_str().is_none() {
            return Err(Error::new(
                "invalid_result",
                "The open finished without an application reference",
            ));
        }
        Ok(
            json!({"version":"15.1","headless":true,"broker":broker,"application":app["return"],
            "timings_ms":{"broker_activation":broker_ms,"application_open_or_create":opening_ms,"total":started.elapsed().as_millis()}}),
        )
    }
    fn probe(&self) -> Result<Value> {
        let path = PathBuf::from(self.catalog.profile.install_dir).join("pserver.dll");
        let version = file_version(&path)?;
        Ok(
            json!({"platform":"windows","profile":self.catalog.profile.version,"installed_version":version,
            "compatible":self.catalog.profile.accepts_runtime_version(&version),"server_binary":path,
            "open_objects":self.objects.len(),"com_apartment":"STA",
            "application_activation_tested":false,
            "compatibility_scope":"Only the installation and version of pserver.dll; does not verify that the Control Expert COM server can start."}),
        )
    }
    fn ensure_version(&self) -> Result<()> {
        let p = self.probe()?;
        if p["compatible"] != true {
            return Err(Error::new(
                "version_mismatch",
                format!(
                    "Profile 15.1 is incompatible with {}",
                    p["installed_version"]
                ),
            ));
        }
        Ok(())
    }
    fn create(&mut self, class_id: &str) -> Result<Value> {
        if matches!(
            class_id,
            "UnityProServer.Application" | "PSExeLib.PSEConnector"
        ) {
            return Err(Error::new(
                "operation_blocked",
                "Start Control Expert through ce_session_start or the broker methods; activating the application directly does not guarantee a headless start.",
            ));
        }
        let class = self.catalog.coclass(class_id)?;
        self.policy.check_category(
            "coclass",
            class_id,
            class["category"].as_str().unwrap_or(""),
        )?;
        self.ensure_version()?;
        let guid = parse_guid(class["clsid"].as_str().unwrap_or(""))?;
        let lib = class_id.rsplit_once('.').unwrap().0;
        let interface = class["interfaces"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|i| {
                i["flags"]
                    .as_str()
                    .is_some_and(|s| s.contains("FDEFAULT") && !s.contains("FSOURCE"))
            })
            .and_then(|i| i["name"].as_str())
            .map(|n| format!("{lib}.{n}"));
        let object: IUnknown =
            unsafe { CoCreateInstance(&guid, None, CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER)? };
        self.store(object, interface)
    }
    fn dispatch_for(&self, handle: &str, interface: &str) -> Result<IDispatch> {
        let i = self.catalog.interface(interface)?;
        if !i.dispatch {
            return Err(Error::unsupported(
                "This interface has no IDispatch contract",
            ));
        }
        query_dispatch(&self.object(handle)?.value, &i.guid)
    }
    fn call(
        &mut self,
        handle: &str,
        op: &Operation,
        arguments: &BTreeMap<String, Value>,
    ) -> Result<Value> {
        self.policy.check_static(op)?;
        if policy::is_ui_entry(op) {
            if op.id != "UnityProServer.IApplication.DisplayStart:method" {
                return Err(policy::viewer_blocked(op));
            }
            let read_only = self
                .catalog
                .symbolic_enum(&op.parameters[0].com_type, "psrHmiReadOnly")?;
            let requested = match arguments.get("OptionReadWrite") {
                Some(Value::String(name)) => self
                    .catalog
                    .symbolic_enum(&op.parameters[0].com_type, name)
                    .ok(),
                value => value.cloned(),
            };
            if requested.as_ref() != Some(&read_only)
                || arguments.get("comandLine").and_then(Value::as_str) != Some("")
            {
                return Err(policy::viewer_blocked(op));
            }
            if !self.known_application(&self.object(handle)?.value)? {
                return Err(Error::new(
                    "application_not_ready",
                    "The viewer requires an application of this client started headless.",
                ));
            }
        }
        let dispatch = self.dispatch_for(handle, &op.interface)?;
        // Boxed storage remains stable while wrappers are reversed for Invoke.
        let mut slots: Vec<Option<Box<VARIANT>>> = Vec::with_capacity(op.parameters.len());
        let mut args = Vec::with_capacity(op.parameters.len());
        for p in &op.parameters {
            let pointer = self.catalog.resolve_type(&p.com_type)?;
            let pointee = pointer
                .element
                .as_deref()
                .map(|t| self.catalog.resolve_type(t))
                .transpose()?;
            let input_reference = pointer.kind == "VT_PTR"
                && pointee.as_ref().is_some_and(|t| {
                    !(t.kind == "VT_USERDEFINED"
                        && matches!(
                            t.type_kind.as_deref(),
                            Some("TKIND_DISPATCH" | "TKIND_INTERFACE")
                        ))
                });
            if p.optional
                && p.is_input()
                && !arguments.contains_key(&p.name)
                && p.default_value.is_none()
            {
                slots.push(None);
                args.push(missing());
            } else if p.is_output() || input_reference {
                let t = self.indirect_type(&p.com_type)?;
                let initial = if p.is_input() {
                    self.input(
                        arguments
                            .get(&p.name)
                            .or(p.default_value.as_ref())
                            .ok_or_else(|| Error::invalid(format!("Missing {}", p.name)))?,
                        &t,
                    )?
                } else {
                    empty_typed(&t)?
                };
                let mut slot = Box::new(initial);
                let wrapper = by_ref(&mut slot, &t)?;
                slots.push(Some(slot));
                args.push(wrapper);
            } else {
                let arg = if let Some(value) = arguments.get(&p.name).or(p.default_value.as_ref()) {
                    self.input(value, &p.com_type)?
                } else {
                    missing()
                };
                slots.push(None);
                args.push(arg);
            }
        }
        args.reverse();
        let flags = match op.invoke_kind.as_str() {
            "INVOKE_FUNC" => DISPATCH_METHOD,
            "INVOKE_PROPERTYGET" => DISPATCH_PROPERTYGET,
            "INVOKE_PROPERTYPUT" => DISPATCH_PROPERTYPUT,
            "INVOKE_PROPERTYPUTREF" => DISPATCH_PROPERTYPUTREF,
            _ => return Err(Error::unsupported("Unrecognized invocation kind")),
        };
        let mut named = DISPID_PROPERTYPUT;
        let put = matches!(
            op.invoke_kind.as_str(),
            "INVOKE_PROPERTYPUT" | "INVOKE_PROPERTYPUTREF"
        );
        let params = DISPPARAMS {
            rgvarg: if args.is_empty() {
                ptr::null_mut()
            } else {
                args.as_mut_ptr()
            },
            rgdispidNamedArgs: if put { &mut named } else { ptr::null_mut() },
            cArgs: args.len() as u32,
            cNamedArgs: u32::from(put),
        };
        let mut result = VARIANT::default();
        let mut ex = EXCEPINFO::default();
        let mut arg_error = u32::MAX;
        // Immediately before Invoke, so the verified target is current.
        let connection_to_plc = self.check_live_policy(handle, op, arguments)?;
        let invocation = unsafe {
            dispatch.Invoke(
                op.dispid,
                &GUID::zeroed(),
                0,
                flags,
                &params,
                Some(&mut result),
                Some(&mut ex),
                Some(&mut arg_error),
            )
        };
        if let Some(fill) = ex.pfnDeferredFillIn
            && invocation.is_err()
        {
            let _ = unsafe { fill(&mut ex) };
        }
        let source = unsafe { ManuallyDrop::take(&mut ex.bstrSource) }.to_string();
        let description = unsafe { ManuallyDrop::take(&mut ex.bstrDescription) }.to_string();
        let help = unsafe { ManuallyDrop::take(&mut ex.bstrHelpFile) }.to_string();
        if let Err(e) = invocation {
            let mut error = Error::from(e);
            if !description.is_empty() {
                error.message = description;
            }
            error.details = Some(
                json!({"api_id":op.id,"source":source,"exception_scode":format!("0x{:08X}",ex.scode as u32),"help_file":help,
                "argument":(arg_error as usize).checked_add(1).and_then(|n|op.parameters.len().checked_sub(n)).and_then(|i|op.parameters.get(i)).map(|p|&p.name)}),
            );
            if op.interface == "UnityProBroker.IPServerBroker"
                && (op.name.starts_with("NewApplication") || op.name.starts_with("OpenApplication"))
            {
                error = error.with_activation_guidance();
            }
            return Err(error);
        }
        if let Some(to_plc) = connection_to_plc {
            let identity = self.identity(handle)?;
            if to_plc {
                self.plc_connections.insert(identity);
            } else {
                self.plc_connections.remove(&identity);
            }
        } else if op.id == "UnityProServer.IApplication.TargetConnectionClose:method" {
            let identity = self.identity(handle)?;
            self.plc_connections.remove(&identity);
        }
        // If serialization fails, discard handles that could not be returned
        // to the client. This does not roll back the COM operation itself.
        let checkpoint = self.next_id;
        let mut serialized: Result<Value> = (|| {
            let value = self.output(&result, Some(&op.return_type), 0)?;
            let mut outputs = serde_json::Map::new();
            for (p, slot) in op.parameters.iter().zip(slots.iter()) {
                if let Some(slot) = slot {
                    outputs.insert(
                        p.name.clone(),
                        self.output(slot, Some(&self.indirect_type(&p.com_type)?), 0)?,
                    );
                }
            }
            if op.interface == "UnityProBroker.IPServerBroker"
                && (op.name.starts_with("NewApplication") || op.name.starts_with("OpenApplication"))
            {
                for handle in self.handles_since(checkpoint) {
                    let object = self.object(&handle)?;
                    if object.interface.as_deref() == Some("UnityProServer.IApplication") {
                        self.require_headless(&object.value)?;
                    }
                }
            }
            Ok(json!({"api_id":op.id,"return":value,"outputs":outputs}))
        })();
        if let Err(error) = &mut serialized {
            self.release_handles(&self.handles_since(checkpoint));
            let mut details = error.details.take().unwrap_or_else(|| json!({}));
            details["api_id"] = json!(op.id);
            details["com_operation_completed"] = json!(true);
            details["note"] = json!(
                "The COM operation finished, but its result could not be validated or serialized. Do not retry automatically."
            );
            error.details = Some(details);
        }
        serialized
    }
    fn viewer_open(&mut self, handle: &str) -> Result<Value> {
        self.require_application()?;
        if self.object(handle)?.interface.as_deref() != Some("UnityProServer.IApplication") {
            return Err(Error::invalid(
                "handle must be the IApplication reference returned by the session",
            ));
        }
        let id = "UnityProServer.IApplication.DisplayStart:method";
        let operation = self.catalog.operation(id)?.clone();
        let read_only = self
            .catalog
            .symbolic_enum(&operation.parameters[0].com_type, "psrHmiReadOnly")?;
        let arguments = BTreeMap::from([
            ("OptionReadWrite".into(), read_only),
            ("comandLine".into(), json!("")),
        ]);
        self.call(handle, &self.catalog.prepare(id, &arguments)?, &arguments)?;
        Ok(
            json!({"application":{"$object":handle},"viewer_mode":"read_only","same_session":true,"note":"View of the agent session. Editing through COM remains available under the policy; the UI was opened with psrHmiReadOnly."}),
        )
    }
    fn project_uses_simulator(&mut self, project_handle: &str) -> Result<bool> {
        // Compare COM identity against each application's CURRENT project.
        // A simulator selected in another application must never authorize it.
        let project: IUnknown = self.object(project_handle)?.value.cast()?;
        let apps: Vec<String> = self
            .objects
            .iter()
            .filter(|(_, object)| {
                object.interface.as_deref() == Some("UnityProServer.IApplication")
            })
            .map(|(handle, _)| handle.clone())
            .collect();
        let project_op = self
            .catalog
            .prepare("UnityProServer.IApplication.Project:get", &BTreeMap::new())?;
        for app in apps {
            let checkpoint = self.checkpoint();
            let same_project: Result<bool> = (|| {
                let result = self.call(&app, &project_op, &BTreeMap::new())?;
                let Some(handle) = result["return"]["$object"].as_str() else {
                    return Ok(false);
                };
                let current: IUnknown = self.object(handle)?.value.cast()?;
                Ok(current.as_raw() == project.as_raw())
            })();
            self.release_handles(&self.handles_since(checkpoint));
            // An inaccessible or closed application cannot authorize this transfer.
            if !matches!(same_project, Ok(true)) {
                continue;
            }
            return self.application_targets_simulator(&app);
        }
        Ok(false)
    }
    fn application_targets_simulator(&mut self, app: &str) -> Result<bool> {
        // A connection this server opened to a PLC stays authoritative even if
        // SelectTarget is switched back to the simulator without reconnecting.
        if self.plc_connections.contains(&self.identity(app)?) {
            return Ok(false);
        }
        let target_op = self.catalog.prepare(
            "UnityProServer.IApplication.SelectTarget:get",
            &BTreeMap::new(),
        )?;
        let simulator = self
            .catalog
            .symbolic_enum(&target_op.return_type, "psrConnectToSimulator")?;
        let result = self.call(app, &target_op, &BTreeMap::new())?;
        Ok(result["return"] == simulator)
    }
    /// Checks that need the call arguments or the live application state.
    /// Returns, for TargetConnectionOpen, whether the target is a PLC.
    fn check_live_policy(
        &mut self,
        handle: &str,
        op: &Operation,
        arguments: &BTreeMap<String, Value>,
    ) -> Result<Option<bool>> {
        match policy::restriction(op) {
            Restriction::PlcConnection {
                simulator_exempt: true,
            } => {
                let parameter = op
                    .parameters
                    .iter()
                    .find(|p| p.name == "target")
                    .ok_or_else(|| Error::new("invalid_catalog", "Missing target parameter"))?;
                let simulator = self
                    .catalog
                    .symbolic_enum(&parameter.com_type, "psrConnectToSimulator")?;
                let requested = match arguments.get(&parameter.name) {
                    Some(Value::String(name)) => {
                        self.catalog.symbolic_enum(&parameter.com_type, name).ok()
                    }
                    value => value.cloned(),
                };
                let to_plc = requested.as_ref() != Some(&simulator);
                if to_plc && !self.policy.allow_plc_connection {
                    return Err(policy::blocked(
                        op,
                        "plc_connection_disabled",
                        policy::ENV_PLC_CONNECTION,
                        "Connecting to a PLC is blocked. A connection with target=psrConnectToSimulator needs no unlock.",
                    ));
                }
                Ok(Some(to_plc))
            }
            Restriction::PlcWrite {
                simulator_exempt: true,
            } if !self.policy.allow_plc_write => {
                let verification = if op.interface == "UnityProServer.IApplication" {
                    self.application_targets_simulator(handle)
                } else {
                    self.project_uses_simulator(handle)
                };
                match verification {
                    Ok(true) => Ok(None),
                    verification => {
                        let mut error = policy::blocked(
                            op,
                            "plc_write_disabled_or_target_unverified",
                            policy::ENV_PLC_WRITE,
                            "Blocked: the simulator target of the owning application could not be verified. Writing to a PLC requires an explicit unlock.",
                        );
                        if let Err(cause) = verification {
                            error.details.as_mut().unwrap()["verification_error"] = json!(cause);
                        }
                        Err(error)
                    }
                }
            }
            _ => Ok(None),
        }
    }
    fn identity(&self, handle: &str) -> Result<usize> {
        let identity: IUnknown = self.object(handle)?.value.cast()?;
        Ok(identity.as_raw() as usize)
    }
    fn indirect_type(&self, t: &Type) -> Result<Type> {
        let t = self.catalog.resolve_type(t)?;
        // [out] T* -> T; [out] IInterface** -> IInterface* (encoded DISPATCH).
        match t.element {
            Some(e) => self.catalog.resolve_type(&e),
            None => Ok(t),
        }
    }
    fn input(&self, value: &Value, expected: &Type) -> Result<VARIANT> {
        let resolved = self.catalog.resolve_type(expected)?;
        if resolved.kind == "VT_PTR" {
            return self.input(
                value,
                resolved
                    .element
                    .as_deref()
                    .ok_or_else(|| Error::unsupported("Untyped pointer"))?,
            );
        }
        if resolved.kind == "VT_USERDEFINED"
            && matches!(resolved.type_kind.as_deref(), Some("TKIND_ENUM"))
        {
            let number = if let Some(name) = value.as_str() {
                self.catalog.symbolic_enum(expected, name)?
            } else {
                value.clone()
            };
            return number
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .map(VARIANT::from)
                .ok_or_else(|| Error::invalid("Enumeration outside the LONG range"));
        }
        if let Some(name) = value.get("$object").and_then(Value::as_str) {
            if !matches!(
                resolved.kind.as_str(),
                "VT_USERDEFINED" | "VT_DISPATCH" | "VT_UNKNOWN" | "VT_VARIANT"
            ) {
                return Err(Error::invalid(
                    "Handle received where a scalar was expected",
                ));
            }
            let obj = &self.object(name)?.value;
            if resolved.kind == "VT_UNKNOWN" {
                return Ok(obj.clone().into());
            }
            let dispatch = if resolved.kind == "VT_USERDEFINED" {
                let id = format!(
                    "{}.{}",
                    resolved.library.as_deref().unwrap_or(""),
                    resolved.name.as_deref().unwrap_or("")
                );
                self.dispatch_for(name, &id)?
            } else {
                obj.cast()?
            };
            return Ok(dispatch_variant(dispatch));
        }
        if let Some(typed) = value.get("$variant").and_then(Value::as_str) {
            if resolved.kind != "VT_VARIANT" && resolved.kind != "VT_SAFEARRAY" {
                return Err(Error::invalid(
                    "$variant is only accepted for VARIANT/SAFEARRAY",
                ));
            }
            return self.explicit_variant(typed, value.get("value").unwrap_or(&Value::Null));
        }
        match resolved.kind.as_str() {
            "VT_VARIANT" => self.dynamic_input(value),
            "VT_BSTR" => value
                .as_str()
                .map(VARIANT::from)
                .ok_or_else(|| Error::invalid("Expected a BSTR string")),
            "VT_BOOL" => value
                .as_bool()
                .map(VARIANT::from)
                .ok_or_else(|| Error::invalid("Expected a boolean")),
            "VT_I4" | "VT_INT" => {
                signed(value, i32::MIN as i64, i32::MAX as i64).map(|v| VARIANT::from(v as i32))
            }
            "VT_I2" => {
                signed(value, i16::MIN as i64, i16::MAX as i64).map(|v| VARIANT::from(v as i16))
            }
            "VT_I1" => {
                signed(value, i8::MIN as i64, i8::MAX as i64).map(|v| VARIANT::from(v as i8))
            }
            "VT_UI4" | "VT_UINT" => {
                unsigned(value, u32::MAX as u64).map(|v| VARIANT::from(v as u32))
            }
            "VT_UI2" => unsigned(value, u16::MAX as u64).map(|v| VARIANT::from(v as u16)),
            "VT_UI1" => unsigned(value, u8::MAX as u64).map(|v| VARIANT::from(v as u8)),
            "VT_I8" => signed(value, i64::MIN, i64::MAX).map(VARIANT::from),
            "VT_UI8" => unsigned(value, u64::MAX).map(VARIANT::from),
            "VT_R4" => number(value).and_then(|v| {
                if v.abs() <= f32::MAX as f64 {
                    Ok(VARIANT::from(v as f32))
                } else {
                    Err(Error::invalid("FLOAT out of range"))
                }
            }),
            "VT_R8" => number(value).map(VARIANT::from),
            "VT_DISPATCH" | "VT_UNKNOWN" | "VT_USERDEFINED"
                if value.is_null() || value.as_i64() == Some(0) =>
            {
                empty_typed(&resolved)
            }
            "VT_SAFEARRAY" => Err(Error::unsupported(
                "Use {$variant: 'array', value: [...]} or {$variant: 'bytes', value: [...]} for input arrays",
            )),
            _ => Err(Error::unsupported(format!(
                "Unsupported input type: {}",
                resolved.kind
            ))),
        }
    }
    fn dynamic_input(&self, v: &Value) -> Result<VARIANT> {
        match v {
            Value::Null => Ok(null()),
            Value::Bool(b) => Ok((*b).into()),
            Value::String(s) => Ok(s.as_str().into()),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(if let Ok(i) = i32::try_from(i) {
                        i.into()
                    } else {
                        i.into()
                    })
                } else if let Some(i) = n.as_u64() {
                    Ok(i.into())
                } else {
                    Ok(number(v)?.into())
                }
            }
            Value::Array(_) => self.explicit_variant("array", v),
            _ => Err(Error::invalid(
                "JSON object not convertible to VARIANT; use $object or $variant",
            )),
        }
    }
    fn explicit_variant(&self, kind: &str, v: &Value) -> Result<VARIANT> {
        match kind {
            "empty" => Ok(VARIANT::default()),
            "null" => Ok(null()),
            "missing" => Ok(missing()),
            "i2" => signed(v, i16::MIN as i64, i16::MAX as i64).map(|v| VARIANT::from(v as i16)),
            "i4" => signed(v, i32::MIN as i64, i32::MAX as i64).map(|v| VARIANT::from(v as i32)),
            "ui4" => unsigned(v, u32::MAX as u64).map(|v| VARIANT::from(v as u32)),
            "i8" => signed(v, i64::MIN, i64::MAX).map(VARIANT::from),
            "r8" => number(v).map(VARIANT::from),
            "bytes" | "array" => {
                let items = v
                    .as_array()
                    .ok_or_else(|| Error::invalid("Expected an array"))?;
                if items.len() > 1_000_000 {
                    return Err(Error::invalid("Array too large"));
                }
                let vt = if kind == "bytes" { VT_UI1 } else { VT_VARIANT };
                let psa = unsafe { SafeArrayCreateVector(vt, 0, items.len() as u32) };
                if psa.is_null() {
                    return Err(Error::new("allocation_failed", "SafeArrayCreateVector"));
                }
                let mut result = VARIANT::default();
                unsafe {
                    (*result.Anonymous.Anonymous).vt = VARENUM(VT_ARRAY.0 | vt.0);
                    (*result.Anonymous.Anonymous).Anonymous.parray = psa;
                }
                for (i, item) in items.iter().enumerate() {
                    let index = i as i32;
                    if kind == "bytes" {
                        let byte = unsigned(item, 255)? as u8;
                        unsafe {
                            SafeArrayPutElement(psa, &index, (&byte as *const u8).cast())?;
                        }
                    } else {
                        let value =
                            if item.get("$object").is_some() || item.get("$variant").is_some() {
                                self.input(item, &variant_type())?
                            } else {
                                self.dynamic_input(item)?
                            };
                        unsafe {
                            SafeArrayPutElement(psa, &index, (&value as *const VARIANT).cast())?;
                        }
                    }
                }
                Ok(result)
            }
            _ => Err(Error::unsupported(format!("Unknown $variant: {kind}"))),
        }
    }
    fn output(&mut self, v: &VARIANT, expected: Option<&Type>, depth: usize) -> Result<Value> {
        if depth > 16 {
            return Err(Error::new("result_too_deep", "COM result exceeds depth 16"));
        }
        if v.vt().0 & VT_BYREF.0 != 0 {
            let mut copied = VARIANT::default();
            unsafe {
                VariantCopyInd(&mut copied, v)?;
            }
            return self.output(&copied, expected, depth + 1);
        }
        if v.vt().0 & VT_ARRAY.0 != 0 {
            return self.array_output(v, depth);
        }
        unsafe {
            let d = &(*v.Anonymous.Anonymous).Anonymous;
            match v.vt() {
                VT_EMPTY | VT_NULL => Ok(Value::Null),
                VT_BSTR => Ok(json!(d.bstrVal.to_string())),
                VT_BOOL => Ok(json!(d.boolVal.0 != 0)),
                VT_I1 => Ok(json!(d.cVal)),
                VT_UI1 => Ok(json!(d.bVal)),
                VT_I2 => Ok(json!(d.iVal)),
                VT_UI2 => Ok(json!(d.uiVal)),
                VT_I4 | VT_INT => Ok(json!(d.lVal)),
                VT_UI4 | VT_UINT => Ok(json!(d.ulVal)),
                VT_I8 => Ok(json!(d.llVal)),
                VT_UI8 => Ok(json!(d.ullVal)),
                VT_R4 => finite(d.fltVal as f64),
                VT_R8 => finite(d.dblVal),
                VT_DATE => Ok(json!({"$variant":"date","value":d.date})),
                VT_ERROR => {
                    Ok(json!({"$variant":"error","hresult":format!("0x{:08X}",d.scode as u32)}))
                }
                VT_DISPATCH => {
                    if let Some(obj) = &*d.pdispVal {
                        self.store_result(obj.cast()?, expected)
                    } else {
                        Ok(Value::Null)
                    }
                }
                VT_UNKNOWN => {
                    if let Some(obj) = &*d.punkVal {
                        self.store_result(obj.clone(), expected)
                    } else {
                        Ok(Value::Null)
                    }
                }
                other => Err(Error::unsupported(format!(
                    "Result VARTYPE {} is not serializable",
                    other.0
                ))),
            }
        }
    }
    fn store_result(&mut self, obj: IUnknown, expected: Option<&Type>) -> Result<Value> {
        let mut t = expected;
        while let Some(e) = t.and_then(|t| t.element.as_deref()) {
            t = Some(e);
        }
        let interface = t
            .filter(|t| t.kind == "VT_USERDEFINED")
            .and_then(|t| Some(format!("{}.{}", t.library.as_ref()?, t.name.as_ref()?)));
        self.store(obj, interface)
    }
    fn validate_application(&self, obj: &IUnknown) -> Result<()> {
        let version = self.application_property(obj, "UnityProServer.IApplication.Version:get")?;
        let version = BSTR::try_from(&version)?.to_string();
        if !self.catalog.profile.accepts_runtime_version(&version) {
            return Err(Error::new(
                "version_mismatch",
                format!("The session returned Control Expert {version}; 15.1 is required"),
            ));
        }
        // An already managed application can have its read-only viewer open.
        // A new application must be demonstrably headless before acceptance.
        if !self.known_application(obj)? {
            self.require_headless(obj)?;
        }
        Ok(())
    }
    fn known_application(&self, obj: &IUnknown) -> Result<bool> {
        let identity: IUnknown = obj.cast()?;
        for object in self
            .objects
            .values()
            .filter(|o| o.interface.as_deref() == Some("UnityProServer.IApplication"))
        {
            let known: IUnknown = object.value.cast()?;
            if known.as_raw() == identity.as_raw() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn require_headless(&self, obj: &IUnknown) -> Result<()> {
        let visible =
            self.application_property(obj, "UnityProServer.IApplication.IsVisible:get")?;
        if visible.vt() != VT_I4 || i32::try_from(&visible)? != 0 {
            return Err(Error::new(
                "headless_required",
                "Control Expert returned a visible application or could not confirm IsVisible=0. That session is not adopted; start a headless application and use ce_viewer_open to observe it read-only.",
            ));
        }
        Ok(())
    }
    fn application_property(&self, obj: &IUnknown, id: &str) -> Result<VARIANT> {
        let app = query_dispatch(
            obj,
            &self.catalog.interface("UnityProServer.IApplication")?.guid,
        )?;
        let op = self.catalog.operation(id)?;
        let mut version = VARIANT::default();
        unsafe {
            app.Invoke(
                op.dispid,
                &GUID::zeroed(),
                0,
                DISPATCH_PROPERTYGET,
                &DISPPARAMS::default(),
                Some(&mut version),
                None,
                None,
            )?;
        }
        Ok(version)
    }
    fn array_output(&mut self, v: &VARIANT, depth: usize) -> Result<Value> {
        let psa = unsafe { (*v.Anonymous.Anonymous).Anonymous.parray };
        if psa.is_null() {
            return Ok(json!([]));
        }
        let dims = unsafe { SafeArrayGetDim(psa) };
        if dims == 0 || dims > 4 {
            return Err(Error::unsupported(
                "SAFEARRAY with 1 to 4 dimensions are supported",
            ));
        }
        let mut bounds = Vec::new();
        let mut total = 1usize;
        for i in 1..=dims {
            let low = unsafe { SafeArrayGetLBound(psa, i)? };
            let high = unsafe { SafeArrayGetUBound(psa, i)? };
            total = total.saturating_mul((i64::from(high) - i64::from(low) + 1).max(0) as usize);
            bounds.push((low, high));
        }
        if total > 1_000_000 {
            return Err(Error::new(
                "result_too_large",
                "SAFEARRAY exceeds 1,000,000 elements",
            ));
        }
        let vt = unsafe { SafeArrayGetVartype(psa) }.unwrap_or(VARENUM(v.vt().0 & 0x0fff));
        if !matches!(
            vt,
            VT_VARIANT
                | VT_BSTR
                | VT_BOOL
                | VT_I1
                | VT_UI1
                | VT_I2
                | VT_UI2
                | VT_I4
                | VT_UI4
                | VT_I8
                | VT_UI8
                | VT_INT
                | VT_UINT
                | VT_R4
                | VT_R8
                | VT_DATE
                | VT_ERROR
                | VT_UNKNOWN
                | VT_DISPATCH
        ) {
            return Err(Error::unsupported(format!(
                "SAFEARRAY with VARTYPE {} is not supported",
                vt.0
            )));
        }
        let mut indices = vec![0; dims as usize];
        let values = self.array_dimension(psa, vt, &bounds, &mut indices, 0, depth + 1)?;
        // Preserve nonzero lower bounds and multidimensional layout explicitly.
        if dims == 1 && bounds[0].0 == 0 {
            Ok(values)
        } else {
            Ok(
                json!({"$array":values,"bounds":bounds.iter().map(|(l,h)|json!({"lower":l,"upper":h})).collect::<Vec<_>>()}),
            )
        }
    }
    fn array_dimension(
        &mut self,
        psa: *mut SAFEARRAY,
        vt: VARENUM,
        bounds: &[(i32, i32)],
        indices: &mut [i32],
        dim: usize,
        depth: usize,
    ) -> Result<Value> {
        let mut values = Vec::new();
        for i in bounds[dim].0..=bounds[dim].1 {
            indices[dim] = i;
            if dim + 1 < bounds.len() {
                values.push(self.array_dimension(psa, vt, bounds, indices, dim + 1, depth)?);
            } else {
                let mut value = VARIANT::default();
                unsafe {
                    if vt == VT_VARIANT {
                        SafeArrayGetElement(
                            psa,
                            indices.as_ptr(),
                            (&mut value as *mut VARIANT).cast(),
                        )?;
                    } else {
                        (*value.Anonymous.Anonymous).vt = vt;
                        SafeArrayGetElement(
                            psa,
                            indices.as_ptr(),
                            (&mut (*value.Anonymous.Anonymous).Anonymous as *mut VARIANT_0_0_0)
                                .cast(),
                        )?;
                    }
                }
                values.push(self.output(&value, None, depth + 1)?);
            }
        }
        Ok(Value::Array(values))
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.release_handles(&self.handles_since(0));
    }
}

fn parse_guid(value: &str) -> Result<GUID> {
    GUID::try_from(value.trim_matches(['{', '}'])).map_err(|_| Error::invalid("Invalid GUID"))
}
fn query_dispatch(obj: &IUnknown, iid: &str) -> Result<IDispatch> {
    let guid = parse_guid(iid)?;
    let mut raw = ptr::null_mut();
    unsafe {
        obj.query(&guid, &mut raw).ok()?;
    }
    if raw.is_null() {
        return Err(Error::new(
            "null_interface",
            "QueryInterface returned a null pointer",
        ));
    }
    // Called only for catalogued dual/dispatch interfaces: IDispatch is the
    // base vtable of the returned interface. Do not QueryInterface back to the
    // object's default IDispatch, which may hide the secondary members.
    Ok(unsafe { IDispatch::from_raw(raw) })
}
fn dispatch_variant(obj: IDispatch) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_DISPATCH;
        (*v.Anonymous.Anonymous).Anonymous.pdispVal = ManuallyDrop::new(Some(obj));
    }
    v
}
fn null() -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_NULL;
    }
    v
}
fn missing() -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VT_ERROR;
        (*v.Anonymous.Anonymous).Anonymous.scode = DISP_E_PARAMNOTFOUND.0;
    }
    v
}
fn variant_type() -> Type {
    Type {
        kind: "VT_VARIANT".into(),
        element: None,
        name: None,
        library: None,
        type_kind: None,
    }
}
fn signed(v: &Value, min: i64, max: i64) -> Result<i64> {
    v.as_i64()
        .filter(|n| *n >= min && *n <= max)
        .ok_or_else(|| Error::invalid(format!("Expected an integer between {min} and {max}")))
}
fn unsigned(v: &Value, max: u64) -> Result<u64> {
    v.as_u64()
        .filter(|n| *n <= max)
        .ok_or_else(|| Error::invalid(format!("Expected an integer between 0 and {max}")))
}
fn number(v: &Value) -> Result<f64> {
    v.as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| Error::invalid("Expected a finite number"))
}
fn finite(v: f64) -> Result<Value> {
    if v.is_finite() {
        Ok(json!(v))
    } else {
        Err(Error::unsupported("COM returned NaN or infinity"))
    }
}
fn empty_typed(t: &Type) -> Result<VARIANT> {
    let mut v = VARIANT::default();
    let vt = match t.kind.as_str() {
        "VT_PTR" | "VT_DISPATCH" => VT_DISPATCH,
        "VT_UNKNOWN" => VT_UNKNOWN,
        "VT_USERDEFINED" if t.type_kind.as_deref() == Some("TKIND_ENUM") => VT_I4,
        "VT_USERDEFINED" if t.type_kind.as_deref() == Some("TKIND_DISPATCH") => VT_DISPATCH,
        "VT_VARIANT" => return Ok(v),
        "VT_BSTR" => VT_BSTR,
        "VT_I2" => VT_I2,
        "VT_I4" | "VT_INT" => VT_I4,
        "VT_UI4" | "VT_UINT" => VT_UI4,
        "VT_BOOL" => VT_BOOL,
        "VT_R8" => VT_R8,
        "VT_R4" => VT_R4,
        _ => {
            return Err(Error::unsupported(format!(
                "Unsupported output type: {}",
                t.kind
            )));
        }
    };
    unsafe {
        (*v.Anonymous.Anonymous).vt = vt;
    }
    Ok(v)
}
fn by_ref(slot: &mut VARIANT, t: &Type) -> Result<VARIANT> {
    let mut v = VARIANT::default();
    unsafe {
        let d = &mut (*slot.Anonymous.Anonymous);
        if t.kind == "VT_VARIANT" {
            (*v.Anonymous.Anonymous).vt = VARENUM(VT_BYREF.0 | VT_VARIANT.0);
            (*v.Anonymous.Anonymous).Anonymous.pvarVal = slot;
        } else {
            (*v.Anonymous.Anonymous).vt = VARENUM(VT_BYREF.0 | d.vt.0);
            (*v.Anonymous.Anonymous).Anonymous.byref =
                (&mut d.Anonymous as *mut VARIANT_0_0_0).cast();
        }
    }
    Ok(v)
}
fn file_version(path: &std::path::Path) -> Result<String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return Err(Error::new(
                "installation_not_found",
                format!("Could not read the version of {}", path.display()),
            ));
        }
        let mut data = vec![0u64; (size as usize).div_ceil(8)];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, data.as_mut_ptr().cast())?;
        let mut info: *mut c_void = ptr::null_mut();
        let mut length = 0;
        // Schneider's fixed binary version is an internal build identifier
        // (22.2.12.0 for this 15.1 installation). The localized ProductVersion
        // string carries the public product release (15, 1, 0, 0).
        let mut translations = vec![(0x0409u16, 0x04b0u16), (0x0409u16, 0x04e4u16)];
        if VerQueryValueW(
            data.as_ptr().cast(),
            ::windows::core::w!("\\VarFileInfo\\Translation"),
            &mut info,
            &mut length,
        )
        .as_bool()
            && !info.is_null()
        {
            let values = std::slice::from_raw_parts(info.cast::<u16>(), length as usize / 2);
            translations.splice(0..0, values.as_chunks::<2>().0.iter().map(|v| (v[0], v[1])));
        }
        for (language, page) in translations {
            let query: Vec<u16> =
                format!("\\StringFileInfo\\{language:04x}{page:04x}\\ProductVersion")
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
            if VerQueryValueW(
                data.as_ptr().cast(),
                PCWSTR(query.as_ptr()),
                &mut info,
                &mut length,
            )
            .as_bool()
                && !info.is_null()
                && length > 0
            {
                let value = String::from_utf16_lossy(std::slice::from_raw_parts(
                    info.cast::<u16>(),
                    length as usize,
                ));
                let numbers: Vec<_> = value
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .collect();
                if numbers.len() >= 2 {
                    return Ok(numbers.join("."));
                }
            }
        }
        if !VerQueryValueW(
            data.as_ptr().cast(),
            ::windows::core::w!("\\"),
            &mut info,
            &mut length,
        )
        .as_bool()
            || length < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
        {
            return Err(Error::new(
                "version_unavailable",
                "File without VS_FIXEDFILEINFO",
            ));
        }
        let info = &*info.cast::<VS_FIXEDFILEINFO>();
        Ok(format!(
            "{}.{}.{}.{}",
            info.dwProductVersionMS >> 16,
            info.dwProductVersionMS & 65535,
            info.dwProductVersionLS >> 16,
            info.dwProductVersionLS & 65535
        ))
    }
}
