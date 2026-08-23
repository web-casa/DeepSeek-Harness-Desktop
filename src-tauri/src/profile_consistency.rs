//! Read-only Web-profile consistency reporting plus deliberately narrow,
//! explicitly confirmed cleanup of stale profile patch entries.
//!
//! A Web profile combines a user-owned `package.json` with an upstream-owned
//! `cordis.patch.yml`.  A failed install or manual deletion can leave a
//! dependency declaration and a loader patch behind after the package itself
//! has disappeared.  Reporting that drift is safe; automatically rewriting
//! YAML is not.  The only supported cleanup path therefore removes a whole
//! top-level `- insert:` block when all of the following are true:
//!
//! * the named package is a valid direct profile dependency whose materialized
//!   package entry is absent;
//! * the block has exactly one simple `name: <package>` value;
//! * it contains no comments, anchors/tags, flow syntax, or block scalars;
//! * a user has reviewed the preview and explicitly confirmed it; and
//! * the patch bytes have not changed between preview and confirmation.
//!
//! This is intentionally incomplete.  Any YAML we cannot prove belongs to
//! the missing direct dependency remains untouched for the user to inspect
//! and edit in Harness.

use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_ISSUES: usize = 128;
const MAX_CLEANUP_BLOCKS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConsistencyIssue {
    /// Stable controller-facing issue code.  It deliberately does not expose
    /// filesystem paths or untrusted YAML text.
    pub kind: String,
    pub package_name: String,
    pub active: bool,
    pub cleanup_available: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConsistencyReport {
    pub issues: Vec<ProfileConsistencyIssue>,
    pub cleanup_eligible_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCleanupPreview {
    pub transaction_id: String,
    pub packages: Vec<String>,
    pub removal_count: usize,
}

#[derive(Debug)]
struct PendingProfileCleanupEntry {
    transaction_id: String,
    dsh_home: PathBuf,
    original_patch: Vec<u8>,
    replacement_patch: Vec<u8>,
    packages: Vec<String>,
    removal_count: usize,
}

/// Volatile, controller-local preview state.  It intentionally never enters
/// the profile or a persistent Desktop file: closing/restarting Desktop makes
/// the confirmation invalid, which is safer than replaying an old mutation.
#[derive(Default)]
pub struct PendingProfileCleanup(Mutex<Option<PendingProfileCleanupEntry>>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CleanupBlock {
    package_name: String,
    byte_start: usize,
    byte_end: usize,
}

#[derive(Debug)]
struct CleanupPlan {
    original_patch: Vec<u8>,
    replacement_patch: Vec<u8>,
    packages: Vec<String>,
    removal_count: usize,
}

/// Return bounded, read-only evidence of direct dependencies that are still
/// declared in `profiles/web/package.json` but have no materialized package
/// entry.  A symlink/reparse point is *not* considered missing here: only an
/// actual `NotFound` result qualifies, so the report cannot turn a surprising
/// filesystem shape into a deletion candidate.
pub fn report(dsh_home: &Path) -> ProfileConsistencyReport {
    let Ok((profile, manifest)) = crate::plugins::read_profile_manifest(dsh_home) else {
        return ProfileConsistencyReport::default();
    };
    let Ok(missing) = missing_direct_dependencies(&profile, &manifest) else {
        return ProfileConsistencyReport::default();
    };
    if missing.is_empty() {
        return ProfileConsistencyReport::default();
    }

    // An enabled bundle has an authoritative recovery path that pre-disables
    // the exact bundle and preserves a rollback journal. Removing only a
    // user-patch row cannot repair that startup boundary, so it is never an
    // eligible cleanup candidate. A malformed bundle list is similarly an
    // unknown activation state: report no deletion offer rather than trying
    // to infer ownership from broken profile metadata.
    let active = crate::plugins::profile_bundles(&manifest).ok();
    let inactive_missing = active
        .as_ref()
        .map(|active| {
            missing
                .iter()
                .filter(|package_name| !active.contains(package_name.as_str()))
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let cleanup_packages = match read_profile_patch(&profile)
        .and_then(|patch| cleanup_blocks(&patch, &inactive_missing).map(|blocks| (patch, blocks)))
    {
        Ok((_patch, blocks)) => blocks
            .into_iter()
            .map(|block| block.package_name)
            .collect::<HashSet<_>>(),
        Err(_) => HashSet::new(),
    };
    let mut issues = missing
        .into_iter()
        .take(MAX_ISSUES)
        .map(|package_name| ProfileConsistencyIssue {
            kind: "missingDependency".to_string(),
            active: active
                .as_ref()
                .is_some_and(|bundles| bundles.contains(package_name.as_str())),
            cleanup_available: cleanup_packages.contains(&package_name),
            package_name,
        })
        .collect::<Vec<_>>();
    issues.sort_by(|left, right| left.package_name.cmp(&right.package_name));
    let cleanup_eligible_count = issues
        .iter()
        .filter(|issue| issue.cleanup_available)
        .count();
    ProfileConsistencyReport {
        issues,
        cleanup_eligible_count,
    }
}

/// Capture a narrowly scoped cleanup plan after the caller has acquired the
/// profile-mutation gate.  This is a preview only: no user profile file is
/// written until `apply_cleanup` receives the opaque transaction id back.
pub fn preview_cleanup(
    dsh_home: &Path,
    state: &PendingProfileCleanup,
) -> Result<ProfileCleanupPreview, String> {
    let plan = build_cleanup_plan(dsh_home)?;
    let transaction_id = crate::secure_fs::random_suffix()?;
    let preview = ProfileCleanupPreview {
        transaction_id: transaction_id.clone(),
        packages: plan.packages.clone(),
        removal_count: plan.removal_count,
    };
    let mut slot = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(PendingProfileCleanupEntry {
        transaction_id,
        dsh_home: dsh_home.to_path_buf(),
        original_patch: plan.original_patch,
        replacement_patch: plan.replacement_patch,
        packages: plan.packages,
        removal_count: plan.removal_count,
    });
    Ok(preview)
}

/// Apply a previously previewed plan only if both the current DSH_HOME and
/// the exact original patch bytes still match.  Every mismatch invalidates
/// the volatile plan and requires a fresh review instead of trying to merge
/// user edits.
pub fn apply_cleanup(
    dsh_home: &Path,
    transaction_id: &str,
    state: &PendingProfileCleanup,
) -> Result<ProfileCleanupPreview, String> {
    if !is_transaction_id(transaction_id) {
        return Err("invalid profile cleanup confirmation".to_string());
    }
    let mut slot = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(pending) = slot.take() else {
        return Err("profile cleanup preview expired; review it again".to_string());
    };
    if pending.transaction_id != transaction_id || pending.dsh_home != dsh_home {
        // A caller with the wrong id must not invalidate the legitimate
        // controller's preview.  Restore the untouched slot in this case.
        *slot = Some(pending);
        return Err("profile cleanup confirmation does not match the reviewed preview".to_string());
    }

    let result = (|| {
        let profile = crate::plugins::profile_dir(dsh_home)?;
        if read_profile_patch(&profile)? != pending.original_patch {
            return Err(
                "web profile changed after cleanup review; no changes were made, review it again"
                    .to_string(),
            );
        }
        let plan = build_cleanup_plan(dsh_home)?;
        if plan.original_patch != pending.original_patch
            || plan.replacement_patch != pending.replacement_patch
            || plan.packages != pending.packages
            || plan.removal_count != pending.removal_count
        {
            return Err(
                "web profile changed after cleanup review; no changes were made, review it again"
                    .to_string(),
            );
        }
        crate::plugins::write_profile_patch_bytes(&profile, &pending.replacement_patch)?;
        Ok(ProfileCleanupPreview {
            transaction_id: pending.transaction_id.clone(),
            packages: pending.packages.clone(),
            removal_count: pending.removal_count,
        })
    })();
    // A matching confirmation is single-use even if publication fails.  This
    // avoids a retry accidentally applying an old replacement after a user
    // edits the profile or an uncertain filesystem failure.
    result
}

fn is_transaction_id(value: &str) -> bool {
    value.len() == 24
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn build_cleanup_plan(dsh_home: &Path) -> Result<CleanupPlan, String> {
    let (profile, manifest) = crate::plugins::read_profile_manifest(dsh_home)?;
    let missing = missing_direct_dependencies(&profile, &manifest)?;
    if missing.is_empty() {
        return Err("web profile has no missing direct dependencies to review".to_string());
    }
    let active = crate::plugins::profile_bundles(&manifest)?;
    let inactive_missing = missing
        .iter()
        .filter(|package_name| !active.contains(package_name.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    if inactive_missing.is_empty() {
        return Err(
            "missing web profile dependencies are enabled bundles; use safe plugin recovery instead"
                .to_string(),
        );
    }
    let original_patch = read_profile_patch(&profile)?;
    let blocks = cleanup_blocks(&original_patch, &inactive_missing)?;
    if blocks.is_empty() {
        return Err(
            "no exact Desktop-cleanable profile patch entries were found; review the profile manually"
                .to_string(),
        );
    }
    if blocks.len() > MAX_CLEANUP_BLOCKS {
        return Err("too many profile cleanup entries to review safely".to_string());
    }
    let replacement_patch = remove_blocks(&original_patch, &blocks)?;
    let packages = blocks
        .iter()
        .map(|block| block.package_name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(CleanupPlan {
        original_patch,
        replacement_patch,
        removal_count: blocks.len(),
        packages,
    })
}

fn missing_direct_dependencies(
    profile: &Path,
    manifest: &serde_json::Value,
) -> Result<BTreeSet<String>, String> {
    let dependencies = crate::plugins::profile_dependencies(manifest)?;
    let mut missing = BTreeSet::new();
    for (package_name, spec) in dependencies {
        if !crate::plugins::is_valid_package_name(package_name)
            || spec.as_str().is_none_or(str::is_empty)
        {
            continue;
        }
        if package_entry_is_absent(profile, package_name)? {
            missing.insert(package_name.clone());
        }
    }
    Ok(missing)
}

fn package_entry_is_absent(profile: &Path, package_name: &str) -> Result<bool, String> {
    let node_modules = profile.join("node_modules");
    let package = match package_name.split_once('/') {
        Some((scope, name)) => node_modules.join(scope).join(name),
        None => node_modules.join(package_name),
    };
    // Do not follow a package symlink/reparse point for a cleanup decision.
    // A dangling or surprising link may be a recoverable pnpm state; only a
    // genuinely absent directory entry can become a candidate.
    match std::fs::symlink_metadata(&package) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Ok(false),
    }
}

fn read_profile_patch(profile: &Path) -> Result<Vec<u8>, String> {
    let path = profile.join("cordis.patch.yml");
    crate::secure_fs::read_bounded(&path, crate::plugins::PROFILE_PATCH_MAX_BYTES)?
        .ok_or_else(|| "web profile cordis.patch.yml is missing".to_string())
}

/// Return only unambiguous top-level `- insert:` blocks.  This scanner is
/// intentionally *not* a YAML parser: parsing and re-serializing could
/// execute semantic tags in a future parser or erase comments/formatting.  A
/// block that uses any non-simple YAML construct is rejected rather than
/// being cleaned.
fn cleanup_blocks(bytes: &[u8], missing: &BTreeSet<String>) -> Result<Vec<CleanupBlock>, String> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| "web profile patch is not valid UTF-8; cleanup is unavailable".to_string())?;
    let lines = line_spans(source);
    let mut top_level = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line_text(source, line).starts_with("- ") {
            top_level.push(index);
        }
    }
    let mut blocks = Vec::new();
    for (position, start_line) in top_level.iter().enumerate() {
        let end_line = top_level.get(position + 1).copied().unwrap_or(lines.len());
        let start = lines[*start_line].0;
        let end = if end_line < lines.len() {
            lines[end_line].0
        } else {
            source.len()
        };
        let block = &source[start..end];
        let Some(package_name) = exact_insert_package(block, missing) else {
            continue;
        };
        blocks.push(CleanupBlock {
            package_name,
            byte_start: start,
            byte_end: end,
        });
    }
    Ok(blocks)
}

fn line_spans(source: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for segment in source.split_inclusive('\n') {
        let end = start + segment.len();
        spans.push((start, end));
        start = end;
    }
    if start < source.len() || source.is_empty() {
        spans.push((start, source.len()));
    }
    spans
}

fn line_text<'a>(source: &'a str, span: &(usize, usize)) -> &'a str {
    source[span.0..span.1]
        .strip_suffix('\n')
        .unwrap_or(&source[span.0..span.1])
        .strip_suffix('\r')
        .unwrap_or_else(|| source[span.0..span.1].trim_end_matches('\n'))
}

fn exact_insert_package(block: &str, missing: &BTreeSet<String>) -> Option<String> {
    // A package's patch can carry arbitrary user configuration.  Even a
    // confirmation dialog should not imply that Desktop understands or owns
    // that configuration, so retain only the canonical, configuration-free
    // three-line row emitted by the simplest bundle fixtures:
    //
    // - insert:
    //     - id: stable-id
    //       name: package-name
    //
    // Blank lines are formatting only; every other line rejects cleanup.
    let meaningful = block
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    if meaningful.len() != 3
        || meaningful[0] != "- insert:"
        || !meaningful[1].starts_with("    - id: ")
        || !meaningful[2].starts_with("      name: ")
    {
        return None;
    }
    let id = meaningful[1].strip_prefix("    - id: ")?;
    if !is_simple_patch_id(id) {
        return None;
    }
    let package_name = yaml_plain_or_quoted_scalar(meaningful[2].strip_prefix("      name: ")?)?;
    if !crate::plugins::is_valid_package_name(package_name) || !missing.contains(package_name) {
        return None;
    }
    Some(package_name.to_string())
}

fn is_simple_patch_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn yaml_plain_or_quoted_scalar(value: &str) -> Option<&str> {
    if value.is_empty() {
        return None;
    }
    match (value.as_bytes().first(), value.as_bytes().last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) if value.len() >= 2 => {
            let inner = &value[1..value.len() - 1];
            (!inner.is_empty() && !inner.contains(['\'', '"', '\\'])).then_some(inner)
        }
        (Some(b'\'' | b'"'), _) | (_, Some(b'\'' | b'"')) => None,
        _ if value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'/' | b'-' | b'_' | b'.')
        }) =>
        {
            Some(value)
        }
        _ => None,
    }
}

