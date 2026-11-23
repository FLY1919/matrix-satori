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

/// Render a single node (and its subtree) as `(plain, html)` for inline
/// decoration elements. `satori_to_matrix` uses this for elements that wrap
/// their children (bold / italic / spoiler / code / link / message / ...), so
/// nested decorations and mentions keep their HTML semantics instead of being
/// flattened to bare text.
fn render_node_inline(node: &Node) -> (String, String) {
    match node {
        Node::Text(t) => (t.clone(), html_escape_text(t)),
        Node::Element(el) => {
            // Leaf elements whose data lives entirely in attributes.
            match el.tag.as_str() {
                "at" | "sharp" => {
                    let id = el.attrs.get("id").cloned().unwrap_or_default();
                    let name = el.attrs.get("name").cloned().unwrap_or_else(|| id.clone());
                    let href = format!("https://matrix.to/#/{}", html_escape_attr(&id));
                    return (
                        name.clone(),
                        format!("<a href=\"{}\">{}</a>", href, html_escape_text(&name)),
                    );
                }
                "emoji" => {
                    let name = el
                        .attrs
                        .get("name")
                        .or_else(|| el.attrs.get("id"))
                        .cloned()
                        .unwrap_or_default();
                    return (name.clone(), html_escape_text(&name));
                }
                "br" => return ("\n".into(), "<br/>".into()),
                _ => {}
            }

            // Recurse into children first, then wrap with this element's tag.
            let mut child_plain = String::new();
            let mut child_html = String::new();
            for child in &el.children {
                let (p, h) = render_node_inline(child);
                child_plain.push_str(&p);
                child_html.push_str(&h);
            }

            let wrapped: (Option<String>, Option<String>) = match el.tag.as_str() {
                "b" | "strong" => (Some("b".into()), Some("b".into())),
                "i" | "em" => (Some("i".into()), Some("i".into())),
                "u" | "ins" => (Some("u".into()), Some("u".into())),
                "s" | "del" => (Some("del".into()), Some("del".into())),
                "spl" => (Some("span data-mx-spoiler".into()), Some("span".into())),
                "code" => (Some("code".into()), Some("code".into())),
                "sup" => (Some("sup".into()), Some("sup".into())),
                "sub" => (Some("sub".into()), Some("sub".into())),
                "a" => {
                    let href = el.attrs.get("href").cloned().unwrap_or_default();
                    if href.is_empty() {
                        (None, None)
                    } else {
                        (
                            Some(format!("a href=\"{}\"", html_escape_attr(&href))),
                            Some("a".into()),
                        )
                    }
                }
                _ => (None, None),
            };

            match wrapped {
                (Some(open), Some(close)) => {
                    (child_plain, format!("<{}>{}</{}>", open, child_html, close))
                }
                _ => (child_plain, child_html),
            }
        }
    }
}

