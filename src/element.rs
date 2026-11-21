//! Conversion between Satori element strings and Matrix message content.
//!
//! Satori elements are an XML-like mini language:
//!   `hello <at id="@u:s"/> world <img src="mxc://s/abc"/>`
//! Matrix messages carry plain `body` plus optional `formatted_body` (HTML).

use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub tag: String,
    pub attrs: HashMap<String, String>,
    pub children: Vec<Node>,
    pub self_closing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text(String),
    Element(Element),
}

impl Element {
    pub fn new(tag: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            attrs: HashMap::new(),
            children: Vec::new(),
            self_closing: false,
        }
    }
}
/// Parse a Satori element string into a node tree.
///
/// Faithful port of the official `@satorijs/element` parser (`parse` in
/// `packages/element/src/index.ts`): a regex tokenizer produces tokens,
/// `fold_tokens` folds them into a tree using a stack, and `parse_tokens`
/// builds the final nodes. Comments are dropped, text is unescaped, and
/// attributes follow `key`, `no-key`, `key="value"` / `key='value'`.
pub fn parse(source: &str) -> Vec<Node> {
    let tokens = tokenize(source);
    let folded = fold_tokens(&tokens);
    parse_tokens(&folded)
}

// Official: tagRegExp1 (context-free form)
//   /(?<comment><!--[\s\S]*?-->)|(?<tag><(\/?)([^!\s>/]*)([^>]*?)\s*(\/?)>)/
// Official: attrRegExp1
//   /([^\s=]+)(?:="(?<value1>[^"]*)"|='(?<value2>[^']*)')?/g
use std::sync::OnceLock;

static TAG_RE: OnceLock<Regex> = OnceLock::new();
static ATTR_RE: OnceLock<Regex> = OnceLock::new();

