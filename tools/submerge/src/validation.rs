use crate::md::SectionTracker;
use anyhow::{Context, Result, bail};
use html5ever::TokenizerResult;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, Tag as HtmlTag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer,
};
use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Event, Parser as MarkdownParser, Tag, TagEnd, html};
use regex::Regex;
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use url::{Url, form_urlencoded};

const MAX_COMMUNITY_ITEM_TEXT_CHARS: usize = 150;
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub links: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Inspection {
    #[default]
    All,
    Links,
    Markdown,
}

impl Inspection {
    fn includes_links(self) -> bool {
        matches!(self, Self::All | Self::Links)
    }

    fn includes_markdown(self) -> bool {
        matches!(self, Self::All | Self::Markdown)
    }
}

impl Report {
    fn append(&mut self, mut other: Self) {
        self.links.append(&mut other.links);
        self.errors.append(&mut other.errors);
        self.warnings.append(&mut other.warnings);
    }
}

#[derive(Default)]
struct CommunityItemState {
    visible_text: String,
    outside_link_text: String,
    links: usize,
}

impl CommunityItemState {
    fn finish(self, report: &mut Report) {
        if self.links == 0 {
            return;
        }
        if has_descriptive_text(&self.outside_link_text) {
            report.warnings.push(
                "community list item has text outside its link; include the description inside the link title"
                    .to_string(),
            );
        }
        if self.visible_text.chars().count() > MAX_COMMUNITY_ITEM_TEXT_CHARS {
            report.warnings.push(
                "community list item is unusually long; use a concise link title".to_string(),
            );
        }
    }
}

