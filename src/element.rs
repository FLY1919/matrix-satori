//! Conversion between Satori element strings and Matrix message content.
//!
//! Satori elements are an XML-like mini language:
//!   `hello <at id="@u:s"/> world <img src="mxc://s/abc"/>`
//! Matrix messages carry plain `body` plus optional `formatted_body` (HTML).

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

/// Minimal, forgiving parser for Satori element strings.
/// Unknown or malformed constructs degrade to literal text.
pub fn parse(input: &str) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut text = String::new();
    let bytes: Vec<char> = input.chars().collect();
    let mut i = 0;

    let flush = |text: &mut String, nodes: &mut Vec<Node>| {
        if !text.is_empty() {
            nodes.push(Node::Text(std::mem::take(text)));
        }
    };

    while i < bytes.len() {
        if bytes[i] == '<' {
            // find closing '>', respecting quotes
            let mut j = i + 1;
            let mut quote: Option<char> = None;
            while j < bytes.len() {
                let c = bytes[j];
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
            if j >= bytes.len() {
                text.push('<');
                i += 1;
                continue;
            }
            let inner: String = bytes[i + 1..j].iter().collect();
            let inner = inner.trim();

            if let Some(close_tag) = inner.strip_prefix('/') {
                // a closing tag; if it matches the last open element we just ignore here
                // (the recursive parser consumes its own closing tags).
                let _ = close_tag;
                flush(&mut text, &mut nodes);
                i = j + 1;
                continue;
            }

            if inner.is_empty() || inner.starts_with('!') || inner.starts_with('?') {
                text.extend(bytes[i..=j].iter());
                i = j + 1;
                continue;
            }

            let self_closing = inner.ends_with('/');
            let content = if self_closing {
                inner[..inner.len() - 1].trim_end()
            } else {
                inner
            };

            let (tag, attrs) = parse_tag(content);
            if tag.is_empty() {
                text.extend(bytes[i..=j].iter());
                i = j + 1;
                continue;
            }

            flush(&mut text, &mut nodes);
            let mut el = Element::new(tag);
            el.attrs = attrs;
            el.self_closing = self_closing;

            if self_closing {
                nodes.push(Node::Element(el));
                i = j + 1;
            } else {
                // find matching closing tag, nesting-aware
                let (children, next) = parse_children(&bytes, j + 1, &el.tag);
                el.children = children;
                nodes.push(Node::Element(el));
                i = next;
            }
        } else {
            text.push(bytes[i]);
            i += 1;
        }
    }
    flush(&mut text, &mut nodes);
    nodes
}

fn parse_children(bytes: &[char], start: usize, tag: &str) -> (Vec<Node>, usize) {
    let mut nodes = Vec::new();
    let mut text = String::new();
    let mut i = start;
    let close_literal: Vec<char> = format!("</{}", tag).chars().collect();

    while i < bytes.len() {
        if bytes[i] == '<' && starts_with(bytes, i, &close_literal) {
            // skip until '>'
            let mut j = i;
            while j < bytes.len() && bytes[j] != '>' {
                j += 1;
            }
            if !text.is_empty() {
                nodes.push(Node::Text(std::mem::take(&mut text)));
            }
            return (nodes, (j + 1).min(bytes.len()));
        }
        if bytes[i] == '<' {
            let mut j = i + 1;
            let mut quote: Option<char> = None;
            while j < bytes.len() {
                let c = bytes[j];
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
            if j >= bytes.len() {
                text.push('<');
                i += 1;
                continue;
            }
            let inner: String = bytes[i + 1..j].iter().collect();
            let inner = inner.trim();
            let self_closing = inner.ends_with('/');
            let content = if self_closing {
                inner[..inner.len() - 1].trim_end()
            } else {
                inner
            };
            let (child_tag, attrs) = parse_tag(content);
            if !child_tag.is_empty() {
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                let mut el = Element::new(child_tag);
                el.attrs = attrs;
                el.self_closing = self_closing;
                if self_closing {
                    nodes.push(Node::Element(el));
                    i = j + 1;
                } else {
                    let (kids, next) = parse_children(bytes, j + 1, &el.tag);
                    el.children = kids;
                    nodes.push(Node::Element(el));
                    i = next;
                }
            } else {
                text.extend(bytes[i..=j].iter());
                i = j + 1;
            }
        } else {
            text.push(bytes[i]);
            i += 1;
        }
    }
    if !text.is_empty() {
        nodes.push(Node::Text(text));
    }
    (nodes, i)
}

fn starts_with(bytes: &[char], at: usize, pat: &[char]) -> bool {
    if at + pat.len() > bytes.len() {
        return false;
    }
    bytes[at..at + pat.len()] == *pat
}

fn parse_tag(content: &str) -> (String, HashMap<String, String>) {
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    let tag: String = chars[..i].iter().collect();
    let mut attrs = HashMap::new();
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            if !key.is_empty() {
                attrs.insert(key, String::new());
            }
            continue;
        }
        i += 1; // skip '='
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
            let q = chars[i];
            i += 1;
            while i < chars.len() && chars[i] != q {
                value.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                value.push(chars[i]);
                i += 1;
            }
        }
        if !key.is_empty() {
            attrs.insert(key, html_unescape(&value));
        }
    }
    (tag, attrs)
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
fn html_to_satori(html: &str, homeserver: &str, self_id: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let mut out = String::new();
    let mut i = 0;
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
                out.push('<');
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

            let emit_open = |out: &mut String, tag: &str, self_close: bool| match tag {
                "at" | "sharp" => {
                    let id = attrs.get("id").cloned().unwrap_or_default();
                    let name = attrs.get("name").cloned().unwrap_or_else(|| id.clone());
                    if self_close {
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
                    out.push_str(&format!("<{} src=\"{}\"/>", tag, html_escape_attr(&src)));
                }
                _ => {
                    if self_close {
                        out.push_str("<br/>");
                    } else {
                        out.push('<');
                        out.push_str(tag);
                        out.push('>');
                    }
                }
            };

            if tag == "a" && !closing {
                let href = attrs.get("href").cloned().unwrap_or_default();
                if let Some(mxid) = href.strip_prefix("https://matrix.to/#/") {
                    out.push_str(&format!("<at id=\"{}\"/>", html_escape_attr(mxid)));
                } else {
                    out.push_str("<a href=\"");
                    out.push_str(&html_escape_attr(&href));
                    out.push_str("\">");
                }
            } else if tag == "a" && closing {
                out.push_str("</a>");
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
                            emit_open(&mut out, tag, self_closing);
                        }
                    }
                }
            }
            i = j + 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
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
        assert!(s.contains("<at id=\"@u:s\"/>"), "got: {}", s);
    }

    #[test]
    fn matrix_image() {
        let v = serde_json::json!({"msgtype": "m.image", "body": "pic.png", "url": "mxc://s/abc"});
        let s = matrix_to_satori(&v, "https://m.example.com", "@bot:s");
        assert_eq!(s, "<img src=\"https://m.example.com/_matrix/media/v3/download/s/abc\" title=\"pic.png\"/>");
    }
}