fn tag_re() -> &'static Regex {
    TAG_RE.get_or_init(|| {
        Regex::new(r"(?P<comment><!--[\s\S]*?-->)|(?P<tag><(?P<close>/?)(?P<type>[^!\s>/]*)(?P<extra>[^>]*?)\s*(?P<empty>/?)>)").unwrap()
    })
}
fn attr_re() -> &'static Regex {
    ATTR_RE.get_or_init(|| {
        Regex::new(r#"([^\s=]+)(?:="(?P<value1>[^"]*)"|='(?P<value2>[^']*)')?"#).unwrap()
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Position {
    Open,
    Close,
    Empty,
}

#[derive(Debug, Clone)]
enum Token {
    Text(String),
    Angle {
        name: String,
        position: Position,
        extra: String,
        children: Vec<Token>,
    },
}

/// Official `parse`'s tokenize loop + `parseContent`.
fn tokenize(source: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut rest = source;
    let mut trim_start = true;

    while let Some(cap) = tag_re().captures(rest) {
        let whole = cap.get(0).unwrap();
        // trimEnd = !curly ; we have no curly tokens, so always true.
        parse_content(&rest[..whole.start()], trim_start, true, &mut tokens);
        trim_start = true;
        rest = &rest[whole.end()..];

        // comment -> skip
        if cap.name("comment").is_some() {
            continue;
        }

        let close = cap
            .name("close")
            .map(|m| m.as_str() == "/")
            .unwrap_or(false);
        let empty = cap
            .name("empty")
            .map(|m| m.as_str() == "/")
            .unwrap_or(false);
        let typ = cap.name("type").map(|m| m.as_str()).unwrap_or("");
        let extra = cap.name("extra").map(|m| m.as_str()).unwrap_or("");
        let name = if typ.is_empty() {
            "template".to_string()
        } else {
            typ.to_string()
        };
        let position = if close {
            Position::Close
        } else if empty {
            Position::Empty
        } else {
            Position::Open
        };
        tokens.push(Token::Angle {
            name,
            position,
            extra: extra.to_string(),
            children: Vec::new(),
        });
    }

    parse_content(rest, trim_start, true, &mut tokens);
    tokens
}

/// Official `parseContent`: unescape + trim a single leading/trailing newline
/// with surrounding whitespace, then push as a text token if non-empty.
fn parse_content(raw: &str, trim_start: bool, trim_end: bool, out: &mut Vec<Token>) {
    let mut text = official_unescape(raw);
    if trim_start {
        text = trim_newline_start(&text);
    }
    if trim_end {
        text = trim_newline_end(&text);
    }
    if !text.is_empty() {
        out.push(Token::Text(text));
    }
}

// /^\s*\n\s*/ and /\s*\n\s*$/ — only strip whitespace that wraps a newline.
fn trim_newline_start(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    // find a newline within the leading whitespace run
    if let Some(nl) = s[..i].find('\n') {
        let mut j = nl + 1;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        s[j..].to_string()
    } else {
        s.to_string()
    }
}
fn trim_newline_end(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut j = bytes.len();
    while j > 0 && (bytes[j - 1] as char).is_whitespace() {
        j -= 1;
    }
    if let Some(nl_rel) = s[j..].rfind('\n') {
        let nl = j + nl_rel;
        let mut i = nl;
        while i > 0 && (bytes[i - 1] as char).is_whitespace() {
            i -= 1;
        }
        s[..i].to_string()
    } else {
        s.to_string()
    }
}

/// Official `foldTokens`: reduce the flat token stream into a tree via a stack.
/// On CLOSE, pop the stack while the top name matches. OPEN pushes a new frame;
/// EMPTY/CONTINUE/text attach to the current frame.
fn fold_tokens(tokens: &[Token]) -> Vec<Token> {
    // Each frame: (name, children). Root is a synthetic "template" frame.
    let mut stack: Vec<(String, Vec<Token>)> = vec![("template".to_string(), Vec::new())];

    for tok in tokens {
        match tok {
            Token::Text(_)
            | Token::Angle {
                position: Position::Empty,
                ..
            } => {
                stack.last_mut().unwrap().1.push(tok.clone());
            }
            Token::Angle {
                position: Position::Close,
                name,
                ..
            } => {
                if stack.len() > 1 && stack.last().unwrap().0 == *name {
                    // Pop the frame and write its collected children back into
                    // the OPEN node we pushed into the parent frame. Official
                    // JS does this via shared object references; Rust needs an
                    // explicit assignment.
                    let (_, children) = stack.pop().unwrap();
                    if let Some(Token::Angle { children: slot, .. }) =
                        stack.last_mut().unwrap().1.last_mut()
                    {
                        *slot = children;
                    }
                }
            }
            Token::Angle {
                position: Position::Open,
                name,
                extra,
                ..
            } => {
                // Attach the open token to the current frame, then descend into it.
                let node = Token::Angle {
                    name: name.clone(),
                    position: Position::Open,
                    extra: extra.clone(),
                    children: Vec::new(),
                };
                stack.last_mut().unwrap().1.push(node);
                stack.push((name.clone(), Vec::new()));
            }
        }
    }

    // Unwind: fold each frame's children into the pushed open node.
    // Because we pushed children into frames by index, rebuild recursively.
    // The root frame (index 0) holds the final tree.
    rebuild_from_stack(stack)
}

/// The stack frames hold children separately from their parent node; walk from
/// the innermost frame outward, attaching each frame's children to the last
/// OPEN node pushed into the parent frame.
fn rebuild_from_stack(mut stack: Vec<(String, Vec<Token>)>) -> Vec<Token> {
    while stack.len() > 1 {
        let (_, children) = stack.pop().unwrap();
        let parent = stack.last_mut().unwrap();
        if let Some(Token::Angle { children: slot, .. }) = parent.1.last_mut() {
            *slot = children;
        }
    }
    stack
        .pop()
        .map(|(_, children)| children)
        .unwrap_or_default()
}

/// Official `parseTokens`: turn folded tokens into nodes. Text tokens become
/// `Node::Text`; angle tokens become `Node::Element` with parsed attributes.
fn parse_tokens(tokens: &[Token]) -> Vec<Node> {
    let mut result = Vec::new();
    for tok in tokens {
        match tok {
            Token::Text(t) => result.push(Node::Text(t.clone())),
            Token::Angle {
                name,
                extra,
                position,
                children,
            } => {
                let attrs = parse_attr_string(extra);
                let mut el = Element::new(name.clone());
                el.attrs = attrs;
                el.self_closing = *position == Position::Empty || children.is_empty();
                el.children = parse_tokens(children);
                result.push(Node::Element(el));
            }
        }
    }
    result
}

/// Official attribute loop using attrRegExp1:
///   key="v" / key='v' -> unescape(v)
///   key              -> "" (boolean true, stored as empty value)
///   no-key           -> "" (boolean false under key without `no-`)
/// Official stores JS booleans; our Element uses String values, so `true`
/// becomes an empty string and `no-*` maps to the key without the prefix.
fn parse_attr_string(extra: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    for cap in attr_re().captures_iter(extra) {
        let key = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if key.is_empty() {
            continue;
        }
        if let Some(v) = cap.name("value1").or_else(|| cap.name("value2")) {
            attrs.insert(key.to_string(), official_unescape(v.as_str()));
        } else if let Some(rest) = key.strip_prefix("no-") {
            attrs.insert(rest.to_string(), String::new());
        } else {
            attrs.insert(key.to_string(), String::new());
        }
    }
    attrs
}

/// Official `Element.unescape`, same order:
///   &lt; &gt; &quot; &#\d+; &#xHEX; &(amp|#38|#x26);
fn official_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
/// Serialize nodes back to a Satori element string.
pub fn to_string(nodes: &[Node]) -> String {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(&html_escape_text(t)),
            Node::Element(el) => {
                out.push('<');
                out.push_str(&el.tag);
                let mut keys: Vec<_> = el.attrs.keys().collect();
                keys.sort();
                for k in keys {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&html_escape_attr(&el.attrs[k]));
                    out.push('"');
                }
                if el.self_closing {
                    out.push_str("/>");
                } else {
                    out.push('>');
                    out.push_str(&to_string(&el.children));
                    out.push_str("</");
                    out.push_str(&el.tag);
                    out.push('>');
                }
            }
        }
    }
    out
}

