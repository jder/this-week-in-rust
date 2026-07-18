use anyhow::{Context, Result, bail};
use pulldown_cmark::{Event, HeadingLevel, Parser as MarkdownParser, Tag, TagEnd, html};
use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use url::{Url, form_urlencoded};

const STRICT_TITLE: &str = "updates from rust community";
const VALID_TAGS: &[&str] = &[
    "p",
    "a",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "strong",
    "hr",
    "li",
    "ul",
    "ol",
    "em",
    "code",
    "blockquote",
    "small",
    "br",
];

static FILENAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d\d\d\d-\d\d-\d\d-this-week-in-rust\.md$").unwrap());
static GITHUB_REPO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https://github\.com/[^/]+/[^/]+/?[^/]*$").unwrap());
static PARENTHESIZED_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\((https?://[^()\s]+)\)").unwrap());
static HTML_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<(?P<tag>[a-z][a-z0-9-]*)(?:\s[^>]*)?(?P<close>/?)>").unwrap()
});
static HTML_CLOSE_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)</(?P<tag>[a-z][a-z0-9-]*)\s*>").unwrap());

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub links: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Report {
    fn append(&mut self, mut other: Self) {
        self.links.append(&mut other.links);
        self.errors.append(&mut other.errors);
        self.warnings.append(&mut other.warnings);
    }
}

#[derive(Default)]
struct HeadingState {
    strict: bool,
    strict_level: Option<HeadingLevel>,
    heading: Option<(HeadingLevel, String)>,
}

impl HeadingState {
    fn start(&mut self, level: HeadingLevel) {
        self.heading = Some((level, String::new()));
    }

    fn push_text(&mut self, text: &str) {
        if let Some((_level, title)) = self.heading.as_mut() {
            title.push_str(text);
        }
    }

    fn push_break(&mut self) {
        if let Some((_level, title)) = self.heading.as_mut() {
            title.push(' ');
        }
    }

    fn end(&mut self) {
        let Some((level, title)) = self.heading.take() else {
            return;
        };
        if !matches!(
            level,
            HeadingLevel::H1 | HeadingLevel::H2 | HeadingLevel::H3 | HeadingLevel::H4
        ) {
            return;
        }
        if self
            .strict_level
            .is_some_and(|strict_level| level > strict_level)
        {
            return;
        }
        self.strict = title.trim().eq_ignore_ascii_case(STRICT_TITLE);
        self.strict_level = self.strict.then_some(level);
    }

    fn in_heading(&self) -> bool {
        self.heading.is_some()
    }
}

pub fn inspect_links(text: &str) -> Report {
    let mut report = Report::default();
    let mut headings = HeadingState::default();
    let mut active_link: Option<(String, String)> = None;

    for event in MarkdownParser::new(text) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => headings.start(level),
            Event::End(TagEnd::Heading(_)) => headings.end(),
            Event::Start(Tag::Link { dest_url, .. }) if headings.strict => {
                active_link = Some((dest_url.to_string(), String::new()));
            }
            Event::End(TagEnd::Link) => {
                if let Some((url, title)) = active_link.take() {
                    if title.ends_with("...") && title.len() == 70 {
                        report.warnings.push(format!(
                            "this link title may be unintentionally truncated: '{}'",
                            title
                        ));
                    }
                    let normalized_url = normalize_url(&url, &mut report);
                    report.links.push(normalized_url);
                }
            }
            Event::Text(value) => {
                if headings.in_heading() {
                    headings.push_text(&value);
                } else if let Some((_url, title)) = active_link.as_mut() {
                    title.push_str(&value);
                } else if headings.strict {
                    for captures in PARENTHESIZED_URL_RE.captures_iter(&value) {
                        report
                            .errors
                            .push(format!("bare URL is not a markdown link: {}", &captures[1]));
                    }
                }
            }
            Event::Code(value) => {
                if headings.in_heading() {
                    headings.push_text(&value);
                } else if let Some((_url, title)) = active_link.as_mut() {
                    title.push_str(&value);
                }
            }
            Event::SoftBreak | Event::HardBreak => headings.push_break(),
            _ => {}
        }
    }
    report
}

