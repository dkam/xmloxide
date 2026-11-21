//! Error-tolerant HTML parser.
//!
//! This module implements an error-tolerant HTML 4.01 parser, similar to
//! libxml2's `HTMLparser.c`. Unlike the strict XML parser, this parser handles
//! common HTML patterns that are technically malformed:
//!
//! - Missing closing tags (auto-closed based on HTML content model rules)
//! - Unquoted attribute values (`<div class=main>`)
//! - Void elements that never need closing (`<br>`, `<img>`, `<hr>`, etc.)
//! - Case-insensitive tag name matching
//! - Bare `&` characters (not just `&amp;`)
//! - Missing doctype
//! - Boolean attributes without values (`<input disabled>`)
//!
//! The parser produces the same `Document` tree structure as the XML parser.
//!
//! # Examples
//!
//! ```
//! use xmloxide::html::parse_html;
//!
//! let doc = parse_html("<p>Hello <b>world</b>").unwrap();
//! let root = doc.root_element().unwrap();
//! assert_eq!(doc.node_name(root), Some("html"));
//! ```

pub mod entities;

use crate::error::{ErrorSeverity, ParseDiagnostic, ParseError};
use crate::events::{Event, EventHandler, Reporter};
use crate::parser::input::ParserInput;
use crate::tree::{Attribute, Document, NodeId, NodeKind};

/// Options controlling HTML parser behavior.
///
/// Use the builder pattern to configure options:
///
/// ```
/// use xmloxide::html::HtmlParseOptions;
///
/// let opts = HtmlParseOptions::default()
///     .recover(true)
///     .no_blanks(true)
///     .no_implied(true);
/// ```
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct HtmlParseOptions {
    /// If true, attempt to recover from errors and produce a partial tree.
    /// This is always effectively true for HTML parsing since HTML is
    /// inherently error-tolerant, but setting this to false makes the parser
    /// stricter about certain issues.
    pub recover: bool,
    /// If true, strip ignorable whitespace-only text nodes.
    pub no_blanks: bool,
    /// If true, do not add implied `html`, `head`, and `body` elements.
    pub no_implied: bool,
    /// If true, suppress warning diagnostics.
    pub no_warnings: bool,
    /// If true, do not add the default DOCTYPE to a document without one
    /// (libxml2's `HTML_PARSE_NODEFDTD`).
    pub no_default_doctype: bool,
}

impl Default for HtmlParseOptions {
    fn default() -> Self {
        Self {
            recover: true,
            no_blanks: false,
            no_implied: false,
            no_warnings: false,
            no_default_doctype: false,
        }
    }
}

impl HtmlParseOptions {
    /// Enables or disables error recovery mode.
    ///
    /// When enabled, the parser attempts to produce a partial tree even when
    /// it encounters malformed markup, collecting errors as diagnostics on
    /// the resulting [`Document`]. HTML parsing is inherently error-tolerant,
    /// but disabling this makes the parser stricter about certain issues.
    /// Enabled by default.
    #[must_use]
    pub fn recover(mut self, yes: bool) -> Self {
        self.recover = yes;
        self
    }

    /// Enables or disables stripping of blank (whitespace-only) text nodes.
    ///
    /// When enabled, text nodes that contain only whitespace (spaces, tabs,
    /// newlines) are discarded during parsing. This is useful for reducing
    /// tree size when whitespace between elements is not significant.
    /// Disabled by default.
    #[must_use]
    pub fn no_blanks(mut self, yes: bool) -> Self {
        self.no_blanks = yes;
        self
    }

    /// Enables or disables generation of implied `<html>`, `<head>`, and
    /// `<body>` elements.
    ///
    /// By default, the HTML parser automatically wraps content in these
    /// structural elements when they are missing (matching browser behavior).
    /// When enabled, the parser omits these implied wrappers, producing a
    /// tree that more closely reflects the literal input.
    /// Disabled by default.
    #[must_use]
    pub fn no_implied(mut self, yes: bool) -> Self {
        self.no_implied = yes;
        self
    }

    /// Enables or disables suppression of warning-level diagnostics.
    ///
    /// When enabled, only errors and fatal issues are recorded in
    /// [`Document::diagnostics`]; warnings about non-critical issues (e.g.,
    /// missing optional closing tags) are silently discarded.
    /// Disabled by default.
    #[must_use]
    pub fn no_warnings(mut self, yes: bool) -> Self {
        self.no_warnings = yes;
        self
    }

    /// Enables or disables the default DOCTYPE.
    ///
    /// By default, a document whose input has no DOCTYPE gets HTML 4.0
    /// Transitional's, as libxml2 gives it. When enabled, it gets none, as
    /// with libxml2's `HTML_PARSE_NODEFDTD`. Disabled by default.
    #[must_use]
    pub fn no_default_doctype(mut self, yes: bool) -> Self {
        self.no_default_doctype = yes;
        self
    }
}

/// Parses an HTML string into a `Document` with default options.
///
/// The parser is error-tolerant and will produce a tree even for malformed
/// HTML. Diagnostics about any issues found during parsing are stored in
/// `Document::diagnostics`.
///
/// # Errors
///
/// Returns `ParseError` only for truly unrecoverable errors (e.g., empty input
/// when recovery is disabled).
///
/// # Examples
///
/// ```
/// use xmloxide::html::parse_html;
///
/// let doc = parse_html("<p>Hello <b>world</b>").unwrap();
/// let root = doc.root_element().unwrap();
/// assert_eq!(doc.node_name(root), Some("html"));
/// ```
pub fn parse_html(input: &str) -> Result<Document, ParseError> {
    parse_html_with_options(input, &HtmlParseOptions::default())
}

/// Parses an HTML string into a `Document` with the given options.
///
/// # Errors
///
/// Returns `ParseError` if the input cannot be parsed and recovery mode is
/// disabled.
///
/// # Examples
///
/// ```
/// use xmloxide::html::{parse_html_with_options, HtmlParseOptions};
///
/// let opts = HtmlParseOptions::default().no_blanks(true);
/// let doc = parse_html_with_options("<html><body><p>Hi</p></body></html>", &opts).unwrap();
/// ```
pub fn parse_html_with_options(
    input: &str,
    options: &HtmlParseOptions,
) -> Result<Document, ParseError> {
    let mut parser = HtmlParser::new(input, options);
    parser.parse()
}

/// Parses an HTML string, reporting each event to `handler` as it occurs
/// (see [`crate::events::parse_html`]).
pub(crate) fn parse_html_reporting(
    input: &str,
    options: &HtmlParseOptions,
    handler: &mut dyn EventHandler,
) -> Result<(), ParseError> {
    let mut parser = HtmlParser::new(input, options);
    parser.reporter = Reporter::new(Some(handler));
    let result = parser.parse().map(drop);
    let location = parser.input.location();
    parser
        .reporter
        .finish(&parser.doc.diagnostics, result.as_ref().err(), location);
    result
}

// --- Void elements (elements that must not have content) ---
// See HTML 4.01 and common usage. These elements are self-closing.

/// Returns true if the given tag name (lowercase) is a void element that
/// must not have content.
pub(crate) fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
            | "basefont"
            | "frame"
            | "isindex"
    )
}

/// Returns true if `tag` is an element whose opening auto-closes `open_tag`.
///
/// For example, a `<p>` auto-closes a previous `<p>`, and a `<li>` auto-closes
/// a previous `<li>`.
///
/// See HTML 4.01 DTD for the optional end tag rules.
fn auto_closes(open_tag: &str, tag: &str) -> bool {
    match open_tag {
        "p" => matches!(
            tag,
            "p" | "div"
                | "ul"
                | "ol"
                | "dl"
                | "pre"
                | "table"
                | "blockquote"
                | "address"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "hr"
                | "form"
                | "fieldset"
                | "section"
                | "article"
                | "aside"
                | "header"
                | "footer"
                | "nav"
                | "figure"
                | "figcaption"
                | "main"
                | "details"
                | "summary"
        ),
        "li" => tag == "li",
        "dt" => matches!(tag, "dt" | "dd"),
        "dd" => tag == "dt",
        "tr" => tag == "tr",
        "td" => matches!(tag, "td" | "th" | "tr"),
        "th" => matches!(tag, "td" | "th" | "tr"),
        "thead" => matches!(tag, "tbody" | "tfoot"),
        "tbody" => matches!(tag, "tbody" | "tfoot"),
        "tfoot" => tag == "tbody",
        "option" => matches!(tag, "option" | "optgroup"),
        "optgroup" => tag == "optgroup",
        "colgroup" => {
            // colgroup is auto-closed by most things that are not col
            tag != "col" && matches!(tag, "thead" | "tbody" | "tfoot" | "tr" | "colgroup")
        }
        "head" => matches!(tag, "body" | "frameset"),
        _ => false,
    }
}