fn text_of(nodes: &[Node]) -> String {
    let mut s = String::new();
    for n in nodes {
        match n {
            Node::Text(t) => s.push_str(t),
            Node::Element(el) => s.push_str(&text_of(&el.children)),
        }
    }
    s
}

/// Convert a Satori element string into a Matrix `m.room.message` content object.
pub fn satori_to_matrix(content: &str, homeserver: &str) -> Value {
    let nodes = parse(content);
    let mut plain = String::new();
    let mut html = String::new();
    let mut media: Option<(String, String, Option<Value>)> = None; // (tag, mxc, info)
    let mut reply_to: Option<String> = None;

    for node in &nodes {
        match node {
            Node::Text(t) => {
                plain.push_str(t);
                html.push_str(&html_escape_text(t));
            }
            Node::Element(el) => match el.tag.as_str() {
                "at" | "sharp" => {
                    let id = el.attrs.get("id").cloned().unwrap_or_default();
                    let name = el.attrs.get("name").cloned().unwrap_or_else(|| id.clone());
                    plain.push_str(&name);
                    let href = format!("https://matrix.to/#/{}", html_escape_attr(&id));
                    html.push_str(&format!(
                        "<a href=\"{}\">{}</a>",
                        href,
                        html_escape_text(&name)
                    ));
                }
                "emoji" => {
                    let name = el
                        .attrs
                        .get("name")
                        .or_else(|| el.attrs.get("id"))
                        .cloned()
                        .unwrap_or_default();
                    plain.push_str(&name);
                    html.push_str(&html_escape_text(&name));
                }
                "quote" => {
                    if let Some(id) = el.attrs.get("id") {
                        reply_to = Some(id.clone());
                    }
                    let inner = text_of(&el.children);
                    if !inner.is_empty() {
                        plain.push_str(&inner);
                        html.push_str(&format!(
                            "<blockquote>{}</blockquote>",
                            html_escape_text(&inner)
                        ));
                    }
                }
                "img" | "audio" | "video" | "file" => {
                    let src = el
                        .attrs
                        .get("src")
                        .or_else(|| el.attrs.get("url"))
                        .cloned()
                        .unwrap_or_default();
                    if src.is_empty() {
                        continue;
                    }
                    let mxc = if src.starts_with("mxc://") {
                        src.clone()
                    } else {
                        // remote URL: Matrix cannot embed directly; keep as link
                        plain.push_str(&src);
                        html.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            html_escape_attr(&src),
                            html_escape_text(&src)
                        ));
                        continue;
                    };
                    let title = el.attrs.get("title").cloned().unwrap_or_default();
                    let info = build_media_info(el);
                    media = Some((el.tag.clone(), mxc, info));
                    if !title.is_empty() {
                        plain.push_str(&title);
                    }
                }
                "br" => {
                    plain.push('\n');
                    html.push_str("<br/>");
                }
                _ => {
                    let inner = text_of(&el.children);
                    plain.push_str(&inner);
                    html.push_str(&html_escape_text(&inner));
                }
            },
        }
    }

    let (msgtype, url, info) = match media {
        Some((tag, mxc, info)) => {
            let mt = match tag.as_str() {
                "img" => "m.image",
                "audio" => "m.audio",
                "video" => "m.video",
                _ => "m.file",
            };
            (mt.to_string(), Some(mxc), info)
        }
        None => ("m.text".to_string(), None, None),
    };

    if html.is_empty() {
        html = html_escape_text(&plain);
    }

    let mut content = json!({
        "msgtype": msgtype,
        "body": if plain.is_empty() { media_filename(&url) } else { plain },
        "format": "org.matrix.custom.html",
        "formatted_body": html,
    });
    if let Some(u) = url {
        content["url"] = json!(u);
    }
    if let Some(i) = info {
        content["info"] = i;
    }
    if let Some(rel) = reply_to {
        content["m.relates_to"] = json!({"m.in_reply_to": {"event_id": rel}});
    }
    let _ = homeserver;
    content
}