/// Collect every remote (http/https) media URL found in img/audio/video/file
/// elements. Used by the async upload pass before calling
/// `satori_to_matrix_with_media`; mxc:// sources are already uploadable and are
/// skipped. Order is preserved and duplicates removed so the caller uploads
/// each distinct URL once.
pub fn collect_remote_media_urls(content: &str) -> Vec<String> {
    fn walk(nodes: &[Node], out: &mut Vec<String>) {
        for node in nodes {
            if let Node::Element(el) = node {
                if matches!(el.tag.as_str(), "img" | "audio" | "video" | "file") {
                    if let Some(src) = el
                        .attrs
                        .get("src")
                        .or_else(|| el.attrs.get("url"))
                        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
                    {
                        if !out.contains(src) {
                            out.push(src.clone());
                        }
                    }
                }
                walk(&el.children, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&parse(content), &mut out);
    out
}

/// Convert a Satori element string into a Matrix `m.room.message` content object.
///
/// Remote media URLs that appear in `resolved` (url -> mxc://) are emitted as
/// real media messages; anything else is kept as a plain link. The async layer
/// downloads/uploads remote URLs first, then passes the map here.
pub fn satori_to_matrix_with_media(
    content: &str,
    homeserver: &str,
    resolved: &HashMap<String, String>,
) -> Value {
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
                    } else if let Some(uploaded) = resolved.get(&src) {
                        // Remote URL was downloaded and uploaded to the
                        // homeserver by the async layer; embed the real mxc.
                        uploaded.clone()
                    } else {
                        // Download failed (e.g. anti-hotlink 403 on a music
                        // CDN). Keep the media message type with the original
                        // URL so the client shows a playable/downloadable
                        // attachment instead of a bare text link.
                        src.clone()
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
                    // Decoration / link / layout elements wrap their children.
                    // Recurse so nested tags (bold containing a mention, links
                    // with href, spoilers, ...) keep their HTML semantics.
                    let (p, h) = render_node_inline(node);
                    plain.push_str(&p);
                    html.push_str(&h);
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

/// Convenience wrapper with no pre-uploaded media. Remote media URLs stay as
/// links. Production code should use `satori_to_matrix_with_media` after the
/// async upload pass.
pub fn satori_to_matrix(content: &str, homeserver: &str) -> Value {
    satori_to_matrix_with_media(content, homeserver, &HashMap::new())
}

/// Split one Satori content string into one or more Matrix message contents.
///
/// Matrix has a single `msgtype` per event, so a mixed message (text + several
/// images/audio) cannot be one event without losing parts. We split by top
/// level node:
///   * contiguous text / inline decoration nodes -> one `m.text`
///   * each `img` / `audio` / `video` / `file` -> its own media message
///   * a leading `quote` element is remembered as the reply target and applied
///     to every segment, so a quoted mixed message keeps its reply relation.
///
/// Elements Matrix cannot represent (message/author/button/...) are rendered
/// by the normal inline fallback into the current text segment, so nothing is
/// silently dropped.
pub fn satori_to_matrix_segments(
    content: &str,
    homeserver: &str,
    resolved: &HashMap<String, String>,
) -> Vec<Value> {
    let nodes = parse(content);
    let mut segments: Vec<Value> = Vec::new();
    let mut text_buf: Vec<Node> = Vec::new();
    let mut quote_target: Option<String> = None;

    /// Render accumulated inline/text nodes into one m.text content.
    fn flush_text(
        buf: &mut Vec<Node>,
        homeserver: &str,
        quote: Option<&str>,
        out: &mut Vec<Value>,
    ) {
        if buf.is_empty() {
            return;
        }
        let s = to_string(buf);
        let mut v = satori_to_matrix_with_media(&s, homeserver, &HashMap::new());
        if let Some(qid) = quote {
            v["m.relates_to"] = json!({"m.in_reply_to": {"event_id": qid}});
        }
        out.push(v);
        buf.clear();
    }

    for node in nodes {
        match node {
            Node::Element(el) if el.tag == "quote" => {
                if let Some(id) = el.attrs.get("id") {
                    quote_target = Some(id.clone());
                }
                // The quoted text itself is decoration; keep it as text so the
                // reply still shows the quoted excerpt inline.
                text_buf.push(Node::Element(el));
            }
            Node::Element(el) if matches!(el.tag.as_str(), "img" | "audio" | "video" | "file") => {
                flush_text(
                    &mut text_buf,
                    homeserver,
                    quote_target.as_deref(),
                    &mut segments,
                );
                // Turn this single media element into one message. Reuse the
                // existing converter by serializing it back to a Satori string.
                let s = to_string(&[Node::Element(el)]);
                let mut v = satori_to_matrix_with_media(&s, homeserver, resolved);
                if let Some(qid) = quote_target.as_deref() {
                    v["m.relates_to"] = json!({"m.in_reply_to": {"event_id": qid}});
                }
                segments.push(v);
            }
            other => text_buf.push(other),
        }
    }
    flush_text(
        &mut text_buf,
        homeserver,
        quote_target.as_deref(),
        &mut segments,
    );

    // Empty content still produces one empty text message, matching the old
    // single-message behaviour (callers expect at least one event id back).
    if segments.is_empty() {
        let mut v = json!({
            "msgtype": "m.text", "body": "",
            "format": "org.matrix.custom.html", "formatted_body": "",
        });
        if let Some(qid) = quote_target.as_deref() {
            v["m.relates_to"] = json!({"m.in_reply_to": {"event_id": qid}});
        }
        segments.push(v);
    }
    segments
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
    // Matrix spoilers are `<span data-mx-spoiler>...</span>`; track whether
    // each open <span> is a spoiler so the matching </span> becomes </spl>.
    let mut spoiler_stack: Vec<bool> = Vec::new();
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
                    "span" => {
                        if spoiler_stack.pop().unwrap_or(false) {
                            out.push_str("</spl>");
                        } else {
                            out.push_str("</span>");
                        }
                    }
                    "b" | "strong" | "i" | "em" | "u" | "s" | "del" | "code" | "pre" | "sup"
                    | "sub" | "font" => {
                        out.push_str("</");
                        out.push_str(tag);
                        out.push('>');
                    }
                    _ => {}
                }
            } else {
                match tag {
                    "span" => {
                        let is_spoiler = lower.contains("data-mx-spoiler");
                        spoiler_stack.push(is_spoiler);
                        if is_spoiler {
                            out.push_str("<spl>");
                        } else {
                            out.push_str("<span>");
                        }
                    }
                    "b" | "strong" | "i" | "em" | "u" | "s" | "del" | "code" | "pre" | "sup"
                    | "sub" | "font" => {
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

    // ===== 覆盖 Satori 官方全部标准元素 =====
    // 依据 https://satori.chat/zh-CN/protocol/elements.html
    const HS: &str = "https://m.example.com";

    fn collect_tags(nodes: &[Node], out: &mut Vec<String>) {
        for n in nodes {
            if let Node::Element(el) = n {
                out.push(el.tag.clone());
                collect_tags(&el.children, out);
            }
        }
    }

    fn find_el<'a>(nodes: &'a [Node], tag: &str) -> Option<&'a Element> {
        for n in nodes {
            if let Node::Element(el) = n {
                if el.tag == tag {
                    return Some(el);
                }
                if let Some(f) = find_el(&el.children, tag) {
                    return Some(f);
                }
            }
        }
        None
    }

    #[test]
    fn parse_all_standard_element_tags() {
        let src = r#"<at id="@u:s" name="Bob" role="mod" type="all"/>
<sharp id="!c:s" name="general"/>
<emoji id="👍" name="thumbsup"/>
<a href="https://example.com">link</a>
<img src="mxc://s/i" width="100" height="50" title="pic"/>
<audio src="mxc://s/a" duration="12" poster="mxc://s/cover"/>
<video src="mxc://s/v" width="640" height="480" duration="30" poster="mxc://s/cover"/>
<file src="mxc://s/f" title="doc.pdf" poster="mxc://s/thumb"/>
<b>b</b><strong>bs</strong><i>i</i><em>im</em><u>u</u><ins>ins</ins>
<s>s</s><del>del</del><spl>spl</spl><code>code</code><sup>sup</sup><sub>sub</sub>
<br/><p>para</p>
<message id="$m1"><author id="@a:s" name="Alice" avatar="mxc://s/av"/>hello</message>
<quote id="$q1">quoted</quote>
<button id="btn1" type="action" href="https://x" text="fill" theme="primary">press</button>"#;
        let nodes = parse(src);
        let mut tags = Vec::new();
        collect_tags(&nodes, &mut tags);
        for t in [
            "at", "sharp", "emoji", "a", "img", "audio", "video", "file", "b", "strong", "i", "em",
            "u", "ins", "s", "del", "spl", "code", "sup", "sub", "br", "p", "message", "author",
            "quote", "button",
        ] {
            assert!(tags.iter().any(|x| x == t), "missing parsed tag: {}", t);
        }
    }

    #[test]
    fn parse_attributes_of_every_element() {
        let nodes = parse(
            r#"<at id="@u:s" name="Bob" role="mod" type="all"/>
<button id="btn1" type="action" href="https://x" text="fill" theme="primary">press</button>
<video src="mxc://s/v" width="640" height="480" duration="30" poster="mxc://s/cover"/>"#,
        );
        let at = find_el(&nodes, "at").unwrap();
        assert_eq!(at.attrs["id"], "@u:s");
        assert_eq!(at.attrs["name"], "Bob");
        assert_eq!(at.attrs["role"], "mod");
        assert_eq!(at.attrs["type"], "all");
        let btn = find_el(&nodes, "button").unwrap();
        assert_eq!(btn.attrs["id"], "btn1");
        assert_eq!(btn.attrs["type"], "action");
        assert_eq!(btn.attrs["href"], "https://x");
        assert_eq!(btn.attrs["text"], "fill");
        assert_eq!(btn.attrs["theme"], "primary");
        let vid = find_el(&nodes, "video").unwrap();
        assert_eq!(vid.attrs["width"], "640");
        assert_eq!(vid.attrs["height"], "480");
        assert_eq!(vid.attrs["duration"], "30");
        assert_eq!(vid.attrs["poster"], "mxc://s/cover");
    }

    #[test]
    fn roundtrip_all_standard_elements() {
        let src = r#"<at id="@u:s"/><sharp id="!c:s"/><emoji name="🎉"/><a href="https://e.com">x</a><img src="mxc://s/i" width="10"/><audio src="mxc://s/a" duration="3"/><video src="mxc://s/v"/><file src="mxc://s/f"/><b>b</b><i>i</i><u>u</u><s>d</s><spl>p</spl><code>c</code><sup>s</sup><sub>b</sub><br/><p>para</p><quote id="$q">q</quote><button id="btn">press</button>"#;
        let nodes = parse(src);
        let out = to_string(&nodes);
        assert_eq!(parse(&out), nodes, "roundtrip changed tree: {}", out);
    }

    #[test]
    fn element_at_sharp_emoji_link() {
        let v = satori_to_matrix(
            r#"hi <at id="@u:s" name="Bob"/> <sharp id="!c:s" name="general"/> <emoji name="🎉"/> <a href="https://example.com">site</a>"#,
            HS,
        );
        assert_eq!(v["msgtype"], "m.text");
        let body = v["body"].as_str().unwrap();
        for s in ["Bob", "general", "🎉", "site"] {
            assert!(body.contains(s), "body missing {}: {}", s, body);
        }
        let html = v["formatted_body"].as_str().unwrap();
        assert!(
            html.contains(r#"<a href="https://matrix.to/#/@u:s">Bob</a>"#),
            "at link: {}",
            html
        );
        assert!(
            html.contains(r#"<a href="https://matrix.to/#/!c:s">general</a>"#),
            "sharp link: {}",
            html
        );
        assert!(
            html.contains(r#"<a href="https://example.com">site</a>"#),
            "a href lost: {}",
            html
        );
    }

    #[test]
    fn element_resources_carry_info() {
        let img = satori_to_matrix(
            r#"<img src="mxc://s/i" width="100" height="50" title="pic"/>"#,
            HS,
        );
        assert_eq!(img["msgtype"], "m.image");
        assert_eq!(img["url"], "mxc://s/i");
        assert_eq!(img["info"]["w"], 100);
        assert_eq!(img["info"]["h"], 50);

        let aud = satori_to_matrix(r#"<audio src="mxc://s/a" duration="12"/>"#, HS);
        assert_eq!(aud["msgtype"], "m.audio");
        assert_eq!(aud["url"], "mxc://s/a");
        assert_eq!(aud["info"]["duration"], 12);

        let vid = satori_to_matrix(
            r#"<video src="mxc://s/v" width="640" height="480" duration="30"/>"#,
            HS,
        );
        assert_eq!(vid["msgtype"], "m.video");
        assert_eq!(vid["url"], "mxc://s/v");
        assert_eq!(vid["info"]["w"], 640);
        assert_eq!(vid["info"]["h"], 480);
        assert_eq!(vid["info"]["duration"], 30);

        let file = satori_to_matrix(r#"<file src="mxc://s/f" title="doc.pdf"/>"#, HS);
        assert_eq!(file["msgtype"], "m.file");
        assert_eq!(file["url"], "mxc://s/f");
        assert_eq!(file["body"], "doc.pdf");
    }

    #[test]
    fn element_decorations_preserve_html() {
        let cases = [
            (r#"<b>bold</b>"#, "<b>bold</b>"),
            (r#"<strong>bold</strong>"#, "<b>bold</b>"),
            (r#"<i>it</i>"#, "<i>it</i>"),
            (r#"<em>it</em>"#, "<i>it</i>"),
            (r#"<u>ul</u>"#, "<u>ul</u>"),
            (r#"<ins>ul</ins>"#, "<u>ul</u>"),
            (r#"<s>del</s>"#, "<del>del</del>"),
            (r#"<del>del</del>"#, "<del>del</del>"),
            (r#"<code>code</code>"#, "<code>code</code>"),
            (r#"<sup>sup</sup>"#, "<sup>sup</sup>"),
            (r#"<sub>sub</sub>"#, "<sub>sub</sub>"),
            (
                r#"<spl>secret</spl>"#,
                r#"<span data-mx-spoiler>secret</span>"#,
            ),
        ];
        for (src, expected_html) in cases {
            let v = satori_to_matrix(src, HS);
            assert_eq!(
                v["formatted_body"].as_str().unwrap(),
                expected_html,
                "src={}",
                src
            );
        }
        // 嵌套修饰：标签必须成对保留
        let nested = satori_to_matrix("<b>bold <i>and italic</i></b>", HS);
        assert_eq!(
            nested["formatted_body"].as_str().unwrap(),
            "<b>bold <i>and italic</i></b>"
        );
    }

    #[test]
    fn element_layout_and_meta() {
        let v = satori_to_matrix("a<br/>b<p>c</p>", HS);
        let body = v["body"].as_str().unwrap();
        let html = v["formatted_body"].as_str().unwrap();
        assert!(body.contains('\n'), "br -> newline: {}", body);
        assert!(html.contains("<br/>"), "br -> html: {}", html);

        let q = satori_to_matrix(r#"<quote id="$e1">quoted text</quote>"#, HS);
        assert_eq!(q["m.relates_to"]["m.in_reply_to"]["event_id"], "$e1");
        assert!(q["body"].as_str().unwrap().contains("quoted text"));

        // message/author/button 在 Matrix 无直接对应，须优雅降级为文本内容
        let m = satori_to_matrix(
            r#"<message id="$m"><author id="@a:s" name="Alice"/>hello</message>"#,
            HS,
        );
        assert!(m["body"].as_str().unwrap().contains("hello"));
        let b = satori_to_matrix(r#"<button id="btn" type="action">press</button>"#, HS);
        assert!(b["body"].as_str().unwrap().contains("press"));
    }

    #[test]
    fn matrix_html_to_all_satori_decorations() {
        let doc = |formatted: &str| {
            serde_json::json!({
                "msgtype": "m.text", "body": "x",
                "format": "org.matrix.custom.html", "formatted_body": formatted
            })
        };
        let cases = [
            ("<b>bold</b>", "<b>bold</b>"),
            ("<strong>bold</strong>", "<strong>bold</strong>"),
            ("<i>it</i>", "<i>it</i>"),
            ("<u>ul</u>", "<u>ul</u>"),
            ("<del>del</del>", "<del>del</del>"),
            ("<code>code</code>", "<code>code</code>"),
            (
                "<a href=\"https://example.com\">site</a>",
                "<a href=\"https://example.com\">site</a>",
            ),
        ];
        for (html, expected) in cases {
            assert_eq!(
                matrix_to_satori(&doc(html), HS, "@bot:s"),
                expected,
                "html={}",
                html
            );
        }
        // Matrix 剧透 -> Satori spl
        let spoiler = matrix_to_satori(&doc("<span data-mx-spoiler>secret</span>"), HS, "@bot:s");
        assert!(
            spoiler.contains("secret") && spoiler.contains("spl"),
            "spoiler: {}",
            spoiler
        );
    }

    #[test]
    fn matrix_native_msgs_to_elements() {
        let emote = serde_json::json!({"msgtype": "m.emote", "body": "waves"});
        assert_eq!(matrix_to_satori(&emote, HS, "@bot:s"), "* waves");
        let notice = serde_json::json!({"msgtype": "m.notice", "body": "heads up"});
        assert_eq!(matrix_to_satori(&notice, HS, "@bot:s"), "heads up");
        let video =
            serde_json::json!({"msgtype": "m.video", "body": "clip.mp4", "url": "mxc://s/v"});
        let s = matrix_to_satori(&video, HS, "@bot:s");
        assert!(s.starts_with("<video src="), "{}", s);
        assert!(s.contains("clip.mp4"), "{}", s);
        let file = serde_json::json!({"msgtype": "m.file", "body": "doc.pdf", "url": "mxc://s/f"});
        let s = matrix_to_satori(&file, HS, "@bot:s");
        assert!(s.starts_with("<file src="), "{}", s);
    }

    #[test]
    fn collect_remote_media_urls_dedups_and_skips_mxc() {
        let src = r#"<img src="https://pighub.top/images/万猪拱塔.jpg"/><b>x<img src="https://e.com/b.png"/></b><img src="https://pighub.top/images/万猪拱塔.jpg"/><img src="mxc://s/already"/>"#;
        let urls = collect_remote_media_urls(src);
        assert_eq!(urls.len(), 2, "{:?}", urls);
        assert_eq!(urls[0], "https://pighub.top/images/万猪拱塔.jpg");
        assert_eq!(urls[1], "https://e.com/b.png");
    }

    #[test]
    fn remote_media_resolved_becomes_real_image() {
        let src = r#"<img src="https://pighub.top/images/万猪拱塔.jpg" title="pig"/>"#;
        // Without the upload map it keeps the media type with the raw URL.
        let plain = satori_to_matrix(src, HS);
        assert_eq!(plain["msgtype"], "m.image");
        assert_eq!(plain["url"], "https://pighub.top/images/万猪拱塔.jpg");

        // With the map it becomes a real m.image pointing at the uploaded mxc.
        let mut resolved = HashMap::new();
        resolved.insert(
            "https://pighub.top/images/万猪拱塔.jpg".to_string(),
            "mxc://s/uploaded123".to_string(),
        );
        let v = satori_to_matrix_with_media(src, HS, &resolved);
        assert_eq!(v["msgtype"], "m.image");
        assert_eq!(v["url"], "mxc://s/uploaded123");
        assert_eq!(v["body"], "pig");
    }

    #[test]
    fn remote_media_failed_upload_keeps_media_semantics() {
        // A CDN that rejects our download (e.g. anti-hotlink 403) must not
        // degrade the media message into a plain text link. Keep the msgtype
        // and the original URL so the client can still show a playable element.
        let src = r#"<audio src="https://m701.music.126.net/song.mp3?vuutv=x"/>"#;
        let v = satori_to_matrix_with_media(src, HS, &HashMap::new());
        assert_eq!(v["msgtype"], "m.audio");
        assert_eq!(v["url"], "https://m701.music.126.net/song.mp3?vuutv=x");
        // No bare <a href> link should be emitted in place of the media.
        assert!(!v["formatted_body"]
            .as_str()
            .unwrap_or("")
            .contains("<a href="));
    }

    #[test]
    fn mixed_content_splits_into_separate_messages() {
        let src = "先看这个 <img src=\"mxc://s/a\"/> 还有 <audio src=\"mxc://s/b\"/> 结束";
        let segs = satori_to_matrix_segments(src, HS, &HashMap::new());
        // text + image + text + audio + text => contiguous text merged:
        // text("先看这个 "), image, text(" 还有 "), audio, text(" 结束")
        assert_eq!(segs.len(), 5, "{:?}", segs);
        assert_eq!(segs[0]["msgtype"], "m.text");
        assert!(segs[0]["body"].as_str().unwrap().contains("先看这个"));
        assert_eq!(segs[1]["msgtype"], "m.image");
        assert_eq!(segs[1]["url"], "mxc://s/a");
        assert_eq!(segs[2]["msgtype"], "m.text");
        assert_eq!(segs[3]["msgtype"], "m.audio");
        assert_eq!(segs[3]["url"], "mxc://s/b");
        assert_eq!(segs[4]["msgtype"], "m.text");
        assert!(segs[4]["body"].as_str().unwrap().contains("结束"));
    }

    #[test]
    fn leading_quote_applies_reply_to_every_segment() {
        let src = "<quote id=\"$orig\">原文</quote>文字 <img src=\"mxc://s/a\"/>";
        let segs = satori_to_matrix_segments(src, HS, &HashMap::new());
        assert!(segs.len() >= 2);
        for seg in &segs {
            assert_eq!(
                seg["m.relates_to"]["m.in_reply_to"]["event_id"], "$orig",
                "every segment must keep the reply relation: {:?}",
                seg
            );
        }
    }

    #[test]
    fn unhandled_elements_are_not_dropped() {
        // Matrix has no <button>/<message> equivalent; they must degrade into
        // the text segment instead of vanishing.
        let src = "<message id=\"$m\"><author id=\"@a:s\" name=\"A\"/>hi</message><button id=\"b\">go</button>";
        let segs = satori_to_matrix_segments(src, HS, &HashMap::new());
        assert_eq!(segs.len(), 1);
        let body = segs[0]["body"].as_str().unwrap();
        assert!(body.contains("hi"), "{}", body);
        assert!(body.contains("go"), "{}", body);
    }
}
