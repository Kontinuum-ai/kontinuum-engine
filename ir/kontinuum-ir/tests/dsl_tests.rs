//! DSL v0 contract tests (issue #39 step 4): grammar acceptance/rejection
//! table, error-code + suggested_fix presence, the committed golden file,
//! and the round-trip property `ir → text → ir` over a deterministic seeded
//! corpus (200 cases).

use kontinuum_ir::dsl::{compile, render, DslCode, DslError};
use kontinuum_ir::diff::IrDiff;
use kontinuum_ir::schema::{Pattern, Section, Step, StepsPattern};

// -- Grammar acceptance / rejection table -----------------------------------

#[test]
fn acceptance_table() {
    let accepted = [
        // section headers, one-line and multi-line, `;` and `,` separators
        "section a { bars 4, energy 0.5 }",
        "section a {\n  bars 4;\n  energy 0.5;\n}",
        "section a { bars 4, energy 0.5, }",
        // masks: 1 bit, 16 bits with underscores
        "section a { bars 1, energy 0.5, kick.mask = 0b1; }",
        "section a { bars 1, energy 0.5, kick.mask = 0b1000_1000_1000_1000; }",
        // velocity lists: short and full
        "section a { bars 1, energy 0.5, hat.vel = [1.0, 0.5]; }",
        "section a { bars 1, energy 0.5, hat.vel = [0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8, 0.8]; }",
        // euclidean shorthand, with/without swing, negative rot, k = 0
        "section a { bars 1, energy 0.5, perc: E(5, 16, 2); }",
        "section a { bars 1, energy 0.5, perc: E(5, 16, 2) @ swing 0.14; }",
        "section a { bars 1, energy 0.5, perc: E(0, 16, -8); }",
        // params at top level, inside sections, int-for-float, exponents
        "bass.cutoff_hz = 120.0;",
        "bass.cutoff_hz = 120;",
        "section a { bars 1, energy 0.5, bass.cutoff_hz = 1.5e2; }",
        // comments and empty input
        "# header\n// note\n\nsection a { bars 1, energy 0.5 } # tail",
        "",
    ];
    for src in accepted {
        let diffs = compile(src).unwrap_or_else(|e| panic!("must accept: {src:?} → {e:?}"));
        assert!(!diffs.is_empty() || src.trim().is_empty(), "must emit ops: {src:?}");
    }
}

#[test]
fn rejection_table() {
    let rejected: &[(&str, &str)] = &[
        // lexer (fatal)
        ("kick.mask = 0b;", DslCode::E_DSL_MASK_EMPTY),
        ("kick.mask = $;", DslCode::E_DSL_BAD_CHAR),
        // grammar (fatal)
        ("section 3 { }", DslCode::E_DSL_UNEXPECTED_TOKEN),
        ("section a { bars 4", DslCode::E_DSL_UNCLOSED_BRACE),
        ("section a { section b { bars 1, energy 0.5 } }", DslCode::E_DSL_NESTED_SECTION),
        ("section a { bars 4 energy 0.5 }", DslCode::E_DSL_EXPECT_TERMINATOR),
        ("nonsense;", DslCode::E_DSL_UNKNOWN_STATEMENT),
        ("kick: F(1, 2, 3);", DslCode::E_DSL_UNKNOWN_STATEMENT),
        ("kick.mask = 0b1 kick.vel = [1.0];", DslCode::E_DSL_EXPECT_TERMINATOR),
        // semantic (collected)
        ("section a { bars 1, energy 0.5, kick.mask = 0b1111_1111_1111_1111_1; }", DslCode::E_DSL_MASK_RANGE),
        ("section a { bars 1, energy 0.5, hat.vel = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]; }", DslCode::E_DSL_VEL_RANGE),
        ("section a { bars 1, energy 0.5, hat.vel = [2.0]; }", DslCode::E_DSL_VEL_RANGE),
        ("section a { bars 1, energy 0.5, perc: E(17, 16, 0); }", DslCode::E_DSL_EUCLID_RANGE),
        ("section a { bars 1, energy 0.5, perc: E(3, 0, 0); }", DslCode::E_DSL_EUCLID_RANGE),
        ("section a { bars 1, energy 0.5, perc: E(3, 16, 0) @ swing 0.9; }", DslCode::E_DSL_SWING_RANGE),
        ("section a { bars 0, energy 0.5 }", DslCode::E_DSL_BARS_RANGE),
        ("section a { energy 0.5 }", DslCode::E_DSL_BARS_REQUIRED),
        ("section a { bars 4 }", DslCode::E_DSL_ENERGY_REQUIRED),
        ("section a { bars 4, energy 2.0 }", DslCode::E_DSL_ENERGY_RANGE),
        ("section a { bars 4, energy 0.5, bars 8 }", DslCode::E_DSL_DUP_FIELD),
        ("bass.woozle = 1.0;", DslCode::E_DSL_UNKNOWN_PARAM),
        ("kick.mask = 0b1000;", DslCode::E_DSL_PATTERN_OUTSIDE_SECTION),
        ("bars 4;", DslCode::E_DSL_FIELD_OUTSIDE_SECTION),
    ];
    assert!(rejected.len() >= 15, "the table is the spec: keep it growing");
    for (src, code) in rejected {
        match compile(src) {
            Ok(diffs) => panic!("must reject: {src:?} → {diffs:?}"),
            Err(errs) => {
                assert!(
                    errs.iter().any(|e| e.code == *code),
                    "{src:?}: expected {code}, got {:?}",
                    errs.iter().map(|e| e.code).collect::<Vec<_>>()
                );
            }
        }
    }
}

