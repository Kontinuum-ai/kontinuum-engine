//! Assignment parsing: `track.<field> = …` (mask / vel / instrument param)
//! and the `track: E(k, n, rot) [@ swing s]` shorthand, plus the operand
//! parsers they share.

use super::ast::{pattern_path, Assign, Inner};
use super::error::{DslCode, DslError, KNOWN_PARAMS};
use super::lex::Tok;
use super::parse::Parser;

impl Parser {
    /// Parses `track.<field> = …` after the track ident was consumed.
    /// `section` is `Some` inside a block, `None` at top level; it only
    /// shapes error paths. `Ok(None)` = semantic error recorded, skip.
    pub(super) fn assignment(
        &mut self,
        track: String,
        line: usize,
        section: Option<&str>,
    ) -> Result<Option<Assign>, Vec<DslError>> {
        self.bump(); // Dot
        let field = match self.bump() {
            Tok::Ident(f) => f,
            other => {
                return Err(self.fatal(
                    DslCode::E_DSL_UNEXPECTED_TOKEN,
                    pattern_path(section, &track),
                    format!("expected a field after `{track}.`, found `{other:?}`"),
                    "use `mask`, `vel`, or a param name after the dot",
                ));
            }
        };
        if field == "mask" {
            self.expect_eq(
                pattern_path(section, &track),
                format!("write `{track}.mask = 0b1000;`"),
            )?;
            let bits = match self.bump() {
                Tok::Bin(bits) => bits,
                other => {
                    return Err(self.fatal(
                        DslCode::E_DSL_UNEXPECTED_TOKEN,
                        pattern_path(section, &track),
                        format!("`mask` takes a binary literal, found `{other:?}`"),
                        "write a `0b…` literal, e.g. `0b1000_1000_1000_1000`",
                    ));
                }
            };
            return Ok(Some(Assign::Mask { track, bits, line }));
        }
        if field == "vel" {
            self.expect_eq(
                pattern_path(section, &track),
                format!("write `{track}.vel = [1.0, 0.5, …];`"),
            )?;
            let values = self.float_list(&pattern_path(section, &track))?;
            return Ok(Some(Assign::Vel { track, values, line }));
        }
        self.expect_eq(
            format!("/tracks/{track}/instrument/{field}"),
            format!("write `{track}.{field} = <value>;`"),
        )?;
        let value = self.float_value(&format!("/tracks/{track}/instrument/{field}"))?;
        if !KNOWN_PARAMS.contains(&field.as_str()) {
            self.semantic_err(
                DslCode::E_DSL_UNKNOWN_PARAM,
                format!("/tracks/{track}/instrument/{field}"),
                format!("`{field}` is not an IR instrument param"),
                format!("use one of: {}", KNOWN_PARAMS.join(", ")),
            );
            return Ok(None);
        }
        Ok(Some(Assign::Param { track, param: field, value, line }))
    }

    fn expect_eq(&mut self, path: impl Into<String>, fix: String) -> Result<(), Vec<DslError>> {
        match self.bump() {
            Tok::Eq => Ok(()),
            other => Err(self.fatal(
                DslCode::E_DSL_UNEXPECTED_TOKEN,
                path,
                format!("expected `=`, found `{other:?}`"),
                fix,
            )),
        }
    }

    /// Parses `track: E(k, n, rot)` (plus optional `@ swing s`) after the
    /// track ident was consumed.
    pub(super) fn euclid_shorthand(&mut self, track: String, line: usize) -> Result<Inner, Vec<DslError>> {
        self.bump(); // Colon
        match self.bump() {
            Tok::Ident(name) if name == "E" => {}
            other => {
                return Err(self.fatal(
                    DslCode::E_DSL_UNKNOWN_STATEMENT,
                    "/source",
                    format!("`{track}:` must be followed by the `E(k, n, rot)` shorthand, found `{other:?}`"),
                    format!("write `{track}: E(5, 16, 2);`"),
                ));
            }
        }
        match self.bump() {
            Tok::LParen => {}
            other => {
                return Err(self.fatal(
                    DslCode::E_DSL_UNEXPECTED_TOKEN,
                    "/source",
                    format!("`E` must open a paren list, found `{other:?}`"),
                    format!("write `{track}: E(5, 16, 2);`"),
                ));
            }
        }
        let k = self.int_arg()?;
        self.expect_comma()?;
        let n = self.int_arg()?;
        self.expect_comma()?;
        let rot = self.int_arg()?;
        match self.bump() {
            Tok::RParen => {}
            other => {
                return Err(self.fatal(
                    DslCode::E_DSL_UNEXPECTED_TOKEN,
                    "/source",
                    format!("`E` takes exactly three arguments, found trailing `{other:?}`"),
                    "write `E(k, n, rot)` with three integers",
                ));
            }
        }
        let mut swing = None;
        if matches!(self.peek(), Tok::At) {
            self.bump();
            match self.bump() {
                Tok::Ident(name) if name == "swing" => {}
                other => {
                    return Err(self.fatal(
                        DslCode::E_DSL_UNKNOWN_STATEMENT,
                        "/source",
                        format!("`@` must be followed by `swing`, found `{other:?}`"),
                        format!("write `{track}: E(5, 16, 2) @ swing 0.14;`"),
                    ));
                }
            }
            swing = Some(self.float_value("/source")?);
        }
        Ok(Inner::Euclid { track, k, n, rot, swing, line })
    }

    fn int_arg(&mut self) -> Result<i64, Vec<DslError>> {
        match self.bump() {
            Tok::Int(v) => Ok(v),
            other => Err(self.fatal(
                DslCode::E_DSL_UNEXPECTED_TOKEN,
                "/source",
                format!("`E` takes integer arguments, found `{other:?}`"),
                "write whole numbers, e.g. `E(5, 16, 2)`",
            )),
        }
    }

    fn expect_comma(&mut self) -> Result<(), Vec<DslError>> {
        match self.bump() {
            Tok::Comma => Ok(()),
            other => Err(self.fatal(
                DslCode::E_DSL_UNEXPECTED_TOKEN,
                "/source",
                format!("expected `,` between arguments, found `{other:?}`"),
                "separate `E` arguments with commas: `E(5, 16, 2)`",
            )),
        }
    }

    pub(super) fn float_value(&mut self, path: &str) -> Result<f32, Vec<DslError>> {
        match self.bump() {
            Tok::Float(v) => Ok(v),
            Tok::Int(v) => Ok(v as f32),
            other => Err(self.fatal(
                DslCode::E_DSL_UNEXPECTED_TOKEN,
                path,
                format!("expected a number, found `{other:?}`"),
                "write a decimal number, e.g. `0.7`",
            )),
        }
    }

    fn float_list(&mut self, path: &str) -> Result<Vec<f32>, Vec<DslError>> {
        match self.bump() {
            Tok::LBracket => {}
            other => {
                return Err(self.fatal(
                    DslCode::E_DSL_UNEXPECTED_TOKEN,
                    path,
                    format!("expected a bracket list, found `{other:?}`"),
                    "write `[v0, v1, …]`",
                ));
            }
        }
        let mut values = Vec::new();
        loop {
            match self.peek() {
                Tok::RBracket => {
                    self.bump();
                    break;
                }
                Tok::Eof => {
                    return Err(self.fatal(
                        DslCode::E_DSL_UNCLOSED_BRACE,
                        path,
                        "velocity list is never closed",
                        "add the closing `]`",
                    ));
                }
                Tok::Comma => {
                    self.bump();
                }
                _ => values.push(self.float_value(path)?),
            }
        }
        Ok(values)
    }
}
