//! Operator screens have no COM API: they only travel as `<IOScreen>` inside
//! XFM files. Importing over an existing functional module never replaces its
//! screens (it adds renamed copies), so a replacement deletes the module first.
//! `IFctModule.Delete` removes everything attached to the module, which is why
//! the import only accepts modules whose export holds nothing but screens.
use crate::{
    error::{Error, Result},
    runtime::Action,
    server::Server,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const PROJECT: &str = "UnityProServer.IApplication.Project:get";
const FCT_MODULES: &str = "UnityProServer.IProject.FctModules:get";
const ITEM: &str = "UnityProServer.IFctModules.Item:method";
const IMPORT: &str = "UnityProServer.IFctModules.ImportWithStrategy:method";
const EXPORT: &str = "UnityProServer.IFctModule.Export:method";
const DELETE: &str = "UnityProServer.IFctModule.Delete:method";
const HAS_CHILDREN: &str = "UnityProServer.IFctModule.HasChildren:get";
const PROG: &str = "UnityProServer.IFctModule.ProgFctModule:get";
const SECTIONS: &str = "UnityProServer.IProgFctModule.GetListOfSectionsAttached:method";
const OBJECTS: &str = "UnityProServer.IProgFctModule.GetListOfObjectsAttached:method";
const SAVE: &str = "UnityProServer.IProject.Save:method";
/// Creation and modification dates and the module identity change on every
/// import; they are not part of what the caller edits.
const VOLATILE: [&str; 4] = ["FMId", "location", "creationDate", "modificationDate"];

/// Arguments of `ce_screens_export`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::doc_markdown,
    reason = "field docs become the MCP tool schema shown to agents"
)]
pub struct ScreensExport {
    /// application.$object returned by ce_session_start.
    pub handle: String,
    /// project.$object to use instead of IApplication.Project.
    pub project: Option<String>,
    /// Name of a top-level functional module.
    pub module: String,
    /// Absolute path of the .XFM file to write.
    pub file_path: String,
    /// Also return every object with its raw description and variables.
    #[serde(default)]
    pub include_objects: bool,
}
/// Arguments of `ce_screens_import`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::doc_markdown,
    reason = "field docs become the MCP tool schema shown to agents"
)]
pub struct ScreensImport {
    /// application.$object returned by ce_session_start.
    pub handle: String,
    /// project.$object with write access to use instead of IApplication.Project(lWriteAccessToken=1). Pass it when this client already holds a write-access IProject: Control Expert reserves write access to a single reference.
    pub project: Option<String>,
    /// Name of the top-level functional module to replace; it must hold only operator screens.
    pub module: String,
    /// Absolute path of the edited .XFM, usually written by ce_screens_export.
    pub file_path: String,
    /// Absolute .XFM path for the export of the current module taken before deleting it. Defaults to the temp folder.
    pub backup_path: Option<String>,
    /// Run every check and write the backup without deleting or importing.
    #[serde(default)]
    pub dry_run: bool,
    /// Call IProject.Save after a verified import.
    #[serde(default)]
    pub save: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub name: String,
    pub fm_id: String,
    /// None for modules directly under the project root.
    pub parent: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// varAnim or varPilot.
    pub role: String,
    pub attrs: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    pub object_id: String,
    pub description: String,
    pub links: Vec<Link>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub family: Option<String>,
    pub attrs: BTreeMap<String, String>,
    pub comment: Option<String>,
    pub objects: Vec<Object>,
}
impl Screen {
    pub fn name(&self) -> &str {
        self.attrs.get("name").map_or("", String::as_str)
    }
    fn normalized(&self) -> Self {
        let mut s = self.clone();
        s.attrs.retain(|k, _| !VOLATILE.contains(&k.as_str()));
        s
    }
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Xfm {
    pub modules: Vec<Module>,
    pub screens: Vec<Screen>,
    /// (name, `FMId`) of each family.
    pub families: Vec<(String, Option<String>)>,
    pub messages: Vec<BTreeMap<String, String>>,
    /// Paths of elements that are not operator screens or module descriptions.
    pub foreign: Vec<String>,
}

/// How much of each screen [`summary`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Object counts and the variables each screen links.
    Counts,
    /// Also every object with its raw description and variables.
    Objects,
}