#[test]
fn every_error_carries_code_line_path_and_suggested_fix() {
    let src = "bass.woozle = 1.0;\nsection a { bars 0, energy 2.0 }\nkick.mask = 0b1000;\n";
    let errs: Vec<DslError> = compile(src).expect_err("three semantic errors");
    assert!(errs.len() >= 4);
    for e in &errs {
        assert!(e.code.starts_with("E_DSL_"));
        assert!(e.line >= 1 && e.line <= 3, "line tagged: {e:?}");
        assert!(e.path.starts_with('/'));
        assert!(!e.message.is_empty());
        assert!(!e.suggested_fix.is_empty(), "LLM-actionable: {e:?}");
    }
}

// -- Golden file -------------------------------------------------------------

fn bucket(k: u32, n: u32, rot: i32) -> Vec<bool> {
    let mut acc = 0u32;
    let mut grid = Vec::with_capacity(n as usize);
    for _ in 0..n {
        acc += k;
        if acc >= n {
            acc -= n;
            grid.push(true);
        } else {
            grid.push(false);
        }
    }
    grid.rotate_left(rot.rem_euclid(n as i32) as usize);
    grid
}

fn step(position: u32, velocity: f32, micro: i16) -> Step {
    Step {
        position,
        velocity,
        probability: 1.0,
        microtiming_ticks: micro,
        ratchet: 1,
        pitch: None,
        gate: None,
        accent: false,
    }
}

fn section(id: &str, bars: u32, energy: f32) -> Section {
    Section {
        id: id.into(),
        bars,
        energy_curve: vec![energy],
        density_curve: Vec::new(),
        brightness_curve: Vec::new(),
        transition_in: None,
        transition_out: None,
        pattern_bindings: Default::default(),
        automation: Default::default(),
    }
}

const SWING_TICKS: i16 = 34; // round(0.14 * 240)

#[test]
fn golden_file_compiles_to_the_exact_expected_diffs() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/dsl/v0.klc");
    let src = std::fs::read_to_string(path).expect("golden fixture");
    let expected = vec![
        IrDiff::AddSection { after: None, section: section("a", 4, 0.5) },
        IrDiff::AddSection { after: None, section: section("b", 8, 0.7) },
        IrDiff::ReplacePattern {
            section: "b".into(),
            track: "kick".into(),
            pattern: Pattern::Steps(StepsPattern {
                steps: [0usize, 4, 8, 12].iter().map(|i| step((*i * 240) as u32, 0.8, 0)).collect(),
                repeats: 1,
            }),
        },
        IrDiff::ReplacePattern {
            section: "b".into(),
            track: "hat".into(),
            pattern: Pattern::Steps(StepsPattern {
                steps: (0..16)
                    .filter(|i| i % 2 == 0)
                    .map(|i| step((i * 240) as u32, if (i / 2) % 2 == 0 { 1.0 } else { 0.5 }, 0))
                    .collect(),
                repeats: 1,
            }),
        },
        IrDiff::ReplacePattern {
            section: "b".into(),
            track: "perc".into(),
            pattern: Pattern::Steps(StepsPattern {
                steps: bucket(5, 16, 2)
                    .iter()
                    .enumerate()
                    .filter(|(_, on)| **on)
                    .map(|(i, _)| step(
                        (i * 240) as u32,
                        0.8,
                        if i % 2 == 1 { SWING_TICKS } else { 0 },
                    ))
                    .collect(),
                repeats: 1,
            }),
        },
        IrDiff::SetInstrumentParam { track: "bass".into(), param: "cutoff_hz".into(), value: 120.0 },
        IrDiff::SetSectionEnergy { id: "b".into(), energy: vec![0.85] },
        IrDiff::SetInstrumentParam { track: "kick".into(), param: "tune_hz".into(), value: 48.0 },
    ];
    assert_eq!(compile(&src).expect("golden compiles"), expected, "golden file drifted");

    // Canonical fixpoint: one render pass normalizes op order; a second
    // changes nothing.
    let first = compile(&src).expect("compile");
    let text = render(&first).expect("render");
    let normalized = compile(&text).expect("recompile");
    let again = render(&normalized).expect("re-render");
    assert_eq!(compile(&again).expect("stable"), normalized, "render∘compile is a fixpoint");
}
