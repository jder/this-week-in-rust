use crate::validation::{self, Inspection, Report};
use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use gix::{ObjectId, Repository};
use log::info;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

const MAX_COMMENT_CHARS: usize = 60_000;

#[derive(Debug, Parser)]
pub struct Args {}

#[derive(Debug, Default, PartialEq, Eq)]
struct Diagnostics {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Diagnostics {
    fn is_empty(&self) -> bool {
        self.errors.is_empty() && self.warnings.is_empty()
    }

    fn from_report(report: Report) -> Self {
        Self {
            errors: report.errors,
            warnings: report.warnings,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CiReport {
    links: Diagnostics,
    markdown: Diagnostics,
}

struct CommentContext {
    owner: String,
    repository: String,
    pull_request: u64,
    head_sha: String,
    run_url: String,
}

impl CommentContext {
    fn from_env() -> Result<Option<Self>> {
        let Some(pull_request) = optional_env("SUBMERGE_CI_PR_NUMBER") else {
            return Ok(None);
        };
        let pull_request = pull_request
            .parse::<u64>()
            .context("parse SUBMERGE_CI_PR_NUMBER")?;
        if pull_request == 0 {
            bail!("SUBMERGE_CI_PR_NUMBER must be positive");
        }
        let head_sha = required_env("SUBMERGE_CI_HEAD_SHA")?;
        validate_sha(&head_sha)?;
        let repository_name = required_env("GITHUB_REPOSITORY")?;
        let (owner, repository) = parse_repository(&repository_name)?;
        Ok(Some(Self {
            owner: owner.to_string(),
            repository: repository.to_string(),
            pull_request,
            head_sha,
            run_url: required_env("SUBMERGE_CI_RUN_URL")?,
        }))
    }
}

pub async fn run(_args: Args) -> Result<()> {
    let report = run_checks()?;
    if let Some(context) = CommentContext::from_env()? {
        publish_comment(&report, &context).await?;
    }
    fail_for_report_errors(&report)
}

fn run_checks() -> Result<CiReport> {
    let repo = gix::discover(".").context("open git repository")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("submerge ci must run in a non-bare repository"))?;
    let shared_cases =
        checked_untrusted_directory(workdir, Path::new("tools/validation_tests/cases"))?;
    let rust_cases =
        checked_untrusted_directory(workdir, Path::new("tools/validation_tests/rust_cases"))?;
    let content = checked_untrusted_directory(workdir, Path::new("content"))?;
    let draft = checked_untrusted_directory(workdir, Path::new("draft"))?;
    validation::verify_contract(&shared_cases)?;
    validation::verify_contract(&rust_cases)?;
    let paths = format!("{}:{}", content.display(), draft.display());

    let link_files = validation::recent_files(&paths, 25)?;
    let current_links = validation::inspect_files_with(&link_files, Inspection::Links)?;
    let links = match baseline_commit(&repo)? {
        Some(base) => {
            let baseline =
                inspect_files_at_commit(&repo, workdir, base, &link_files, Inspection::Links)?;
            new_diagnostics(current_links, &baseline)
        }
        None => current_links,
    };

    let markdown_files = validation::recent_files(&paths, 5)?;
    let markdown = validation::inspect_files_with(&markdown_files, Inspection::Markdown)?;

    log_diagnostics("link", &links);
    log_diagnostics("markdown", &markdown);
    Ok(CiReport {
        links: Diagnostics::from_report(links),
        markdown: Diagnostics::from_report(markdown),
    })
}

fn checked_untrusted_directory(workdir: &Path, relative: &Path) -> Result<PathBuf> {
    let mut path = workdir.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!(
                "CI data directory must be a relative path without traversal: {}",
                relative.display()
            );
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect CI data path {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("CI data path must not contain symlinks: {}", path.display());
        }
    }
    if !path.is_dir() {
        bail!("CI data path is not a directory: {}", path.display());
    }
    Ok(path)
}

fn baseline_commit(repo: &Repository) -> Result<Option<ObjectId>> {
    let base = optional_env("SUBMERGE_CI_BASE_SHA");
    let head = optional_env("SUBMERGE_CI_HEAD_SHA");
    match (base, head) {
        (None, None) => Ok(None),
        (Some(base), Some(head)) => {
            let base = base.parse::<ObjectId>().context("parse CI base SHA")?;
            let head = head.parse::<ObjectId>().context("parse CI head SHA")?;
            Ok(Some(
                repo.merge_base(base, head)
                    .context("find CI merge base")?
                    .detach(),
            ))
        }
        _ => bail!("SUBMERGE_CI_BASE_SHA and SUBMERGE_CI_HEAD_SHA must be set together"),
    }
}

fn inspect_files_at_commit(
    repo: &Repository,
    workdir: &Path,
    commit: ObjectId,
    files: &[PathBuf],
    inspection: Inspection,
) -> Result<Report> {
    let tree = repo
        .find_commit(commit)
        .with_context(|| format!("find baseline commit {commit}"))?
        .tree()
        .with_context(|| format!("read tree for baseline commit {commit}"))?;
    let mut named_texts = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(workdir)
            .with_context(|| format!("{} is outside the git worktree", path.display()))?;
        let Some(entry) = tree
            .lookup_entry_by_path(relative)
            .with_context(|| format!("find {} in baseline tree", relative.display()))?
        else {
            continue;
        };
        let mut blob = entry
            .object()
            .with_context(|| format!("read {} from baseline tree", relative.display()))?
            .try_into_blob()
            .map_err(|_| anyhow!("{} is not a file in the baseline tree", relative.display()))?;
        let text = String::from_utf8(blob.take_data()).with_context(|| {
            format!("decode {} from baseline tree as UTF-8", relative.display())
        })?;
        named_texts.push((path.display().to_string(), text));
    }
    Ok(validation::inspect_named_texts_with(
        &named_texts,
        inspection,
    ))
}

fn new_diagnostics(mut current: Report, baseline: &Report) -> Report {
    current
        .errors
        .retain(|message| !baseline.errors.contains(message));
    current
        .warnings
        .retain(|message| !baseline.warnings.contains(message));
    current
}

fn log_diagnostics(section: &str, report: &Report) {
    for message in &report.errors {
        log_message(section, "error", message);
    }
    for message in &report.warnings {
        log_message(section, "warning", message);
    }
}

fn log_message(section: &str, level: &str, message: &str) {
    for line in message.lines() {
        println!("{section} {level}: {}", sanitize_log_line(line));
    }
}

fn sanitize_log_line(line: &str) -> String {
    let mut sanitized = String::new();
    for character in line.chars() {
        if character.is_control() {
            sanitized.extend(character.escape_default());
        } else {
            sanitized.push(character);
        }
    }
    sanitized
}

async fn publish_comment(report: &CiReport, context: &CommentContext) -> Result<()> {
    let Some(body) = build_comment(report, &context.run_url) else {
        info!("CI found no diagnostics; skipping comment");
        return Ok(());
    };
    let client = super::github_client()?;
    let pull = client
        .pulls(&context.owner, &context.repository)
        .get(context.pull_request)
        .await
        .with_context(|| format!("read pull request #{}", context.pull_request))?;
    let current_head = pull
        .head
        .ok_or_else(|| anyhow!("pull request response is missing its head"))?
        .sha;
    if current_head != context.head_sha {
        info!("pull request head has changed; skipping stale comment");
        return Ok(());
    }
    client
        .issues(&context.owner, &context.repository)
        .create_comment(context.pull_request, body)
        .await
        .with_context(|| format!("comment on pull request #{}", context.pull_request))?;
    Ok(())
}

fn fail_for_report_errors(report: &CiReport) -> Result<()> {
    let error_count = report.links.errors.len() + report.markdown.errors.len();
    if error_count == 0 {
        Ok(())
    } else {
        bail!("CI validation found {error_count} error(s)")
    }
}

fn validate_sha(value: &str) -> Result<()> {
    if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        bail!("invalid git SHA in CI metadata")
    }
}