fn attrs(node: roxmltree::Node<'_, '_>) -> BTreeMap<String, String> {
    node.attributes()
        .map(|a| (a.name().to_owned(), a.value().to_owned()))
        .collect()
}
fn elements<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children().filter(roxmltree::Node::is_element)
}
impl Xfm {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let doc = roxmltree::Document::parse(text)
            .map_err(|e| Error::new("invalid_xfm", format!("The file is not valid XML: {e}")))?;
        let root = doc.root_element();
        if root.tag_name().name() != "FMExchangeFile" {
            return Err(Error::new(
                "invalid_xfm",
                format!(
                    "Expected an FMExchangeFile (.XFM) root, found {}",
                    root.tag_name().name()
                ),
            ));
        }
        let mut xfm = Self::default();
        for child in elements(root) {
            match child.tag_name().name() {
                "fileHeader" | "contentHeader" => {}
                "logicConf" => xfm.logic_conf(child),
                "IOScreen" => xfm.io_screen(child)?,
                other => xfm.foreign.push(other.to_owned()),
            }
        }
        Ok(xfm)
    }
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            Error::new(
                "file_error",
                format!("Could not read {}: {e}", path.display()),
            )
        })?;
        Self::parse(&text)
    }
    fn logic_conf(&mut self, node: roxmltree::Node<'_, '_>) {
        for resource in elements(node) {
            if resource.tag_name().name() != "resource" {
                self.foreign
                    .push(format!("logicConf/{}", resource.tag_name().name()));
                continue;
            }
            for fm in elements(resource) {
                self.fm_desc(fm, None, "logicConf/resource");
            }
        }
    }
    /// Collection exports wrap modules in FMId="ROOT"; module exports do not.
    fn fm_desc(&mut self, node: roxmltree::Node<'_, '_>, parent: Option<&str>, path: &str) {
        if node.tag_name().name() != "FMDesc" {
            self.foreign
                .push(format!("{path}/{}", node.tag_name().name()));
            return;
        }
        let name = node.attribute("name").unwrap_or_default();
        let fm_id = node.attribute("FMId").unwrap_or_default();
        let parent = if fm_id == "ROOT" {
            parent
        } else {
            self.modules.push(Module {
                name: name.to_owned(),
                fm_id: fm_id.to_owned(),
                parent: parent.map(str::to_owned),
            });
            Some(name)
        };
        let path = format!("{path}/FMDesc[{name}]");
        for child in elements(node) {
            self.fm_desc(child, parent, &path);
        }
    }
    fn io_screen(&mut self, node: roxmltree::Node<'_, '_>) -> Result<()> {
        for child in elements(node) {
            match child.tag_name().name() {
                "screen" => self.screens.push(screen(child, None)?),
                "family" => {
                    let name = child.attribute("name").unwrap_or_default();
                    self.families
                        .push((name.to_owned(), child.attribute("FMId").map(str::to_owned)));
                    for s in elements(child) {
                        if s.tag_name().name() == "screen" {
                            self.screens.push(screen(s, Some(name))?);
                        } else {
                            self.foreign
                                .push(format!("IOScreen/family/{}", s.tag_name().name()));
                        }
                    }
                }
                "message" => self.messages.push(attrs(child)),
                other => self.foreign.push(format!("IOScreen/{other}")),
            }
        }
        Ok(())
    }
    fn normalized_screens(&self) -> Vec<Screen> {
        self.screens.iter().map(Screen::normalized).collect()
    }
}
fn screen(node: roxmltree::Node<'_, '_>, family: Option<&str>) -> Result<Screen> {
    let mut s = Screen {
        family: family.map(str::to_owned),
        attrs: attrs(node),
        comment: None,
        objects: Vec::new(),
    };
    for child in elements(node) {
        match child.tag_name().name() {
            "comment" => s.comment = Some(child.text().unwrap_or_default().to_owned()),
            "object" => s.objects.push(Object {
                object_id: child.attribute("objectID").unwrap_or_default().to_owned(),
                description: child
                    .attribute("description")
                    .unwrap_or_default()
                    .to_owned(),
                links: elements(child)
                    .map(|l| Link {
                        role: l.tag_name().name().to_owned(),
                        attrs: attrs(l),
                    })
                    .collect(),
            }),
            other => {
                return Err(Error::new(
                    "invalid_xfm",
                    format!("Unexpected <{other}> in screen {}", s.name()),
                ));
            }
        }
    }
    Ok(s)
}

