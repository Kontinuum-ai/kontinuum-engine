//! `cargo run -p kontinuum-export --example export_cli -- <session.json> <spec.json>`
use kontinuum_export::{export_session, ExportReportJson, ExportSpec};
use kontinuum_mastering::targets::MasteringTargets;
use kontinuum_offline::parse_session;
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let session_path = args.next().expect("usage: export_cli <session.json> <spec.json>");
    let spec_path = args.next().expect("usage: export_cli <session.json> <spec.json>");
    let session = parse_session(Path::new(&session_path)).expect("session");
    let spec: ExportSpec =
        serde_json::from_str(&std::fs::read_to_string(spec_path).expect("spec")).expect("spec json");
    let request = spec.into_request(session.tracks.len());
    let report = export_session(&session, &request, &MasteringTargets::hypothesis()).expect("export");
    println!(
        "{}",
        serde_json::to_string_pretty(&ExportReportJson::from_report(&report, &session)).unwrap()
    );
}
