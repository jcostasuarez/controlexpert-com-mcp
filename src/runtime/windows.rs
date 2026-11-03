//! Windows backend: owns every COM object on the STA worker and marshals JSON
//! arguments and results to and from `IDispatch::Invoke`.
//!
//! Every `unsafe` block here is a COM or Win32 call, or a read of the tagged
//! union inside a `VARIANT`. Union access goes through [`fields`] and
//! [`payload`] so that each block does one thing and says why it is sound.
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
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
                CoCreateInstance, CoInitializeEx, CoUninitialize, DISPATCH_FLAGS, DISPATCH_METHOD,
                DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPATCH_PROPERTYPUTREF, DISPPARAMS,
                EXCEPINFO, IDispatch, SAFEARRAY,
            },
            Ole::{
                DISPID_PROPERTYPUT, SafeArrayCreateVector, SafeArrayGetDim, SafeArrayGetElement,
                SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype, SafeArrayPutElement,
            },
            Variant::{
                VARENUM, VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_ARRAY, VT_BOOL, VT_BSTR, VT_BYREF,
                VT_DATE, VT_DISPATCH, VT_EMPTY, VT_ERROR, VT_I1, VT_I2, VT_I4, VT_I8, VT_INT,
                VT_NULL, VT_R4, VT_R8, VT_UI1, VT_UI2, VT_UI4, VT_UI8, VT_UINT, VT_UNKNOWN,
                VT_VARIANT, VariantCopyInd,
            },
        },
        UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        },
    },
    core::{BSTR, GUID, IUnknown, Interface as ComInterface, PCWSTR},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::c_void,
    marker::PhantomData,
    mem::ManuallyDrop,
    path::{Path, PathBuf},
    ptr,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(test)]
mod tests;

/// COM initialization of the current thread as a single-threaded apartment.
///
/// Not `Send`: `CoUninitialize` must run on the thread that initialized COM.
#[derive(Debug)]
pub(super) struct Apartment(PhantomData<*const ()>);
impl Apartment {
    pub(super) fn initialize() -> Result<Self> {
        // SAFETY: no reserved pointer is passed. The matching CoUninitialize
        // runs in Drop on this same thread, because Apartment is not Send.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx of `initialize` on
        // the same thread; the worker drops every COM object before this.
        unsafe { CoUninitialize() };
    }
}