/// The file must describe exactly the named top-level module and nothing but
/// its screens: anything else would be lost or misplaced by delete + import.
pub fn check_screens_only<'a>(xfm: &'a Xfm, module: &str) -> Result<&'a Module> {
    let refuse = |message: String| Err(Error::new("not_screens_only", message));
    if !xfm.foreign.is_empty() {
        return refuse(format!(
            "The XFM holds content other than operator screens: {}",
            xfm.foreign.join(", ")
        ));
    }
    let [target] = xfm.modules.as_slice() else {
        let names: Vec<_> = xfm.modules.iter().map(|m| m.name.as_str()).collect();
        return refuse(format!(
            "The XFM must describe exactly one functional module, found {}: {}",
            names.len(),
            names.join(", ")
        ));
    };
    if !target.name.eq_ignore_ascii_case(module) || target.parent.is_some() {
        return refuse(format!(
            "The XFM describes module {} (parent {:?}), not the top-level module {module}",
            target.name, target.parent
        ));
    }
    let owner = |fm: Option<&String>| fm.is_none_or(|id| *id == target.fm_id);
    if let Some(s) = xfm.screens.iter().find(|s| !owner(s.attrs.get("FMId"))) {
        return refuse(format!(
            "Screen {} belongs to FMId {:?}, not to {} ({})",
            s.name(),
            s.attrs.get("FMId"),
            target.name,
            target.fm_id
        ));
    }
    if let Some((name, fm)) = xfm.families.iter().find(|(_, fm)| !owner(fm.as_ref())) {
        return refuse(format!(
            "Family {name} belongs to FMId {fm:?}, not to {}",
            target.fm_id
        ));
    }
    Ok(target)
}

pub fn summary(xfm: &Xfm, detail: Detail) -> Value {
    let screens: Vec<_> = xfm
        .screens
        .iter()
        .map(|s| {
            let mut types = BTreeMap::<&str, usize>::new();
            let mut variables = BTreeMap::<(&str, &str, &str), usize>::new();
            for o in &s.objects {
                *types.entry(&o.object_id).or_default() += 1;
                for l in &o.links {
                    let get = |k| l.attrs.get(k).map_or("", String::as_str);
                    *variables
                        .entry((get("name"), get("typeName"), l.role.as_str()))
                        .or_default() += 1;
                }
            }
            let mut v = json!({"name":s.name(),"family":s.family,"attributes":s.attrs,"comment":s.comment,
                "object_count":s.objects.len(),"objects_by_type":types,
                "variables":variables.iter().map(|((name,t,role),uses)| json!({"name":name,"type":t,"role":role,"uses":uses})).collect::<Vec<_>>()});
            if detail == Detail::Objects {
                v["objects"] = s.objects.iter().enumerate().map(|(i, o)| json!({"index":i,"object_id":o.object_id,"description":o.description,
                    "variables":o.links.iter().map(|l| json!({"role":l.role,"attributes":l.attrs})).collect::<Vec<_>>()})).collect();
            }
            v
        })
        .collect();
    json!({"modules":xfm.modules.iter().map(|m| json!({"name":m.name,"fm_id":m.fm_id,"parent":m.parent})).collect::<Vec<_>>(),
        "screens":screens,"families":xfm.families.iter().map(|(n,fm)| json!({"name":n,"fm_id":fm})).collect::<Vec<_>>(),
        "messages":xfm.messages.len(),"foreign_content":xfm.foreign})
}

/// Names of the screens whose editable content differs, each once and sorted,
/// followed by `<messages>` when the message tables differ.
pub fn differences(expected: &Xfm, actual: &Xfm) -> Vec<String> {
    let (a, b) = (expected.normalized_screens(), actual.normalized_screens());
    let changed: BTreeSet<&str> = a
        .iter()
        .filter(|s| !b.contains(s))
        .chain(b.iter().filter(|s| !a.contains(s)))
        .map(Screen::name)
        .collect();
    let mut names: Vec<String> = changed.into_iter().map(str::to_owned).collect();
    if expected.messages != actual.messages {
        names.push("<messages>".into());
    }
    names
}

/// Whether [`xfm_path`] requires the file to exist already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A file to read: it must exist.
    Existing,
    /// A file to write: its folder must exist.
    New,
}
/// Whether [`Scope::module`] asks for a write-access project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
}

