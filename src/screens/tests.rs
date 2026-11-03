use super::*;

const HEADER: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<FMExchangeFile>
	<fileHeader company="Schneider Automation" product="Control Expert V15.1 - 220222A" dateTime="date_and_time#2026-10-18-15:36:21" content="Fichero fuente de módulo funcional" DTDVersion="41"></fileHeader>
	<contentHeader name="Proyecto" version="0.0.000"></contentHeader>"#;
const SCREEN: &str = r#"<screen name="Pantalla" screenX="1280" screenY="1024" BKColor="-1" valScreen="0" location="HMI_SCREENS" FMId="efK" creationDate="18/10/2026 - 15:23:04" modificationDate="18/10/2026 - 15:24:35" customInfos="" isPattern="0" valPattern="0">
			<object objectID="2" description="(196,57,215,202),29"></object>
			<object objectID="15" description="(357,49,376,108),(5,1,0),(10,0,0),|Dynamic|Fixedsys|,(9,0,0,0,1,0,0)"></object>
			<object objectID="15" description="(707,92,724,106),(0,1,0),(11,0,0),|&quot;0&quot;|Arial|,(9,1,1,0,0,0,16711935)">
				<varAnim name="%M3" typeName="EBOOL" description="Anim:0,|-|-|,(1,0),(0,0)"></varAnim>
			</object>
			<object objectID="10" description="(206,70,212,70),(0,3,12632256),4">
				<varAnim name="%M3" typeName="EBOOL" description="Anim:2,|-|-|,(0,0),(0,0)"></varAnim>
				<varPilot name="%M4" typeName="EBOOL" description="Pilot"></varPilot>
			</object>
		</screen>"#;

fn module_xfm(screen: &str) -> String {
    format!(
        r#"{HEADER}
	<logicConf>
		<resource resName="Micro Basic" resIdent="BMX P34 2020 03.50">
			<FMDesc name="HMI_SCREENS" FMId="efK"></FMDesc>
		</resource>
	</logicConf>
	<IOScreen>
		{screen}
	</IOScreen>
</FMExchangeFile>"#
    )
}