/// Dispatches pending window messages, which an STA must do to receive calls.
pub(super) fn pump_messages() {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid, writable MSG for the duration of the call.
    while unsafe { PeekMessageW(&raw mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
        // SAFETY: `msg` was just filled in by PeekMessageW.
        let _ = unsafe { TranslateMessage(&raw const msg) };
        // SAFETY: as above; the message belongs to this thread.
        unsafe { DispatchMessageW(&raw const msg) };
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
fn opens_application(op: &Operation) -> bool {
    op.interface == "UnityProBroker.IPServerBroker"
        && (op.name.starts_with("NewApplication") || op.name.starts_with("OpenApplication"))
}

/// Arguments ready for `Invoke`, in reverse order. `args` may hold BYREF
/// pointers into `slots`; each slot is boxed so its address stays stable.
struct Marshalled {
    args: Vec<VARIANT>,
    slots: Vec<Option<Box<VARIANT>>>,
}

pub(super) struct State {
    catalog: Arc<Catalog>,
    objects: HashMap<String, Object>,
    next_id: u64,
    nonce: String,
    policy: ExecutionPolicy,
    /// COM identities of applications that this server connected to a
    /// controller other than the simulator, until `TargetConnectionClose`.
    plc_connections: HashSet<usize>,
}
impl State {
    #[cfg(test)]
    pub(super) fn new(catalog: Arc<Catalog>) -> Self {
        Self::with_policy(catalog, ExecutionPolicy::default())
    }
    pub(super) fn with_policy(catalog: Arc<Catalog>, policy: ExecutionPolicy) -> Self {
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
        let reply = json!({"$object":handle,"interface":interface});
        self.objects.insert(
            handle,
            Object {
                value,
                interface,
                serial: self.next_id,
            },
        );
        Ok(reply)
    }
    pub(super) fn checkpoint(&self) -> u64 {
        self.next_id
    }
    pub(super) fn application_ready(&self) -> bool {
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
    pub(super) fn handles_since(&self, checkpoint: u64) -> Vec<String> {
        self.objects
            .iter()
            .filter(|(_, o)| o.serial > checkpoint)
            .map(|(h, _)| h.clone())
            .collect()
    }
    pub(super) fn release_handles(&mut self, handles: &[String]) -> usize {
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
    pub(super) fn run(&mut self, action: Action, trace: &Trace) -> Result<Value> {
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
        let (lib, _) = class_id.rsplit_once('.').ok_or_else(|| {
            Error::new(
                "invalid_catalog",
                format!("Coclass ID without a library: {class_id}"),
            )
        })?;
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
        // SAFETY: `guid` is a CLSID from the catalog and outlives the call;
        // there is no outer object; the Apartment initialized COM here.
        let object: IUnknown = unsafe {
            CoCreateInstance(
                &raw const guid,
                None,
                CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER,
            )?
        };
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
            self.check_viewer(handle, op, arguments)?;
        }
        let dispatch = self.dispatch_for(handle, &op.interface)?;
        let mut marshalled = self.marshal(op, arguments)?;
        let flags = invoke_flags(op)?;
        // Immediately before Invoke, so the verified target is current.
        let connection_to_plc = self.check_live_policy(handle, op, arguments)?;
        let result = invoke(&dispatch, op, flags, &mut marshalled.args)?;
        self.track_connection(handle, op, connection_to_plc)?;
        self.serialize(op, &result, &marshalled.slots)
    }
    /// Among the UI entry points only `DisplayStart(psrHmiReadOnly, "")` on
    /// an application of this client is allowed.
    fn check_viewer(
        &self,
        handle: &str,
        op: &Operation,
        arguments: &BTreeMap<String, Value>,
    ) -> Result<()> {
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
        Ok(())
    }
    /// Converts named JSON arguments to positional VARIANTs. Outputs and
    /// by-reference inputs get a boxed slot that `Invoke` writes through.
    fn marshal(&self, op: &Operation, arguments: &BTreeMap<String, Value>) -> Result<Marshalled> {
        let mut slots = Vec::with_capacity(op.parameters.len());
        let mut args = Vec::with_capacity(op.parameters.len());
        for p in &op.parameters {
            let resolved = self.catalog.resolve_type(&p.com_type)?;
            let element = resolved
                .element
                .as_deref()
                .map(|t| self.catalog.resolve_type(t))
                .transpose()?;
            let input_reference = resolved.kind == "VT_PTR"
                && element.as_ref().is_some_and(|t| {
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
                let wrapper = by_ref(&mut slot, &t);
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
        Ok(Marshalled { args, slots })
    }
    /// Records which applications this server connected to a PLC.
    fn track_connection(
        &mut self,
        handle: &str,
        op: &Operation,
        connection_to_plc: Option<bool>,
    ) -> Result<()> {
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
        Ok(())
    }
    /// Converts the return value and the out-parameters to JSON. If that
    /// fails, discards the handles that could not be returned to the client;
    /// this does not roll back the COM operation itself.
    fn serialize(
        &mut self,
        op: &Operation,
        result: &VARIANT,
        slots: &[Option<Box<VARIANT>>],
    ) -> Result<Value> {
        let checkpoint = self.next_id;
        let mut serialized: Result<Value> = (|| {
            let value = self.output(result, Some(&op.return_type), 0)?;
            let mut outputs = serde_json::Map::new();
            for (p, slot) in op.parameters.iter().zip(slots) {
                if let Some(slot) = slot {
                    outputs.insert(
                        p.name.clone(),
                        self.output(slot, Some(&self.indirect_type(&p.com_type)?), 0)?,
                    );
                }
            }
            if opens_application(op) {
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
    /// Returns, for `TargetConnectionOpen`, whether the target is a PLC.
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
                if matches!(verification, Ok(true)) {
                    return Ok(None);
                }
                let mut error = policy::blocked(
                    op,
                    "plc_write_disabled_or_target_unverified",
                    policy::ENV_PLC_WRITE,
                    "Blocked: the simulator target of the owning application could not be verified. Writing to a PLC requires an explicit unlock.",
                );
                if let (Err(cause), Some(details)) = (verification, error.details.as_mut()) {
                    details["verification_error"] = json!(cause);
                }
                Err(error)
            }
            _ => Ok(None),
        }
    }
    /// The address of the object's canonical `IUnknown`, which COM
    /// guarantees to be the same for every reference to one object.
    fn identity(&self, handle: &str) -> Result<usize> {
        let identity: IUnknown = self.object(handle)?.value.cast()?;
        Ok(identity.as_raw().addr())
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
        if resolved.kind == "VT_VARIANT" {
            return self.dynamic_input(value);
        }
        scalar_input(value, &resolved)
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
            Value::Object(_) => Err(Error::invalid(
                "JSON object not convertible to VARIANT; use $object or $variant",
            )),
        }
    }
    fn explicit_variant(&self, kind: &str, v: &Value) -> Result<VARIANT> {
        match kind {
            "empty" => Ok(VARIANT::default()),
            "null" => Ok(null()),
            "missing" => Ok(missing()),
            "i2" => signed(v, i16::MIN, i16::MAX).map(VARIANT::from),
            "i4" => signed(v, i32::MIN, i32::MAX).map(VARIANT::from),
            "ui4" => unsigned(v, u32::MAX).map(VARIANT::from),
            "i8" => signed(v, i64::MIN, i64::MAX).map(VARIANT::from),
            "r8" => number(v).map(VARIANT::from),
            "bytes" | "array" => self.array_input(kind == "bytes", v),
            _ => Err(Error::unsupported(format!("Unknown $variant: {kind}"))),
        }
    }
    /// A one-dimensional SAFEARRAY of bytes or of VARIANTs.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "private helper; its only caller is the match arm right above"
    )]
    fn array_input(&self, bytes: bool, v: &Value) -> Result<VARIANT> {
        let items = v
            .as_array()
            .ok_or_else(|| Error::invalid("Expected an array"))?;
        let len = u32::try_from(items.len())
            .ok()
            .filter(|n| *n <= 1_000_000)
            .ok_or_else(|| Error::invalid("Array too large"))?;
        let vt = if bytes { VT_UI1 } else { VT_VARIANT };
        // SAFETY: creating a vector has no preconditions; null is checked below.
        let psa = unsafe { SafeArrayCreateVector(vt, 0, len) };
        if psa.is_null() {
            return Err(Error::new("allocation_failed", "SafeArrayCreateVector"));
        }
        let mut result = VARIANT::default();
        // SAFETY: the tag and the payload are set together; from here on the
        // VARIANT owns `psa` and destroys it if an element fails below.
        let tagged = unsafe { fields(&mut result) };
        tagged.vt = VARENUM(VT_ARRAY.0 | vt.0);
        tagged.Anonymous.parray = psa;
        for (i, item) in items.iter().enumerate() {
            let index = i32::try_from(i).map_err(|_| Error::invalid("Array too large"))?;
            if bytes {
                let byte = unsigned(item, u8::MAX)?;
                // SAFETY: `psa` is a live VT_UI1 vector longer than `index`;
                // SafeArrayPutElement copies the byte.
                unsafe { SafeArrayPutElement(psa, &raw const index, ptr::from_ref(&byte).cast()) }?;
            } else {
                let value = if item.get("$object").is_some() || item.get("$variant").is_some() {
                    self.input(item, &variant_type())?
                } else {
                    self.dynamic_input(item)?
                };
                // SAFETY: `psa` is a live VT_VARIANT vector longer than
                // `index`; SafeArrayPutElement copies the VARIANT.
                unsafe {
                    SafeArrayPutElement(psa, &raw const index, ptr::from_ref(&value).cast())
                }?;
            }
        }
        Ok(result)
    }
    fn output(&mut self, v: &VARIANT, expected: Option<&Type>, depth: usize) -> Result<Value> {
        if depth > 16 {
            return Err(Error::new("result_too_deep", "COM result exceeds depth 16"));
        }
        if v.vt().0 & VT_BYREF.0 != 0 {
            let mut copied = VARIANT::default();
            // SAFETY: both pointers are valid VARIANTs; `copied` receives an
            // owned copy of the referenced value.
            unsafe { VariantCopyInd(&raw mut copied, ptr::from_ref(v)) }?;
            return self.output(&copied, expected, depth + 1);
        }
        if v.vt().0 & VT_ARRAY.0 != 0 {
            return self.array_output(v, depth);
        }
        let d = payload(v);
        // In each arm below, the union field read is the one the matched
        // VARTYPE stores, so every read is of initialized data of that type.
        match v.vt() {
            VT_EMPTY | VT_NULL => Ok(Value::Null),
            // SAFETY: VT_BSTR stores `bstrVal`.
            VT_BSTR => Ok(json!(unsafe { &d.bstrVal }.to_string())),
            // SAFETY: VT_BOOL stores `boolVal`.
            VT_BOOL => Ok(json!(unsafe { d.boolVal }.0 != 0)),
            // SAFETY: VT_I1 stores `cVal`.
            VT_I1 => Ok(json!(unsafe { d.cVal })),
            // SAFETY: VT_UI1 stores `bVal`.
            VT_UI1 => Ok(json!(unsafe { d.bVal })),
            // SAFETY: VT_I2 stores `iVal`.
            VT_I2 => Ok(json!(unsafe { d.iVal })),
            // SAFETY: VT_UI2 stores `uiVal`.
            VT_UI2 => Ok(json!(unsafe { d.uiVal })),
            // SAFETY: VT_I4 and VT_INT store `lVal`.
            VT_I4 | VT_INT => Ok(json!(unsafe { d.lVal })),
            // SAFETY: VT_UI4 and VT_UINT store `ulVal`.
            VT_UI4 | VT_UINT => Ok(json!(unsafe { d.ulVal })),
            // SAFETY: VT_I8 stores `llVal`.
            VT_I8 => Ok(json!(unsafe { d.llVal })),
            // SAFETY: VT_UI8 stores `ullVal`.
            VT_UI8 => Ok(json!(unsafe { d.ullVal })),
            // SAFETY: VT_R4 stores `fltVal`.
            VT_R4 => finite(f64::from(unsafe { d.fltVal })),
            // SAFETY: VT_R8 stores `dblVal`.
            VT_R8 => finite(unsafe { d.dblVal }),
            // SAFETY: VT_DATE stores `date`.
            VT_DATE => Ok(json!({"$variant":"date","value":unsafe { d.date }})),
            VT_ERROR => {
                // SAFETY: VT_ERROR stores `scode`.
                let scode = unsafe { d.scode };
                Ok(json!({"$variant":"error","hresult":format!("0x{:08X}",scode.cast_unsigned())}))
            }
            VT_DISPATCH => {
                // SAFETY: VT_DISPATCH stores `pdispVal`, null or a live reference.
                if let Some(obj) = unsafe { &*d.pdispVal } {
                    self.store_result(obj.cast()?, expected)
                } else {
                    Ok(Value::Null)
                }
            }
            VT_UNKNOWN => {
                // SAFETY: VT_UNKNOWN stores `punkVal`, null or a live reference.
                if let Some(obj) = unsafe { &*d.punkVal } {
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
        let iid_null = GUID::zeroed();
        let params = DISPPARAMS::default();
        let mut value = VARIANT::default();
        // SAFETY: an empty DISPPARAMS for a property get; `value` is a valid
        // out-parameter; the DISPID comes from the catalog.
        unsafe {
            app.Invoke(
                op.dispid,
                &raw const iid_null,
                0,
                DISPATCH_PROPERTYGET,
                &raw const params,
                Some(&raw mut value),
                None,
                None,
            )
        }?;
        Ok(value)
    }
    fn array_output(&mut self, v: &VARIANT, depth: usize) -> Result<Value> {
        // SAFETY: the caller checked VT_ARRAY, which stores `parray`.
        let psa = unsafe { payload(v).parray };
        if psa.is_null() {
            return Ok(json!([]));
        }
        // SAFETY: `psa` is a non-null SAFEARRAY owned by `v`.
        let dims = unsafe { SafeArrayGetDim(psa) };
        if dims == 0 || dims > 4 {
            return Err(Error::unsupported(
                "SAFEARRAY with 1 to 4 dimensions are supported",
            ));
        }
        let mut bounds = Vec::new();
        let mut total = 1usize;
        for i in 1..=dims {
            // SAFETY: `psa` is live and `i` is one of its dimensions.
            let low = unsafe { SafeArrayGetLBound(psa, i) }?;
            // SAFETY: as above.
            let high = unsafe { SafeArrayGetUBound(psa, i) }?;
            let len = usize::try_from(i64::from(high) - i64::from(low) + 1).unwrap_or(0);
            total = total.saturating_mul(len);
            bounds.push((low, high));
        }
        if total > 1_000_000 {
            return Err(Error::new(
                "result_too_large",
                "SAFEARRAY exceeds 1,000,000 elements",
            ));
        }
        // SAFETY: `psa` is live.
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
        let mut indices = vec![0; bounds.len()];
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
                continue;
            }
            let mut value = VARIANT::default();
            if vt == VT_VARIANT {
                // SAFETY: `indices` is within the bounds of the live `psa`,
                // whose elements are VARIANTs; `value` receives a copy.
                unsafe {
                    SafeArrayGetElement(psa, indices.as_ptr(), ptr::from_mut(&mut value).cast())
                }?;
            } else {
                // SAFETY: the tag is set to the array's element type and the
                // payload is filled with one element of that type just below.
                let tagged = unsafe { fields(&mut value) };
                tagged.vt = vt;
                // SAFETY: `indices` is within the bounds of the live `psa`;
                // the payload has room for one element of type `vt`.
                unsafe {
                    SafeArrayGetElement(psa, indices.as_ptr(), (&raw mut tagged.Anonymous).cast())
                }?;
            }
            values.push(self.output(&value, None, depth + 1)?);
        }
        Ok(Value::Array(values))
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.release_handles(&self.handles_since(0));
    }
}

/// The tagged half of a VARIANT, for setting its type and payload.
///
/// # Safety
///
/// The caller must leave `vt` consistent with the payload, because dropping a
/// VARIANT calls `VariantClear`, which frees the payload according to `vt`.
unsafe fn fields(v: &mut VARIANT) -> &mut VARIANT_0_0 {
    // SAFETY: every bit pattern is a valid VARIANT_0_0, and this module never
    // builds VT_DECIMAL, the only type that uses the other union field.
    unsafe { &mut v.Anonymous.Anonymous }
}
/// The payload of a VARIANT. Reading one of its fields is only meaningful for
/// the field that `v.vt()` selects.
fn payload(v: &VARIANT) -> &VARIANT_0_0_0 {
    // SAFETY: a shared view of plain bytes; every bit pattern is a valid
    // VARIANT_0_0, and callers read only the field that `vt` selects.
    unsafe { &v.Anonymous.Anonymous.Anonymous }
}

fn invoke_flags(op: &Operation) -> Result<DISPATCH_FLAGS> {
    match op.invoke_kind.as_str() {
        "INVOKE_FUNC" => Ok(DISPATCH_METHOD),
        "INVOKE_PROPERTYGET" => Ok(DISPATCH_PROPERTYGET),
        "INVOKE_PROPERTYPUT" => Ok(DISPATCH_PROPERTYPUT),
        "INVOKE_PROPERTYPUTREF" => Ok(DISPATCH_PROPERTYPUTREF),
        _ => Err(Error::unsupported("Unrecognized invocation kind")),
    }
}
/// Calls `IDispatch::Invoke` with arguments already in reverse order, and
/// turns a failure, with its EXCEPINFO, into an [`Error`].
fn invoke(
    dispatch: &IDispatch,
    op: &Operation,
    flags: DISPATCH_FLAGS,
    args: &mut [VARIANT],
) -> Result<VARIANT> {
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
        rgdispidNamedArgs: if put { &raw mut named } else { ptr::null_mut() },
        cArgs: u32::try_from(args.len()).map_err(|_| Error::invalid("Too many arguments"))?,
        cNamedArgs: u32::from(put),
    };
    let iid_null = GUID::zeroed();
    let mut result = VARIANT::default();
    let mut ex = EXCEPINFO::default();
    let mut arg_error = u32::MAX;
    // SAFETY: `params` points into `args` and `named`, which outlive the call;
    // `result`, `ex` and `arg_error` are valid out-parameters; the DISPID
    // comes from the catalog entry of this interface.
    let invocation = unsafe {
        dispatch.Invoke(
            op.dispid,
            &raw const iid_null,
            0,
            flags,
            &raw const params,
            Some(&raw mut result),
            Some(&raw mut ex),
            Some(&raw mut arg_error),
        )
    };
    if let Some(fill) = ex.pfnDeferredFillIn
        && invocation.is_err()
    {
        // SAFETY: the server supplied this callback to complete `ex`.
        let _ = unsafe { fill(&raw mut ex) };
    }
    // SAFETY: each EXCEPINFO string is null or owned by `ex`, and is taken
    // exactly once; EXCEPINFO does not free them on drop.
    let source = unsafe { ManuallyDrop::take(&mut ex.bstrSource) }.to_string();
    // SAFETY: as above.
    let description = unsafe { ManuallyDrop::take(&mut ex.bstrDescription) }.to_string();
    // SAFETY: as above.
    let help = unsafe { ManuallyDrop::take(&mut ex.bstrHelpFile) }.to_string();
    let Err(e) = invocation else {
        return Ok(result);
    };
    let mut error = Error::from(e);
    if !description.is_empty() {
        error.message = description;
    }
    // Invoke reports the failing argument by its reverse position.
    let argument = usize::try_from(arg_error)
        .ok()
        .and_then(|n| n.checked_add(1))
        .and_then(|n| op.parameters.len().checked_sub(n))
        .and_then(|i| op.parameters.get(i))
        .map(|p| &p.name);
    error.details = Some(
        json!({"api_id":op.id,"source":source,"exception_scode":format!("0x{:08X}",ex.scode.cast_unsigned()),"help_file":help,
        "argument":argument}),
    );
    if opens_application(op) {
        error = error.with_activation_guidance();
    }
    Err(error)
}

fn parse_guid(value: &str) -> Result<GUID> {
    GUID::try_from(value.trim_matches(['{', '}'])).map_err(|_| Error::invalid("Invalid GUID"))
}
fn query_dispatch(obj: &IUnknown, iid: &str) -> Result<IDispatch> {
    let guid = parse_guid(iid)?;
    let mut interface = ptr::null_mut();
    // SAFETY: `guid` and `interface` are valid for the call; on success
    // `interface` holds one reference that is handed to `from_raw` below.
    unsafe { obj.query(&raw const guid, &raw mut interface) }.ok()?;
    if interface.is_null() {
        return Err(Error::new(
            "null_interface",
            "QueryInterface returned a null pointer",
        ));
    }
    // SAFETY: called only for catalogued dual/dispatch interfaces, so IDispatch
    // is the base vtable of the returned interface, and the reference is ours.
    // Do not QueryInterface back to the object's default IDispatch, which may
    // hide the secondary members.
    Ok(unsafe { IDispatch::from_raw(interface) })
}
fn dispatch_variant(obj: IDispatch) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: the tag and the payload are set together; the VARIANT takes
    // ownership of the reference and releases it when dropped.
    let tagged = unsafe { fields(&mut v) };
    tagged.vt = VT_DISPATCH;
    tagged.Anonymous.pdispVal = ManuallyDrop::new(Some(obj));
    v
}
fn null() -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: VT_NULL has no payload.
    unsafe { fields(&mut v) }.vt = VT_NULL;
    v
}
/// The VARIANT that COM uses for an omitted optional argument.
fn missing() -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: the tag and the payload are set together; VT_ERROR stores
    // `scode`, which owns nothing.
    let tagged = unsafe { fields(&mut v) };
    tagged.vt = VT_ERROR;
    tagged.Anonymous.scode = DISP_E_PARAMNOTFOUND.0;
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
/// Converts a JSON scalar to the VARIANT of a resolved scalar COM type.
fn scalar_input(value: &Value, resolved: &Type) -> Result<VARIANT> {
    match resolved.kind.as_str() {
        "VT_BSTR" => value
            .as_str()
            .map(VARIANT::from)
            .ok_or_else(|| Error::invalid("Expected a BSTR string")),
        "VT_BOOL" => value
            .as_bool()
            .map(VARIANT::from)
            .ok_or_else(|| Error::invalid("Expected a boolean")),
        "VT_I4" | "VT_INT" => signed(value, i32::MIN, i32::MAX).map(VARIANT::from),
        "VT_I2" => signed(value, i16::MIN, i16::MAX).map(VARIANT::from),
        "VT_I1" => signed(value, i8::MIN, i8::MAX).map(VARIANT::from),
        "VT_UI4" | "VT_UINT" => unsigned(value, u32::MAX).map(VARIANT::from),
        "VT_UI2" => unsigned(value, u16::MAX).map(VARIANT::from),
        "VT_UI1" => unsigned(value, u8::MAX).map(VARIANT::from),
        "VT_I8" => signed(value, i64::MIN, i64::MAX).map(VARIANT::from),
        "VT_UI8" => unsigned(value, u64::MAX).map(VARIANT::from),
        "VT_R4" => number(value).and_then(single),
        "VT_R8" => number(value).map(VARIANT::from),
        "VT_DISPATCH" | "VT_UNKNOWN" | "VT_USERDEFINED"
            if value.is_null() || value.as_i64() == Some(0) =>
        {
            empty_typed(resolved)
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
/// A JSON integer that fits `T`, whose range `min..=max` the error reports.
fn signed<T>(v: &Value, min: T, max: T) -> Result<T>
where
    T: TryFrom<i64> + Into<i64> + Copy,
{
    v.as_i64().and_then(|n| T::try_from(n).ok()).ok_or_else(|| {
        Error::invalid(format!(
            "Expected an integer between {} and {}",
            min.into(),
            max.into()
        ))
    })
}
/// A JSON integer that fits `T`, whose largest value is `max`.
fn unsigned<T>(v: &Value, max: T) -> Result<T>
where
    T: TryFrom<u64> + Into<u64> + Copy,
{
    v.as_u64()
        .and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| Error::invalid(format!("Expected an integer between 0 and {}", max.into())))
}
fn number(v: &Value) -> Result<f64> {
    v.as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| Error::invalid("Expected a finite number"))
}
/// Narrows a finite number to FLOAT, refusing values outside its range.
fn single(v: f64) -> Result<VARIANT> {
    if v.abs() > f64::from(f32::MAX) {
        return Err(Error::invalid("FLOAT out of range"));
    }
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "the range is checked above; rounding to the nearest FLOAT is intended"
    )]
    let single = v as f32;
    Ok(VARIANT::from(single))
}
fn finite(v: f64) -> Result<Value> {
    if v.is_finite() {
        Ok(json!(v))
    } else {
        Err(Error::unsupported("COM returned NaN or infinity"))
    }
}
/// An output slot of type `t` with a zero payload, which `Invoke` overwrites.
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
    // SAFETY: a zero payload is a valid value of each tag above: a number,
    // or a null BSTR or interface pointer.
    unsafe { fields(&mut v) }.vt = vt;
    Ok(v)
}
/// A BYREF VARIANT that points at `slot`, which must outlive it. Clearing a
/// BYREF VARIANT never frees what it points at.
fn by_ref(slot: &mut VARIANT, t: &Type) -> VARIANT {
    let mut v = VARIANT::default();
    if t.kind == "VT_VARIANT" {
        // SAFETY: VT_BYREF|VT_VARIANT stores a pointer to a VARIANT.
        let tagged = unsafe { fields(&mut v) };
        tagged.vt = VARENUM(VT_BYREF.0 | VT_VARIANT.0);
        tagged.Anonymous.pvarVal = ptr::from_mut(slot);
    } else {
        let vt = slot.vt();
        // SAFETY: only a pointer to the payload is taken; the tag of `slot`
        // is left unchanged.
        let target = &raw mut unsafe { fields(slot) }.Anonymous;
        // SAFETY: VT_BYREF|vt stores a pointer to a payload of type `vt`,
        // which `target` is.
        let tagged = unsafe { fields(&mut v) };
        tagged.vt = VARENUM(VT_BYREF.0 | vt.0);
        tagged.Anonymous.byref = target.cast();
    }
    v
}
/// Looks up `sub_block` in a version resource; returns the value and its
/// length, whose unit depends on the block.
fn ver_query(data: &[u64], sub_block: PCWSTR) -> Option<(*mut c_void, u32)> {
    let mut info: *mut c_void = ptr::null_mut();
    let mut length = 0;
    // SAFETY: `data` holds a resource written by GetFileVersionInfoW,
    // `sub_block` is NUL-terminated, and both out-pointers are valid.
    let found = unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            sub_block,
            &raw mut info,
            &raw mut length,
        )
    }
    .as_bool();
    (found && !info.is_null()).then_some((info, length))
}
fn file_version(path: &Path) -> Result<String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None) };
    if size == 0 {
        return Err(Error::new(
            "installation_not_found",
            format!("Could not read the version of {}", path.display()),
        ));
    }
    let bytes = usize::try_from(size).expect("a u32 fits in usize on Windows");
    let mut data = vec![0u64; bytes.div_ceil(8)];
    // SAFETY: `data` has at least `size` writable, 8-byte aligned bytes.
    unsafe { GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, data.as_mut_ptr().cast()) }?;
    // Schneider's fixed binary version is an internal build identifier
    // (22.2.12.0 for this 15.1 installation). The localized ProductVersion
    // string carries the public product release (15, 1, 0, 0).
    let mut translations = vec![(0x0409u16, 0x04b0u16), (0x0409u16, 0x04e4u16)];
    if let Some((info, length)) =
        ver_query(&data, ::windows::core::w!("\\VarFileInfo\\Translation"))
    {
        let units = usize::try_from(length).expect("a u32 fits in usize on Windows") / 2;
        // SAFETY: the Translation value is `length` bytes of WORD pairs
        // inside `data`, which outlives `values`.
        let values = unsafe { std::slice::from_raw_parts(info.cast::<u16>(), units) };
        translations.splice(0..0, values.as_chunks::<2>().0.iter().map(|v| (v[0], v[1])));
    }
    for (language, page) in translations {
        let query: Vec<u16> = format!("\\StringFileInfo\\{language:04x}{page:04x}\\ProductVersion")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let Some((info, length)) = ver_query(&data, PCWSTR(query.as_ptr())) else {
            continue;
        };
        if length == 0 {
            continue;
        }
        let units = usize::try_from(length).expect("a u32 fits in usize on Windows");
        // SAFETY: a string value is `length` UTF-16 units inside `data`.
        let value = String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(info.cast::<u16>(), units)
        });
        let numbers: Vec<_> = value
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .collect();
        if numbers.len() >= 2 {
            return Ok(numbers.join("."));
        }
    }
    let fixed = ver_query(&data, ::windows::core::w!("\\")).filter(|(_, length)| {
        usize::try_from(*length).is_ok_and(|l| l >= size_of::<VS_FIXEDFILEINFO>())
    });
    let Some((info, _)) = fixed else {
        return Err(Error::new(
            "version_unavailable",
            "File without VS_FIXEDFILEINFO",
        ));
    };
    // SAFETY: the root block is a complete, DWORD-aligned VS_FIXEDFILEINFO
    // inside `data`, which outlives `info`.
    let info = unsafe { &*info.cast::<VS_FIXEDFILEINFO>() };
    Ok(format!(
        "{}.{}.{}.{}",
        info.dwProductVersionMS >> 16,
        info.dwProductVersionMS & 65535,
        info.dwProductVersionLS >> 16,
        info.dwProductVersionLS & 65535
    ))
}