/// Returns true if `tag` is a raw text element whose content is not parsed
/// as HTML (script, style).
pub(crate) fn is_raw_text_element(tag: &str) -> bool {
    matches!(tag, "script" | "style")
}

/// Returns true if `tag` is an element that belongs in `<head>`.
fn is_head_content_element(tag: &str) -> bool {
    matches!(
        tag,
        "title" | "meta" | "link" | "base" | "style" | "script" | "noscript"
    )
}

// --- The HTML Parser ---

/// The core HTML parser state machine.
///
/// Implements a hand-rolled, error-tolerant parser for HTML 4.01 that produces
/// a `Document` tree. Unlike the XML parser, this parser:
/// - Normalizes tag names to lowercase
/// - Handles void elements (self-closing elements like `<br>`)
/// - Auto-closes elements based on HTML content model rules
/// - Accepts unquoted and boolean attributes
/// - Resolves HTML named character references
/// - Adds implied elements (html, head, body) when missing
struct HtmlParser<'a, 'h> {
    /// Shared low-level input state (position, peek, advance, etc.).
    input: ParserInput<'a>,
    /// The document being built.
    doc: Document,
    /// Parser options.
    options: HtmlParseOptions,
    /// Stack of open element node IDs and their lowercase tag names.
    open_elements: Vec<(NodeId, String)>,
    /// Set when a fatal error (e.g. depth limit) occurs; stops parsing.
    fatal_error: Option<ParseError>,
    /// How many misplaced `<head>` start tags were dropped, whose end tags
    /// are dropped too (libxml2's `ctxt->depth`).
    dropped_heads: usize,
    /// Where to report parse events, if anywhere.
    reporter: Reporter<'h>,
}

