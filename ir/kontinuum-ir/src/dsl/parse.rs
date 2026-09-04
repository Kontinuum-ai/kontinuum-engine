//! Recursive-descent parser core: token stream → statement AST. This file
//! owns the cursor, the program loop, and top-level statement dispatch;
//! `parse_block` parses section bodies and `parse_assign` parses
//! `track.field` assignments and the `E(k, n, rot)` shorthand.
//!
//! Grammar-shape violations are fatal; well-formed statements with bad
//! values stay in the AST and are judged by the emitter (semantic errors,
//! collected per line).
//!
//! Section scoping: `section <id> { … }` opens a block; pattern statements
//! (`mask`/`vel`/`E`) are section-scoped. Parameter statements
//! (`track.param = value;`) are track-level and legal anywhere. `bars` and
//! `energy` are section fields.

use super::ast::{Assign, Inner, Stmt};
use super::error::{dsl_err, DslCode, DslError};
use super::lex::{lex, Tok, Token};

/// Parses a program. `Ok((stmts, semantic))` carries collected semantic
/// errors; `Err` is fatal grammar errors (plus any semantics gathered so
/// far, fatal last).
pub fn parse(src: &str) -> Result<(Vec<Stmt>, Vec<DslError>), Vec<DslError>> {
    let tokens = lex(src)?;
    Parser { tokens, pos: 0, semantic: Vec::new() }.program()
}

pub(super) struct Parser {
    pub(super) tokens: Vec<Token>,
    pub(super) pos: usize,
    pub(super) semantic: Vec<DslError>,
}

impl Parser {
    pub(super) fn peek(&self) -> &Tok {
        &self.tokens[self.pos.min(self.tokens.len() - 1)].tok
    }

    pub(super) fn line(&self) -> usize {
        self.tokens[self.pos.min(self.tokens.len() - 1)].line
    }

    pub(super) fn bump(&mut self) -> Tok {
        let t = self.tokens[self.pos.min(self.tokens.len() - 1)].clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        t.tok
    }

    /// Fatal errors carry the semantics collected so far, fatal last.
    pub(super) fn fatal(
        &mut self,
        code: &'static str,
        path: impl Into<String>,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) -> Vec<DslError> {
        let line = self.line();
        let mut errs = std::mem::take(&mut self.semantic);
        errs.push(dsl_err(code, line, path, message, fix));
        errs
    }

    pub(super) fn semantic_err(
        &mut self,
        code: &'static str,
        path: impl Into<String>,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) {
        let line = self.line();
        self.semantic.push(dsl_err(code, line, path, message, fix));
    }

    /// A statement must end in `;` or `,` unless the block closes or the
    /// file ends.
    pub(super) fn expect_terminator(&mut self, in_block: bool) -> Result<(), Vec<DslError>> {
        match self.peek() {
            Tok::Semi | Tok::Comma => {
                while matches!(self.peek(), Tok::Semi | Tok::Comma) {
                    self.bump();
                }
                Ok(())
            }
            Tok::RBrace if in_block => Ok(()),
            Tok::Eof => Ok(()),
            _ => Err(self.fatal(
                DslCode::E_DSL_EXPECT_TERMINATOR,
                "/source",
                "statement is not terminated",
                "end the statement with `;` (or `,` inside a section block)",
            )),
        }
    }

