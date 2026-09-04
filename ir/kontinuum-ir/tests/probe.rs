#[test]
fn probe_totals() {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json")).unwrap();
    let s: kontinuum_ir::Session = serde_json::from_str(&raw).unwrap();
    println!("total_bars = {}", s.total_bars());
    let blocks = kontinuum_ir::compile_session(&s, 48_000).unwrap();
    for b in &blocks {
        println!("block start_bar {} bars {} start_frame {}", b.start_bar, b.bars, b.start_frame);
    }
}
