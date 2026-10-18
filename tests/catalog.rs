use controlexpert_com_mcp::{catalog::Catalog, versions::Profile};
use serde_json::json;
use std::collections::BTreeMap;

fn catalog() -> Catalog {
    Catalog::load(Profile::select("15.1").unwrap()).unwrap()
}
#[test]
fn catalog_is_complete_and_interfaces_exist() {
    let c = catalog();
    assert_eq!(c.operations.len(), 1277);
    assert_eq!(c.interfaces.len(), 120);
    for op in c.operations.values() {
        assert!(c.interfaces.contains_key(&op.interface));
    }
    assert_eq!(
        c.operations
            .values()
            .filter(|op| op.kind == "event")
            .count(),
        54
    );
}
#[test]
fn validate_named_arguments_and_optional_defaults() {
    let c = catalog();
    let id = "UnityProServer.IVariables.Add:method";
    assert_eq!(
        c.prepare(id, &BTreeMap::new()).unwrap_err().code,
        "invalid_arguments"
    );
    let valid = BTreeMap::from([
        ("sVarName".into(), json!("MotorRun")),
        ("sTypeName".into(), json!("BOOL")),
    ]);
    assert!(c.prepare(id, &valid).is_ok());
    let mut invalid = valid;
    invalid.insert("typo".into(), json!(42));
    assert!(c.prepare(id, &invalid).is_err());
    assert!(
        c.prepare(
            "UnityProServer.IProject.Export:method",
            &BTreeMap::from([("FileNameToExport".into(), json!("C:\\temp\\out.xef"))])
        )
        .is_ok()
    );
    assert!(
        c.prepare(
            "UnityProServer.IProject.GetVersion:method",
            &BTreeMap::new()
        )
        .is_ok()
    );
    assert!(
        c.prepare(
            "UnityProServer.IProject.GetVersion:method",
            &BTreeMap::from([("plBuildVersion".into(), json!(2))])
        )
        .is_err()
    );
}
#[test]
fn enum_aliases_and_event_collections() {
    let c = catalog();
    let op = c.operation("UnityProServer.ISections.Add:method").unwrap();
    let language = &op.parameters[1].com_type;
    assert!(c.symbolic_enum(language, "not_a_language").is_err()); // Unknown names are never guessed.
    let enum_info = &c.enums["UnityProServer.epsrLanguage"];
    for value in enum_info["values"].as_array().unwrap() {
        assert_eq!(
            c.symbolic_enum(language, value["name"].as_str().unwrap())
                .unwrap(),
            value["value"]
        );
    }
    assert!(
        c.prepare("UnityProServer.IIOEvents.Add:method", &BTreeMap::new())
            .is_err()
    );
    assert!(!c.interfaces["UnityProServer.IIOEvents"].events);
    assert!(
        c.prepare(
            "UnityProServer.DIApplicationEvents.AfterSave:method",
            &BTreeMap::from([("VarSignature".into(), json!(0))])
        )
        .is_err()
    );
}
#[test]
fn version_is_exact_not_a_prefix() {
    let p = Profile::select("15.1").unwrap();
    for valid in ["15.1", "V15.1 - 22044", "15.01.0.0"] {
        assert!(p.accepts_runtime_version(valid), "{valid}");
    }
    for invalid in ["15.10", "15.2", "16.1", "unknown version"] {
        assert!(!p.accepts_runtime_version(invalid), "{invalid}");
    }
    assert!(Profile::select("16.0").is_err());
}
#[test]
fn search_paginates_without_losing_contract_identity() {
    let c = catalog();
    let first = c.search("Build", None, 0, 1);
    let next = c.search("Build", None, 1, 1);
    assert!(first["total"].as_u64().unwrap() > 1);
    assert_ne!(first["items"][0]["id"], next["items"][0]["id"]);
    assert_eq!(
        c.search("", Some("UnityProServer.IVariables"), 0, 100)["total"],
        16
    );
}