pub fn inspect_markdown(text: &str, filename: &str) -> Report {
    let mut report = Report::default();

    for line in text.lines() {
        let line = line.replace("````", "").replace("```", "");
        if line.contains("``") {
            report
                .errors
                .push(format!("{filename}: found empty backticks: \"{line}\""));
        }
        if line.chars().filter(|character| *character == '`').count() % 2 != 0 {
            report
                .errors
                .push(format!("{filename}: found odd backticks: \"{line}\""));
        }
    }

    let mut rendered_html = String::new();
    html::push_html(&mut rendered_html, MarkdownParser::new(text));
    let mut item_stack = Vec::new();
    let mut list_stack = Vec::new();
    for (event, range) in MarkdownParser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::List(_)) => list_stack.push(ListState {
                item_depth: item_stack.len(),
                last_item_start: None,
                split_reported: false,
            }),
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                if let Some(list) = list_stack.last_mut()
                    && item_stack.len() == list.item_depth
                    && !list.split_reported
                    && let Some(previous_start) = list.last_item_start
                    && text[previous_start..range.start].contains("\n\n")
                {
                    report
                        .errors
                        .push(format!("{filename}: blank line splits a Markdown list"));
                    list.split_reported = true;
                }
                if let Some(list) = list_stack.last_mut()
                    && item_stack.len() == list.item_depth
                {
                    list.last_item_start = Some(range.start);
                }
                item_stack.push(false);
            }
            Event::End(TagEnd::Item) => {
                if let Some(has_content) = item_stack.pop()
                    && !has_content
                {
                    report.errors.push(format!(
                        "{filename}: empty <li> tag after {}",
                        rendered_list_html(&rendered_html)
                    ));
                }
            }
            Event::Text(_) | Event::Code(_) | Event::Start(Tag::Link { .. }) => {
                if let Some(has_content) = item_stack.last_mut() {
                    *has_content = true;
                }
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                if let Some(has_content) = item_stack.last_mut() {
                    *has_content = true;
                }
            }
            _ => {}
        }
    }
    inspect_html_tags(text, filename, &mut report);
    report
}

struct ListState {
    item_depth: usize,
    last_item_start: Option<usize>,
    split_reported: bool,
}

pub fn inspect_files(files: &[PathBuf]) -> Result<Report> {
    let named_texts = files
        .iter()
        .map(|path| {
            let name = path.display().to_string();
            let text = fs::read_to_string(path).with_context(|| format!("read {name}"))?;
            Ok((name, text))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(inspect_named_texts(&named_texts))
}

pub fn inspect_text(filename: &str, text: &str) -> Report {
    inspect_named_texts(&[(filename.to_string(), text.to_string())])
}

pub fn recent_files(paths: &str, count: usize) -> Result<Vec<PathBuf>> {
    let mut listing = Vec::new();
    for directory in paths.split(':') {
        let mut files = fs::read_dir(directory)
            .with_context(|| format!("read {directory}"))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name();
                FILENAME_RE
                    .is_match(&name.to_string_lossy())
                    .then(|| Path::new(directory).join(name))
            })
            .collect::<Vec<_>>();
        if files.is_empty() {
            bail!("no matching files found in {directory}");
        }
        listing.append(&mut files);
    }
    listing.sort();
    let keep_from = listing.len().saturating_sub(count);
    Ok(listing.split_off(keep_from))
}

fn inspect_named_texts(files: &[(String, String)]) -> Report {
    let mut report = Report::default();
    let mut seen_links = BTreeMap::new();
    for (filename, text) in files {
        let links = inspect_links(text);
        for link in &links.links {
            if let Some(previous) = seen_links.get(link) {
                if previous == filename {
                    report.errors.push(format!(
                        "possible duplicate link {link} found twice in {filename}"
                    ));
                } else {
                    report.errors.push(format!(
                        "possible duplicate link {link} in file {filename} (also found in {previous})"
                    ));
                }
            } else {
                seen_links.insert(link.clone(), filename.clone());
            }
        }
        report.append(links);
        report.append(inspect_markdown(text, filename));
    }
    report
}