fn has_descriptive_text(text: &str) -> bool {
    let mut text = text.trim();
    while let Some(after_open) = text.strip_prefix('[')
        && let Some((_label, remainder)) = after_open.split_once(']')
    {
        text = remainder.trim_start();
    }
    text.chars().any(char::is_alphanumeric)
}
pub fn inspect_links(text: &str) -> Report {
    let mut report = Report::default();
    let mut sections = SectionTracker::default();
    let mut active_link: Option<(String, String)> = None;
    let mut community_items: Vec<Option<CommunityItemState>> = Vec::new();
    let mut in_code_block = false;

    for event in MarkdownParser::new(text) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => sections.start_heading(level),
            Event::End(TagEnd::Heading(_)) => {
                sections.end_heading();
            }
            Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
            Event::End(TagEnd::CodeBlock) => in_code_block = false,
            Event::Start(Tag::Item) => {
                community_items.push(sections.in_community().then(CommunityItemState::default));
            }
            Event::End(TagEnd::Item) => {
                if let Some(Some(item)) = community_items.pop() {
                    item.finish(&mut report);
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) if sections.in_community() => {
                if let Some(Some(item)) = community_items.last_mut() {
                    item.links += 1;
                } else if !sections.in_heading() {
                    report.warnings.push(format!(
                        "community link is not a Markdown list item: {dest_url}"
                    ));
                }
                active_link = Some((dest_url.to_string(), String::new()));
            }
            Event::End(TagEnd::Link) => {
                if let Some((url, title)) = active_link.take() {
                    if title == url || url.strip_prefix("mailto:") == Some(title.as_str()) {
                        report
                            .errors
                            .push("expected a descriptive title for link".to_string());
                    }
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
                if let Some(Some(item)) = community_items.last_mut() {
                    item.visible_text.push_str(&value);
                    if active_link.is_none() {
                        item.outside_link_text.push_str(&value);
                    }
                }
                if sections.in_heading() {
                    sections.push_text(&value);
                } else if let Some((_url, title)) = active_link.as_mut() {
                    title.push_str(&value);
                } else if sections.in_community() && !in_code_block {
                    for link in LinkFinder::new()
                        .links(&value)
                        .filter(|link| link.kind() == &LinkKind::Url)
                    {
                        let bare_url = link.as_str();
                        if Url::parse(bare_url)
                            .is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
                        {
                            report
                                .errors
                                .push("expected a descriptive title for link".to_string());
                        }
                    }
                }
            }
            Event::Code(value) => {
                if let Some(Some(item)) = community_items.last_mut() {
                    item.visible_text.push_str(&value);
                    if active_link.is_none() {
                        item.outside_link_text.push_str(&value);
                    }
                }
                if sections.in_heading() {
                    sections.push_text(&value);
                } else if let Some((_url, title)) = active_link.as_mut() {
                    title.push_str(&value);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                sections.push_break();
                if let Some(Some(item)) = community_items.last_mut() {
                    item.visible_text.push(' ');
                    if active_link.is_none() {
                        item.outside_link_text.push(' ');
                    }
                }
            }
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
            Event::Text(_) | Event::Code(_) => {
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

#[derive(Deserialize)]
struct ContractMetadata {
    files: Vec<String>,
    links: Option<Vec<String>>,
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    warnings: Vec<String>,
}

pub fn verify_contract(directory: &Path) -> Result<()> {
    let paths = fs::read_dir(directory)
        .with_context(|| format!("read validation cases from {}", directory.display()))?
        .map(|entry| {
            let entry = entry?;
            Ok(entry.file_type()?.is_file().then(|| entry.path()))
        })
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("read validation case in {}", directory.display()))?;
    let mut paths = paths.into_iter().flatten().collect::<Vec<_>>();
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "md"));
    paths.sort();

    for path in paths {
        let text =
            fs::read_to_string(&path).with_context(|| format!("read case {}", path.display()))?;
        let mut documents = text.split("\n---\n");
        let metadata = serde_json::from_str::<ContractMetadata>(
            documents
                .next()
                .ok_or_else(|| anyhow::anyhow!("{} is empty", path.display()))?,
        )
        .with_context(|| format!("parse case metadata from {}", path.display()))?;
        let documents = documents.collect::<Vec<_>>();
        if metadata.files.len() != documents.len() {
            bail!(
                "{}: expected {} virtual files, found {}",
                path.display(),
                metadata.files.len(),
                documents.len()
            );
        }
        let files = metadata
            .files
            .iter()
            .cloned()
            .zip(documents.into_iter().map(str::to_string))
            .collect::<Vec<_>>();
        let report = inspect_named_texts(&files);
        if let Some(expected) = metadata.links
            && report.links != expected
        {
            bail!(
                "{}: links differ\nexpected: {:?}\nactual: {:?}",
                path.display(),
                expected,
                report.links
            );
        }
        if report.errors != metadata.errors {
            bail!(
                "{}: errors differ\nexpected: {:?}\nactual: {:?}",
                path.display(),
                metadata.errors,
                report.errors
            );
        }
        if report.warnings != metadata.warnings {
            bail!(
                "{}: warnings differ\nexpected: {:?}\nactual: {:?}",
                path.display(),
                metadata.warnings,
                report.warnings
            );
        }
    }
    Ok(())
}

pub fn inspect_files(files: &[PathBuf]) -> Result<Report> {
    inspect_files_with(files, Inspection::All)
}

pub fn inspect_files_with(files: &[PathBuf], inspection: Inspection) -> Result<Report> {
    let named_texts = files
        .iter()
        .map(|path| {
            let name = path.display().to_string();
            let text = fs::read_to_string(path).with_context(|| format!("read {name}"))?;
            Ok((name, text))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(inspect_named_texts_with(&named_texts, inspection))
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
                if !entry.file_type().ok()?.is_file() {
                    return None;
                }
                let name = entry.file_name();
                FILENAME_RE
                    .is_match(&name.to_string_lossy())
                    .then(|| (name, entry.path()))
            })
            .collect::<Vec<_>>();
        if files.is_empty() {
            bail!("no matching files found in {directory}");
        }
        listing.append(&mut files);
    }
    listing.sort();
    let keep_from = listing.len().saturating_sub(count);
    Ok(listing
        .split_off(keep_from)
        .into_iter()
        .map(|(_name, path)| path)
        .collect())
}

fn inspect_named_texts(files: &[(String, String)]) -> Report {
    inspect_named_texts_with(files, Inspection::All)
}

pub(crate) fn inspect_named_texts_with(
    files: &[(String, String)],
    inspection: Inspection,
) -> Report {
    let mut report = Report::default();
    let mut seen_links = BTreeMap::new();
    for (filename, text) in files {
        if inspection.includes_links() {
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
        }
        if inspection.includes_markdown() {
            report.append(inspect_markdown(text, filename));
        }
    }
    report
}

fn inspect_html_tags(text: &str, filename: &str, report: &mut Report) {
    let mut open_tags: Vec<(String, usize)> = Vec::new();
    let comment_ranges = html_comment_ranges(text);
    for (event, range) in MarkdownParser::new(text).into_offset_iter() {
        let (Event::Html(value) | Event::InlineHtml(value)) = event else {
            continue;
        };
        if comment_ranges
            .iter()
            .any(|(start, end)| (*start..*end).contains(&range.start))
        {
            continue;
        }
        for tag in html_tags(&value) {
            let name = tag.name.to_string();
            if VALID_TAGS.contains(&name.as_str()) {
                continue;
            }
            match tag.kind {
                TagKind::StartTag => {
                    if tag.self_closing {
                        report_unknown_html_tag(
                            text,
                            filename,
                            &name,
                            range.start,
                            range.end,
                            report,
                        );
                    } else {
                        open_tags.push((name, range.start));
                    }
                }
                TagKind::EndTag => {
                    if let Some(index) = open_tags
                        .iter()
                        .rposition(|(open_tag, _)| open_tag == &name)
                    {
                        let (_tag, start) = open_tags.remove(index);
                        report_unknown_html_tag(text, filename, &name, start, range.end, report);
                    }
                }
            }
        }
    }
    for (tag, start) in open_tags {
        report_unknown_html_tag(text, filename, &tag, start, start + tag.len() + 2, report);
    }
}

fn html_comment_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    while let Some(relative_start) = text[offset..].find("<!--") {
        let start = offset + relative_start;
        let after_start = start + "<!--".len();
        let end = text[after_start..]
            .find("-->")
            .map_or(text.len(), |relative_end| {
                after_start + relative_end + "-->".len()
            });
        ranges.push((start, end));
        offset = end;
    }
    ranges
}

