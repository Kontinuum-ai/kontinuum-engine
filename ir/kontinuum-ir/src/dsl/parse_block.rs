//! Section-block parsing: `section <id> { … }` headers and the statements
//! legal inside them (`bars`, `energy`, assignments, the `E` shorthand).

use super::ast::{Assign, Inner, Stmt};
use super::error::{DslCode, DslError};
use super::lex::Tok;
use super::parse::Parser;

impl Parser {
    pub(super) fn section(&mut self, line: usize) -> Result<Stmt, Vec<DslError>> {
        let id = match self.bump() {
            Tok::Ident(id) => id,
            other => {
                return Err(self.fatal(
                    DslCode::E_DSL_UNEXPECTED_TOKEN,
                    "/source",
                    format!("`section` must be followed by an id, found `{other:?}`"),
                    "name the section, e.g. `section drop { bars 8, energy 0.9 }`",
                ));
            }
        };
        match self.bump() {
            Tok::LBrace => {}
            other => {
                return Err(self.fatal(
                    DslCode::E_DSL_UNEXPECTED_TOKEN,
                    "/source",
                    format!("`section {id}` must open a block with `{{`, found `{other:?}`"),
                    format!("write `section {id} {{ … }}`"),
                ));
            }
        }
        let mut body = Vec::new();
        loop {
            match self.peek().clone() {
                Tok::RBrace => {
                    self.bump();
                    break;
                }
                Tok::Eof => {
                    return Err(self.fatal(
                        DslCode::E_DSL_UNCLOSED_BRACE,
                        format!("/sections/{id}"),
                        format!("section `{id}` is never closed"),
                        "add the closing `}`",
                    ));
                }
                Tok::Semi | Tok::Comma => {
                    self.bump();
                }
                Tok::Ident(name) if name == "section" => {
                    return Err(self.fatal(
                        DslCode::E_DSL_NESTED_SECTION,
                        format!("/sections/{id}"),
                        "sections cannot nest in v0",
                        "close the current section with `}` before starting a new one",
                    ));
                }
                Tok::Ident(head) => {
                    if let Some(i) = self.inner_statement(head, &id)? {
                        body.push(i);
                    }
                },
                other => {
                    return Err(self.fatal(
                        DslCode::E_DSL_UNEXPECTED_TOKEN,
                        format!("/sections/{id}"),
                        format!("unexpected `{other:?}` inside section `{id}`"),
                        "sections contain `bars`, `energy`, pattern, and param statements",
                    ));
                }
            }
        }
        Ok(Stmt::Section { id, line, body })
    }

    /// One statement inside a section block.
    fn inner_statement(
        &mut self,
        head: String,
        section: &str,
    ) -> Result<Option<Inner>, Vec<DslError>> {
        let line = self.line();
        self.bump();
        if head == "bars" {
            let bars = match self.bump() {
                Tok::Int(v) => v,
                other => {
                    return Err(self.fatal(
                        DslCode::E_DSL_UNEXPECTED_TOKEN,
                        format!("/sections/{section}/bars"),
                        format!("`bars` takes an integer, found `{other:?}`"),
                        "write a whole number of bars, e.g. `bars 8`",
                    ));
                }
            };
            self.expect_terminator(true)?;
            return Ok(Some(Inner::Bars { bars, line }));
        }
        if head == "energy" {
            let value = self.float_value(&format!("/sections/{section}/energy_curve"))?;
            self.expect_terminator(true)?;
            return Ok(Some(Inner::Energy { value, line }));
        }
        match self.peek() {
            Tok::Dot => {
                let a = self.assignment(head, line, Some(section))?;
                self.expect_terminator(true)?;
                Ok(a.map(|a| match a {
                    Assign::Mask { track, bits, line } => Inner::Mask { track, bits, line },
                    Assign::Vel { track, values, line } => Inner::Vel { track, values, line },
                    Assign::Param { track, param, value, line } => {
                        Inner::Param { track, param, value, line }
                    }
                }))
            }
            Tok::Colon => {
                let e = self.euclid_shorthand(head, line)?;
                self.expect_terminator(true)?;
                Ok(Some(e))
            }
            _ => Err(self.fatal(
                DslCode::E_DSL_UNKNOWN_STATEMENT,
                format!("/sections/{section}"),
                format!("cannot parse `{head}` inside section `{section}`"),
                "sections contain `bars`, `energy`, pattern, and param statements",
            )),
        }
    }
}