fn build_media_info(el: &Element) -> Option<Value> {
    let mut info = serde_json::Map::new();
    if let Some(v) = el.attrs.get("mime_type").or_else(|| el.attrs.get("mime")) {
        info.insert("mimetype".into(), json!(v));
    }
    if let Some(v) = el.attrs.get("size").and_then(|s| s.parse::<i64>().ok()) {
        info.insert("size".into(), json!(v));
    }
    if let Some(v) = el.attrs.get("width").and_then(|s| s.parse::<i64>().ok()) {
        info.insert("w".into(), json!(v));
    }
    if let Some(v) = el.attrs.get("height").and_then(|s| s.parse::<i64>().ok()) {
        info.insert("h".into(), json!(v));
    }
    if let Some(v) = el.attrs.get("duration").and_then(|s| s.parse::<i64>().ok()) {
        info.insert("duration".into(), json!(v));
    }
    if info.is_empty() {
        None
    } else {
        Some(Value::Object(info))
    }
}

fn media_filename(url: &Option<String>) -> String {
    url.as_ref()
        .and_then(|u| u.rsplit('/').next())
        .unwrap_or("file")
        .to_string()
}

/// Convert a Matrix message event content into a Satori element string.
pub fn matrix_to_satori(content: &Value, homeserver: &str, self_id: &str) -> String {
    let msgtype = content
        .get("msgtype")
        .and_then(Value::as_str)
        .unwrap_or("m.text");
    let body = content.get("body").and_then(Value::as_str).unwrap_or("");

    // Prefer HTML formatted_body and translate known tags.
    if let Some(html) = content.get("formatted_body").and_then(Value::as_str) {
        if content.get("format").and_then(Value::as_str) == Some("org.matrix.custom.html") {
            return html_to_satori(html, homeserver, self_id);
        }
    }

    match msgtype {
        "m.image" | "m.audio" | "m.video" | "m.file" => {
            let tag = match msgtype {
                "m.image" => "img",
                "m.audio" => "audio",
                "m.video" => "video",
                _ => "file",
            };
            let src = content.get("url").and_then(Value::as_str).unwrap_or("");
            let http = mxc_to_http(src, homeserver);
            if src.is_empty() {
                html_escape_text(body)
            } else {
                format!(
                    "<{} src=\"{}\" title=\"{}\"/>",
                    tag,
                    html_escape_attr(&http),
                    html_escape_attr(body)
                )
            }
        }
        "m.emote" => format!("* {}", html_escape_text(body)),
        "m.notice" => html_escape_text(body),
        _ => html_escape_text(body),
    }
}

