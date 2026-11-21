//! Parse events: what a parser reports as it reads an XML document, in
//! document order.
//!
//! This is the interface for streaming consumers such as SAX callbacks. For
//! now the events come from the tree parser ([`parse_str`]): it builds the
//! document as usual, reporting each node as it creates it and each warning
//! or error as it occurs.
//!
//! # Examples
//!
//! ```
//! use xmloxide::events::{parse_str, Event};
//! use xmloxide::parser::ParseOptions;
//!
//! let mut names = Vec::new();
//! parse_str("<r><a/><b/></r>", &ParseOptions::default(), &mut |event: Event<'_>, _at| {
//!     if let Event::StartElement { name, .. } = event {
//!         names.push(name.to_owned());
//!     }
//! })
//! .unwrap();
//! assert_eq!(names, ["r", "a", "b"]);
//! ```

use crate::error::{ErrorSeverity, ParseDiagnostic, ParseError, SourceLocation};
use crate::html::HtmlParseOptions;
use crate::parser::ParseOptions;
use crate::tree::Attribute;

/// One thing the parser found, borrowed from the parser for the duration of
/// the call.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// The XML declaration. Reported first, before `StartDocument`.
    XmlDeclaration {
        version: &'a str,
        encoding: Option<&'a str>,
        standalone: Option<bool>,
    },
    /// The start of the document, after the XML declaration if there is one.
    StartDocument,
    /// A start tag or an empty-element tag. The attributes are as the tree
    /// stores them, in document order, namespace declarations (`xmlns` and
    /// `xmlns:p`) included.
    StartElement {
        name: &'a str,
        prefix: Option<&'a str>,
        namespace: Option<&'a str>,
        attributes: &'a [Attribute],
    },
    /// An end tag, or the end of an empty-element tag.
    EndElement {
        name: &'a str,
        prefix: Option<&'a str>,
        namespace: Option<&'a str>,
    },
    /// Character data, with references to character and predefined
    /// entities replaced.
    Text(&'a str),
    /// The content of a CDATA section.
    CData(&'a str),
    /// The content of an HTML element whose content is raw text (`script`,
    /// `style`), as it is in the input.
    RawText(&'a str),
    /// The content of a comment.
    Comment(&'a str),
    /// A processing instruction.
    ProcessingInstruction {
        target: &'a str,
        data: Option<&'a str>,
    },
    /// A reference to a general entity other than the predefined ones,
    /// reported after the events for its replacement text. The replacement
    /// text is `None` if it isn't known: the entity is undeclared, or
    /// external and not loaded.
    EntityReference {
        name: &'a str,
        replacement: Option<&'a str>,
    },
    /// A warning or error, where it occurs. A fatal error is the last event
    /// before `EndDocument`.
    Diagnostic(&'a ParseDiagnostic),
    /// The end of the document: always the last event, after a fatal error
    /// too.
    EndDocument,
}

/// Receives parse events.
pub trait EventHandler {
    /// Called for each event, in document order, with where the parser is
    /// in the input: for a start tag, at its closing `>` (or `/>`).
    fn event(&mut self, event: Event<'_>, location: SourceLocation);
}

impl<F: FnMut(Event<'_>, SourceLocation)> EventHandler for F {
    fn event(&mut self, event: Event<'_>, location: SourceLocation) {
        self(event, location);
    }
}

/// Parses `input`, reporting what it finds to `handler`.
///
/// # Errors
///
/// Returns the fatal error that stopped the parse, which `handler` has
/// already seen as a [`Event::Diagnostic`].
pub fn parse_str(
    input: &str,
    options: &ParseOptions,
    handler: &mut dyn EventHandler,
) -> Result<(), ParseError> {
    crate::parser::parse_str_reporting(input, options, handler)
}

/// Parses `input` as HTML, reporting what it finds to `handler`. Elements
/// are reported as they are opened and closed, implied ones included.
///
/// # Errors
///
/// Returns the fatal error that stopped the parse, which `handler` has
/// already seen as a [`Event::Diagnostic`].
pub fn parse_html(
    input: &str,
    options: &HtmlParseOptions,
    handler: &mut dyn EventHandler,
) -> Result<(), ParseError> {
    crate::html::parse_html_reporting(input, options, handler)
}

/// Where a parser reports its events, if anywhere, and how many of the
/// parser's diagnostics it has reported.
pub(crate) struct Reporter<'h> {
    handler: Option<&'h mut dyn EventHandler>,
    reported: usize,
}

impl<'h> Reporter<'h> {
    pub(crate) fn new(handler: Option<&'h mut dyn EventHandler>) -> Self {
        Reporter {
            handler,
            reported: 0,
        }
    }

    /// Reports `event` at `location`, after the `diagnostics` not yet
    /// reported.
    pub(crate) fn event(
        &mut self,
        diagnostics: &[ParseDiagnostic],
        event: Event<'_>,
        location: SourceLocation,
    ) {
        self.diagnostics(diagnostics);
        if let Some(handler) = self.handler.as_deref_mut() {
            handler.event(event, location);
        }
    }

    /// Reports the `diagnostics` not yet reported.
    pub(crate) fn diagnostics(&mut self, diagnostics: &[ParseDiagnostic]) {
        if let (Some(handler), Some(new)) = (
            self.handler.as_deref_mut(),
            diagnostics.get(self.reported..),
        ) {
            for diagnostic in new {
                handler.event(Event::Diagnostic(diagnostic), diagnostic.location);
            }
            self.reported = diagnostics.len();
        }
    }

    /// Lends the handler to the parser of an entity's replacement text,
    /// which has diagnostics of its own.
    pub(crate) fn lend(&mut self) -> Reporter<'h> {
        Reporter::new(self.handler.take())
    }

    /// Takes the handler back from `sub`, once the parser's `diagnostics`,
    /// `sub`'s included, have all been reported.
    pub(crate) fn take_back(&mut self, sub: &mut Reporter<'h>, diagnostics: &[ParseDiagnostic]) {
        self.handler = sub.handler.take();
        self.reported = diagnostics.len();
    }

    /// Reports how a parse ended: the `diagnostics` not yet reported, the
    /// fatal error that stopped it, if one did, then the end of the
    /// document, at `location`.
    pub(crate) fn finish(
        &mut self,
        diagnostics: &[ParseDiagnostic],
        error: Option<&ParseError>,
        location: SourceLocation,
    ) {
        self.diagnostics(diagnostics);
        let Some(handler) = self.handler.take() else {
            return;
        };
        if let Some(error) = error {
            let fatal = ParseDiagnostic {
                severity: ErrorSeverity::Fatal,
                message: error.message.clone(),
                location: error.location,
            };
            handler.event(Event::Diagnostic(&fatal), error.location);
        }
        handler.event(Event::EndDocument, location);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// The events for `input`, one line each, and whether the parse failed.
    fn events(input: &str, options: &ParseOptions) -> (Vec<String>, bool) {
        lines_of(|handler| parse_str(input, options, handler))
    }

    /// The events for HTML `input`, one line each.
    fn html_events(input: &str) -> Vec<String> {
        lines_of(|handler| parse_html(input, &HtmlParseOptions::default(), handler)).0
    }

    /// The name, line and column of each start tag in `input`.
    fn start_locations(input: &str) -> Vec<(String, u32, u32)> {
        let mut starts = Vec::new();
        parse_str(
            input,
            &ParseOptions::default(),
            &mut |event: Event<'_>, at: SourceLocation| {
                if let Event::StartElement { name, .. } = event {
                    starts.push((name.to_owned(), at.line, at.column));
                }
            },
        )
        .unwrap();
        starts
    }

    /// The events `parse` reports, one line each, and whether it failed.
    fn lines_of(
        parse: impl FnOnce(&mut dyn EventHandler) -> Result<(), ParseError>,
    ) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        let result = parse(&mut |event: Event<'_>, _at| {
            let name = |prefix: Option<&str>, name: &str, namespace: Option<&str>| {
                let qname = prefix.map_or(name.to_owned(), |p| format!("{p}:{name}"));
                namespace.map_or(qname.clone(), |ns| format!("{qname}{{{ns}}}"))
            };
            lines.push(match event {
                Event::XmlDeclaration {
                    version,
                    encoding,
                    standalone,
                } => format!("decl {version} {encoding:?} {standalone:?}"),
                Event::StartDocument => "start-document".to_owned(),
                Event::StartElement {
                    name: local,
                    prefix,
                    namespace,
                    attributes,
                } => {
                    let attributes = attributes.iter().map(|a| {
                        let qname = name(a.prefix.as_deref(), &a.name, a.namespace.as_deref());
                        format!(" {qname}={}", a.value)
                    });
                    format!("start {}", name(prefix, local, namespace))
                        + &attributes.collect::<String>()
                }
                Event::EndElement {
                    name: local,
                    prefix,
                    namespace,
                } => format!("end {}", name(prefix, local, namespace)),
                Event::Text(text) => format!("text {text:?}"),
                Event::CData(text) => format!("cdata {text:?}"),
                Event::RawText(text) => format!("raw-text {text:?}"),
                Event::Comment(text) => format!("comment {text:?}"),
                Event::ProcessingInstruction { target, data } => format!("pi {target} {data:?}"),
                Event::EntityReference { name, replacement } => {
                    format!("reference {name} {replacement:?}")
                }
                Event::Diagnostic(d) => {
                    let level = match d.severity {
                        ErrorSeverity::Warning => "warning",
                        ErrorSeverity::Error => "error",
                        ErrorSeverity::Fatal => "fatal",
                    };
                    format!("{level} {}", d.message)
                }
                Event::EndDocument => "end-document".to_owned(),
            });
        });
        (lines, result.is_err())
    }

    #[test]
    fn test_parse_str_reports_events_in_document_order() {
        let (lines, failed) = events(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<!--before--><r a="1" xmlns:x="u" x:b="2"><x:e/>t&amp;u<![CDATA[c]]><!--m--><?p d?></r>"#,
            &ParseOptions::default(),
        );
        assert!(!failed);
        assert_eq!(
            lines,
            [
                r#"decl 1.0 Some("UTF-8") Some(true)"#,
                "start-document",
                r#"comment "before""#,
                "start r a=1 xmlns:x=u x:b{u}=2",
                "start x:e{u}",
                "end x:e{u}",
                r#"text "t&u""#,
                r#"cdata "c""#,
                r#"comment "m""#,
                r#"pi p Some("d")"#,
                "end r",
                "end-document",
            ]
        );
    }

    #[test]
    fn test_parse_str_reports_an_entity_reference_after_its_text() {
        let (lines, _) = events(
            r#"<!DOCTYPE r [<!ENTITY t "q"><!ENTITY m "x<b>y</b>">]><r>&t;&m;.</r>"#,
            &ParseOptions::default(),
        );
        assert_eq!(
            lines,
            [
                "start-document",
                "start r",
                r#"text "q""#,
                r#"reference t Some("q")"#,
                r#"text "x""#,
                "start b",
                r#"text "y""#,
                "end b",
                r#"reference m Some("x<b>y</b>")"#,
                r#"text ".""#,
                "end r",
                "end-document",
            ]
        );
    }

    #[test]
    fn test_parse_str_reports_a_diagnostic_where_it_occurs() {
        let (lines, failed) = events("<r><a>&u;</a></r>", &ParseOptions::default().recover(true));
        assert!(!failed);
        assert_eq!(
            lines,
            [
                "start-document",
                "start r",
                "start a",
                "warning unknown entity reference: &u;",
                "reference u None",
                "end a",
                "end r",
                "end-document",
            ]
        );
    }

    #[test]
    fn test_parse_str_locates_a_start_tag_at_its_end() {
        let input = "<hello>\n\nworld\n<inter>\n    <net>\n    </net>\n</inter>\n</hello>";
        assert_eq!(
            start_locations(input),
            [
                ("hello".to_owned(), 1, 7),
                ("inter".to_owned(), 4, 7),
                ("net".to_owned(), 5, 9)
            ]
        );
    }

    #[test]
    fn test_parse_html_reports_elements_as_they_open_and_close() {
        let lines = html_events(concat!(
            r#"<html lang="en"><head><title>T</title></head><body class="b">"#,
            "<p>a<p>b<br>c</b><script>x<y</script><!--m--></body></html>",
        ));
        assert_eq!(
            lines,
            [
                "start-document",
                "start html lang=en",
                "start head",
                "start title",
                r#"text "T""#,
                "end title",
                "end head",
                "start body class=b",
                "start p",
                r#"text "a""#,
                "end p",
                "start p",
                r#"text "b""#,
                "start br",
                "end br",
                r#"text "c""#,
                "warning stray end tag </b>",
                "start script",
                r#"raw-text "x<y""#,
                "end script",
                r#"comment "m""#,
                "warning implicitly closing <p> before </body>",
                "end p",
                "end body",
                "end html",
                "end-document",
            ]
        );
    }

    #[test]
    fn test_parse_html_reports_implied_elements_and_closes_them_at_the_end() {
        let lines = html_events("  <p>x</p>  <head><title>a</title>");
        let lines: Vec<_> = lines.iter().filter(|l| !l.starts_with("warning")).collect();
        assert_eq!(
            lines,
            [
                "start-document",
                "start html",
                "start body",
                "start p",
                r#"text "x""#,
                "end p",
                r#"text "  ""#,
                "start title",
                r#"text "a""#,
                "end title",
                "end body",
                "end html",
                "end-document",
            ]
        );
    }

    #[test]
    fn test_parse_str_locates_entity_text_within_the_entity() {
        // As libxml2 does, and as the replacement text's diagnostics are.
        let input = "<!DOCTYPE r [<!ENTITY m \"x<b>y</b>\">]>\n\n\n<r>&m;</r>";
        assert_eq!(
            start_locations(input),
            [("r".to_owned(), 4, 3), ("b".to_owned(), 1, 4)]
        );
    }

    #[test]
    fn test_parse_html_reports_each_implied_close_after_its_warning() {
        let lines = html_events("<div><b><i>x</div>");
        let from = lines.iter().position(|l| l == r#"text "x""#).unwrap();
        assert_eq!(
            lines[from..from + 6],
            [
                r#"text "x""#,
                "warning implicitly closing <i> before </div>",
                "end i",
                "warning implicitly closing <b> before </div>",
                "end b",
                "end div",
            ]
        );
    }

    #[test]
    fn test_parse_str_ends_with_a_fatal_error() {
        let (lines, failed) = events("<r><a></r>", &ParseOptions::default());
        assert!(failed);
        assert_eq!(
            lines,
            [
                "start-document",
                "start r",
                "start a",
                "fatal mismatched end tag: expected </a>, found </r>",
                "end-document",
            ]
        );
    }
}