fn remove_blocks(original: &[u8], blocks: &[CleanupBlock]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(original.len());
    let mut cursor = 0;
    for block in blocks {
        if block.byte_start < cursor
            || block.byte_start >= block.byte_end
            || block.byte_end > original.len()
        {
            return Err("profile cleanup plan contains overlapping entries".to_string());
        }
        output.extend_from_slice(&original[cursor..block.byte_start]);
        cursor = block.byte_end;
    }
    output.extend_from_slice(&original[cursor..]);
    if output.len() as u64 > crate::plugins::PROFILE_PATCH_MAX_BYTES {
        return Err("cleaned web profile patch exceeds the reviewed size limit".to_string());
    }
    Ok(output)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::fs;

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dshd-profile-consistency-{name}-{}",
            crate::secure_fs::random_suffix().unwrap()
        ))
    }

    fn write_profile(home: &Path, dependencies: &str, patch: &str) -> PathBuf {
        let profile = home.join("profiles/web");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            profile.join("package.json"),
            format!(r#"{{"dependencies":{dependencies},"dsh":{{"profile":{{"bundles":[]}}}}}}"#),
        )
        .unwrap();
        fs::write(profile.join("cordis.patch.yml"), patch).unwrap();
        profile
    }

    #[test]
    fn report_finds_only_absent_direct_dependencies_and_marks_exact_cleanup() {
        let home = root("report");
        let profile = write_profile(
            &home,
            r#"{"gone":"1.0.0","present":"1.0.0"}"#,
            "- insert:\n    - id: gone\n      name: gone\n",
        );
        fs::create_dir_all(profile.join("node_modules/present")).unwrap();

        assert_eq!(
            report(&home),
            ProfileConsistencyReport {
                issues: vec![ProfileConsistencyIssue {
                    kind: "missingDependency".to_string(),
                    package_name: "gone".to_string(),
                    active: false,
                    cleanup_available: true,
                }],
                cleanup_eligible_count: 1,
            }
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn active_missing_bundle_is_reported_but_never_offered_patch_cleanup() {
        let home = root("active-bundle");
        let profile = home.join("profiles/web");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            profile.join("package.json"),
            r#"{"dependencies":{"gone":"1.0.0"},"dsh":{"profile":{"bundles":["gone"]}}}"#,
        )
        .unwrap();
        fs::write(
            profile.join("cordis.patch.yml"),
            "- insert:\n    - id: gone\n      name: gone\n",
        )
        .unwrap();

        assert_eq!(
            report(&home),
            ProfileConsistencyReport {
                issues: vec![ProfileConsistencyIssue {
                    kind: "missingDependency".to_string(),
                    package_name: "gone".to_string(),
                    active: true,
                    cleanup_available: false,
                }],
                cleanup_eligible_count: 0,
            }
        );
        assert!(preview_cleanup(&home, &PendingProfileCleanup::default())
            .unwrap_err()
            .contains("safe plugin recovery"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn cleanup_refuses_comments_multiple_names_and_user_configuration() {
        let missing = BTreeSet::from(["gone".to_string()]);
        assert!(cleanup_blocks(
            b"- insert:\n    # user note\n    - id: gone\n      name: gone\n",
            &missing
        )
        .unwrap()
        .is_empty());
        assert!(cleanup_blocks(
            b"- insert:\n    - id: gone\n      name: gone\n    - id: other\n      name: other\n",
            &missing
        )
        .unwrap()
        .is_empty());
        assert!(cleanup_blocks(
            b"- insert:\n    - id: gone\n      name: gone\n      config:\n        token: user-owned\n",
            &missing
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn cleanup_preview_is_explicit_and_invalidated_by_profile_drift() {
        let home = root("preview");
        let profile = write_profile(
            &home,
            r#"{"gone":"1.0.0"}"#,
            "- insert:\n    - id: gone\n      name: gone\n",
        );
        let state = PendingProfileCleanup::default();
        let preview = preview_cleanup(&home, &state).unwrap();
        assert_eq!(preview.packages, ["gone"]);
        fs::write(profile.join("cordis.patch.yml"), "# changed by user\n").unwrap();
        assert!(apply_cleanup(&home, &preview.transaction_id, &state)
            .unwrap_err()
            .contains("changed after cleanup review"));
        assert_eq!(
            fs::read_to_string(profile.join("cordis.patch.yml")).unwrap(),
            "# changed by user\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn cleanup_confirmation_writes_only_the_reviewed_block() {
        let home = root("apply");
        let profile = write_profile(
            &home,
            r#"{"gone":"1.0.0"}"#,
            "# keep this comment\n- insert:\n    - id: gone\n      name: gone\n- insert:\n    - id: live\n      name: live\n",
        );
        let state = PendingProfileCleanup::default();
        let preview = preview_cleanup(&home, &state).unwrap();
        assert_eq!(preview.removal_count, 1);
        apply_cleanup(&home, &preview.transaction_id, &state).unwrap();
        assert_eq!(
            fs::read_to_string(profile.join("cordis.patch.yml")).unwrap(),
            "# keep this comment\n- insert:\n    - id: live\n      name: live\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn wrong_confirmation_id_does_not_consume_the_reviewed_preview() {
        let home = root("wrong-id");
        let profile = write_profile(
            &home,
            r#"{"gone":"1.0.0"}"#,
            "- insert:\n    - id: gone\n      name: gone\n",
        );
        let state = PendingProfileCleanup::default();
        let preview = preview_cleanup(&home, &state).unwrap();
        let mut wrong = preview.transaction_id.clone();
        let first = if wrong.starts_with('0') { '1' } else { '0' };
        wrong.replace_range(..1, &first.to_string());
        assert!(apply_cleanup(&home, &wrong, &state)
            .unwrap_err()
            .contains("does not match"));
        apply_cleanup(&home, &preview.transaction_id, &state).unwrap();
        assert_eq!(
            fs::read_to_string(profile.join("cordis.patch.yml")).unwrap(),
            ""
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_direct_package_link_is_reported_for_manual_repair_only() {
        use std::os::unix::fs::symlink;

        let home = root("dangling-link");
        let profile = write_profile(
            &home,
            r#"{"gone":"1.0.0"}"#,
            "- insert:\n    - id: gone\n      name: gone\n",
        );
        fs::create_dir_all(profile.join("node_modules")).unwrap();
        symlink(
            profile.join("not-a-package"),
            profile.join("node_modules/gone"),
        )
        .unwrap();

        // A pnpm link can be transient during repair. It is not proof that
        // Desktop owns a stale entry, so a link — even a dangling one — is
        // never offered as a deletion candidate.
        assert_eq!(report(&home), ProfileConsistencyReport::default());
        fs::remove_dir_all(home).unwrap();
    }
}