fn inspect_html_tags(text: &str, filename: &str, report: &mut Report) {
    let mut open_tags: Vec<(String, usize)> = Vec::new();
    for (event, range) in MarkdownParser::new(text).into_offset_iter() {
        let (Event::Html(value) | Event::InlineHtml(value)) = event else {
            continue;
        };
        for captures in HTML_TAG_RE.captures_iter(&value) {
            let tag = captures["tag"].to_ascii_lowercase();
            if VALID_TAGS.contains(&tag.as_str()) {
                continue;
            }
            let start = range.start + captures.get(0).unwrap().start();
            if &captures["close"] == "/" {
                report_unknown_html_tag(text, filename, &tag, start, range.end, report);
            } else {
                open_tags.push((tag, start));
            }
        }
        for captures in HTML_CLOSE_TAG_RE.captures_iter(&value) {
            let tag = captures["tag"].to_ascii_lowercase();
            if let Some(index) = open_tags.iter().rposition(|(open_tag, _)| open_tag == &tag) {
                let (_tag, start) = open_tags.remove(index);
                report_unknown_html_tag(text, filename, &tag, start, range.end, report);
            }
        }
    }
    for (tag, start) in open_tags {
        report_unknown_html_tag(text, filename, &tag, start, start + tag.len() + 2, report);
    }
}

fn report_unknown_html_tag(
    text: &str,
    filename: &str,
    tag: &str,
    start: usize,
    end: usize,
    report: &mut Report,
) {
    let tag_str = text[start..end].trim().chars().take(50).collect::<String>();
    report.errors.push(format!(
        "{filename}: unrecognized tag \"{tag}\" in `{tag_str}`"
    ));
}

fn rendered_list_html(html: &str) -> String {
    let Some(start) = html.find("<ul>") else {
        return "<ul>".to_string();
    };
    let Some(end) = html[start..].find("</ul>") else {
        return "<ul>".to_string();
    };
    html[start..start + end + "</ul>".len()]
        .trim_end()
        .to_string()
}

fn normalize_url(link: &str, report: &mut Report) -> String {
    let mut parsed = match Url::parse(link) {
        Ok(parsed) => parsed,
        Err(_) => {
            report
                .errors
                .push(format!("unexpected/malformed link scheme: {link}"));
            let simplified = collapse_slashes(link);
            if simplified != link {
                report
                    .errors
                    .push(format!("link can be simplified: {link} -> {simplified}"));
            }
            return simplified;
        }
    };
    if !matches!(parsed.scheme(), "mailto" | "http" | "https") {
        report
            .errors
            .push(format!("unexpected/malformed link scheme: {link}"));
    }

    let query = scrub_tracking_parameters(link, &parsed, report);
    let trailing_slash = parsed.path().ends_with('/');
    let path_components = parsed
        .path()
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let mut path = if parsed.path().starts_with('/') {
        format!("/{path_components}")
    } else {
        path_components
    };
    if trailing_slash && !path.ends_with('/') {
        path.push('/');
    }
    parsed.set_path(&path);
    parsed.set_query(query.as_deref());
    let reconstituted = parsed.to_string();
    if reconstituted != link {
        report
            .errors
            .push(format!("link can be simplified: {link} -> {reconstituted}"));
    }

    path = path.trim_end_matches('/').to_string();
    parsed.set_path(&path);
    if parsed.scheme() == "http" {
        let _ = parsed.set_scheme("https");
    }
    check_suspicious(parsed.host_str().unwrap_or(""), link, report);
    let normalized = parsed.to_string();
    if path.is_empty() && parsed.host_str().is_some() {
        normalized.trim_end_matches('/').to_string()
    } else {
        normalized
    }
}