impl<'a> HtmlParser<'a, '_> {
    fn new(input: &'a str, options: &HtmlParseOptions) -> Self {
        let mut pi = ParserInput::new(input);
        pi.set_recover(options.recover);

        Self {
            input: pi,
            doc: Document::new(),
            options: options.clone(),
            open_elements: Vec::new(),
            fatal_error: None,
            dropped_heads: 0,
            reporter: Reporter::new(None),
        }
    }

    /// Reports `event`, after any diagnostics not yet reported.
    fn emit(&mut self, event: Event<'_>) {
        let location = self.input.location();
        self.reporter.event(&self.doc.diagnostics, event, location);
    }

    /// Opens `element`, which `tag` names: it is where content goes until
    /// it is closed.
    fn open_element(&mut self, element: NodeId, tag: String) {
        if let NodeKind::Element { attributes, .. } = &self.doc.node(element).kind {
            let event = Event::StartElement {
                name: &tag,
                prefix: None,
                namespace: None,
                attributes,
            };
            let location = self.input.location();
            self.reporter.event(&self.doc.diagnostics, event, location);
        }
        self.open_elements.push((element, tag));
    }

    /// Closes the innermost open element.
    fn close_element(&mut self) {
        if let Some((_, tag)) = self.open_elements.pop() {
            self.emit(Event::EndElement {
                name: &tag,
                prefix: None,
                namespace: None,
            });
        }
    }

    /// Appends a comment to the current element.
    fn append_comment(&mut self, content: String) {
        self.emit(Event::Comment(&content));
        let parent = self.current_parent();
        let comment_id = self.doc.create_node(NodeKind::Comment { content });
        self.doc.append_child(parent, comment_id);
    }

    /// Returns the input from `start` to `end` as an owned string.
    ///
    /// Takes the text straight from the input string. Only if a bound splits
    /// a character does it fall back to a lossy conversion, so the result is
    /// always what `String::from_utf8_lossy` on the same bytes would give.
    fn input_text(&self, start: usize, end: usize) -> String {
        match self.input.str_slice(start, end) {
            Some(text) => text.to_owned(),
            None => String::from_utf8_lossy(self.input.slice(start, end)).into_owned(),
        }
    }

    /// Main parse entry point. Parses the entire HTML document.
    fn parse(&mut self) -> Result<Document, ParseError> {
        self.emit(Event::StartDocument);
        self.input.skip_whitespace();

        // Track whether a DOCTYPE was found in the input
        let mut has_doctype = false;

        // Parse optional DOCTYPE
        if self.input.looking_at_ci(b"<!doctype") {
            self.parse_doctype();
            self.input.skip_whitespace();
            has_doctype = true;
        }

        // Parse the body content
        self.parse_content();

        // If a fatal error was recorded (e.g. depth limit), return it.
        if let Some(err) = self.fatal_error.take() {
            return Err(err);
        }

        // Close any remaining open elements
        while let Some((_, tag)) = self.open_elements.last() {
            let message = format!("unclosed element <{tag}> at end of document");
            self.push_warning(message);
            self.close_element();
        }
        self.reporter.diagnostics(&self.doc.diagnostics);

        // Add default DOCTYPE if none was in the input and not disabled
        if !has_doctype && !self.options.no_default_doctype {
            let doctype_id = self.doc.create_node(NodeKind::DocumentType {
                name: "html".to_string(),
                public_id: Some("-//W3C//DTD HTML 4.0 Transitional//EN".to_string()),
                system_id: Some("http://www.w3.org/TR/REC-html40/loose.dtd".to_string()),
                internal_subset: None,
            });
            // Prepend DOCTYPE before the first child of the document root
            let root = self.doc.root();
            self.doc.prepend_child(root, doctype_id);
        }

        Ok(std::mem::take(&mut self.doc))
    }

    /// Ensures an `<html>` element exists at the document root, with
    /// `attributes` (from a start tag) merged into one that does, and
    /// opening one it creates. Returns its `NodeId`.
    fn ensure_html(&mut self, attributes: Vec<Attribute>) -> NodeId {
        let root = self.doc.root();
        // Look for existing html element
        for id in self.doc.children(root) {
            if matches!(&self.doc.node(id).kind, NodeKind::Element { name, .. } if name == "html") {
                self.merge_attributes(id, attributes);
                return id;
            }
        }
        // Create implied html
        let html_id = self.doc.create_node(NodeKind::Element {
            name: "html".to_string(),
            prefix: None,
            namespace: None,
            attributes,
        });
        self.doc.append_child(root, html_id);
        self.open_element(html_id, "html".to_string());
        html_id
    }

    /// Ensures a `<body>` element exists under `<html>`, as `ensure_html`
    /// does an `<html>`. Returns its `NodeId`.
    fn ensure_body(&mut self, attributes: Vec<Attribute>) -> NodeId {
        let html_id = self.ensure_html(Vec::new());
        // Look for existing body element
        for id in self.doc.children(html_id) {
            if matches!(&self.doc.node(id).kind, NodeKind::Element { name, .. } if name == "body") {
                self.merge_attributes(id, attributes);
                return id;
            }
        }
        // Create implied body
        let body_id = self.doc.create_node(NodeKind::Element {
            name: "body".to_string(),
            prefix: None,
            namespace: None,
            attributes,
        });
        self.doc.append_child(html_id, body_id);
        self.open_element(body_id, "body".to_string());
        body_id
    }

    /// Ensures an implied `<head>` element exists under `<html>`.
    /// Returns its `NodeId`.
    fn ensure_head(&mut self) -> NodeId {
        let html_id = self.ensure_html(Vec::new());
        // Look for existing head element
        for id in self.doc.children(html_id) {
            if matches!(&self.doc.node(id).kind, NodeKind::Element { name, .. } if name == "head") {
                return id;
            }
        }
        // Create implied head. Insert before body if body exists,
        // otherwise append to html.
        let head_id = self.doc.create_node(NodeKind::Element {
            name: "head".to_string(),
            prefix: None,
            namespace: None,
            attributes: vec![],
        });
        // Find body — if it exists, insert head before it
        let body_id = self.doc.children(html_id).find(|&id| {
            matches!(&self.doc.node(id).kind, NodeKind::Element { name, .. } if name == "body")
        });
        if let Some(body) = body_id {
            self.doc.insert_before(body, head_id);
        } else {
            self.doc.append_child(html_id, head_id);
        }
        head_id
    }

    /// Returns the current insertion point (the innermost open element, or
    /// the document root).
    fn current_parent(&self) -> NodeId {
        self.open_elements
            .last()
            .map_or_else(|| self.doc.root(), |&(id, _)| id)
    }

    /// Parses the HTML content: elements, text, comments, etc.
    fn parse_content(&mut self) {
        while !self.input.at_end() && self.fatal_error.is_none() {
            if self.input.looking_at(b"<!--") {
                self.parse_comment();
            } else if self.input.looking_at_ci(b"<!doctype") {
                // Ignore extra doctypes
                self.skip_to_gt();
            } else if self.input.looking_at(b"</") {
                self.parse_end_tag();
            } else if self.input.peek() == Some(b'<')
                && self
                    .input
                    .peek_at(1)
                    .is_some_and(|b| b.is_ascii_alphabetic())
            {
                self.parse_start_tag();
            } else if self.input.peek() == Some(b'<') && self.input.peek_at(1) == Some(b'!') {
                // `<!` not followed by `--` or `DOCTYPE`: either an IE
                // conditional comment marker (`<![if ...]>`) or other bogus
                // markup such as `<!– ... –>` with typographic dashes. Like
                // libxml2's `htmlSkipBogusComment`, skip it up to the next
                // `>` instead of leaking it into the text content.
                if self.input.peek_at(2) == Some(b'[') {
                    self.skip_conditional_comment();
                } else {
                    self.push_warning("bogus comment skipped".to_string());
                    self.skip_to_gt();
                }
            } else if self.input.peek() == Some(b'<') && self.input.peek_at(1) == Some(b'?') {
                self.parse_processing_instruction();
            } else if self.input.peek() == Some(b'<') {
                // Bare '<' not followed by alpha, '/', '!', or '?' — treat as text.
                // We must consume it here to avoid an infinite loop, since
                // parse_text() breaks on '<' without advancing.
                self.input.advance(1);
                if !self.options.no_implied && self.open_elements.is_empty() {
                    self.ensure_body(Vec::new());
                }
                let parent = self.current_parent();
                self.append_text(parent, "<".to_string());
            } else {
                self.parse_text();
            }
        }
    }

    // --- DOCTYPE ---

    fn parse_doctype(&mut self) {
        // Skip <!DOCTYPE (case-insensitive)
        self.input.advance(9); // "<!DOCTYPE" or "<!doctype"
        self.input.skip_whitespace();

        // Read the root element name
        let name = self.parse_tag_name();

        self.input.skip_whitespace();

        let mut system_id = None;
        let mut public_id = None;

        if self.input.looking_at_ci(b"system") {
            self.input.advance(6);
            self.input.skip_whitespace();
            system_id = self.try_parse_quoted_value();
            self.input.skip_whitespace();
        } else if self.input.looking_at_ci(b"public") {
            self.input.advance(6);
            self.input.skip_whitespace();
            public_id = self.try_parse_quoted_value();
            self.input.skip_whitespace();
            system_id = self.try_parse_quoted_value();
            self.input.skip_whitespace();
        }

        // Skip to end of DOCTYPE
        while !self.input.at_end() && self.input.peek() != Some(b'>') {
            self.input.advance(1);
        }
        if !self.input.at_end() {
            self.input.advance(1); // consume '>'
        }

        let doctype_id = self.doc.create_node(NodeKind::DocumentType {
            name,
            system_id,
            public_id,
            internal_subset: None,
        });
        self.doc.append_child(self.doc.root(), doctype_id);
    }

    // --- Start Tag ---

    #[allow(clippy::too_many_lines)]
    fn parse_start_tag(&mut self) {
        self.input.advance(1); // consume '<'
        if let Err(e) = self.input.increment_depth() {
            self.fatal_error = Some(e);
            return;
        }
        let tag = self.parse_tag_name();

        if tag.is_empty() {
            self.push_warning("empty tag name".to_string());
            self.input.decrement_depth();
            self.skip_to_gt();
            return;
        }

        let lower_tag = tag.to_ascii_lowercase();

        // Parse attributes
        let attributes = self.parse_attributes();

        self.input.skip_whitespace();

        // Handle self-closing slash
        let explicit_self_close = self.input.peek() == Some(b'/');
        if explicit_self_close {
            self.input.advance(1);
        }

        // Consume '>'
        if self.input.peek() == Some(b'>') {
            self.input.advance(1);
        } else if self.input.at_end() {
            // A tag the input ends in is unfinished, and libxml2 2.14's
            // tokenizer drops it.
            self.push_warning(format!(
                "start tag <{lower_tag}> cut off by the end of input"
            ));
            self.input.decrement_depth();
            return;
        } else {
            self.push_warning(format!("expected '>' after tag <{lower_tag}>"));
            self.skip_to_gt();
        }

        // Handle structural elements by merging with existing implied elements
        if !self.options.no_implied {
            if lower_tag == "html" {
                let html_id = self.ensure_html(attributes);
                // Ensure html is on the open_elements stack
                if !self.open_elements.iter().any(|(_, t)| t == "html") {
                    self.open_element(html_id, "html".to_string());
                }
                self.input.decrement_depth();
                return;
            }
            if lower_tag == "head" {
                // With anything but <html> open, or after the body, a <head>
                // is misplaced, and libxml2's htmlParseStartTag drops it.
                if self.html_has_child("body")
                    || self.open_elements.iter().any(|(_, t)| t != "html")
                {
                    self.push_warning("misplaced <head> tag".to_string());
                    self.dropped_heads += 1;
                } else {
                    let head_id = self.ensure_head();
                    self.merge_attributes(head_id, attributes);
                    // Push head to open_elements so child elements go inside it
                    self.open_element(head_id, "head".to_string());
                }
                self.input.decrement_depth();
                return;
            }
            if lower_tag == "body" && !self.is_in_frameset() {
                // Close head if open
                self.close_head_if_open();
                let body_id = self.ensure_body(attributes);
                if !self.open_elements.iter().any(|(_, t)| t == "body") {
                    self.open_element(body_id, "body".to_string());
                }
                self.input.decrement_depth();
                return;
            }
        }

        // Handle auto-closing: if the new element auto-closes an open one,
        // pop the stack accordingly.
        self.handle_auto_close(&lower_tag);

        // For non-structural elements, ensure proper containment
        if !self.options.no_implied {
            if lower_tag == "frameset" {
                // <frameset> replaces <body> in HTML 4.01 — place it
                // directly under <html>, not inside <body>
                self.close_head_if_open();
                self.ensure_html(Vec::new());
            } else if is_head_content_element(&lower_tag)
                && !self.is_in_body()
                && !self.html_has_child("body")
            {
                // Head-content elements go under <head>, but only before
                // <body> has been created: after </body> they stay at the
                // current insertion point rather than jumping back into
                // <head>.
                let head_id = self.ensure_head();
                if !self.open_elements.iter().any(|(_, t)| t == "head") {
                    self.open_element(head_id, "head".to_string());
                }
            } else if self.is_in_frameset() {
                // Inside a frameset context, don't auto-create body.
                // Elements like <frame>, <noframes> stay inside frameset.
            } else {
                // Body-content elements: close head, ensure body exists
                self.close_head_if_open();
                self.ensure_body(Vec::new());
            }
        }

        let parent = self.current_parent();

        let elem_id = self.doc.create_node(NodeKind::Element {
            name: lower_tag.clone(),
            prefix: None,
            namespace: None,
            attributes,
        });
        self.doc.append_child(parent, elem_id);

        // Void elements and explicit self-close are closed at once
        if is_void_element(&lower_tag) || explicit_self_close {
            self.open_element(elem_id, lower_tag);
            self.close_element();
            self.input.decrement_depth();
            return;
        }

        // Raw text elements (script, style) need special handling
        if is_raw_text_element(&lower_tag) {
            self.open_element(elem_id, lower_tag.clone());
            self.parse_raw_text(&lower_tag);
            // pop the element after raw text
            self.close_element();
            self.input.decrement_depth();
            return;
        }

        self.open_element(elem_id, lower_tag);
    }

    /// Merges attributes from a parsed tag into an existing element node.
    fn merge_attributes(&mut self, elem_id: NodeId, attrs: Vec<Attribute>) {
        if attrs.is_empty() {
            return;
        }
        if let NodeKind::Element { attributes, .. } = &mut self.doc.node_mut(elem_id).kind {
            for attr in attrs {
                if !attributes.iter().any(|a| a.name == attr.name) {
                    attributes.push(attr);
                }
            }
        }
    }

    /// Closes `<head>` if it's currently open on the element stack.
    fn close_head_if_open(&mut self) {
        if self.open_elements.last().is_some_and(|(_, t)| t == "head") {
            self.close_element();
        }
    }

    /// Returns true if the parser is currently inside `<body>`.
    fn is_in_body(&self) -> bool {
        self.open_elements.iter().any(|(_, t)| t == "body")
    }

    /// Returns true if the parser is currently inside a `<frameset>`.
    fn is_in_frameset(&self) -> bool {
        self.open_elements.iter().any(|(_, t)| t == "frameset")
    }

    /// Handles auto-closing of open elements when a new element is encountered.
    fn handle_auto_close(&mut self, new_tag: &str) {
        // Walk the open elements stack from top to find elements that should
        // be auto-closed by the new tag.
        loop {
            let should_close = self
                .open_elements
                .last()
                .is_some_and(|(_, open_tag)| auto_closes(open_tag, new_tag));
            if should_close {
                self.close_element();
                self.input.decrement_depth();
            } else {
                break;
            }
        }
    }

    // --- End Tag ---

    fn parse_end_tag(&mut self) {
        self.input.advance(2); // consume '</'
        let tag = self.parse_tag_name();
        let lower_tag = tag.to_ascii_lowercase();

        // Skip to '>'
        self.input.skip_whitespace();
        if self.input.peek() == Some(b'>') {
            self.input.advance(1);
        } else if !self.input.at_end() {
            self.push_warning(format!("expected '>' after end tag </{lower_tag}>"));
            self.skip_to_gt();
        }

        // Void elements should not have end tags; ignore them.
        if is_void_element(&lower_tag) {
            self.push_warning(format!("end tag for void element </{lower_tag}> ignored"));
            return;
        }
        if lower_tag == "head" && self.dropped_heads > 0 {
            self.dropped_heads -= 1;
            return;
        }

        // Find the matching open element in the stack
        let found = self
            .open_elements
            .iter()
            .rposition(|(_, name)| *name == lower_tag);

        if let Some(idx) = found {
            // Close everything above and including the matched element,
            // innermost first, each intervening one after a warning.
            while let Some((_, closed_tag)) = self.open_elements[idx..].last() {
                if *closed_tag != lower_tag {
                    let message =
                        format!("implicitly closing <{closed_tag}> before </{lower_tag}>");
                    self.push_warning(message);
                }
                self.close_element();
                self.input.decrement_depth();
            }
        } else {
            // No matching open element — stray end tag
            self.push_warning(format!("stray end tag </{lower_tag}>"));
        }
    }

    // --- Attributes ---

    fn parse_attributes(&mut self) -> Vec<Attribute> {
        let mut attributes = Vec::new();

        loop {
            self.input.skip_whitespace();

            // Check for end of tag
            if self.input.at_end()
                || self.input.peek() == Some(b'>')
                || self.input.peek() == Some(b'/')
                || self.input.looking_at(b"/>")
            {
                break;
            }

            // Parse attribute name
            let name = self.parse_attr_name();
            if name.is_empty() {
                // Skip the bad character and continue
                self.input.advance(1);
                continue;
            }

            let lower_name = name.to_ascii_lowercase();

            self.input.skip_whitespace();

            // Check for = (attribute with value)
            let value = if self.input.peek() == Some(b'=') {
                self.input.advance(1); // consume '='
                self.input.skip_whitespace();
                self.parse_attr_value()
            } else {
                // Boolean attribute (no value) — value equals the attribute name
                // per HTML spec
                lower_name.clone()
            };

            // A repeated attribute is dropped: the first one counts, as in
            // libxml2.
            if attributes.iter().any(|a: &Attribute| a.name == lower_name) {
                continue;
            }
            attributes.push(Attribute {
                name: lower_name,
                value,
                prefix: None,
                namespace: None,
                raw_value: None,
            });
        }

        attributes
    }

    /// Parses an attribute name (sequence of non-whitespace, non-special chars).
    fn parse_attr_name(&mut self) -> String {
        let start = self.input.pos();
        while let Some(b) = self.input.peek() {
            if b == b' '
                || b == b'\t'
                || b == b'\r'
                || b == b'\n'
                || b == b'='
                || b == b'>'
                || b == b'/'
                || b == b'<'
                || b == b'"'
                || b == b'\''
            {
                break;
            }
            self.input.advance(1);
        }
        self.input_text(start, self.input.pos())
    }

    /// Parses an attribute value. Handles quoted and unquoted values.
    fn parse_attr_value(&mut self) -> String {
        if self.input.at_end() {
            return String::new();
        }

        let b = self.input.peek();
        if b == Some(b'"') || b == Some(b'\'') {
            // Quoted attribute value — we know `b` is Some from the check above
            let quote = b.unwrap_or(b'"');
            self.input.advance(1); // consume opening quote
            let mut value = String::new();
            while !self.input.at_end() {
                let ch = self.input.peek();
                if ch == Some(quote) {
                    self.input.advance(1);
                    break;
                }
                if ch == Some(b'&') {
                    let resolved = self.parse_html_reference(true);
                    value.push_str(&resolved);
                } else {
                    let c = self.next_char_html();
                    value.push(c);
                }
            }
            value
        } else {
            // Unquoted attribute value
            let mut value = String::new();
            while let Some(b) = self.input.peek() {
                if b == b' '
                    || b == b'\t'
                    || b == b'\r'
                    || b == b'\n'
                    || b == b'>'
                    || b == b'<'
                    || b == b'`'
                {
                    break;
                }
                // Backslash escaping: \c includes both chars literally
                // (libxml2 treats \" as literal backslash+quote)
                if b == b'\\' {
                    let c1 = self.next_char_html();
                    value.push(c1);
                    if !self.input.at_end() {
                        let c2 = self.next_char_html();
                        value.push(c2);
                    }
                    continue;
                }
                if b == b'"' || b == b'\'' {
                    break;
                }
                if b == b'&' {
                    let resolved = self.parse_html_reference(true);
                    value.push_str(&resolved);
                } else {
                    let c = self.next_char_html();
                    value.push(c);
                }
            }
            value
        }
    }

    // --- Text Content ---

    fn parse_text(&mut self) {
        let mut text = String::new();

        while !self.input.at_end() {
            if self.input.peek() == Some(b'<') {
                break;
            }

            if self.input.peek() == Some(b'&') {
                let resolved = self.parse_html_reference(false);
                text.push_str(&resolved);
            } else {
                let ch = self.next_char_html();
                text.push(ch);
            }
        }

        if !text.is_empty() {
            // Strip blank text nodes if configured
            if self.options.no_blanks && text.chars().all(char::is_whitespace) {
                return;
            }
            // Ensure body exists for text content (unless whitespace between
            // structural elements or we're already inside an element)
            if !self.options.no_implied && self.open_elements.is_empty() {
                // Whitespace-only text before any elements is ignored
                if text.chars().all(char::is_whitespace) {
                    return;
                }
                self.ensure_body(Vec::new());
            } else if self.text_belongs_in_body(&text) {
                // Character data cannot live in <head> (or directly in
                // <html> before <body>): leading whitespace stays where it
                // is, and the rest implies </head><body>. This matches the
                // WHATWG "in head" / "after head" insertion modes and
                // libxml2 2.14 (libxml2 2.13 also moves the text into the
                // body, wrapped in an implied <p>).
                let ws_len = text.len()
                    - text
                        .trim_start_matches(|c: char| c.is_ascii_whitespace())
                        .len();
                let rest = text.split_off(ws_len);
                if !text.is_empty() {
                    let parent = self.current_parent();
                    self.append_text(parent, text);
                }
                self.close_head_if_open();
                self.ensure_body(Vec::new());
                text = rest;
            }
            let parent = self.current_parent();
            self.append_text(parent, text);
        }
    }

    /// Appends `text` to `parent`: to its last child, if that is text (after
    /// an end tag that closed nothing, say), as libxml2's SAX handler
    /// merges adjacent character data, and otherwise as a new text node.
    fn append_text(&mut self, parent: NodeId, text: String) {
        self.emit(Event::Text(&text));
        if let Some(last) = self.doc.last_child(parent) {
            if let NodeKind::Text { content } = self.doc.kind_mut(last) {
                content.push_str(&text);
                return;
            }
        }
        let text_id = self.doc.create_node(NodeKind::Text { content: text });
        self.doc.append_child(parent, text_id);
    }

    /// Returns true if `text`, about to be inserted at the current position,
    /// must instead go into an implied `<body>`: the insertion point is
    /// `<head>` or `<html>`, no `<body>` or `<frameset>` has been created
    /// yet, and the text is not whitespace-only.
    fn text_belongs_in_body(&self, text: &str) -> bool {
        if self.options.no_implied
            || !self
                .open_elements
                .last()
                .is_some_and(|(_, t)| t == "head" || t == "html")
            || text.chars().all(|c| c.is_ascii_whitespace())
        {
            return false;
        }
        !self.html_has_child("body") && !self.html_has_child("frameset")
    }

    /// Returns true if the root `<html>` element has a child element named
    /// `name` (whether or not it is still open).
    fn html_has_child(&self, name: &str) -> bool {
        let root = self.doc.root();
        self.doc
            .children(root)
            .filter(|&id| {
                matches!(&self.doc.node(id).kind, NodeKind::Element { name: n, .. } if n == "html")
            })
            .any(|html_id| {
                self.doc.children(html_id).any(|id| {
                    matches!(&self.doc.node(id).kind, NodeKind::Element { name: n, .. } if n == name)
                })
            })
    }

    // --- Raw text (script/style) ---

    fn parse_raw_text(&mut self, tag: &str) {
        let mut content = String::new();
        let end_tag_bytes: Vec<u8> = format!("</{tag}").bytes().collect();

        while !self.input.at_end() {
            if self.input.looking_at_ci(&end_tag_bytes) {
                break;
            }
            let ch = self.next_char_html();
            content.push(ch);
        }

        if !content.is_empty() {
            self.emit(Event::RawText(&content));
            let parent = self.current_parent();
            let text_id = self.doc.create_node(NodeKind::Text { content });
            self.doc.append_child(parent, text_id);
        }

        // Consume the end tag
        if !self.input.at_end() {
            self.input.advance(end_tag_bytes.len());
            self.input.skip_whitespace();
            if self.input.peek() == Some(b'>') {
                self.input.advance(1);
            }
        }
    }

    // --- Comments ---

    fn parse_comment(&mut self) {
        self.input.advance(4); // consume '<!--'

        // Handle abrupt closing: <!-->  and <!--->
        if self.input.peek() == Some(b'>') {
            self.input.advance(1);
            self.append_comment(String::new());
            return;
        }
        if self.input.looking_at(b"->") {
            self.input.advance(2);
            self.append_comment(String::new());
            return;
        }

        let mut content = String::new();
        let mut terminated = false;

        loop {
            if self.input.at_end() {
                self.push_warning("unterminated comment".to_string());
                break;
            }
            // Standard comment end: -->
            if self.input.looking_at(b"-->") {
                self.input.advance(3);
                terminated = true;
                break;
            }
            // Quirky comment end: --!> (libxml2 treats this as -->)
            if self.input.looking_at(b"--!>") {
                self.input.advance(4);
                terminated = true;
                break;
            }
            let ch = self.next_char_html();
            content.push(ch);
        }

        // libxml2 drops unterminated comments (those that reach EOF)
        if !terminated {
            return;
        }
        self.append_comment(content);
    }

    // --- Processing Instructions ---

    fn parse_processing_instruction(&mut self) {
        self.input.advance(2); // consume '<?'
        let target = self.parse_tag_name();
        self.input.skip_whitespace();

        let mut data = String::new();
        // In HTML mode, libxml2 reads PI content up to '>' only.
        // The '?' before '>' (if present) is included in the content,
        // so <?pi data?> stores data as "data?" and serializes as "<?pi data?>".
        loop {
            if self.input.at_end() {
                self.push_warning("unterminated processing instruction".to_string());
                break;
            }
            if self.input.peek() == Some(b'>') {
                self.input.advance(1);
                break;
            }
            let ch = self.next_char_html();
            data.push(ch);
        }

        let pi_data = if data.is_empty() { None } else { Some(data) };
        self.emit(Event::ProcessingInstruction {
            target: &target,
            data: pi_data.as_deref(),
        });

        let parent = self.current_parent();
        let pi_id = self.doc.create_node(NodeKind::ProcessingInstruction {
            target,
            data: pi_data,
        });
        self.doc.append_child(parent, pi_id);
    }

    // --- HTML Entity/Character References ---

    /// Parses an HTML character or entity reference, or returns a bare `&`
    /// if it doesn't look like a valid reference (error-tolerant).
    ///
    /// A named reference without a trailing `;` is only decoded when its
    /// name is one of the legacy references that the WHATWG HTML standard
    /// allows without a semicolon (`amp`, `lt`, `copy`, ...; see HTML
    /// §13.2.5.73 Named character reference state). In attribute values
    /// (`in_attribute`), such a reference is additionally left undecoded
    /// when it is followed by `=`, so query strings like `?a=1&lang=de` or
    /// `&copy=2` survive intact. libxml2 2.13 decodes no semicolon-less
    /// references at all; libxml2 2.14 follows the WHATWG rules.
    fn parse_html_reference(&mut self, in_attribute: bool) -> String {
        // Save position for backtracking
        let saved = self.input.save_position();

        self.input.advance(1); // consume '&'

        if self.input.peek() == Some(b'#') {
            // Character reference
            self.input.advance(1);
            if self.input.peek() == Some(b'x') || self.input.peek() == Some(b'X') {
                // Hex
                self.input.advance(1);
                let hex = self.take_while_ascii(|b| b.is_ascii_hexdigit());
                if !hex.is_empty() && self.input.peek() == Some(b';') {
                    self.input.advance(1);
                    if let Ok(value) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(value) {
                            return ch.to_string();
                        }
                    }
                }
                // Invalid — backtrack
                self.input.restore_position(saved);
                self.input.advance(1);
                return "&".to_string();
            }
            // Decimal
            let dec = self.take_while_ascii(|b| b.is_ascii_digit());
            if !dec.is_empty() && self.input.peek() == Some(b';') {
                self.input.advance(1);
                if let Ok(value) = dec.parse::<u32>() {
                    if let Some(ch) = char::from_u32(value) {
                        return ch.to_string();
                    }
                }
            }
            // Invalid — backtrack
            self.input.restore_position(saved);
            self.input.advance(1);
            return "&".to_string();
        }

        // Named entity reference
        let name = self.take_while_ascii(|b| b.is_ascii_alphanumeric());
        if !name.is_empty() {
            // Try with semicolon
            if self.input.peek() == Some(b';') {
                self.input.advance(1);
                if let Some(value) = entities::lookup_entity(&name) {
                    return value.to_string();
                }
                // Unknown entity — return it as-is with the & and ;
                self.push_warning(format!("unknown entity reference: &{name};"));
                return format!("&{name};");
            }
            // Without a semicolon, only legacy references are recognized,
            // and never in an attribute value when followed by '='.
            let followed_by_equals = self.input.peek() == Some(b'=');
            if crate::html5::entities::is_legacy_named_entity(&name)
                && !(in_attribute && followed_by_equals)
            {
                if let Some(value) = entities::lookup_entity(&name) {
                    self.push_warning(format!("entity reference &{name} missing semicolon"));
                    return value.to_string();
                }
            }
        }

        // Not a valid reference — backtrack and return bare &
        self.input.restore_position(saved);
        self.input.advance(1);
        "&".to_string()
    }

    // --- Tag Name Parsing ---

    /// Parses an HTML tag name. Tag names can contain letters, digits, hyphens,
    /// and a few other characters. Returns the name as-is (caller normalizes case).
    fn parse_tag_name(&mut self) -> String {
        let start = self.input.pos();
        while let Some(b) = self.input.peek() {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b':' || b == b'.' {
                self.input.advance(1);
            } else {
                break;
            }
        }
        self.input_text(start, self.input.pos())
    }

    /// Tries to parse a quoted value for DOCTYPE SYSTEM/PUBLIC identifiers.
    fn try_parse_quoted_value(&mut self) -> Option<String> {
        let quote = self.input.peek()?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        self.input.advance(1);
        let start = self.input.pos();
        while !self.input.at_end() && self.input.peek() != Some(quote) {
            self.input.advance(1);
        }
        let value = self.input_text(start, self.input.pos());
        if !self.input.at_end() {
            self.input.advance(1); // closing quote
        }
        Some(value)
    }

    // --- HTML-specific low-level helpers ---
    // These methods are NOT shared with the XML parser because HTML has
    // different rules (error tolerance, different character handling, etc.).

    /// Reads the next character, handling CR/LF normalization.
    /// Returns `'\0'` at end of input (error-tolerant, unlike the XML parser
    /// which returns an error).
    fn next_char_html(&mut self) -> char {
        if self.input.at_end() {
            return '\0';
        }
        if let Some(ch) = self.input.peek_char() {
            self.input.advance_char(ch);
            // Handle \r\n normalization
            if ch == '\r' {
                if self.input.peek() == Some(b'\n') {
                    self.input.advance(1);
                }
                return '\n';
            }
            ch
        } else {
            // Invalid UTF-8 — skip the byte and return replacement character
            self.input.advance(1);
            '\u{FFFD}'
        }
    }

    /// Skips an IE conditional comment marker (`<![if ...]>` or `<![endif]>`).
    ///
    /// These are Microsoft extensions to HTML. Each `<![...>` marker is
    /// individually consumed, but the content between them flows as normal
    /// text, matching libxml2 behavior.
    fn skip_conditional_comment(&mut self) {
        self.push_warning("incorrectly opened comment".to_string());
        self.skip_to_gt();
    }

    /// Skips forward to and past the next `>` character.
    fn skip_to_gt(&mut self) {
        while !self.input.at_end() {
            if self.input.peek() == Some(b'>') {
                self.input.advance(1);
                return;
            }
            self.input.advance(1);
        }
    }

    /// Takes characters while they match the predicate (ASCII only).
    fn take_while_ascii(&mut self, pred: impl Fn(u8) -> bool) -> String {
        let start = self.input.pos();
        while let Some(b) = self.input.peek() {
            if pred(b) {
                self.input.advance(1);
            } else {
                break;
            }
        }
        self.input_text(start, self.input.pos())
    }

    /// Pushes a warning diagnostic to the document. Respects the
    /// `no_warnings` option.
    fn push_warning(&mut self, message: String) {
        if self.options.no_warnings {
            return;
        }
        self.doc.diagnostics.push(ParseDiagnostic {
            severity: ErrorSeverity::Warning,
            message,
            location: self.input.location(),
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(input: &str) -> Document {
        parse_html(input).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    /// Parses without implied elements or the default DOCTYPE, so that the
    /// input's first node is the document's first child.
    fn parse_no_implied(input: &str) -> Document {
        let opts = HtmlParseOptions::default()
            .no_implied(true)
            .no_default_doctype(true);
        parse_html_with_options(input, &opts).unwrap_or_else(|e| panic!("parse failed: {e}"))
    }

    // --- Basic parsing ---

    #[test]
    fn test_parse_non_ascii_text_and_attribute_values() {
        let doc = parse("<p title=\"naïve €\" lang=ü>café</p>");
        let p = doc
            .descendants(doc.root())
            .find(|&n| doc.node_name(n) == Some("p"))
            .unwrap();
        assert_eq!(doc.attribute(p, "title"), Some("naïve €"));
        assert_eq!(doc.attribute(p, "lang"), Some("ü"));
        assert_eq!(doc.text_content(p), "café");
    }

    #[test]
    fn test_text_after_an_ignored_end_tag_joins_the_text_before_it() {
        let doc = parse("<div>a</p>b</div>");
        let body = doc.children(doc.root_element().unwrap()).last().unwrap();
        let div = doc.first_child(body).unwrap();
        let texts: Vec<_> = doc.children(div).map(|id| doc.node_text(id)).collect();
        assert_eq!(texts, vec![Some("ab")]);
    }

    #[test]
    fn test_no_default_doctype_option() {
        let opts = HtmlParseOptions::default().no_default_doctype(true);
        let doc = parse_html_with_options("<p>x</p>", &opts).unwrap();
        let kinds: Vec<_> = doc
            .children(doc.root())
            .map(|id| doc.node_name(id))
            .collect();
        assert_eq!(kinds, vec![Some("html")]);
        let doc = parse_html_with_options("<!DOCTYPE html><p>x</p>", &opts).unwrap();
        assert!(matches!(
            doc.node(doc.first_child(doc.root()).unwrap()).kind,
            NodeKind::DocumentType { .. }
        ));
    }

    #[test]
    fn test_parse_simple_html() {
        let doc = parse("<html><body><p>Hello</p></body></html>");
        let html = doc.root_element().unwrap();
        assert_eq!(doc.node_name(html), Some("html"));
    }

    #[test]
    fn test_parse_implied_structure() {
        // Even without explicit html/body, the parser adds them
        let doc = parse("<p>Hello</p>");
        let html = doc.root_element().unwrap();
        assert_eq!(doc.node_name(html), Some("html"));

        // Should have body child (and no head, with nothing to put in it)
        let children: Vec<_> = doc.children(html).collect();
        assert_ne!(children, [] as [crate::tree::NodeId; 0]);
        // Body should contain the <p>
        let body = children.last().unwrap();
        assert_eq!(doc.node_name(*body), Some("body"));
    }

    #[test]
    fn test_parse_no_implied_option() {
        let doc = parse_no_implied("<p>Hello</p>");
        let root = doc.root();
        let first = doc.first_child(root).unwrap();
        // Without implied structure, p should be directly under root
        assert_eq!(doc.node_name(first), Some("p"));
        assert_eq!(doc.text_content(first), "Hello");
    }

    // --- Void elements ---

    #[test]
    fn test_void_elements() {
        let doc = parse_no_implied("<p>line1<br>line2</p>");
        let root = doc.root();
        let p = doc.first_child(root).unwrap();
        let children: Vec<_> = doc.children(p).collect();
        assert_eq!(children.len(), 3); // "line1", <br>, "line2"
        assert_eq!(doc.node_name(children[1]), Some("br"));
        assert!(doc.first_child(children[1]).is_none()); // br has no children
    }

    #[test]
    fn test_void_element_with_end_tag() {
        let doc = parse_no_implied("<p>text<br></br>more</p>");
        let root = doc.root();
        let p = doc.first_child(root).unwrap();
        // The </br> should be ignored
        let children: Vec<_> = doc.children(p).collect();
        assert_eq!(children.len(), 3); // "text", <br>, "more"
    }

    #[test]
    fn test_img_void_element() {
        let doc = parse_no_implied("<img src=\"test.jpg\" alt=\"test\">");
        let root = doc.root();
        let img = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(img), Some("img"));
        assert_eq!(doc.attribute(img, "src"), Some("test.jpg"));
        assert_eq!(doc.attribute(img, "alt"), Some("test"));
    }

    // --- Case-insensitive tags ---

    #[test]
    fn test_case_insensitive_tags() {
        let doc = parse_no_implied("<DIV><P>Hello</P></DIV>");
        let root = doc.root();
        let div = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(div), Some("div"));
        let p = doc.first_child(div).unwrap();
        assert_eq!(doc.node_name(p), Some("p"));
        assert_eq!(doc.text_content(p), "Hello");
    }

    #[test]
    fn test_mixed_case_tags() {
        let doc = parse_no_implied("<Div><SPAN>Hi</span></div>");
        let root = doc.root();
        let div = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(div), Some("div"));
    }

    // --- Unquoted attributes ---

    #[test]
    fn test_unquoted_attributes() {
        let doc = parse_no_implied("<div class=main id=content>text</div>");
        let root = doc.root();
        let div = doc.first_child(root).unwrap();
        assert_eq!(doc.attribute(div, "class"), Some("main"));
        assert_eq!(doc.attribute(div, "id"), Some("content"));
    }

    #[test]
    fn test_boolean_attributes() {
        let doc = parse_no_implied("<input disabled readonly>");
        let root = doc.root();
        let input = doc.first_child(root).unwrap();
        assert_eq!(doc.attribute(input, "disabled"), Some("disabled"));
        assert_eq!(doc.attribute(input, "readonly"), Some("readonly"));
    }

    // --- Auto-closing ---

    #[test]
    fn test_p_auto_closes_p() {
        let doc = parse_no_implied("<p>First<p>Second");
        let root = doc.root();
        let children: Vec<_> = doc.children(root).collect();
        // Should have two p elements as siblings, not nested
        assert_eq!(children.len(), 2);
        assert_eq!(doc.node_name(children[0]), Some("p"));
        assert_eq!(doc.text_content(children[0]), "First");
        assert_eq!(doc.node_name(children[1]), Some("p"));
        assert_eq!(doc.text_content(children[1]), "Second");
    }

    #[test]
    fn test_li_auto_closes_li() {
        let doc = parse_no_implied("<ul><li>A<li>B<li>C</ul>");
        let root = doc.root();
        let ul = doc.first_child(root).unwrap();
        let items: Vec<_> = doc.children(ul).collect();
        assert_eq!(items.len(), 3);
        assert_eq!(doc.text_content(items[0]), "A");
        assert_eq!(doc.text_content(items[1]), "B");
        assert_eq!(doc.text_content(items[2]), "C");
    }

    #[test]
    fn test_dd_dt_auto_close() {
        let doc = parse_no_implied("<dl><dt>Term<dd>Def<dt>Term2<dd>Def2</dl>");
        let root = doc.root();
        let dl = doc.first_child(root).unwrap();
        let items: Vec<_> = doc.children(dl).collect();
        assert_eq!(items.len(), 4);
        assert_eq!(doc.node_name(items[0]), Some("dt"));
        assert_eq!(doc.node_name(items[1]), Some("dd"));
        assert_eq!(doc.node_name(items[2]), Some("dt"));
        assert_eq!(doc.node_name(items[3]), Some("dd"));
    }

    // --- Entity references ---

    #[test]
    fn test_html_entities() {
        let doc = parse_no_implied("<p>&copy; 2024 &mdash; All rights reserved</p>");
        let root = doc.root();
        let p = doc.first_child(root).unwrap();
        let text = doc.text_content(p);
        assert!(text.contains('\u{00A9}')); // copyright sign
        assert!(text.contains('\u{2014}')); // em dash
    }

    #[test]
    fn test_bare_ampersand() {
        let doc = parse_no_implied("<p>A & B</p>");
        let root = doc.root();
        let p = doc.first_child(root).unwrap();
        assert_eq!(doc.text_content(p), "A & B");
    }

    #[test]
    fn test_numeric_character_reference() {
        let doc = parse_no_implied("<p>&#65; &#x42;</p>");
        let root = doc.root();
        let p = doc.first_child(root).unwrap();
        assert_eq!(doc.text_content(p), "A B");
    }

    // --- Missing closing tags ---

    #[test]
    fn test_missing_closing_tags() {
        let doc = parse_no_implied("<div><p>Hello");
        let root = doc.root();
        let div = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(div), Some("div"));
        let p = doc.first_child(div).unwrap();
        assert_eq!(doc.node_name(p), Some("p"));
        assert_eq!(doc.text_content(p), "Hello");
    }

    // --- Comments ---

    #[test]
    fn test_html_comment() {
        let doc = parse_no_implied("<!-- hello --><p>text</p>");
        let root = doc.root();
        let first = doc.first_child(root).unwrap();
        assert_eq!(doc.node_text(first), Some(" hello "));
    }

    // --- Script/style raw text ---

    #[test]
    fn test_script_raw_text() {
        let doc = parse_no_implied("<script>var x = 1 < 2 && true;</script>");
        let root = doc.root();
        let script = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(script), Some("script"));
        assert_eq!(doc.text_content(script), "var x = 1 < 2 && true;");
    }

    #[test]
    fn test_style_raw_text() {
        let doc = parse_no_implied("<style>p > span { color: red; }</style>");
        let root = doc.root();
        let style = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(style), Some("style"));
        assert_eq!(doc.text_content(style), "p > span { color: red; }");
    }

    // --- DOCTYPE ---

    #[test]
    fn test_html_doctype() {
        let doc = parse_no_implied("<!DOCTYPE html><p>text</p>");
        let root = doc.root();
        let children: Vec<_> = doc.children(root).collect();
        assert!(children.len() >= 2);
        match &doc.node(children[0]).kind {
            NodeKind::DocumentType { name, .. } => {
                assert_eq!(name, "html");
            }
            other => panic!("expected DocumentType, got {other:?}"),
        }
    }

    // --- Single-quoted and double-quoted attributes ---

    #[test]
    fn test_single_quoted_attributes() {
        let doc = parse_no_implied("<div class='main'>text</div>");
        let root = doc.root();
        let div = doc.first_child(root).unwrap();
        assert_eq!(doc.attribute(div, "class"), Some("main"));
    }

    // --- Processing instructions ---

    #[test]
    fn test_html_processing_instruction() {
        let doc = parse_no_implied("<?xml-stylesheet type=\"text/css\"?><p>text</p>");
        let root = doc.root();
        let first = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(first), Some("xml-stylesheet"));
    }

    // --- Attribute case normalization ---

    #[test]
    fn test_attribute_case_normalized() {
        let doc = parse_no_implied("<div CLASS=\"main\" ID=\"1\">text</div>");
        let root = doc.root();
        let div = doc.first_child(root).unwrap();
        assert_eq!(doc.attribute(div, "class"), Some("main"));
        assert_eq!(doc.attribute(div, "id"), Some("1"));
    }

    // --- Whitespace stripping ---

    #[test]
    fn test_no_blanks_option() {
        let opts = HtmlParseOptions::default().no_blanks(true).no_implied(true);
        let doc = parse_html_with_options("<div>  \n  <p>text</p>  \n  </div>", &opts).unwrap();
        let div = doc.root_element().unwrap();
        // With no_blanks, whitespace-only text nodes are stripped
        let children: Vec<_> = doc.children(div).collect();
        assert_eq!(children.len(), 1);
        assert_eq!(doc.node_name(children[0]), Some("p"));
    }

    // --- Stray end tags ---

    #[test]
    fn test_stray_end_tag() {
        let doc = parse_no_implied("</div><p>text</p>");
        let root = doc.root();
        let p = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(p), Some("p"));
        assert_eq!(doc.text_content(p), "text");
        // Should have a diagnostic about the stray end tag
        assert!(!doc.diagnostics.is_empty());
    }

    // --- Complex document ---

    #[test]
    fn test_complex_html_document() {
        let doc = parse(
            r#"<!DOCTYPE html>
<html>
<head><title>Test</title></head>
<body>
<h1>Hello</h1>
<p>A paragraph with <b>bold</b> and <em>emphasis</em>.</p>
<ul>
<li>Item 1
<li>Item 2
<li>Item 3
</ul>
<img src="test.jpg">
</body>
</html>"#,
        );
        let html = doc.root_element().unwrap();
        assert_eq!(doc.node_name(html), Some("html"));
    }

    // --- Self-closing syntax ---

    #[test]
    fn test_self_closing_syntax() {
        let doc = parse_no_implied("<br/>");
        let root = doc.root();
        let br = doc.first_child(root).unwrap();
        assert_eq!(doc.node_name(br), Some("br"));
        assert!(doc.first_child(br).is_none());
    }

    // --- Entity in attribute ---

    #[test]
    fn test_entity_in_attribute() {
        let doc = parse_no_implied("<a href=\"page?a=1&amp;b=2\">link</a>");
        let root = doc.root();
        let a = doc.first_child(root).unwrap();
        assert_eq!(doc.attribute(a, "href"), Some("page?a=1&b=2"));
    }

    /// Parses `input` and returns the serialized content of `<body>`.
    fn body_html(input: &str) -> String {
        let out = crate::serial::html::serialize_html(&parse(input));
        let Some((_, after)) = out.split_once("<body>") else {
            panic!("expected <body> in output, got {out:?}");
        };
        let Some((inner, _)) = after.split_once("</body>") else {
            panic!("expected </body> in output, got {out:?}");
        };
        inner.trim_matches('\n').to_string()
    }

    /// Returns the names of the element children of the root `<html>`.
    fn html_child_names(doc: &Document) -> Vec<String> {
        let html = doc.root_element().unwrap();
        doc.children(html)
            .filter_map(|id| doc.node_name(id).map(str::to_string))
            .collect()
    }

    #[test]
    fn test_parse_html_reference_no_semicolon_non_legacy_text() {
        // `lang` is not a legacy reference, so `&lang` without `;` is text.
        let doc = parse_no_implied("<p>t &lang x &langle y</p>");
        let p = doc.first_child(doc.root()).unwrap();
        assert_eq!(doc.text_content(p), "t &lang x &langle y");
    }

    #[test]
    fn test_parse_html_reference_no_semicolon_legacy_text() {
        // Legacy references are still decoded without `;` in text.
        let doc = parse_no_implied("<p>&copy y &amp z &lt</p>");
        let p = doc.first_child(doc.root()).unwrap();
        assert_eq!(doc.text_content(p), "\u{a9} y & z <");
    }

    #[test]
    fn test_parse_html_reference_attribute_followed_by_equals() {
        let doc = parse_no_implied(r#"<a href="/?x=1&lang=de&copy=2&amp;y=3&not=4">t</a>"#);
        let a = doc.first_child(doc.root()).unwrap();
        assert_eq!(
            doc.attribute(a, "href"),
            Some("/?x=1&lang=de&copy=2&y=3&not=4")
        );
    }

    #[test]
    fn test_parse_html_reference_attribute_legacy_not_followed_by_equals() {
        // From libxml2's attr-ents test: `&lt=` stays literal, `&lta` is not
        // a reference, and `&lt&gt` decodes both.
        let doc = parse_no_implied(
            r#"<a href="a&lt=1&gt=2"></a><a href="a&lta&gta"></a><a href="a&lt&gt"></a><a href=a&copy=1></a>"#,
        );
        let values: Vec<_> = doc
            .children(doc.root())
            .map(|a| doc.attribute(a, "href").unwrap().to_string())
            .collect();
        assert_eq!(values, ["a&lt=1&gt=2", "a&lta&gta", "a<>", "a&copy=1"]);
    }

    #[test]
    fn test_parse_bogus_comment_skipped() {
        // `<!` followed by neither `--` nor `DOCTYPE` (here U+2013 dashes, as
        // some CMSs emit) is skipped up to the next `>`.
        assert_eq!(
            body_html("<body><div>a<!\u{2013} google_ad_section_start \u{2013}>b</div></body>"),
            "<div>ab</div>"
        );
        assert_eq!(body_html("<p>a<!x>b<!>c<!-x->d"), "<p>abcd</p>");
    }

    #[test]
    fn test_parse_text_in_head_implies_body() {
        // libxml2 2.14 test/HTML/implied1.html
        let out = crate::serial::html::serialize_html(&parse("<head>   x\n"));
        assert!(
            out.contains("<html>\n<head>   </head>\n<body>x\n</body>\n</html>"),
            "unexpected output: {out:?}"
        );
    }

    #[test]
    fn test_parse_text_after_title_implies_body() {
        let input = "<title>a</title>b<p>c</p>";
        assert_eq!(html_child_names(&parse(input)), ["head", "body"]);
        assert_eq!(body_html(input), "b<p>c</p>");
    }

    #[test]
    fn test_parse_text_directly_in_html_implies_body() {
        assert_eq!(body_html("<html>x<p>y</p>"), "x<p>y</p>");
    }

    #[test]
    fn test_parse_whitespace_in_head_stays_in_head() {
        let doc = parse("<head> <title>a</title> </head><body>b</body>");
        let html = doc.root_element().unwrap();
        let head = doc.first_child(html).unwrap();
        assert_eq!(doc.node_name(head), Some("head"));
        assert_eq!(doc.children(head).count(), 3);
    }

    #[test]
    fn test_parse_script_after_body_not_moved_into_head() {
        let doc = parse("<html><head></head><body><p>a</p></body><script>x</script></html>");
        assert_eq!(html_child_names(&doc), ["head", "body", "script"]);
    }

    #[test]
    fn test_no_implied_keeps_the_default_doctype() {
        // libxml2 leaves the DOCTYPE out for HTML_PARSE_NODEFDTD alone.
        let opts = HtmlParseOptions::default().no_implied(true);
        let doc = parse_html_with_options("<p>x</p>", &opts).unwrap();
        let first = doc.first_child(doc.root()).unwrap();
        assert!(matches!(
            doc.node(first).kind,
            NodeKind::DocumentType { .. }
        ));
    }

    #[test]
    fn test_parse_html_keeps_the_first_of_duplicate_html_and_body_attributes() {
        let doc = parse(r#"<html lang="en" lang="fr"><body class=a class=b>x"#);
        let attributes = |name: &str| {
            let element = doc
                .descendants(doc.root())
                .find(|&n| doc.node_name(n) == Some(name))
                .unwrap();
            doc.attributes(element)
                .iter()
                .map(|a| format!("{}={}", a.name, a.value))
                .collect::<Vec<_>>()
        };
        assert_eq!(attributes("html"), ["lang=en"]);
        assert_eq!(attributes("body"), ["class=a"]);
    }

    #[test]
    fn test_parse_html_drops_a_start_tag_cut_off_by_the_end_of_input() {
        // As libxml2 2.14 does: the tag is unfinished, so there is no element.
        for (input, tag) in [
            ("<p>a<b", "b"),
            ("<p>a<a title=\"x>y", "a"),
            ("<p>a<div att=", "div"),
            ("<p>a</p><img src=\"a", "img"),
        ] {
            let doc = parse(input);
            let names: Vec<_> = doc
                .descendants(doc.root())
                .filter_map(|n| doc.node_name(n))
                .collect();
            assert!(!names.contains(&tag), "{input}: {names:?}");
            let p = doc
                .descendants(doc.root())
                .find(|&n| doc.node_name(n) == Some("p"))
                .unwrap();
            assert_eq!(doc.text_content(p), "a", "{input}");
        }
    }

    #[test]
    fn test_misplaced_head_is_ignored_with_a_warning() {
        // A <head> with anything but <html> open, as libxml2's
        // htmlParseStartTag reports it.
        for input in ["<p>x</p><head></head>", "<title>t</title><head></head><p>x"] {
            let doc = parse(input);
            let messages: Vec<_> = doc.diagnostics.iter().map(|d| d.message.as_str()).collect();
            assert!(
                messages.contains(&"misplaced <head> tag"),
                "{input}: {messages:?}"
            );
        }
        let doc = parse("<head></head><p>x");
        assert!(doc
            .diagnostics
            .iter()
            .all(|d| d.message != "misplaced <head> tag"));
        // The end tag of a <head> it dropped is dropped too, unreported.
        let doc = parse("<p>x</p><head></head>");
        assert!(doc
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("</head>")));
    }

    #[test]
    fn test_empty_head_is_kept_only_from_a_start_tag_before_the_body() {
        assert_eq!(
            html_child_names(&parse("<head></head><p>x")),
            ["head", "body"]
        );
        assert_eq!(html_child_names(&parse("<p>x</p><head></head>")), ["body"]);
        assert_eq!(html_child_names(&parse("<p>x")), ["body"]);
    }
}
