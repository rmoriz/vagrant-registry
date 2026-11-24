//! Runtime-independent path grammar, dispatch, and reply construction.

use crate::catalogs::{CATALOGS, INDEX};
use crate::validate::valid_box_name;

const NOT_FOUND: &str = "not found";
const METHOD_NOT_ALLOWED: &str = "method not allowed";

static CATALOG_HEADERS: [(&str, &str); 3] = [
    ("content-type", "application/json"),
    ("x-content-type-options", "nosniff"),
    ("cache-control", "public, max-age=60"),
];
static HTML_HEADERS: [(&str, &str); 3] = [
    ("content-type", "text/html; charset=utf-8"),
    ("x-content-type-options", "nosniff"),
    ("cache-control", "no-store"),
];
static TEXT_HEADERS: [(&str, &str); 3] = [
    ("content-type", "text/plain; charset=utf-8"),
    ("x-content-type-options", "nosniff"),
    ("cache-control", "no-store"),
];
static EMPTY_HEADERS: [(&str, &str); 2] = [
    ("x-content-type-options", "nosniff"),
    ("cache-control", "no-store"),
];
static METHOD_NOT_ALLOWED_HEADERS: [(&str, &str); 4] = [
    ("content-type", "text/plain; charset=utf-8"),
    ("x-content-type-options", "nosniff"),
    ("cache-control", "no-store"),
    ("allow", "GET, HEAD"),
];

#[derive(Debug, PartialEq, Eq)]
pub enum Body {
    Empty,
    Json(&'static [u8]),
    Html(String),
    Text(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub headers: &'static [(&'static str, &'static str)],
    pub body: Body,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Index,
    Health,
    Catalog(String),
    Missing,
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes `s` exactly once, or returns `None` for a truncated or
/// non-hex escape, or bytes that are not valid UTF-8.
pub fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_digit(*bytes.get(index + 1)?)?;
            let low = hex_digit(*bytes.get(index + 2)?)?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded).ok()
}

/// Drops a `?query` and then a `#fragment`, leaving percent-encoding untouched.
pub fn request_path(uri: &str) -> &str {
    let without_query = uri.split_once('?').map_or(uri, |(path, _)| path);
    without_query
        .split_once('#')
        .map_or(without_query, |(path, _)| path)
}

pub fn parse_target(raw_path: &str) -> Target {
    let path = raw_path.strip_prefix('/').unwrap_or(raw_path);
    let mut segments = Vec::new();

    if !path.is_empty() {
        for raw in path.split('/') {
            let Some(decoded) = percent_decode(raw) else {
                return Target::Missing;
            };
            segments.extend(decoded.split('/').map(str::to_string));
        }
    }

    if segments.iter().any(String::is_empty) {
        return Target::Missing;
    }

    match segments.as_slice() {
        [] => Target::Index,
        [health] if health == "healthz" => Target::Health,
        [api, v2, vagrant, org, name] if api == "api" && v2 == "v2" && vagrant == "vagrant" => {
            catalog(org, name)
        }
        [prefix, org, file] if prefix == "catalogs" => match file.strip_suffix(".json") {
            Some(name) => catalog(org, name),
            None => Target::Missing,
        },
        [org, name] => catalog(org, name),
        _ => Target::Missing,
    }
}

fn catalog(org: &str, name: &str) -> Target {
    let joined = format!("{org}/{name}");
    if valid_box_name(&joined) {
        Target::Catalog(joined)
    } else {
        Target::Missing
    }
}

pub fn lookup(name: &str) -> Option<&'static [u8]> {
    CATALOGS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, bytes)| *bytes)
}

fn headers_for(body: &Body) -> &'static [(&'static str, &'static str)] {
    match body {
        Body::Json(_) => &CATALOG_HEADERS,
        Body::Html(_) => &HTML_HEADERS,
        Body::Text(_) => &TEXT_HEADERS,
        Body::Empty => &EMPTY_HEADERS,
    }
}

