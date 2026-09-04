#[test]
fn probe_taste_generation() {
    let profile = r#"{"genres": ["minimal techno"]}"#;
    let p: kontinuum_compose::taste::TasteProfile = serde_json::from_str(profile)
        .expect("profile parse");
    let session = kontinuum_compose::taste::session_from_taste(&p, 1_760_000_000_000);
    match kontinuum_ir::validate_session(&session) {
        Ok(()) => println!("valid session, bars = {}", session.total_bars()),
        Err(errs) => {
            for e in &errs { println!("E: {} {} {}", e.code, e.path, e.message); }
            panic!("generated session invalid: {} errors", errs.len());
        }
    }
}
