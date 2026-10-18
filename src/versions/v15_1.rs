use super::Profile;
pub fn profile() -> Profile {
    Profile {
        version: "15.1",
        broker_progid: "PSBroker.PServerBroker.1",
        install_dir: r"C:\Program Files (x86)\Schneider Electric\Control Expert 15.1",
        catalog_json: include_str!("../../control_expert_api_catalog.json"),
    }
}