fn reply(status: u16, body: Body) -> Reply {
    Reply {
        status,
        headers: headers_for(&body),
        body,
    }
}

fn index_document() -> String {
    let mut entries: Vec<&(&str, &str, usize)> = INDEX.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));

    let mut document = String::from(
        "<!doctype html><meta charset=\"utf-8\"><title>Vagrant boxes</title><h1>Vagrant boxes</h1><ul>",
    );

    for (name, description, versions) in entries {
        document.push_str("<li><a href=\"/");
        document.push_str(&html_escape(name));
        document.push_str("\">");
        document.push_str(&html_escape(name));
        document.push_str("</a> — ");
        document.push_str(&html_escape(description));
        document.push_str(&format!(" ({versions} versions)</li>"));
    }

    document.push_str("</ul>\n");
    document
}

pub fn handle(method: &str, raw_path: &str) -> Reply {
    let target = parse_target(request_path(raw_path));

    let resolved = match target {
        Target::Index => Some(reply(200, Body::Html(index_document()))),
        Target::Health => Some(reply(200, Body::Text(format!("ok {}\n", CATALOGS.len())))),
        Target::Catalog(name) => lookup(&name).map(|bytes| reply(200, Body::Json(bytes))),
        Target::Missing => None,
    };

    let mut reply = match resolved {
        None => reply(404, Body::Text(NOT_FOUND.to_string())),
        Some(reply) if method == "GET" || method == "HEAD" => reply,
        Some(_) => Reply {
            status: 405,
            headers: &METHOD_NOT_ALLOWED_HEADERS,
            body: Body::Text(METHOD_NOT_ALLOWED.to_string()),
        },
    };

    if method == "HEAD" {
        reply.body = Body::Empty;
    }

    reply
}

pub fn html_escape(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());

    for character in s.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }

    escaped
}

#[cfg(test)]
mod tests {
    use super::{
        Body, Reply, Target, handle, html_escape, lookup, parse_target, percent_decode,
        request_path,
    };
    use crate::catalogs::{CATALOGS, INDEX};

    const CATALOG_PATHS: [&str; 3] = [
        "/acme/base",
        "/api/v2/vagrant/acme/base",
        "/catalogs/acme/base.json",
    ];

