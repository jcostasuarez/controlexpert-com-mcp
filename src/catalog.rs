use crate::{
    error::{Error, Result},
    versions::Profile,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Type {
    pub kind: String,
    pub element: Option<Box<Type>>,
    pub name: Option<String>,
    pub library: Option<String>,
    pub type_kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Parameter {
    pub name: String,
    pub position: usize,
    #[serde(rename = "type")]
    pub type_name: String,
    pub com_type: Type,
    pub direction: String,
    pub optional: bool,
    pub is_retval: bool,
    pub default_value: Option<Value>,
}
impl Parameter {
    pub fn is_output(&self) -> bool {
        matches!(self.direction.as_str(), "out" | "inout")
    }
    pub fn is_input(&self) -> bool {
        self.direction != "out"
    }
}

#[derive(Debug, Clone)]
pub struct Operation {
    pub id: String,
    pub name: String,
    pub interface: String,
    pub category: String,
    pub kind: String,
    pub dispid: i32,
    pub invoke_kind: String,
    pub parameters: Vec<Parameter>,
    pub return_type: Type,
    pub original: Value,
}
#[derive(Debug, Clone)]
pub struct Interface {
    pub id: String,
    pub guid: String,
    pub category: String,
    pub dispatch: bool,
    pub events: bool,
    pub original: Value,
}

#[derive(Debug)]
pub struct Catalog {
    pub profile: Profile,
    pub document: Value,
    pub operations: BTreeMap<String, Operation>,
    pub interfaces: BTreeMap<String, Interface>,
    aliases: HashMap<String, Type>,
    pub enums: BTreeMap<String, Value>,
}

impl Catalog {
    pub fn load(profile: Profile) -> Result<Self> {
        let document: Value = serde_json::from_str(profile.catalog_json)
            .map_err(|e| Error::new("invalid_catalog", e.to_string()))?;
        let mut cat = Self {
            profile,
            document,
            operations: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            aliases: HashMap::new(),
            enums: BTreeMap::new(),
        };
        for i in cat.document["interfaces"]
            .as_array()
            .ok_or_else(|| Error::new("invalid_catalog", "Missing interfaces"))?
        {
            let id = text(i, "id")?;
            cat.interfaces.insert(
                id.clone(),
                Interface {
                    id,
                    guid: text(i, "guid")?,
                    category: text(i, "category")?,
                    dispatch: i["com_type"] == "TKIND_DISPATCH",
                    events: i["is_event_interface"] == true,
                    original: i.clone(),
                },
            );
        }
        for f in cat.document["functions"]
            .as_array()
            .ok_or_else(|| Error::new("invalid_catalog", "Missing functions"))?
        {
            let id = text(f, "id")?;
            let op = Operation {
                id: id.clone(),
                name: text(f, "name")?,
                interface: text(f, "interface")?,
                category: text(f, "category")?,
                kind: text(f, "kind")?,
                dispid: f["com"]["dispid"]
                    .as_i64()
                    .and_then(|d| i32::try_from(d).ok())
                    .ok_or_else(|| Error::new("invalid_catalog", "Invalid DISPID"))?,
                invoke_kind: text(&f["com"], "invoke_kind")?,
                parameters: serde_json::from_value(f["io"]["parameters"].clone())
                    .map_err(|e| Error::new("invalid_catalog", e.to_string()))?,
                return_type: serde_json::from_value(f["io"]["return"]["com_type"].clone())
                    .map_err(|e| Error::new("invalid_catalog", e.to_string()))?,
                original: f.clone(),
            };
            if !cat.interfaces.contains_key(&op.interface)
                || cat.operations.insert(id, op).is_some()
            {
                return Err(Error::new(
                    "invalid_catalog",
                    "Missing interface or duplicate ID",
                ));
            }
        }
        for a in cat.document["aliases"].as_array().into_iter().flatten() {
            cat.aliases.insert(
                text(a, "id")?,
                serde_json::from_value(a["com_type"].clone())
                    .map_err(|e| Error::new("invalid_catalog", e.to_string()))?,
            );
        }
        for e in cat.document["enumerations"]
            .as_array()
            .into_iter()
            .flatten()
        {
            cat.enums.insert(text(e, "id")?, e.clone());
            let lib = text(&e["location"], "library")?;
            for alias in e["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                cat.enums.insert(format!("{lib}.{alias}"), e.clone());
            }
        }
        Ok(cat)
    }
    pub fn operation(&self, id: &str) -> Result<&Operation> {
        self.operations
            .get(id)
            .ok_or_else(|| Error::new("api_not_found", format!("Unknown API: {id}")))
    }
    pub fn interface(&self, id: &str) -> Result<&Interface> {
        self.interfaces
            .get(id)
            .ok_or_else(|| Error::new("interface_not_found", format!("Unknown interface: {id}")))
    }
    pub fn coclass(&self, id: &str) -> Result<&Value> {
        self.document["coclasses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["id"] == id)
            .ok_or_else(|| Error::new("class_not_found", format!("Uncatalogued coclass: {id}")))
    }
    pub fn resolve_type(&self, t: &Type) -> Result<Type> {
        let mut result = t.clone();
        for _ in 0..32 {
            if result.kind == "VT_USERDEFINED" && result.type_kind.as_deref() == Some("TKIND_ALIAS")
            {
                let id = format!(
                    "{}.{}",
                    result.library.as_deref().unwrap_or(""),
                    result.name.as_deref().unwrap_or("")
                );
                result =
                    self.aliases.get(&id).cloned().ok_or_else(|| {
                        Error::unsupported(format!("Alias without definition: {id}"))
                    })?;
            } else {
                return Ok(result);
            }
        }
        Err(Error::unsupported("Cyclic alias"))
    }
    pub fn symbolic_enum(&self, t: &Type, name: &str) -> Result<Value> {
        let id = format!(
            "{}.{}",
            t.library.as_deref().unwrap_or(""),
            t.name.as_deref().unwrap_or("")
        );
        let e = self
            .enums
            .get(&id)
            .ok_or_else(|| Error::invalid(format!("{id} is not a known enumeration")))?;
        e["values"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|v| v["name"] == name)
            .map(|v| v["value"].clone())
            .ok_or_else(|| Error::invalid(format!("Unknown value {name} for {id}")))
    }
    pub fn invocation_reason(&self, op: &Operation) -> Option<String> {
        if op.kind == "event" {
            return Some(
                "This is a callback; this version does not implement COM event subscriptions."
                    .into(),
            );
        }
        if !self.interfaces[&op.interface].dispatch {
            return Some("Vtable interface without IDispatch; requires a typed adapter.".into());
        }
        for p in &op.parameters {
            if !self.supports_type(&p.com_type, 0) {
                return Some(format!(
                    "Type {} of parameter {} requires an additional adapter.",
                    p.type_name, p.name
                ));
            }
        }
        None
    }
    fn supports_type(&self, t: &Type, depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        let Ok(t) = self.resolve_type(t) else {
            return false;
        };
        match t.kind.as_str() {
            "VT_PTR" => t
                .element
                .as_deref()
                .is_some_and(|e| self.supports_type(e, depth + 1)),
            "VT_USERDEFINED" => matches!(
                t.type_kind.as_deref(),
                Some("TKIND_ENUM" | "TKIND_DISPATCH")
            ),
            "VT_VARIANT" | "VT_BSTR" | "VT_BOOL" | "VT_I1" | "VT_UI1" | "VT_I2" | "VT_UI2"
            | "VT_I4" | "VT_UI4" | "VT_INT" | "VT_UINT" | "VT_I8" | "VT_UI8" | "VT_R4"
            | "VT_R8" | "VT_UNKNOWN" | "VT_DISPATCH" => true,
            _ => false,
        }
    }
    pub fn prepare(&self, id: &str, arguments: &BTreeMap<String, Value>) -> Result<Operation> {
        let op = self.operation(id)?;
        if let Some(reason) = self.invocation_reason(op) {
            return Err(Error::unsupported(reason));
        }
        for name in arguments.keys() {
            let p = op
                .parameters
                .iter()
                .find(|p| p.name == *name)
                .ok_or_else(|| Error::invalid(format!("Unknown parameter: {name}")))?;
            if !p.is_input() {
                return Err(Error::invalid(format!(
                    "{name} is an output and is assigned automatically"
                )));
            }
        }
        for p in &op.parameters {
            if p.is_input()
                && !p.optional
                && p.default_value.is_none()
                && !arguments.contains_key(&p.name)
            {
                return Err(Error::invalid(format!(
                    "Missing required parameter: {}",
                    p.name
                )));
            }
        }
        Ok(op.clone())
    }
    pub fn search(
        &self,
        query: &str,
        interface: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Value {
        let words: Vec<_> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let matches: Vec<_> = self
            .operations
            .values()
            .filter(|f| {
                interface.is_none_or(|i| f.interface == i)
                    && words.iter().all(|w| {
                        format!("{} {}", f.id, f.original["description"])
                            .to_lowercase()
                            .contains(w)
                    })
            })
            .collect();
        json!({"total":matches.len(),"offset":offset,"limit":limit,"items":matches.iter().skip(offset).take(limit).map(|f|json!({"id":f.id,"description":f.original["description"],"signature":f.original["signature"],"kind":f.kind,"category":f.category,"invocable":self.invocation_reason(f).is_none(),"limitation":self.invocation_reason(f)})).collect::<Vec<_>>()})
    }
}
fn text(v: &Value, key: &str) -> Result<String> {
    v[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::new("invalid_catalog", format!("Missing field {key}")))
}