#[test]
fn parses_module_export_with_screens_objects_and_variables() {
    let xfm = Xfm::parse(&module_xfm(SCREEN)).unwrap();
    assert_eq!(
        xfm.modules,
        [Module {
            name: "HMI_SCREENS".into(),
            fm_id: "efK".into(),
            parent: None
        }]
    );
    assert!(xfm.foreign.is_empty());
    let [screen] = xfm.screens.as_slice() else {
        panic!("one screen expected")
    };
    assert_eq!(screen.name(), "Pantalla");
    assert_eq!(screen.objects.len(), 4);
    // Entities are decoded: the caller sees the text the UI shows.
    assert!(screen.objects[2].description.contains(r#"|"0"|Arial|"#));
    assert_eq!(screen.objects[3].links[1].role, "varPilot");
    assert_eq!(
        check_screens_only(&xfm, "hmi_screens").unwrap().fm_id,
        "efK"
    );
}
#[test]
fn collection_exports_wrap_modules_in_root_and_keep_nesting() {
    let text = format!(
        r#"{HEADER}
	<logicConf><resource resName="M" resIdent="X">
		<FMDesc name="Proyecto funcional" FMId="ROOT">
			<FMDesc name="HMI_SCREENS" FMId="e8K"><FMDesc name="HMI_SCREENS" FMId="efK"></FMDesc></FMDesc>
		</FMDesc>
	</resource></logicConf>
	<IOScreen></IOScreen>
</FMExchangeFile>"#
    );
    let xfm = Xfm::parse(&text).unwrap();
    assert_eq!(xfm.modules.len(), 2);
    assert_eq!(xfm.modules[0].parent, None);
    assert_eq!(xfm.modules[1].parent.as_deref(), Some("HMI_SCREENS"));
    // Nested modules cannot be recreated by deleting and importing one module.
    assert_eq!(
        check_screens_only(&xfm, "HMI_SCREENS").unwrap_err().code,
        "not_screens_only"
    );
}
#[test]
fn refuses_files_with_other_content_or_another_module() {
    let with_program = module_xfm(SCREEN).replace(
        "<IOScreen>",
        "<program><identProgram name=\"MAIN\" type=\"section\" task=\"MAST\"></identProgram></program>\n\t<IOScreen>",
    );
    let err = check_screens_only(&Xfm::parse(&with_program).unwrap(), "HMI_SCREENS").unwrap_err();
    assert!(err.message.contains("program"), "{}", err.message);

    let xfm = Xfm::parse(&module_xfm(SCREEN)).unwrap();
    assert!(check_screens_only(&xfm, "OTHER").is_err());

    let foreign_screen = module_xfm(&SCREEN.replace("FMId=\"efK\"", "FMId=\"zzz\""));
    let err = check_screens_only(&Xfm::parse(&foreign_screen).unwrap(), "HMI_SCREENS").unwrap_err();
    assert!(err.message.contains("Pantalla"), "{}", err.message);
}
#[test]
fn rejects_non_xfm_documents_and_unknown_screen_children() {
    assert_eq!(
        Xfm::parse("<FEFExchangeFile/>").unwrap_err().code,
        "invalid_xfm"
    );
    assert_eq!(Xfm::parse("not xml").unwrap_err().code, "invalid_xfm");
    let odd =
        module_xfm(&SCREEN.replace("<object objectID=\"2\"", "<shape/><object objectID=\"2\""));
    assert_eq!(Xfm::parse(&odd).unwrap_err().code, "invalid_xfm");
    // A UTF-8 BOM written by an editor is accepted.
    assert!(Xfm::parse(&format!("\u{feff}{}", module_xfm(SCREEN))).is_ok());
}
#[test]
fn verification_ignores_regenerated_identity_but_not_edits() {
    let original = Xfm::parse(&module_xfm(SCREEN)).unwrap();
    let reimported = module_xfm(
        &SCREEN
            .replace("FMId=\"efK\"", "FMId=\"egK\"")
            .replace("15:24:35", "16:00:00"),
    );
    assert!(differences(&original, &Xfm::parse(&reimported).unwrap()).is_empty());

    for edit in [
        SCREEN.replace("|Dynamic|", "|Dinamico|"),
        SCREEN.replace("name=\"%M4\"", "name=\"%M5\""),
        SCREEN.replace("valScreen=\"0\"", "valScreen=\"1\""),
        SCREEN.replace("name=\"Pantalla\"", "name=\"Copia de Pantalla\""),
    ] {
        let edited = Xfm::parse(&module_xfm(&edit)).unwrap();
        assert!(!differences(&original, &edited).is_empty(), "{edit}");
    }
}
#[test]
fn differences_name_each_changed_screen_once() {
    let second = SCREEN.replace("name=\"Pantalla\"", "name=\"Segunda\"");
    let original = Xfm::parse(&module_xfm(&format!("{SCREEN}{second}"))).unwrap();
    let both_edited = format!("{SCREEN}{second}").replace("|Dynamic|", "|Dinamico|");
    let edited = Xfm::parse(&module_xfm(&both_edited)).unwrap();
    assert_eq!(differences(&original, &edited), ["Pantalla", "Segunda"]);
}
#[test]
fn summary_counts_objects_and_aggregates_variables() {
    let xfm = Xfm::parse(&module_xfm(SCREEN)).unwrap();
    let s = summary(&xfm, Detail::Counts);
    let screen = &s["screens"][0];
    assert_eq!(screen["object_count"], 4);
    assert_eq!(screen["objects_by_type"]["15"], 2);
    assert_eq!(
        screen["variables"],
        json!([
            {"name":"%M3","type":"EBOOL","role":"varAnim","uses":2},
            {"name":"%M4","type":"EBOOL","role":"varPilot","uses":1}
        ])
    );
    assert!(screen.get("objects").is_none());
    let detailed = summary(&xfm, Detail::Objects);
    assert_eq!(
        detailed["screens"][0]["objects"][3]["variables"][1]["role"],
        "varPilot"
    );
}
#[test]
fn paths_must_be_absolute_xfm_files() {
    assert!(xfm_path("relative.XFM", Target::New).is_err());
    let dir = std::env::temp_dir();
    assert!(xfm_path(&dir.join("x.xef").to_string_lossy(), Target::New).is_err());
    assert!(xfm_path(&dir.join("x.xfm").to_string_lossy(), Target::New).is_ok());
    assert!(
        xfm_path(
            &dir.join("missing-ce-mcp.XFM").to_string_lossy(),
            Target::Existing
        )
        .is_err()
    );
    assert!(
        xfm_path(
            &dir.join("no-such-dir").join("x.XFM").to_string_lossy(),
            Target::New
        )
        .is_err()
    );
}
#[test]
fn attached_lists_are_empty_when_null_or_empty_arrays() {
    assert!(is_empty_list(&Value::Null));
    assert!(is_empty_list(&json!([])));
    assert!(!is_empty_list(&json!(["MAIN"])));
}