fn xfm_path(raw: &str, target: Target) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(Error::invalid(format!("{raw} must be an absolute path")));
    }
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xfm"))
    {
        return Err(Error::invalid(format!(
            "{raw} must have the .XFM extension"
        )));
    }
    if target == Target::Existing && !path.is_file() {
        return Err(Error::invalid(format!("{raw} does not exist")));
    }
    if target == Target::New && !path.parent().is_some_and(Path::is_dir) {
        return Err(Error::invalid(format!(
            "The folder of {raw} does not exist"
        )));
    }
    Ok(path)
}
fn default_backup(module: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("controlexpert-com-mcp");
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::new(
            "file_error",
            format!("Could not create {}: {e}", dir.display()),
        )
    })?;
    let safe: String = module
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    Ok(dir.join(format!("{safe}-{stamp}.backup.XFM")))
}
fn path_arg(path: &Path) -> Value {
    json!(path.to_string_lossy())
}

/// Every handle obtained here is released at the end, success or not; the
/// caller only keeps the application handle it passed in.
#[derive(Debug)]
struct Scope<'a> {
    server: &'a Server,
    held: Vec<String>,
}
impl<'a> Scope<'a> {
    fn new(server: &'a Server) -> Self {
        Self {
            server,
            held: Vec::new(),
        }
    }
    async fn call(&mut self, handle: &str, api_id: &str, args: &[(&str, Value)]) -> Result<Value> {
        let args = args
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        let mut out = self.server.invoke(handle.to_owned(), api_id, args).await?;
        if let Some(h) = out["return"]["$object"].as_str() {
            self.held.push(h.to_owned());
        }
        Ok(out["return"].take())
    }
    async fn object(
        &mut self,
        handle: &str,
        api_id: &str,
        args: &[(&str, Value)],
    ) -> Result<String> {
        let r = self.call(handle, api_id, args).await?;
        r["$object"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::new("unexpected_result", format!("{api_id} returned no object")))
    }
    /// Returns (project, top-level collection, module). A caller's project is
    /// used as is and stays owned by the caller.
    async fn module(
        &mut self,
        app: &str,
        project: Option<&str>,
        name: &str,
        access: Access,
    ) -> Result<(String, String, String)> {
        let project = if let Some(p) = project {
            p.to_owned()
        } else {
            let token: &[(&str, Value)] = match access {
                Access::Write => &[("lWriteAccessToken", json!(1))],
                Access::Read => &[],
            };
            self.object(app, PROJECT, token).await.map_err(|mut e| {
                if access == Access::Write {
                    e.message = format!(
                        "{} If this client already holds a write-access IProject, pass it as project.",
                        e.message
                    );
                }
                e
            })?
        };
        let modules = self.object(&project, FCT_MODULES, &[]).await?;
        let module = self.item(&modules, name).await?;
        Ok((project, modules, module))
    }
    async fn item(&mut self, modules: &str, name: &str) -> Result<String> {
        self.object(modules, ITEM, &[("FctModuleName", json!(name))])
            .await
            .map_err(|mut e| {
                e.message = format!(
                    "Could not get the top-level functional module {name}: {}",
                    e.message
                );
                e
            })
    }
    async fn release(self) {
        for handle in self.held.into_iter().rev() {
            let _ = self
                .server
                .runtime
                .execute(Action::Release { handle })
                .await;
        }
    }
}
fn is_empty_list(v: &Value) -> bool {
    v.is_null() || v.as_array().is_some_and(Vec::is_empty)
}

pub(crate) async fn export(server: &Server, p: ScreensExport) -> Result<Value> {
    let path = xfm_path(&p.file_path, Target::New)?;
    let mut scope = Scope::new(server);
    let exported = async {
        let (_, _, module) = scope
            .module(&p.handle, p.project.as_deref(), &p.module, Access::Read)
            .await?;
        scope
            .call(&module, EXPORT, &[("FileNameToExport", path_arg(&path))])
            .await
    }
    .await;
    scope.release().await;
    exported?;
    let xfm = Xfm::read(&path)?;
    let importable = check_screens_only(&xfm, &p.module).err();
    let detail = if p.include_objects {
        Detail::Objects
    } else {
        Detail::Counts
    };
    let mut out = summary(&xfm, detail);
    out["file_path"] = path_arg(&path);
    out["module"] = json!(p.module);
    out["importable"] = json!(importable.is_none());
    out["not_importable_reason"] = json!(importable.map(|e| e.message));
    out["note"] = json!(
        "Edit file_path and pass it to ce_screens_import. Object descriptions use a proprietary encoding: keep their structure; texts sit between | separators. FMId, location and dates are regenerated on import."
    );
    Ok(out)
}