#[derive(Default)]
struct HtmlTagSink {
    tags: RefCell<Vec<HtmlTag>>,
}

impl TokenSink for HtmlTagSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<Self::Handle> {
        if let Token::TagToken(tag) = token {
            self.tags.borrow_mut().push(tag);
        }
        TokenSinkResult::Continue
    }
}

fn html_tags(html: &str) -> Vec<HtmlTag> {
    let input = BufferQueue::default();
    input.push_back(StrTendril::from(html));
    let tokenizer = Tokenizer::new(HtmlTagSink::default(), Default::default());
    while tokenizer.feed(&input) != TokenizerResult::Done {}
    tokenizer.end();
    tokenizer.sink.tags.into_inner()
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

    #[derive(Deserialize)]
    struct RecentFilesCase {
        directories: Vec<RecentFilesDirectory>,
        count: usize,
        expected: Vec<String>,
    }

    #[derive(Deserialize)]
    struct RecentFilesDirectory {
        name: String,
        files: Vec<String>,
    }

    #[test]
    fn shared_validation_contract() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../validation_tests/cases");
        verify_contract(&directory).unwrap();
    }

    #[test]
    fn rust_only_validation_contract() {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../validation_tests/rust_cases");
        verify_contract(&directory).unwrap();
    }

    #[test]
    fn inspection_can_run_link_and_markdown_checks_independently() {
        let files = [(
            "case.md".to_string(),
            "## Updates from Rust Community\n\nhttps://example.com/bare\n\nodd `backtick\n"
                .to_string(),
        )];

        let links = inspect_named_texts_with(&files, Inspection::Links);
        assert_eq!(
            links.errors,
            ["expected a descriptive title for link".to_string()]
        );

        let markdown = inspect_named_texts_with(&files, Inspection::Markdown);
        assert_eq!(
            markdown.errors,
            ["case.md: found odd backticks: \"odd `backtick\"".to_string()]
        );

        let all = inspect_named_texts_with(&files, Inspection::All);
        assert_eq!(
            all.errors,
            [
                "expected a descriptive title for link".to_string(),
                "case.md: found odd backticks: \"odd `backtick\"".to_string(),
            ]
        );
    }

    #[test]
    fn recent_files_are_ordered_by_filename_across_directories() {
        let case_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../validation_tests/recent_files.json");
        let case: RecentFilesCase =
            serde_json::from_str(&fs::read_to_string(case_path).unwrap()).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut directories = Vec::new();
        for directory_case in case.directories {
            let directory = root.path().join(directory_case.name);
            fs::create_dir(&directory).unwrap();
            for filename in directory_case.files {
                fs::write(directory.join(filename), "").unwrap();
            }
            directories.push(directory);
        }
        let paths = recent_files(
            &directories
                .iter()
                .map(|directory| directory.display().to_string())
                .collect::<Vec<_>>()
                .join(":"),
            case.count,
        )
        .unwrap();
        let relative_paths = paths
            .iter()
            .map(|path| {
                path.strip_prefix(root.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        assert_eq!(relative_paths, case.expected);
    }

    #[test]
    fn ten_most_recent_published_issues_have_no_errors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let paths = recent_files(root.join("content").to_str().unwrap(), 10).unwrap();
        assert_eq!(paths.len(), 10);
        let report = inspect_files(&paths).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }
}