fn build_comment(report: &CiReport, run_url: &str) -> Option<String> {
    let mut sections = Vec::new();
    if !report.links.is_empty() {
        sections.push(render_section("Link check", &report.links));
    }
    if !report.markdown.is_empty() {
        sections.push(render_section("Markdown check", &report.markdown));
    }
    if sections.is_empty() {
        return None;
    }

    let details = format!("\n\nYou can see more details here: {run_url}");
    let body = format!(
        "Thank you for your contribution to This Week in Rust! \
Our automated checks found some possible issues with these changes:\n\n{}",
        sections.join("\n\n")
    );
    let available = MAX_COMMENT_CHARS.saturating_sub(details.chars().count());
    let body = body.chars().take(available).collect::<String>();
    Some(body + &details)
}

fn render_section(title: &str, diagnostics: &Diagnostics) -> String {
    let mut output = format!("### {title}\n\n");
    for (level, messages) in [
        ("error", &diagnostics.errors),
        ("warning", &diagnostics.warnings),
    ] {
        for message in messages {
            for line in message.lines() {
                output.push_str("    ");
                output.push_str(level);
                output.push_str(": ");
                output.push_str(line);
                output.push('\n');
            }
        }
    }
    output.pop();
    output
}

fn parse_repository(value: &str) -> Result<(&str, &str)> {
    let (owner, repository) = value
        .split_once('/')
        .ok_or_else(|| anyhow!("GITHUB_REPOSITORY must be in owner/name form"))?;
    if owner.is_empty() || repository.is_empty() || repository.contains('/') {
        bail!("GITHUB_REPOSITORY must be in owner/name form");
    }
    Ok((owner, repository))
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn required_env(name: &str) -> Result<String> {
    optional_env(name).ok_or_else(|| anyhow!("{name} is not set"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_diagnostics_are_filtered() {
        let current = Report {
            errors: vec!["old error".into(), "new error".into()],
            warnings: vec!["old warning".into(), "new warning".into()],
            ..Report::default()
        };
        let baseline = Report {
            errors: vec!["old error".into()],
            warnings: vec!["old warning".into()],
            ..Report::default()
        };
        let report = new_diagnostics(current, &baseline);
        assert_eq!(report.errors, ["new error"]);
        assert_eq!(report.warnings, ["new warning"]);
    }

    #[test]
    fn comment_renders_diagnostics_as_markdown_code() {
        let report = CiReport {
            links: Diagnostics {
                errors: vec!["first line\n# injected heading\n::error::command".into()],
                warnings: Vec::new(),
            },
            markdown: Diagnostics::default(),
        };
        let comment = build_comment(&report, "https://github.com/example/run").unwrap();
        assert!(comment.contains("    error: # injected heading"));
        assert!(comment.contains("    error: ::error::command"));
        assert!(!comment.contains("\n# injected heading"));
        assert!(!comment.contains("\n::error::command"));
    }

    #[test]
    fn empty_report_does_not_create_a_comment() {
        let report = CiReport {
            links: Diagnostics::default(),
            markdown: Diagnostics::default(),
        };
        assert!(build_comment(&report, "https://github.com/example/run").is_none());
    }

    #[test]
    fn git_metadata_is_validated() {
        validate_sha("0123456789abcdef0123456789abcdef01234567").unwrap();
        assert!(validate_sha("not-a-sha").is_err());
    }

    #[test]
    fn comment_result_preserves_check_failure() {
        let mut report = CiReport {
            links: Diagnostics::default(),
            markdown: Diagnostics::default(),
        };
        fail_for_report_errors(&report).unwrap();
        report.markdown.errors.push("broken Markdown".into());
        assert!(fail_for_report_errors(&report).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn untrusted_directories_must_not_escape_through_symlinks() {
        use std::os::unix::fs::symlink;

        let workdir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(workdir.path().join("tools")).unwrap();
        symlink(
            outside.path(),
            workdir.path().join("tools/validation_tests"),
        )
        .unwrap();
        assert!(
            checked_untrusted_directory(workdir.path(), Path::new("tools/validation_tests/cases"))
                .is_err()
        );
    }
}