pub(crate) async fn import(server: &Server, p: ScreensImport) -> Result<Value> {
    let path = xfm_path(&p.file_path, Target::Existing)?;
    let incoming = Xfm::read(&path)?;
    check_screens_only(&incoming, &p.module)?;
    let backup = match &p.backup_path {
        Some(b) => xfm_path(b, Target::New)?,
        None => default_backup(&p.module)?,
    };
    if backup == path {
        return Err(Error::invalid("backup_path must differ from file_path"));
    }
    let mut scope = Scope::new(server);
    let r = replace(&mut scope, &p, &path, &incoming, &backup).await;
    scope.release().await;
    r
}
async fn replace(
    scope: &mut Scope<'_>,
    p: &ScreensImport,
    path: &Path,
    incoming: &Xfm,
    backup: &Path,
) -> Result<Value> {
    let (project, modules, module) = scope
        .module(&p.handle, p.project.as_deref(), &p.module, Access::Write)
        .await?;
    scope
        .call(&module, EXPORT, &[("FileNameToExport", path_arg(backup))])
        .await?;
    let current = Xfm::read(backup)?;
    let guard = |message: String| {
        Error::new(
            "not_screens_only",
            format!("{message}. Module {} was left untouched.", p.module),
        )
    };
    let previous = check_screens_only(&current, &p.module)
        .map_err(|e| {
            guard(format!(
                "The current module does not hold only operator screens: {}",
                e.message
            ))
        })?
        .fm_id
        .clone();
    // Secondary checks only: in 15.1 GetListOfSectionsAttached returned null
    // for a module with an attached section, which the export above catches.
    if scope.call(&module, HAS_CHILDREN, &[]).await? != json!(0) {
        return Err(guard("The module has subordinate modules".into()));
    }
    let prog = scope.object(&module, PROG, &[]).await?;
    for (api, what) in [(SECTIONS, "sections"), (OBJECTS, "objects")] {
        let attached = scope.call(&prog, api, &[]).await?;
        if !is_empty_list(&attached) {
            return Err(guard(format!("The module has attached {what}: {attached}")));
        }
    }
    let names = |x: &Xfm| {
        x.screens
            .iter()
            .map(|s| s.name().to_owned())
            .collect::<Vec<_>>()
    };
    let plan = json!({"module":p.module,"file_path":path_arg(path),"backup_path":path_arg(backup),"previous_fm_id":previous,
        "current_screens":names(&current),"incoming_screens":names(incoming)});
    if p.dry_run {
        return Ok(
            json!({"dry_run":true,"plan":plan,"note":"Checks passed and the backup was written; nothing was deleted or imported."}),
        );
    }
    scope.call(&module, DELETE, &[]).await?;
    let import = |file: &Path| {
        [
            ("FileNameToImport", path_arg(file)),
            ("psrImportStrategy", json!("psrImportOptionOverwrite")),
        ]
    };
    if let Err(mut e) = scope.call(&modules, IMPORT, &import(path)).await {
        // The module is already gone: put the backup back before reporting.
        let restore = scope.call(&modules, IMPORT, &import(backup)).await;
        e.details = Some(
            json!({"original_details":e.details,"module_deleted":true,"restored_from_backup":restore.is_ok(),
            "restore_error":restore.err(),"backup_path":path_arg(backup)}),
        );
        return Err(e);
    }
    let imported = scope.item(&modules, &p.module).await?;
    let verify_path = backup.with_extension("verify.XFM");
    scope
        .call(
            &imported,
            EXPORT,
            &[("FileNameToExport", path_arg(&verify_path))],
        )
        .await?;
    let after = Xfm::read(&verify_path)?;
    let _ = std::fs::remove_file(&verify_path);
    let differences = differences(incoming, &after);
    let verified = differences.is_empty();
    let saved = if p.save && verified {
        scope.call(&project, SAVE, &[]).await?;
        true
    } else {
        false
    };
    Ok(
        json!({"dry_run":false,"plan":plan,"fm_id":after.modules.first().map(|m| &m.fm_id),"screens":names(&after),
        "verified":verified,"differences":differences,"saved":saved,
        "note":if saved {"Imported, verified and saved."} else if verified {"Imported and verified in memory; not saved. Call IProject.Save or pass save=true."} else {"Imported, but the re-export differs from file_path; not saved. Review differences; backup_path holds the previous module."}}),
    )
}

#[cfg(test)]
mod tests;