fn collapse_slashes(value: &str) -> String {
    value
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn scrub_tracking_parameters(link: &str, parsed: &Url, report: &mut Report) -> Option<String> {
    let query = parsed.query()?;
    let mut parameters: Vec<(String, Vec<String>)> = Vec::new();
    let mut tracking = Vec::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        if matches!(
            key.as_ref(),
            "utm_source" | "utm_campaign" | "utm_medium" | "utm_content"
        ) {
            if !tracking.iter().any(|existing| existing == &key) {
                tracking.push(key.into_owned());
            }
            continue;
        }
        if let Some((_existing_key, values)) = parameters
            .iter_mut()
            .find(|(existing_key, _)| existing_key == &key)
        {
            values.push(value.into_owned());
        } else {
            parameters.push((key.into_owned(), vec![value.into_owned()]));
        }
    }
    if !tracking.is_empty() {
        let tracking = tracking
            .iter()
            .map(|parameter| format!("'{parameter}'"))
            .collect::<Vec<_>>()
            .join(", ");
        report
            .errors
            .push(format!("found tracking parameters on {link}: [{tracking}]"));
    }
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (key, values) in parameters {
        for value in values {
            serializer.append_pair(&key, &value);
        }
    }
    Some(serializer.finish())
}

fn check_suspicious(host: &str, link: &str, report: &mut Report) {
    if GITHUB_REPO_RE.is_match(link) {
        report.warnings.push(format!(
            "link {link} is directly to a GitHub repo; please double check our guidelines here: https://github.com/rust-lang/this-week-in-rust#projectstooling-updates"
        ));
    } else if host.eq_ignore_ascii_case("crates.io") {
        report.warnings.push(format!(
            "link {link} is to crates.io -- we do not usually include links directly to crates on crates.io; please double check our guidelines here: https://github.com/rust-lang/this-week-in-rust#projectstooling-updates"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn cases() -> Value {
        serde_json::from_str(include_str!("../../validation_tests/cases.json")).unwrap()
    }

    fn expected_strings(case: &Value, key: &str) -> Vec<String> {
        case[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn link_contract() {
        for case in cases()["link_cases"].as_array().unwrap() {
            let report = inspect_links(case["markdown"].as_str().unwrap());
            if case.get("links").is_some() {
                assert_eq!(report.links, expected_strings(case, "links"), "{case}");
            }
            assert_eq!(report.errors, expected_strings(case, "errors"), "{case}");
            assert_eq!(
                report.warnings,
                expected_strings(case, "warnings"),
                "{case}"
            );
        }
    }

    #[test]
    fn markdown_contract() {
        for case in cases()["markdown_cases"].as_array().unwrap() {
            let report = inspect_markdown(case["markdown"].as_str().unwrap(), "case.md");
            assert_eq!(report.errors, expected_strings(case, "errors"), "{case}");
            assert_eq!(
                report.warnings,
                expected_strings(case, "warnings"),
                "{case}"
            );
        }
    }

    #[test]
    fn duplicate_contract() {
        for case in cases()["duplicate_cases"].as_array().unwrap() {
            let files = case["files"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, text)| (name.clone(), text.as_str().unwrap().to_string()))
                .collect::<Vec<_>>();
            let report = inspect_named_texts(&files);
            assert_eq!(report.errors, expected_strings(case, "errors"), "{case}");
            assert_eq!(
                report.warnings,
                expected_strings(case, "warnings"),
                "{case}"
            );
        }
    }

    #[test]
    fn recent_published_corpus_contract() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for case in cases()["corpus_cases"].as_array().unwrap() {
            let paths = case["paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|path| root.join(path.as_str().unwrap()))
                .collect::<Vec<_>>();
            let report = inspect_files(&paths).unwrap();
            assert_eq!(report.errors, expected_strings(case, "errors"), "{case}");
            assert_eq!(
                report.warnings,
                expected_strings(case, "warnings"),
                "{case}"
            );
        }
    }
}