    fn program(&mut self) -> Result<(Vec<Stmt>, Vec<DslError>), Vec<DslError>> {
        let mut stmts = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::Eof => break,
                Tok::Semi | Tok::Comma => {
                    self.bump();
                }
                Tok::Ident(head) => {
                    if let Some(s) = self.top_statement(head)? {
                        stmts.push(s);
                    }
                }
                other => {
                    return Err(self.fatal(
                        DslCode::E_DSL_UNEXPECTED_TOKEN,
                        "/source",
                        format!("expected a statement, found `{other:?}`"),
                        "start a statement with `section`, `track.param = …;`, or `track: E(…);`",
                    ));
                }
            }
        }
        Ok((std::mem::take(&mut stmts), std::mem::take(&mut self.semantic)))
    }

    /// One top-level statement. `Ok(None)` = semantic error recorded and
    /// resynced; the program continues.
    fn top_statement(&mut self, head: String) -> Result<Option<Stmt>, Vec<DslError>> {
        let line = self.line();
        self.bump();
        if head == "section" {
            return Ok(Some(self.section(line)?));
        }
        if head == "bars" || head == "energy" {
            self.semantic_err(
                DslCode::E_DSL_FIELD_OUTSIDE_SECTION,
                "/sections",
                format!("`{head}` is a section field"),
                format!("move `{head}` inside a `section <id> {{ … }}` block"),
            );
            self.skip_statement();
            return Ok(None);
        }
        match self.peek() {
            Tok::Dot => match self.assignment(head, line, None)? {
                Some(Assign::Param { track, param, value, line }) => {
                    self.expect_terminator(false)?;
                    Ok(Some(Stmt::Param { track, param, value, line }))
                }
                Some(a) => {
                    self.record_pattern_outside_section(a.track());
                    self.expect_terminator(false)?;
                    Ok(None)
                }
                None => {
                    self.expect_terminator(false)?;
                    Ok(None)
                }
            },
            Tok::Colon => {
                if let Inner::Euclid { track, .. } = &self.euclid_shorthand(head, line)? {
                    self.record_pattern_outside_section(track);
                }
                self.expect_terminator(false)?;
                Ok(None)
            }
            _ => Err(self.fatal(
                DslCode::E_DSL_UNKNOWN_STATEMENT,
                "/source",
                format!("cannot parse statement starting at `{head}`"),
                "use `section <id> {{ … }}`, `track.mask = 0b…;`, `track.vel = […];`, \
                 `track: E(k, n, rot);`, or `track.param = value;`",
            )),
        }
    }

    fn record_pattern_outside_section(&mut self, track: &str) {
        self.semantic_err(
            DslCode::E_DSL_PATTERN_OUTSIDE_SECTION,
            format!("/sections/*/pattern_bindings/{track}"),
            format!("pattern statement for `{track}` is section-scoped"),
            "move the statement inside a `section <id> { … }` block",
        );
    }

    /// Resyncs past a skipped statement: everything up to and including the
    /// next terminator — or up to (not past) `}`/EOF, which stay in place.
    fn skip_statement(&mut self) {
        loop {
            match self.peek() {
                Tok::Semi | Tok::Comma => {
                    self.bump();
                    return;
                }
                Tok::RBrace | Tok::Eof => return,
                _ => {
                    self.bump();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semantic(src: &str) -> Vec<DslError> {
        parse(src).expect("parses").1
    }

    #[test]
    fn accepts_section_blocks_and_track_level_params() {
        let (stmts, errs) = parse(
            "section drop {\n  bars 8,\n  energy 0.9,\n  kick.mask = 0b1000;\n  hat: E(3, 16, 2) @ swing 0.14;\n}\nbass.cutoff_hz = 120.0;\n",
        )
        .expect("parses");
        assert!(errs.is_empty());
        assert_eq!(stmts.len(), 2);
        match &stmts[0] {
            Stmt::Section { id, body, .. } => {
                assert_eq!(id, "drop");
                assert_eq!(body.len(), 4);
            }
            other => panic!("expected section, got {other:?}"),
        }
    }

    #[test]
    fn section_scoped_violations_are_collected_not_fatal() {
        let errs = semantic("kick.mask = 0b1000;\nbars 4;\nbass.woozle = 1.0;\n");
        let mut codes: Vec<_> = errs.iter().map(|e| e.code).collect();
        codes.sort_unstable();
        assert_eq!(
            codes,
            vec![
                DslCode::E_DSL_FIELD_OUTSIDE_SECTION,
                DslCode::E_DSL_PATTERN_OUTSIDE_SECTION,
                DslCode::E_DSL_UNKNOWN_PARAM,
            ]
        );
        // The program keeps going: the valid tail statement still parses.
        let (stmts, _) = parse("bass.woozle = 1.0;\nbass.cutoff_hz = 120.0;\n").expect("parses");
        assert_eq!(stmts.len(), 1, "only the valid param survives");
    }

    #[test]
    fn grammar_breaks_are_fatal() {
        assert_eq!(
            parse("section 3 { }").expect_err("no id")[0].code,
            DslCode::E_DSL_UNEXPECTED_TOKEN
        );
        assert_eq!(
            parse("section a { bars 4").expect_err("unclosed")[0].code,
            DslCode::E_DSL_UNCLOSED_BRACE
        );
        assert_eq!(
            parse("section a { section b { } }").expect_err("nested")[0].code,
            DslCode::E_DSL_NESTED_SECTION
        );
        assert_eq!(
            parse("section a { bars 4 energy 0.5 }").expect_err("no terminator")[0].code,
            DslCode::E_DSL_EXPECT_TERMINATOR
        );
        assert_eq!(
            parse("nonsense;").expect_err("unknown head")[0].code,
            DslCode::E_DSL_UNKNOWN_STATEMENT
        );
    }

    #[test]
    fn line_numbers_point_at_the_error() {
        let errs = semantic("# comment\n\nhat.vel = [2.0];\n");
        assert_eq!(errs[0].line, 3);
        assert!(errs[0].suggested_fix.contains("section"), "suggested_fix is actionable");
    }
}
