//! The MCP tool surface: argument schemas and one handler per tool.
use crate::{
    catalog::Catalog,
    error::{Error, Result as CeResult},
    runtime::{Action, Runtime},
    screens::{self, ScreensExport, ScreensImport},
    versions::Profile,
};
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// Name reported to MCP clients and printed by `--version`.
pub const SERVER_NAME: &str = "controlexpert-com-mcp";
/// MCP SDK reported to clients; keep in sync with the `rmcp` pin in Cargo.toml.
pub const SDK: &str = "RMCP 3.5.1";
const DISCLAIMER: &str = "Unofficial project; not affiliated with, endorsed by or supported by Schneider Electric. EcoStruxure and Control Expert are trademarks of their respective owners.";

/// The MCP server: the immutable catalog plus the COM runtime.
#[derive(Clone, Debug)]
pub struct Server {
    pub catalog: Arc<Catalog>,
    pub runtime: Runtime,
}
impl Server {
    pub fn new(version: &str) -> CeResult<Self> {
        Self::with_options(version, Duration::from_secs(10))
    }
    pub fn with_options(version: &str, slow_call_after: Duration) -> CeResult<Self> {
        let catalog = Arc::new(Catalog::load(Profile::select(version)?)?);
        let runtime = Runtime::start_with_options(Arc::clone(&catalog), slow_call_after)?;
        Ok(Self { catalog, runtime })
    }
    pub(crate) async fn invoke(
        &self,
        handle: String,
        api_id: &str,
        args: BTreeMap<String, Value>,
    ) -> CeResult<Value> {
        let operation = self.catalog.prepare(api_id, &args)?;
        self.runtime
            .execute(Action::Call {
                handle,
                operation: Box::new(operation),
                arguments: args,
            })
            .await
    }
}
fn result(r: CeResult<Value>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => {
            let payload = json!({"error":e});
            let mut r = CallToolResult::error(vec![ContentBlock::text(payload.to_string())]);
            r.structured_content = Some(payload);
            r
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// Words from the name or description; empty lists every operation.
    #[serde(default)]
    pub query: String,
    /// Full interface ID, for example UnityProServer.IVariables.
    pub interface: Option<String>,
    #[serde(default)]
    pub offset: usize,
    /// Between 1 and 100; defaults to 20.
    pub limit: Option<usize>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Id {
    pub id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Handle {
    pub handle: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::doc_markdown,
    reason = "field docs become the MCP tool schema shown to agents"
)]
pub struct Call {
    /// Handle received as $object; only valid in the current session.
    pub handle: String,
    /// Exact ID returned by ce_api_search, including :method, :get, :put or :putref.
    pub api_id: String,
    /// Arguments by exact catalog name. Do not include out parameters.
    /// For COM references: {"$object":"handle"}. Enums accept the symbolic name.
    /// For VARIANT: JSON scalar, array, $object or {"$variant":"bytes|array|empty|null|missing|i2|i4|ui4|i8|r8","value":...}.
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Create {
    /// Catalog coclass; for example UnityProBroker.PServerBroker.
    pub class_id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub handle: String,
    /// Full ID; for example PSRDtmLib.IProject3.
    pub interface: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Session {
    /// Absolute path of a project to open. If omitted, creates an empty application.
    pub file_path: Option<String>,
}
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSection {
    Libraries,
    Interfaces,
    Coclasses,
    Enumerations,
}
impl CatalogSection {
    fn key(self) -> &'static str {
        match self {
            Self::Libraries => "libraries",
            Self::Interfaces => "interfaces",
            Self::Coclasses => "coclasses",
            Self::Enumerations => "enumerations",
        }
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogList {
    pub section: CatalogSection,
    /// Case-insensitive text filter; empty lists the whole section.
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub offset: usize,
    /// Between 1 and 100; defaults to 20.
    pub limit: Option<usize>,
}

#[rmcp::tool_router]
impl Server {
    #[rmcp::tool(
        description = "Lists catalog libraries, interfaces, coclasses or enumerations. Discovers IDs without reading the whole file.",
        annotations(read_only_hint = true)
    )]
    fn ce_catalog_list(&self, Parameters(p): Parameters<CatalogList>) -> CallToolResult {
        let limit = p.limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return result(Err(Error::invalid("limit must be between 1 and 100")));
        }
        let query = p.query.to_lowercase();
        let items: Vec<_> = self.catalog.document[p.section.key()]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v.to_string().to_lowercase().contains(&query))
            .collect();
        result(Ok(
            json!({"total":items.len(),"offset":p.offset,"limit":limit,"items":items.iter().skip(p.offset).take(limit).map(|v|json!({"id":v["id"],"name":v["name"],"description":if v["description"].is_null() {&v["original_description"]} else {&v["description"]},"category":v["category"],"guid":v["guid"],"clsid":v["clsid"],"registration":v["registration"]})).collect::<Vec<_>>()}),
        ))
    }
    #[rmcp::tool(
        description = "Supported version, coverage, execution policy and limits of the MCP. Offline query; does not open Control Expert.",
        annotations(read_only_hint = true)
    )]
    fn ce_info(&self) -> CallToolResult {
        let callable = self
            .catalog
            .operations
            .values()
            .filter(|o| self.catalog.invocation_reason(o).is_none())
            .count();
        result(Ok(
            json!({"server":SERVER_NAME,"server_version":env!("CARGO_PKG_VERSION"),"sdk":SDK,"disclaimer":DISCLAIMER,
            "lifecycle":{"atomic_session_start":true,"application_gate":true,"application_ready":self.runtime.status()["application_ready"],"orphan_reference_cleanup":true,"graceful_sta_shutdown":true,"status_tool":"ce_runtime_status","diagnostics":"JSON on stderr, without argument values","slow_call_after_ms":self.runtime.status()["slow_call_after_ms"]},
            "supported_versions":["15.1"],"selected_version":self.catalog.profile.version,"catalog":self.catalog.document["summary"],
            "execution_policy":self.runtime.execution_policy(),
            "dispatch_operations":callable,"limitations":["Does not implement COM event subscriptions or calls to vtable interfaces without IDispatch.",
                "VARIANT with specific contracts may need explicit encoding; unsupported types are rejected.",
                "Presence in the catalog does not guarantee a supported public API. Private interfaces keep their classification.",
                "Cancelling an MCP call does not interrupt a COM operation that already started. Nothing is retried automatically.",
                "ce_probe checks the installation and DLL version; compatible=true does not guarantee that Control Expert can start."],
            "workflow":["ce_api_search","ce_api_describe","ce_session_start or ce_object_create","ce_api_call","ce_object_release"],
            "coverage_note":"Signatures come from the catalog; object availability and preconditions are checked when invoking."}),
        ))
    }
    #[rmcp::tool(
        description = "Searches methods, properties and events in the 15.1 catalog. Paginated results with signature, category and transport availability.",
        annotations(read_only_hint = true)
    )]
    fn ce_api_search(&self, Parameters(p): Parameters<Search>) -> CallToolResult {
        let limit = p.limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return result(Err(Error::invalid("limit must be between 1 and 100")));
        }
        result(Ok(self.catalog.search(
            &p.query,
            p.interface.as_deref(),
            p.offset,
            limit,
        )))
    }
    #[rmcp::tool(
        description = "Full contract of an API: description, inputs, outputs, optionals, DISPID, type library location and execution policy.",
        annotations(read_only_hint = true)
    )]
    fn ce_api_describe(&self, Parameters(p): Parameters<Id>) -> CallToolResult {
        result(self.catalog.operation(&p.id).map(|op|json!({"contract":op.original,"invocable":self.catalog.invocation_reason(op).is_none(),"limitation":self.catalog.invocation_reason(op),"execution_policy":self.runtime.operation_policy(op)})))
    }
    #[rmcp::tool(
        description = "Describes an interface by ID: GUID, members, location and indicative access path.",
        annotations(read_only_hint = true)
    )]
    fn ce_interface_describe(&self, Parameters(p): Parameters<Id>) -> CallToolResult {
        result(self.catalog.interface(&p.id).map(|i| i.original.clone()))
    }
    #[rmcp::tool(
        description = "Exact values of an enumeration. Use the library.alias ID, for example UnityProServer.epsrLanguage.",
        annotations(read_only_hint = true)
    )]
    fn ce_enum_describe(&self, Parameters(p): Parameters<Id>) -> CallToolResult {
        result(
            self.catalog.enums.get(&p.id).cloned().ok_or_else(|| {
                Error::new("enum_not_found", format!("Unknown enumeration: {}", p.id))
            }),
        )
    }
    #[rmcp::tool(
        description = "Checks the installation and version of the Control Expert 15.1 pserver.dll. compatible=true only confirms that version; it does not test that the COM server starts. Opens no project and connects to no PLC.",
        annotations(read_only_hint = true)
    )]
    async fn ce_probe(&self) -> CallToolResult {
        result(self.runtime.execute(Action::Probe).await)
    }
    #[rmcp::tool(
        description = "Immediate runtime status: phase and duration of the active COM operation, queue and shutdown. Does not call COM and answers even while an open is blocked. A long wait does not cancel the operation.",
        annotations(read_only_hint = true)
    )]
    fn ce_runtime_status(&self) -> CallToolResult {
        result(Ok(self.runtime.status()))
    }
    #[rmcp::tool(
        description = "Always starts a headless Control Expert 15.1 session through the broker and verifies IsVisible=0. With file_path it opens the project; without a path it creates an empty application. To watch the changes, use ce_viewer_open afterwards (read-only, same session). If it takes long, query ce_runtime_status instead of retrying.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn ce_session_start(&self, Parameters(p): Parameters<Session>) -> CallToolResult {
        let r = async {
            if p.file_path
                .as_ref()
                .is_some_and(|p| !std::path::Path::new(p).is_absolute())
            {
                return Err(Error::invalid("file_path must be absolute"));
            }
            self.runtime
                .execute(Action::SessionStart {
                    file_path: p.file_path,
                })
                .await
        }
        .await;
        result(r)
    }
    #[rmcp::tool(
        description = "Opens, on request, a read-only view of the agent's own headless session to watch its changes. handle must be application.$object. Uses DisplayStart(psrHmiReadOnly) without a command line; it does not load a copy of the file or enable editing in the UI.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn ce_viewer_open(&self, Parameters(p): Parameters<Handle>) -> CallToolResult {
        result(
            self.runtime
                .execute(Action::ViewerOpen { handle: p.handle })
                .await,
        )
    }
    #[rmcp::tool(
        description = "Creates a catalog coclass by ID and returns a COM handle. Without a validated IApplication only PServerBroker and PServerBrokerContext are allowed; others return application_not_ready. Coclasses of restricted categories (security, private, internal, tests) need their unlock. To open Control Expert use ce_session_start.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn ce_object_create(&self, Parameters(p): Parameters<Create>) -> CallToolResult {
        result(
            self.runtime
                .execute(Action::Create {
                    class_id: p.class_id,
                })
                .await,
        )
    }
    #[rmcp::tool(
        description = "Invokes a catalogued API and may modify the project or a controller. Requires a validated IApplication, except broker startup and diagnostics. Blocked unless unlocked at MCP startup: Build/BuildAll (CE_MCP_ALLOW_BUILD), connections to a PLC (CE_MCP_ALLOW_PLC_CONNECTION), transfers and PLC commands to a non-simulator target (CE_MCP_ALLOW_PLC_WRITE), security administration (CE_MCP_ALLOW_SECURITY_ADMIN) and private/internal/test interfaces (CE_MCP_ALLOW_PRIVATE_APIS). Process termination is always blocked. Check ce_info and ce_api_describe for the effective policy.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn ce_api_call(&self, Parameters(p): Parameters<Call>) -> CallToolResult {
        result(self.invoke(p.handle, &p.api_id, p.arguments).await)
    }
    #[rmcp::tool(
        description = "Exports the operator screens of a top-level functional module to an .XFM file (IFctModule.Export) and returns their structure: screens, object counts and linked variables. importable says whether ce_screens_import can replace the module with that file. Does not modify the project.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn ce_screens_export(&self, Parameters(p): Parameters<ScreensExport>) -> CallToolResult {
        result(screens::export(self, p).await)
    }
    #[rmcp::tool(
        description = "Replaces the operator screens of a top-level functional module with an edited .XFM. Importing over a module never replaces its screens, so this backs up the module, checks that it and the file hold only operator screens, deletes the module (IFctModule.Delete) and imports the file (IFctModules.ImportWithStrategy, Overwrite); on import failure it restores the backup. Then it re-exports and verifies the screens. Needs write access; does not save unless save=true. Use dry_run=true to only check.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn ce_screens_import(&self, Parameters(p): Parameters<ScreensImport>) -> CallToolResult {
        result(screens::import(self, p).await)
    }
    #[rmcp::tool(
        description = "Gets another catalogued IDispatch interface of the same object and returns a new handle. Requires a validated and retained IApplication, except broker startup and diagnostic interfaces. Useful for IConfiguration2 and IProject3.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn ce_object_query(&self, Parameters(p): Parameters<Query>) -> CallToolResult {
        result(
            self.runtime
                .execute(Action::Query {
                    handle: p.handle,
                    interface: p.interface,
                })
                .await,
        )
    }
    #[rmcp::tool(
        description = "Lists the handles retained by this process; does not query the project or the PLC.",
        annotations(read_only_hint = true)
    )]
    async fn ce_objects(&self) -> CallToolResult {
        result(self.runtime.execute(Action::Objects).await)
    }
    #[rmcp::tool(
        description = "Releases one COM reference by handle. Does not save or explicitly close projects; when the last reference is released, Control Expert decides the session lifecycle and unsaved changes may be lost.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn ce_object_release(&self, Parameters(p): Parameters<Handle>) -> CallToolResult {
        result(
            self.runtime
                .execute(Action::Release { handle: p.handle })
                .await,
        )
    }
    #[rmcp::tool(
        description = "Releases every handle of this MCP client. Does not save projects; unsaved changes may be lost when Control Expert ends the session. Previous handles stop being valid.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn ce_reset(&self) -> CallToolResult {
        result(self.runtime.execute(Action::Reset).await)
    }
}
#[rmcp::tool_handler]
#[expect(
    clippy::unused_async_trait_impl,
    reason = "rmcp::tool_handler generates the async ServerHandler methods"
)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(format!("Control Expert 15.1 COM/UDE automation. {DISCLAIMER} Search and describe APIs before invoking them. Use only catalog IDs and parameters. $object handles are session references: pass them as {{\"$object\":\"...\"}} and release them when done. Use ce_reset in a finally block to release references even if a call fails; an error does not remove references already returned. Out parameters are assigned automatically; optionals may be omitted. Check ce_enum_describe for symbolic values: if the parameter is a VARIANT, send the numeric enum value, do not assume a string is converted. Do not run event callbacks as commands. ce_api_call can modify the project or a PLC: choose the operation according to the user's intent. Catalog paths are indicative; obtain real objects through successive calls. If an open takes long, query ce_runtime_status; do not start another open or kill the process because of a client timeout. A COM operation that already started is neither cancelled nor retried automatically. A normal close of stdin waits for it to finish and releases COM. Operator screens have no COM API: use ce_screens_export, edit the .XFM and apply it with ce_screens_import (dry_run first) on a functional module that holds only screens."))
    }
}