fn mxc_to_http(src: &str, homeserver: &str) -> String {
    match src.strip_prefix("mxc://") {
        Some(rest) => format!(
            "{}/_matrix/media/v3/download/{}",
            homeserver.trim_end_matches('/'),
            rest
        ),
        None => src.to_string(),
    }
}

/// Translate a subset of Matrix HTML into Satori elements.
///
/// A Matrix mention is `<a href="https://matrix.to/#/@u:s">display</a>`. It
/// must become a single `<at id="@u:s" name="display"/>` — the link text is the
/// mention label, not a separate text node. We track an active at-link with a
/// small state machine so the inner text is absorbed into the `name` attribute
/// and the closing `</a>` is consumed rather than emitted.
fn html_to_satori(html: &str, homeserver: &str, self_id: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    // When set: we are inside a matrix.to link; collect its label here.
    let mut at_link: Option<String> = None;
    while i < chars.len() {
        if chars[i] == '<' {
            let mut j = i + 1;
            let mut quote: Option<char> = None;
            while j < chars.len() {
                let c = chars[j];
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '>' {
                    break;
                }
                j += 1;
            }
            if j >= chars.len() {
                // Malformed tag: if inside an at link, treat literally.
                if let Some(label) = at_link.as_mut() {
                    label.push('<');
                } else {
                    out.push('<');
                }
                i += 1;
                continue;
            }
            let raw: String = chars[i + 1..j].iter().collect();
            let raw = raw.trim();
            let lower = raw.to_ascii_lowercase();
            let closing = lower.starts_with('/');
            let name_only = lower
                .strip_prefix('/')
                .unwrap_or(&lower)
                .split_whitespace()
                .next()
                .unwrap_or("");
            let self_closing = lower.ends_with('/');

            let attrs = parse_attrs(raw);
            let tag = name_only.trim_end_matches('/');

            if tag == "a" && !closing {
                let href = attrs.get("href").cloned().unwrap_or_default();
                if let Some(mxid) = href.strip_prefix("https://matrix.to/#/") {
                    // Start collecting the mention label; emit once we see </a>.
                    // Buffer stores "mxid\u{1}label" — separator splits the two.
                    at_link = Some(format!("{}\u{1}", mxid));
                } else {
                    out.push_str("<a href=\"");
                    out.push_str(&html_escape_attr(&href));
                    out.push_str("\">");
                }
            } else if tag == "a" && closing {
                if let Some(buf) = at_link.take() {
                    // buf = "<mxid>\u{1}<label>"
                    let (mxid, label) = buf.split_once('\u{1}').unwrap_or((&buf, ""));
                    let name = if label.is_empty() { mxid } else { label };
                    out.push_str(&format!(
                        "<at id=\"{}\" name=\"{}\"/>",
                        html_escape_attr(mxid),
                        html_escape_attr(name)
                    ));
                } else {
                    out.push_str("</a>");
                }
            } else if let Some(buf) = at_link.as_mut() {
                // Inside a mention: absorb nested tags minimally as text.
                buf.push_str(raw);
            } else if tag == "img" {
                let src = attrs.get("src").cloned().unwrap_or_default();
                let src = mxc_to_http(&src, homeserver);
                out.push_str(&format!("<img src=\"{}\"/>", html_escape_attr(&src)));
            } else if tag == "br" || tag == "hr" {
                out.push_str("<br/>");
            } else if closing {
                match tag {
                    "p" | "div" | "h1" | "h2" | "h3" | "blockquote" => out.push_str("<br/>"),
                    "b" | "strong" | "i" | "em" | "u" | "s" | "del" | "code" | "pre" | "span"
                    | "font" => {
                        out.push_str("</");
                        out.push_str(tag);
                        out.push('>');
                    }
                    _ => {}
                }
            } else {
                match tag {
                    "b" | "strong" | "i" | "em" | "u" | "s" | "del" | "code" | "pre" | "span"
                    | "font" => {
                        out.push('<');
                        out.push_str(tag);
                        out.push('>');
                    }
                    "p" | "div" | "h1" | "h2" | "h3" | "blockquote" => {}
                    _ => {
                        if tag.len() <= 16 && !tag.is_empty() {
                            match tag {
                                "at" | "sharp" => {
                                    let id = attrs.get("id").cloned().unwrap_or_default();
                                    let name =
                                        attrs.get("name").cloned().unwrap_or_else(|| id.clone());
                                    if self_closing {
                                        out.push_str(&format!(
                                            "<at id=\"{}\" name=\"{}\"/>",
                                            html_escape_attr(&id),
                                            html_escape_attr(&name)
                                        ));
                                    } else {
                                        out.push_str(&format!(
                                            "<at id=\"{}\" name=\"{}\">",
                                            html_escape_attr(&id),
                                            html_escape_attr(&name)
                                        ));
                                    }
                                }
                                "img" | "audio" | "video" | "file" => {
                                    let src = attrs.get("src").cloned().unwrap_or_default();
                                    let src = mxc_to_http(&src, homeserver);
                                    out.push_str(&format!(
                                        "<{} src=\"{}\"/>",
                                        tag,
                                        html_escape_attr(&src)
                                    ));
                                }
                                _ => out.push_str(tag),
                            }
                        }
                    }
                }
            }
            i = j + 1;
        } else if let Some(buf) = at_link.as_mut() {
            // Absorb the mention label text into the buffer.
            buf.push(chars[i]);
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    // Unclosed at-link: flush it as an at element with whatever label we have.
    if let Some(buf) = at_link.take() {
        let (mxid, label) = buf.split_once('\u{1}').unwrap_or((&buf, ""));
        let name = if label.is_empty() { mxid } else { label };
        out.push_str(&format!(
            "<at id=\"{}\" name=\"{}\"/>",
            html_escape_attr(mxid),
            html_escape_attr(name)
        ));
    }
    let _ = self_id;
    out
}

fn parse_attrs(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut in_tag = raw.split_whitespace();
    let _ = in_tag.next();
    let rest: String = match raw.find(char::is_whitespace) {
        Some(idx) => raw[idx..].to_string(),
        None => return map,
    };
    for pair in rest.split_whitespace() {
        if let Some((k, v)) = pair.split_once('=') {
            let v = v.trim_matches('"').trim_matches('\'');
            map.insert(k.to_ascii_lowercase(), html_unescape(v));
        }
    }
    map
}

pub fn html_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn html_escape_attr(s: &str) -> String {
    html_escape_text(s).replace('"', "&quot;")
}

pub fn html_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_attributes_unescaped() {
        // Official unescape decodes &amp; / &quot; in attribute values.
        let nodes = parse("<at id=\"a&amp;b\" name=\"&quot;Q&quot;\"/>");
        match &nodes[0] {
            Node::Element(el) => {
                assert_eq!(el.attrs.get("id").unwrap(), "a&b");
                assert_eq!(el.attrs.get("name").unwrap(), "\"Q\"");
            }
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn xml_comment_dropped() {
        // The official parser discards <!-- -->; the surrounding texts stay
        // as separate text tokens (parseTokens does not merge them).
        let nodes = parse("a<!-- note -->b");
        assert_eq!(nodes, vec![Node::Text("a".into()), Node::Text("b".into())]);
    }

    #[test]
    fn attr_boolean_and_no_prefix() {
        // Official: bare key -> true (empty value here); `no-key` -> key=false.
        let nodes = parse("<msg hello no-draft id=\"x\"/>");
        match &nodes[0] {
            Node::Element(el) => {
                assert!(el.attrs.contains_key("hello"));
                assert!(el.attrs.contains_key("draft"));
                assert_eq!(el.attrs.get("id").unwrap(), "x");
            }
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn numeric_entities_unescaped() {
        // Official unescape handles &#39; / &apos; etc.
        let nodes = parse("<at name=\"a&#39;b\"/>");
        match &nodes[0] {
            Node::Element(el) => assert_eq!(el.attrs.get("name").unwrap(), "a'b"),
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn parse_simple() {
        let nodes = parse("hello <at id=\"@u:s\"/> world");
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0], Node::Text("hello ".into()));
        match &nodes[1] {
            Node::Element(el) => {
                assert_eq!(el.tag, "at");
                assert_eq!(el.attrs.get("id").unwrap(), "@u:s");
            }
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn parse_nested() {
        let nodes = parse("<quote id=\"$e\">hi <b>there</b></quote>");
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            Node::Element(el) => {
                assert_eq!(el.tag, "quote");
                assert_eq!(el.children.len(), 2);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn roundtrip() {
        let src = "hello <at id=\"@u:s\"/> <img src=\"mxc://s/abc\"/>";
        let nodes = parse(src);
        let out = to_string(&nodes);
        let reparsed = parse(&out);
        assert_eq!(nodes, reparsed);
    }

    #[test]
    fn satori_to_matrix_text() {
        let v = satori_to_matrix(
            "hello <at id=\"@u:s\" name=\"Bob\"/>",
            "https://m.example.com",
        );
        assert_eq!(v["msgtype"], "m.text");
        assert_eq!(v["body"], "hello Bob");
        assert!(v["formatted_body"]
            .as_str()
            .unwrap()
            .contains("<a href=\"https://matrix.to/#/@u:s\">Bob</a>"));
    }

    #[test]
    fn satori_to_matrix_image() {
        let v = satori_to_matrix(
            "<img src=\"mxc://s/abc\" width=\"100\"/>",
            "https://m.example.com",
        );
        assert_eq!(v["msgtype"], "m.image");
        assert_eq!(v["url"], "mxc://s/abc");
        assert_eq!(v["info"]["w"], 100);
    }

    #[test]
    fn matrix_to_satori_text() {
        let v = serde_json::json!({"msgtype": "m.text", "body": "hi <there>"});
        assert_eq!(
            matrix_to_satori(&v, "https://m.example.com", "@bot:s"),
            "hi &lt;there&gt;"
        );
    }

    #[test]
    fn matrix_html_at() {
        let v = serde_json::json!({
            "msgtype": "m.text", "body": "Bob", "format": "org.matrix.custom.html",
            "formatted_body": "hi <a href=\"https://matrix.to/#/@u:s\">Bob</a>"
        });
        let s = matrix_to_satori(&v, "https://m.example.com", "@bot:s");
        // A mention becomes a single <at> carrying the label as name; the link
        // text must NOT be duplicated as a separate text node.
        assert!(s.contains("<at id=\"@u:s\" name=\"Bob\"/>"), "got: {}", s);
        assert!(!s.contains("Bob</a>"), "link text leaked: {}", s);
        assert_eq!(s, "hi <at id=\"@u:s\" name=\"Bob\"/>");
    }

    #[test]
    fn matrix_image() {
        let v = serde_json::json!({"msgtype": "m.image", "body": "pic.png", "url": "mxc://s/abc"});
        let s = matrix_to_satori(&v, "https://m.example.com", "@bot:s");
        assert_eq!(s, "<img src=\"https://m.example.com/_matrix/media/v3/download/s/abc\" title=\"pic.png\"/>");
    }
}