    fn header<'a>(reply: &'a Reply, name: &str) -> Option<&'a str> {
        reply
            .headers
            .iter()
            .find(|(header_name, _)| *header_name == name)
            .map(|(_, value)| *value)
    }

    fn html(reply: Reply) -> String {
        match reply.body {
            Body::Html(html) => html,
            other => panic!("expected HTML body, got {other:?}"),
        }
    }

    #[test]
    fn statuses_follow_the_grammar_and_method_gate() {
        let cases = [
            ("GET", "/", 200),
            ("GET", "/healthz", 200),
            ("GET", "/acme/base", 200),
            ("GET", "/api/v2/vagrant/acme/base", 200),
            ("GET", "/catalogs/acme/base.json", 200),
            ("GET", "/acme/a..b", 200),
            ("GET", "/acme%2Fbase", 200),
            ("GET", "/acme/base?x=1#fragment", 200),
            ("GET", "/acme/..", 404),
            ("GET", "/acme/.", 404),
            ("GET", "/api/v2/vagrant/acme/%2e%2e", 404),
            ("GET", "/acme//base", 404),
            ("GET", "/acme/base/", 404),
            ("GET", "//", 404),
            ("GET", "/catalogs/acme/base", 404),
            ("GET", "/catalogs/acme/base.json.json", 404),
            ("GET", "/acme/nope", 404),
            ("POST", "/acme/nope", 404),
            ("POST", "/acme/base", 405),
            ("DELETE", "/healthz", 405),
            ("PUT", "/", 405),
            ("HEAD", "/", 200),
            ("HEAD", "/healthz", 200),
            ("HEAD", "/acme/base", 200),
            ("HEAD", "/api/v2/vagrant/acme/base", 200),
            ("HEAD", "/catalogs/acme/base.json", 200),
            ("HEAD", "/acme/nope", 404),
            ("HEAD", "/acme/base/", 404),
        ];

        for (method, path, status) in cases {
            assert_eq!(handle(method, path).status, status, "{method} {path}");
        }
    }

    #[test]
    fn catalog_forms_share_one_static_body() {
        let expected = lookup("acme/base").expect("acme/base is seeded");

        for path in CATALOG_PATHS {
            let reply = handle("GET", path);

            match reply.body {
                Body::Json(bytes) => {
                    assert_eq!(bytes.as_ptr(), expected.as_ptr(), "{path} pointer");
                    assert_eq!(bytes, expected, "{path} bytes");
                }
                other => panic!("{path} should serve JSON, got {other:?}"),
            }

            assert_eq!(
                header(&reply, "content-type"),
                Some("application/json"),
                "{path}"
            );
        }
    }

    #[test]
    fn lookup_only_knows_seeded_names() {
        assert!(lookup("acme/base").is_some());
        assert!(lookup("acme/a..b").is_some());
        assert!(lookup("acme/nope").is_none());
    }

    #[test]
    fn parse_target_decides_shape_only() {
        assert_eq!(parse_target("/"), Target::Index);
        assert_eq!(parse_target("/healthz"), Target::Health);
        assert_eq!(
            parse_target("/acme/base"),
            Target::Catalog("acme/base".to_string())
        );
        assert_eq!(
            parse_target("/acme/nope"),
            Target::Catalog("acme/nope".to_string())
        );
        assert_eq!(
            parse_target("/acme%2Fbase"),
            Target::Catalog("acme/base".to_string())
        );
        assert_eq!(
            parse_target("/api/v2/vagrant/acme/base"),
            Target::Catalog("acme/base".to_string())
        );
        assert_eq!(
            parse_target("/catalogs/acme/base.json"),
            Target::Catalog("acme/base".to_string())
        );
        assert_eq!(
            parse_target("/catalogs/acme/base.json.json"),
            Target::Catalog("acme/base.json".to_string())
        );

        for path in [
            "/acme/..",
            "/acme/.",
            "/api/v2/vagrant/acme/%2e%2e",
            "/acme//base",
            "/acme/base/",
            "//",
            "/catalogs/acme/base",
            "/catalogs/acme/base.json/",
        ] {
            assert_eq!(parse_target(path), Target::Missing, "{path}");
        }
    }

    #[test]
    fn head_keeps_status_and_headers_but_empties_the_body() {
        for path in CATALOG_PATHS.into_iter().chain([
            "/",
            "/healthz",
            "/acme/a..b",
            "/acme/nope",
            "/acme/base/",
        ]) {
            let get_reply = handle("GET", path);
            let head_reply = handle("HEAD", path);

            assert_eq!(head_reply.status, get_reply.status, "{path}");
            assert_eq!(head_reply.headers, get_reply.headers, "{path}");
            assert_eq!(head_reply.body, Body::Empty, "{path}");
        }

        let head_404 = handle("HEAD", "/acme/nope");
        assert_eq!(
            header(&head_404, "content-type"),
            Some("text/plain; charset=utf-8")
        );
    }

    #[test]
    fn every_reply_is_nosniff_and_cacheable_only_for_catalogs() {
        for path in CATALOG_PATHS.into_iter().chain(["/acme/a..b"]) {
            for method in ["GET", "HEAD"] {
                let reply = handle(method, path);
                assert_eq!(
                    header(&reply, "x-content-type-options"),
                    Some("nosniff"),
                    "{method} {path}"
                );
                assert_eq!(
                    header(&reply, "cache-control"),
                    Some("public, max-age=60"),
                    "{method} {path}"
                );
            }
        }

        for path in ["/", "/healthz", "/acme/nope", "/acme/base/"] {
            for method in ["GET", "HEAD"] {
                let reply = handle(method, path);
                assert_eq!(
                    header(&reply, "x-content-type-options"),
                    Some("nosniff"),
                    "{method} {path}"
                );
                assert_eq!(
                    header(&reply, "cache-control"),
                    Some("no-store"),
                    "{method} {path}"
                );
            }
        }

        let not_found = handle("GET", "/acme/nope");
        assert_eq!(
            header(&not_found, "content-type"),
            Some("text/plain; charset=utf-8")
        );

        let wrong_method = handle("POST", "/acme/base");
        assert_eq!(
            header(&wrong_method, "x-content-type-options"),
            Some("nosniff")
        );
        assert_eq!(header(&wrong_method, "cache-control"), Some("no-store"));
        assert_eq!(
            header(&wrong_method, "content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(header(&wrong_method, "allow"), Some("GET, HEAD"));
    }

    #[test]
    fn content_types_match_the_body_kind() {
        assert_eq!(
            header(&handle("GET", "/"), "content-type"),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            header(&handle("GET", "/healthz"), "content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            header(&handle("GET", "/acme/a..b"), "content-type"),
            Some("application/json")
        );
        assert_eq!(
            header(&handle("GET", "/acme/nope"), "content-type"),
            Some("text/plain; charset=utf-8")
        );

        let empty = super::headers_for(&Body::Empty);
        assert!(empty.iter().all(|(name, _)| *name != "content-type"));
    }

    #[test]
    fn not_found_body_is_fixed_text_without_a_path_echo() {
        let reply = handle("GET", "/acme/nope");
        assert_eq!(reply.body, Body::Text("not found".to_string()));

        let body = match reply.body {
            Body::Text(text) => text,
            other => panic!("expected text, got {other:?}"),
        };
        assert!(!body.contains("nope"));

        assert_eq!(
            handle("POST", "/acme/base").body,
            Body::Text("method not allowed".to_string())
        );
        assert_eq!(
            handle("DELETE", "/healthz").body,
            Body::Text("method not allowed".to_string())
        );
    }

    #[test]
    fn healthz_reports_the_catalog_count() {
        assert_eq!(
            handle("GET", "/healthz").body,
            Body::Text(format!("ok {}\n", CATALOGS.len()))
        );
    }

    #[test]
    fn index_lists_every_catalog_with_escaped_description_and_count() {
        let body = html(handle("GET", "/"));

        assert!(body.starts_with(
            "<!doctype html><meta charset=\"utf-8\"><title>Vagrant boxes</title><h1>Vagrant boxes</h1><ul>"
        ));
        assert!(body.ends_with("</ul>\n"));

        for (name, description, count) in INDEX {
            assert!(body.contains(name), "{name} missing");
            assert!(
                body.contains(&html_escape(description)),
                "{description} missing"
            );
            assert!(
                body.contains(&format!("({count} versions)")),
                "{count} missing"
            );
        }
    }

    #[test]
    fn html_escape_covers_all_five_characters() {
        assert_eq!(
            html_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&#39;f"
        );
        assert_eq!(html_escape("plain"), "plain");
    }

    #[test]
    fn percent_decode_accepts_escapes_and_rejects_bad_input() {
        assert_eq!(percent_decode("acme%2Fbase"), Some("acme/base".to_string()));
        assert_eq!(percent_decode("a%2e%2eb"), Some("a..b".to_string()));
        assert_eq!(percent_decode("a+b"), Some("a+b".to_string()));
        assert_eq!(percent_decode("plain"), Some("plain".to_string()));
        assert_eq!(percent_decode("%2"), None);
        assert_eq!(percent_decode("acme%"), None);
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%ff"), None);
    }

    #[test]
    fn request_path_drops_query_and_fragment_only() {
        assert_eq!(request_path("/acme/base?x=1#frag"), "/acme/base");
        assert_eq!(request_path("/acme/base#frag?x=1"), "/acme/base");
        assert_eq!(request_path("/acme%2Fbase?x=1"), "/acme%2Fbase");
        assert_eq!(request_path("/acme/base"), "/acme/base");
    }
}
